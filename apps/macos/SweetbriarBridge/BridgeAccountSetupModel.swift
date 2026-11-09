// `BridgeAccountSetupModel`: the Proton Bridge onboarding sheet's state
// (plan M2.3; spec/04 "Bridge", spec/10 "TLS and the Bridge
// certificate"), free of AppKit so the bridge tests exercise it.
//
//   fields (Bridge's defaults prefilled) ─┐
//   certificate: imported cert.pem ───────┼─► submit ─► added
//            or presented ─► confirmed ───┘      │       └ needsAttention?
//                                                 └─► failed (field?)
//
// A presented fingerprint is never trusted until the user confirms it,
// and changing the IMAP endpoint drops it. One idempotency key serves
// the sheet's whole life, so a resubmit after a lost answer replays the
// first account instead of adding a second (spec/07).

import Foundation
import Observation

@MainActor
@Observable
public final class BridgeAccountSetupModel {
    /// Bridge's default IMAP and SMTP ports on `127.0.0.1` (spec/04).
    public static let defaultImapPort: UInt16 = 1143
    public static let defaultSmtpPort: UInt16 = 1025
    /// Bridge's exported cert.pem is a few kilobytes; anything far
    /// larger is not it, and is refused before it is read.
    public static let maxCertificateFileBytes = 64 * 1024

    /// Where the certificate the account will pin comes from (spec/10).
    public enum Certificate: Sendable, Equatable {
        case none
        /// From Bridge's exported `cert.pem`: the preferred path.
        case imported(fingerprint: String)
        /// What Bridge presents, shown for confirmation; not trusted.
        case presented(fingerprint: String)
        /// A presented fingerprint the user confirmed.
        case confirmed(fingerprint: String)
    }

    /// Where submission stands.
    public enum Phase: Sendable, Equatable {
        case editing
        case submitting
        /// The account exists; `needsAttention` when its password could
        /// not be stored (it waits in `needs_attention`, nothing lost).
        case added(accountId: String, needsAttention: CredentialPause?)
    }

    public var displayName = "Proton Mail"
    /// The username Bridge shows (an address).
    public var username = ""
    /// The Bridge-generated password; cleared once the account exists.
    public var password = ""
    public var imapPort: UInt16 = BridgeAccountSetupModel.defaultImapPort {
        didSet { if imapPort != oldValue { endpointChanged() } }
    }
    public var imapSecurity: EndpointSecurity = .startTls {
        didSet { if imapSecurity != oldValue { endpointChanged() } }
    }
    public var smtpPort: UInt16 = BridgeAccountSetupModel.defaultSmtpPort
    public var smtpSecurity: EndpointSecurity = .startTls
    /// The From address; empty means the username.
    public var email = ""
    /// The name shown in From, trimmed; empty sends the address alone.
    public var senderName = ""

    public private(set) var certificate: Certificate = .none
    public private(set) var phase: Phase = .editing
    /// Why the last certificate step failed, for the certificate row.
    public private(set) var certificateError: String?
    /// The last submission failure.
    public private(set) var failure: CoreFailure?
    /// True while Bridge's presented certificate is being read.
    public private(set) var readingCertificate = false

    /// The sheet's one idempotency key (spec/07).
    public let idempotencyKey: String

    private let source: any AccountSetupSource

    public init(source: any AccountSetupSource, idempotencyKey: String = UUID().uuidString) {
        self.source = source
        self.idempotencyKey = idempotencyKey
    }

    /// The fingerprint the account would pin: an imported or a confirmed
    /// one, never one merely presented.
    public var trustedFingerprint: String? {
        switch certificate {
        case .imported(let fingerprint), .confirmed(let fingerprint): fingerprint
        case .none, .presented: nil
        }
    }

    /// What is still missing before Add can run, in field order; empty
    /// when the sheet can submit.
    public var missing: [String] {
        var missing: [String] = []
        if displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            missing.append("a name")
        }
        if username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            missing.append("the username")
        }
        if password.isEmpty { missing.append("the Bridge password") }
        if imapPort == 0 || smtpPort == 0 { missing.append("the ports") }
        if fromAddress.isEmpty || !fromAddress.contains("@") { missing.append("an email address") }
        if trustedFingerprint == nil { missing.append("a trusted certificate") }
        return missing
    }

    public var canSubmit: Bool { phase == .editing && missing.isEmpty }

    /// The message for `field` when the last submission named it.
    public func failure(for field: String) -> String? {
        guard case .invalidInput(let failed, let message) = failure, failed == field else {
            return nil
        }
        return message
    }

    /// spec/10's preferred path: trust the one certificate in the
    /// `cert.pem` the user picked. The caller reads the file.
    public func importCertificate(pem: Data) {
        do {
            certificate = .imported(fingerprint: try source.certificateFingerprint(pem: pem))
            certificateError = nil
        } catch {
            certificateError = Self.describe(error)
        }
    }

    /// The cert.pem the user picked. spec/10: Sweetbriar never reads
    /// key.pem, so a file by that name is refused before it is opened
    /// (the core also refuses any file holding a private key), as is a
    /// file too large to be Bridge's certificate.
    public func importCertificate(fileAt url: URL) {
        // Resolve links first, so a cert.pem linking to key.pem is
        // still refused by name, unopened.
        let resolved = url.resolvingSymlinksInPath()
        if resolved.lastPathComponent.lowercased() == "key.pem" {
            certificateError = "That is Bridge's private key. Choose cert.pem instead; Sweetbriar never reads key.pem."
            return
        }
        do {
            let values = try resolved.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
            // A FIFO or device would block the read on the main actor,
            // and an unknown size is no bound at all.
            guard values.isRegularFile == true, let size = values.fileSize else {
                certificateError = "Choose the cert.pem file Bridge exported."
                return
            }
            guard size <= Self.maxCertificateFileBytes else {
                certificateError = "That file is too large to be Bridge's cert.pem."
                return
            }
            importCertificate(pem: try Data(contentsOf: resolved))
        } catch {
            certificateError = "The file couldn't be read: \(error.localizedDescription)"
        }
    }

    /// A fingerprint as `openssl x509 -fingerprint -sha256` prints it:
    /// uppercase pairs joined by colons, so the user can compare.
    nonisolated public static func displayFingerprint(_ hex: String) -> String {
        var pairs: [String] = []
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2, limitedBy: hex.endIndex) ?? hex.endIndex
            pairs.append(hex[index..<next].uppercased())
            index = next
        }
        return pairs.joined(separator: ":")
    }

    /// spec/10's fallback: read what Bridge presents on the IMAP
    /// endpoint, to show for confirmation. Nothing is trusted yet.
    public func readPresentedCertificate() async {
        guard !readingCertificate else { return }
        readingCertificate = true
        defer { readingCertificate = false }
        let port = imapPort
        let security = imapSecurity
        let before = certificate
        do {
            let fingerprint = try await source.bridgeCertificate(port: port, security: security)
            // The endpoint or the certificate choice may have changed
            // while the probe ran (an imported cert.pem, a
            // confirmation): a stale answer never replaces either.
            guard port == imapPort, security == imapSecurity, certificate == before else { return }
            certificate = .presented(fingerprint: fingerprint)
            certificateError = nil
        } catch is CancellationError {
            return
        } catch {
            certificateError = Self.describe(error)
        }
    }

    /// The user confirmed the presented fingerprint matches Bridge's.
    public func confirmPresented() {
        guard case .presented(let fingerprint) = certificate else { return }
        certificate = .confirmed(fingerprint: fingerprint)
    }

    /// Forget the certificate choice.
    public func clearCertificate() {
        certificate = .none
        certificateError = nil
    }

    /// Add the account. A failure leaves the fields for correction; the
    /// same idempotency key is reused, so a lost answer replays.
    public func submit() async {
        guard canSubmit, let fingerprint = trustedFingerprint else { return }
        phase = .submitting
        failure = nil
        let request = BridgeAccountRequest(
            displayName: displayName.trimmingCharacters(in: .whitespacesAndNewlines),
            username: username.trimmingCharacters(in: .whitespacesAndNewlines),
            password: Data(password.utf8),
            imapPort: imapPort,
            imapSecurity: imapSecurity,
            smtpPort: smtpPort,
            smtpSecurity: smtpSecurity,
            certificateSha256: fingerprint,
            addressMode: .combined,
            identities: [
                BridgeIdentity(
                    email: fromAddress,
                    displayName: senderName.trimmingCharacters(in: .whitespacesAndNewlines),
                    isDefault: true,
                    sendable: .provisional
                )
            ]
        )
        do {
            let added = try await source.addBridgeAccount(
                idempotencyKey: idempotencyKey, request: request
            )
            password = ""
            phase = .added(accountId: added.accountId, needsAttention: added.needsAttention)
        } catch is CancellationError {
            phase = .editing
        } catch let error as CoreFailure {
            failure = error
            phase = .editing
        } catch {
            failure = .internalError(detail: error.localizedDescription)
            phase = .editing
        }
    }

    /// The text for an account added without its password (spec/10).
    nonisolated public static func explain(_ pause: CredentialPause) -> String {
        switch pause {
        case .storeUnavailable:
            "The account was added, but the Keychain was unavailable, so its password isn't stored yet."
        case .storeRefused:
            "The account was added, but the Keychain refused its password."
        case .notStored, .storeFailed:
            "The account was added, but its password couldn't be stored."
        }
    }

    private var fromAddress: String {
        let email = email.trimmingCharacters(in: .whitespacesAndNewlines)
        return email.isEmpty ? username.trimmingCharacters(in: .whitespacesAndNewlines) : email
    }

    /// A presented or confirmed fingerprint belongs to the endpoint it
    /// was read from; an imported `cert.pem` does not depend on it.
    private func endpointChanged() {
        switch certificate {
        case .presented, .confirmed: certificate = .none
        case .none, .imported: break
        }
    }

    private static func describe(_ error: any Error) -> String {
        (error as? CoreFailure)?.errorDescription ?? error.localizedDescription
    }
}
