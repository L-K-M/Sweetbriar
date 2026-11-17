// `ProtonSignInForm`: what the Proton sign-in sheet's fields hold
// (plan M2.14b; spec/08 "Proton sign-in"), free of AppKit so the bridge
// tests exercise it. Each step's secret field is emptied the moment the
// step is sent; the rest of what was typed stays for the next attempt.
// A `String` can't be zeroed, so the field's own storage is only
// dropped; the bytes the model receives are zeroed (spec/10).

import Foundation
import Observation

@MainActor
@Observable
public final class ProtonSignInForm {
    /// Proton's settings page with two-factor authentication, for an
    /// account whose only second factor is a security key (spec/08).
    public nonisolated static let twoFactorSettingsURL = URL(string: "https://account.proton.me/mail/account-password")!

    /// The length of Proton's TOTP codes.
    public nonisolated static let codeLength = 6

    public var displayName: String
    /// Fixed when signing in again (spec/08); the sheet shows it read-only.
    public var username: String
    public var password = ""
    /// What was typed or pasted; the sheet's binding keeps it to
    /// `normalizedCode`, and only that is ever sent.
    public var code = ""
    public var mailboxPassword = ""

    public let model: ProtonSignInModel

    /// A new account, or `account` signed in again with its username
    /// fixed and its name kept.
    public init(source: any ProtonSignInSource, signingInAgain account: Account? = nil) {
        displayName = account?.displayName ?? ""
        username = account?.username ?? ""
        model = ProtonSignInModel(
            source: source, accountId: account?.accountId, username: account?.username ?? ""
        )
    }

    /// Whether the current step has what it needs and no call runs.
    public var canSubmit: Bool {
        guard !model.isBusy else { return false }
        switch model.step {
        case .credentials:
            return !Self.trimmed(displayName).isEmpty && !Self.trimmed(username).isEmpty
                && !password.isEmpty
        case .secondFactor:
            return Self.normalizedCode(code).count == Self.codeLength
        case .mailboxPassword:
            return !mailboxPassword.isEmpty
        case .humanVerification, .done:
            return false
        }
    }

    /// Send the current step, emptying its secret field first.
    public func submit() async {
        guard canSubmit else { return }
        switch model.step {
        case .credentials:
            var bytes = Data(password.utf8)
            password = ""
            await model.begin(
                displayName: Self.trimmed(displayName), username: Self.trimmed(username),
                password: &bytes
            )
        case .secondFactor:
            let entered = Self.normalizedCode(code)
            code = ""
            await model.submitCode(entered)
        case .mailboxPassword:
            var bytes = Data(mailboxPassword.utf8)
            mailboxPassword = ""
            await model.submitMailboxPassword(&bytes)
        case .humanVerification, .done:
            return
        }
    }

    /// The token the verification page handed back.
    public func verify(token: String, tokenType: String) async {
        guard case .humanVerification = model.step else { return }
        await model.submitVerification(token: token, tokenType: tokenType)
    }

    /// Close the conversation and empty every secret field.
    public func cancel() async {
        password = ""
        code = ""
        mailboxPassword = ""
        await model.cancel()
    }

    /// A pasted or typed code: its digits, at most six ("123 456" and
    /// "123-456" both paste).
    public nonisolated static func normalizedCode(_ text: String) -> String {
        String(text.filter(\.isASCIIDigit).prefix(codeLength))
    }

    private nonisolated static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

private extension Character {
    var isASCIIDigit: Bool { isASCII && isNumber }
}
