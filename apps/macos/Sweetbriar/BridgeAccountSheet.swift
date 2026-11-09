// File → Add Proton Bridge Account… (plan M2.3; spec/04 "Bridge",
// spec/10 "TLS and the Bridge certificate"): a SwiftUI sheet over the
// main window (spec/08: onboarding is a SwiftUI island). Bridge's
// host and ports are prefilled; the user copies the username and the
// Bridge-generated password, and trusts Bridge's certificate either
// from its exported cert.pem or by confirming the fingerprint Bridge
// presents. Adding the account is the connection test: the core logs
// in before it stores anything.

import AppKit
import SwiftUI
import UniformTypeIdentifiers
import SweetbriarBridge

/// Presents the setup sheet on `window` and reports an added account.
@MainActor
final class BridgeAccountSheet {
    /// Called after an account was added, so the sidebar can show it.
    var onAdded: @MainActor () -> Void = {}

    private weak var window: NSWindow?
    private let source: any AccountSetupSource
    private var sheet: NSWindow?

    init(window: NSWindow, source: any AccountSetupSource) {
        self.window = window
        self.source = source
    }

    /// Show the sheet, or bring an open one forward.
    func begin() {
        if let sheet {
            // A sheet still attached comes forward; one that somehow
            // left its parent is dropped and rebuilt.
            if sheet.sheetParent != nil {
                sheet.makeKeyAndOrderFront(nil)
                return
            }
            self.sheet = nil
        }
        guard let window else { return }
        let model = BridgeAccountSetupModel(source: source)
        let view = BridgeAccountSetupView(
            model: model,
            chooseCertificate: { [weak self] in self?.chooseCertificate(into: model) },
            finish: { [weak self] added in self?.end(added: added) }
        )
        let sheet = NSWindow(contentViewController: NSHostingController(rootView: view))
        sheet.title = "Add Proton Bridge Account"
        // Only Cancel and Done end the sheet, so its bookkeeping and
        // the sidebar refresh always run; no close box or Cmd-W.
        sheet.styleMask.remove([.closable, .miniaturizable])
        self.sheet = sheet
        window.beginSheet(sheet)
    }

    private func end(added: Bool) {
        guard let sheet else { return }
        window?.endSheet(sheet)
        self.sheet = nil
        if added { onAdded() }
    }

    /// spec/10's preferred path: the user picks the cert.pem Bridge
    /// exported. The model refuses key.pem before reading it.
    private func chooseCertificate(into model: BridgeAccountSetupModel) {
        guard let sheet else { return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = [UTType(filenameExtension: "pem") ?? .data]
        panel.message = "Choose the cert.pem Bridge exported (Settings → Advanced → Export TLS certificates)."
        panel.prompt = "Trust Certificate"
        panel.beginSheetModal(for: sheet) { response in
            guard response == .OK, let url = panel.url else { return }
            MainActor.assumeIsolated {
                model.importCertificate(fileAt: url)
            }
        }
    }
}

/// The form. Errors the core names a field for sit by that field; the
/// rest show above the buttons.
struct BridgeAccountSetupView: View {
    @Bindable var model: BridgeAccountSetupModel
    let chooseCertificate: () -> Void
    let finish: (_ added: Bool) -> Void

    /// Room for every form row without scrolling at the default size.
    private static let formMinimumHeight: CGFloat = 470

    /// Fields whose failures show inline rather than at the bottom.
    private static let inlineFields: Set<String> = [
        "display_name", "username", "password", "imap_port", "smtp_port",
        "imap_security", "identities", "pin", "certificate_sha256",
    ]

    var body: some View {
        Group {
            if case .added(_, let needsAttention) = model.phase {
                added(needsAttention)
            } else {
                form
            }
        }
        .padding(20)
        .frame(width: 540)
    }

    private var form: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Proton Mail Bridge must be running. Copy the username and password from Bridge's mailbox settings: the password is the one Bridge generated, not your Proton password.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Form {
                Section("Account") {
                    TextField("Name", text: $model.displayName)
                    inline("display_name")
                    TextField("Username", text: $model.username)
                        .textContentType(.username)
                    inline("username")
                    SecureField("Bridge password", text: $model.password)
                    inline("password")
                    TextField(
                        "Email address", text: $model.email,
                        prompt: Text(model.username.isEmpty ? "Same as the username" : model.username)
                    )
                    TextField("Your name", text: $model.senderName, prompt: Text("Shown in From"))
                    inline("identities")
                }
                Section("Connection to 127.0.0.1") {
                    endpoint("IMAP", port: $model.imapPort, security: $model.imapSecurity)
                    inline("imap_port")
                    inline("imap_security")
                    endpoint("SMTP", port: $model.smtpPort, security: $model.smtpSecurity)
                    inline("smtp_port")
                }
                Section("Bridge's certificate") {
                    certificateRow
                    if let error = model.certificateError {
                        Text(error).foregroundStyle(.red).font(.callout)
                    }
                    inline("pin")
                    inline("certificate_sha256")
                    HStack {
                        Button("Import cert.pem…", action: chooseCertificate)
                        Button("Read From Bridge") {
                            Task { await model.readPresentedCertificate() }
                        }
                        .disabled(model.readingCertificate)
                        if model.readingCertificate { ProgressView().controlSize(.small) }
                    }
                }
            }
            .formStyle(.grouped)
            // A grouped Form scrolls, so its ideal height is tiny and a
            // sheet sized to it collapses; give it room for every row.
            .frame(minHeight: Self.formMinimumHeight)

            if let failure = model.failure, !isInline(failure) {
                Text(failure.errorDescription ?? "The account couldn't be added.")
                    .foregroundStyle(.red)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if !model.missing.isEmpty {
                Text("Still needed: \(model.missing.joined(separator: ", ")).")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                // A begun command runs to completion (spec/07), so the
                // sheet waits for its answer rather than dropping it.
                Button("Cancel") { finish(false) }
                    .keyboardShortcut(.cancelAction)
                    .disabled(model.phase == .submitting)
                Button("Add Account") {
                    Task { await model.submit() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!model.canSubmit)
                if model.phase == .submitting { ProgressView().controlSize(.small) }
            }
        }
    }

    @ViewBuilder
    private var certificateRow: some View {
        switch model.certificate {
        case .none:
            Text("Not trusted yet. Import the cert.pem Bridge exported, or read the certificate Bridge presents and confirm its fingerprint.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        case .imported(let fingerprint):
            fingerprintRow("Trusted from cert.pem", fingerprint)
        case .presented(let fingerprint):
            VStack(alignment: .leading, spacing: 6) {
                fingerprintRow("Bridge presents this certificate", fingerprint)
                Text("Trust it only if this SHA-256 fingerprint matches your Bridge's certificate.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                Button("Trust This Certificate") { model.confirmPresented() }
            }
        case .confirmed(let fingerprint):
            fingerprintRow("Trusted after confirmation", fingerprint)
        }
    }

    private func fingerprintRow(_ title: String, _ fingerprint: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
            Text(BridgeAccountSetupModel.displayFingerprint(fingerprint))
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
        }
    }

    /// One protocol's port and security on a single row.
    private func endpoint(
        _ title: String, port: Binding<UInt16>, security: Binding<EndpointSecurity>
    ) -> some View {
        LabeledContent(title) {
            HStack {
                TextField("\(title) port", value: port, format: .number.grouping(.never))
                    .labelsHidden()
                    .multilineTextAlignment(.trailing)
                    .frame(width: 70)
                Picker("\(title) security", selection: security) {
                    Text("STARTTLS").tag(EndpointSecurity.startTls)
                    Text("SSL").tag(EndpointSecurity.tls)
                }
                .labelsHidden()
                .fixedSize()
            }
        }
    }

    @ViewBuilder
    private func inline(_ field: String) -> some View {
        if let message = model.failure(for: field) {
            Text(message).foregroundStyle(.red).font(.callout)
        }
    }

    private func isInline(_ failure: CoreFailure) -> Bool {
        guard case .invalidInput(let field, _) = failure else { return false }
        return Self.inlineFields.contains(field)
    }

    private func added(_ needsAttention: CredentialPause?) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Account added").font(.title3)
            if let needsAttention {
                Text(BridgeAccountSetupModel.explain(needsAttention))
                    .fixedSize(horizontal: false, vertical: true)
                Text("It shows as needing attention until the password is stored.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            } else {
                Text("Sweetbriar connected to Bridge and stored the password in your Keychain.")
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button("Done") { finish(true) }.keyboardShortcut(.defaultAction)
            }
        }
    }
}
