// File → Add Proton Account… and the sidebar's Sign In Again… (plan
// M2.14b; spec/08 "Proton sign-in", spec/10 "The Proton API"): a
// SwiftUI sheet over the main window driving `ProtonSignInForm`, with
// human verification in a child sheet. Only Cancel and Done end it, so
// the conversation is always cancelled or finished at the core.

import AppKit
import SwiftUI
import SweetbriarBridge

/// Presents the sign-in sheet on `window` and reports a signed-in account.
@MainActor
final class ProtonSignInSheet {
    /// Called after a sign-in completed, so the sidebar can show it.
    var onAdded: @MainActor () -> Void = {}

    private weak var window: NSWindow?
    private let source: any ProtonSignInSource
    private let writes: ArchiveWriteTasks
    private let canWrite: @MainActor () -> Bool
    private var sheet: NSWindow?
    private var form: ProtonSignInForm?
    private var verification: HumanVerificationSheet?

    init(window: NSWindow, source: any ProtonSignInSource, writes: ArchiveWriteTasks,
         canWrite: @escaping @MainActor () -> Bool) {
        self.window = window
        self.source = source
        self.writes = writes
        self.canWrite = canWrite
    }

    /// Show the sheet for a new account, or for `account` signed in
    /// again; an open sheet comes forward instead.
    func begin(signingInAgain account: Account? = nil) {
        guard canWrite() else { return }
        if let sheet {
            if sheet.sheetParent != nil {
                sheet.makeKeyAndOrderFront(nil)
                return
            }
            // Ended from outside: its conversation must not finish into
            // the new sheet.
            closeVerification()
            if let stale = form { Task { await stale.cancel() } }
            form = nil
            self.sheet = nil
        }
        guard let window else { return }
        let form = ProtonSignInForm(source: source, signingInAgain: account)
        let view = ProtonSignInView(
            form: form, writes: writes, revision: writes.revision, canWrite: canWrite,
            verify: { [weak self] url in self?.openVerification(url) },
            finish: { [weak self] added in self?.end(added: added) }
        )
        let sheet = NSWindow(contentViewController: NSHostingController(rootView: view))
        sheet.title = account == nil ? "Add Proton Account" : "Sign In Again"
        sheet.styleMask.remove([.closable, .miniaturizable])
        self.sheet = sheet
        self.form = form
        window.beginSheet(sheet)
    }

    /// The archive is being replaced: drop the conversation and the
    /// sheets without reporting an account.
    func retire() {
        onAdded = {}
        end(added: false)
    }

    private func end(added: Bool) {
        closeVerification()
        if let form, !added {
            Task { await form.cancel() }
        }
        form = nil
        guard let sheet else { return }
        window?.endSheet(sheet)
        self.sheet = nil
        if added { onAdded() }
    }

    private func openVerification(_ urlString: String) {
        guard let sheet, verification == nil, let form,
              let url = HumanVerificationPolicy.startURL(urlString)
        else { return }
        let verification = HumanVerificationSheet(url: url) { [weak self] message in
            self?.received(message, for: form)
        }
        self.verification = verification
        verification.present(on: sheet)
    }

    private func received(_ message: HumanVerificationMessage, for form: ProtonSignInForm) {
        switch message {
        case .verified(let token, let tokenType):
            closeVerification()
            writes.run(revision: writes.revision) { await form.verify(token: token, tokenType: tokenType) }
        case .closed:
            closeVerification()
        case .failed:
            // The page shows its own error and offers another try.
            break
        }
    }

    private func closeVerification() {
        verification?.close()
        verification = nil
    }
}

/// The steps. Refusals sit by the field they name; Proton's other
/// answers show above the buttons.
struct ProtonSignInView: View {
    @Bindable var form: ProtonSignInForm
    let writes: ArchiveWriteTasks
    let revision: UInt64
    let canWrite: @MainActor () -> Bool
    let verify: (String) -> Void
    let finish: (_ added: Bool) -> Void

    private var model: ProtonSignInModel { form.model }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch model.step {
            case .credentials:
                credentials
            case .secondFactor:
                secondFactor
            case .mailboxPassword:
                mailboxPassword
            case .humanVerification(let url, _):
                humanVerification(url)
            case .done:
                done
            }
            if case .done = model.step {} else { buttons }
        }
        .padding(20)
        .frame(width: 460)
        .onChange(of: model.step, initial: true) { _, step in
            if case .humanVerification(let url, _) = step { verify(url) }
        }
    }

    private var credentials: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Sweetbriar talks to Proton directly. Proton doesn't officially support third-party apps, may ask you to verify that you're human, and could block this access. Your password is used to sign in and is never stored.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Form {
                TextField("Name", text: $form.displayName, prompt: Text("Shown in the sidebar"))
                if model.isSigningInAgain {
                    LabeledContent("Username", value: form.username)
                } else {
                    TextField("Username", text: $form.username, prompt: Text("Proton address or username"))
                        .textContentType(.username)
                }
                SecureField("Password", text: $form.password)
                    .textContentType(.password)
                    .onSubmit(submit)
                if case .password(let message) = model.inlineError { refusal(message) }
            }
            .formStyle(.grouped)
            .frame(minHeight: 170)
            accountRefusal
        }
    }

    private var secondFactor: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Enter the six-digit code from your authenticator app.")
            TextField("Code", text: Binding(
                get: { form.code },
                set: { form.code = ProtonSignInForm.normalizedCode($0) }
            ))
            .font(.system(.title2, design: .monospaced))
            .textContentType(.oneTimeCode)
            .onSubmit(submit)
            if case .secondFactor(let message) = model.inlineError { refusal(message) }
        }
    }

    private var mailboxPassword: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("This account has a separate mailbox password. Sweetbriar uses it once to unlock your keys and never stores it.")
                .fixedSize(horizontal: false, vertical: true)
            SecureField("Mailbox password", text: $form.mailboxPassword)
                .onSubmit(submit)
            if case .mailboxPassword(let message) = model.inlineError { refusal(message) }
        }
    }

    @ViewBuilder
    private func humanVerification(_ url: String) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            if HumanVerificationPolicy.startURL(url) == nil {
                refusal("Proton asked for verification at an address that isn't Proton's verification page, so Sweetbriar won't open it. Cancel and try again later.")
            } else {
                Text("Proton asks you to verify that you're human. The verification page opens in its own sheet; when it's done, signing in continues.")
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open Verification…") { verify(url) }
                    .disabled(model.isBusy)
            }
        }
    }

    private var done: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(model.isSigningInAgain ? "Signed in again" : "Account added").font(.title3)
            Text("Sweetbriar stored the session in your Keychain. It never stores your password.")
                .fixedSize(horizontal: false, vertical: true)
            HStack {
                Spacer()
                Button("Done") { finish(true) }.keyboardShortcut(.defaultAction)
            }
        }
    }

    /// Refusals that name no field of this step, the security-key one
    /// with its way out (spec/08).
    @ViewBuilder
    private var accountRefusal: some View {
        switch model.inlineError {
        case .securityKey(let message):
            refusal(message)
            Link("Open Proton's Two-Factor Settings", destination: ProtonSignInForm.twoFactorSettingsURL)
        case .account(let message):
            refusal(message)
        default:
            EmptyView()
        }
    }

    private var buttons: some View {
        VStack(alignment: .leading, spacing: 12) {
            if model.step != .credentials { accountRefusal }
            if case .unavailable(let message) = model.inlineError { refusal(message) }
            HStack {
                Spacer()
                // A cancel abandons a call in flight: its answer changes
                // nothing, and a conversation it opened is dropped.
                Button("Cancel") { finish(false) }
                    .keyboardShortcut(.cancelAction)
                Button(model.step == .credentials ? "Sign In" : "Continue", action: submit)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!form.canSubmit || !canWrite())
                if model.isBusy { ProgressView().controlSize(.small) }
            }
        }
    }

    private func refusal(_ message: String) -> some View {
        Text(message)
            .foregroundStyle(.red)
            .font(.callout)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func submit() {
        guard form.canSubmit, canWrite() else { return }
        writes.run(revision: revision) { await form.submit() }
    }
}
