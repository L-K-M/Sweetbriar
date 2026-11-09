// Proton Bridge account setup across the L1 boundary (spec/07
// "Bridge account setup", spec/10 "TLS and the Bridge certificate"):
// the bridge's own types, their mapping onto the generated ones, and
// the three calls the onboarding sheet makes. App code never imports
// `SweetbriarCore`.

import Foundation
import SweetbriarCore

/// `imap_security` / `smtp_security`: `tls` is Bridge's "SSL"
/// (implicit TLS), `startTls` its default (spec/04).
public enum EndpointSecurity: Sendable, Equatable, CaseIterable {
    case tls
    case startTls
}

/// Bridge's address mode (spec/02 `imap_endpoints.address_mode`).
public enum AddressMode: Sendable, Equatable {
    case combined
    case split
}

/// What the From list may offer for an identity (spec/02).
public enum IdentitySendable: Sendable, Equatable {
    case validated
    case provisional
    case unavailable
}

/// Why an added account waits for its credential (spec/10): the
/// secret store could not keep the password. The account exists, in
/// `needs_attention`; nothing was deleted.
public enum CredentialPause: Sendable, Equatable {
    case notStored
    case storeUnavailable
    case storeRefused
    case storeFailed
}

/// One from-address offered at setup (spec/02 `identities`).
public struct BridgeIdentity: Sendable, Equatable {
    public var email: String
    public var displayName: String
    public var isDefault: Bool
    public var sendable: IdentitySendable

    public init(email: String, displayName: String, isDefault: Bool, sendable: IdentitySendable) {
        self.email = email
        self.displayName = displayName
        self.isDefault = isDefault
        self.sendable = sendable
    }
}

/// `add_bridge_account`'s parameters (spec/07). The host is always
/// `127.0.0.1` (spec/10), so none is taken. `password` is the
/// Bridge-generated password as UTF-8 bytes, never the Proton password.
public struct BridgeAccountRequest: Sendable, Equatable {
    public var displayName: String
    public var username: String
    public var password: Data
    public var imapPort: UInt16
    public var imapSecurity: EndpointSecurity
    public var smtpPort: UInt16
    public var smtpSecurity: EndpointSecurity
    /// The trusted certificate's DER SHA-256 as 64 hex digits.
    public var certificateSha256: String
    public var addressMode: AddressMode?
    public var identities: [BridgeIdentity]

    public init(
        displayName: String, username: String, password: Data,
        imapPort: UInt16, imapSecurity: EndpointSecurity,
        smtpPort: UInt16, smtpSecurity: EndpointSecurity,
        certificateSha256: String, addressMode: AddressMode?,
        identities: [BridgeIdentity]
    ) {
        self.displayName = displayName
        self.username = username
        self.password = password
        self.imapPort = imapPort
        self.imapSecurity = imapSecurity
        self.smtpPort = smtpPort
        self.smtpSecurity = smtpSecurity
        self.certificateSha256 = certificateSha256
        self.addressMode = addressMode
        self.identities = identities
    }
}

extension BridgeAccountRequest: CustomStringConvertible, CustomDebugStringConvertible {
    /// Never the password (spec/10): logs and test failures print this.
    public var description: String {
        "BridgeAccountRequest(\(displayName), \(username), password: [redacted], "
            + "imap: \(imapPort) \(imapSecurity), smtp: \(smtpPort) \(smtpSecurity), "
            + "certificate: \(certificateSha256), identities: \(identities.count))"
    }

    public var debugDescription: String { description }
}

/// The answer to `add_bridge_account`, first run or replay alike.
public struct BridgeAccountAdded: Sendable, Equatable {
    public var receipt: Receipt
    public var accountId: String
    /// Set when the account waits for its credential (spec/10).
    public var needsAttention: CredentialPause?

    public init(receipt: Receipt, accountId: String, needsAttention: CredentialPause?) {
        self.receipt = receipt
        self.accountId = accountId
        self.needsAttention = needsAttention
    }
}

/// The setup calls the onboarding sheet needs (spec/07). `CoreBridge`
/// adopts it; tests substitute a fake.
public protocol AccountSetupSource: Sendable {
    /// The command, under the sheet's one idempotency key.
    func addBridgeAccount(idempotencyKey: String, request: BridgeAccountRequest) async throws
        -> BridgeAccountAdded
    /// spec/10's fingerprint fallback: what Bridge presents on `port`,
    /// for the user to confirm. Never trusted on its own.
    func bridgeCertificate(port: UInt16, security: EndpointSecurity) async throws -> String
    /// spec/10's preferred path: the fingerprint of the one certificate
    /// in Bridge's exported `cert.pem`. Synchronous, no I/O.
    func certificateFingerprint(pem: Data) throws -> String
}

extension EndpointSecurity {
    var generated: SweetbriarCore.EndpointSecurity {
        switch self {
        case .tls: .tls
        case .startTls: .startTls
        }
    }
}

extension BridgeAccountRequest {
    /// Map onto the generated record field by field.
    var generated: SweetbriarCore.BridgeAccountParams {
        SweetbriarCore.BridgeAccountParams(
            displayName: displayName,
            username: username,
            password: password,
            imapPort: imapPort,
            imapSecurity: imapSecurity.generated,
            smtpPort: smtpPort,
            smtpSecurity: smtpSecurity.generated,
            certificateSha256: certificateSha256,
            addressMode: addressMode.map { mode -> SweetbriarCore.AddressMode in
                switch mode {
                case .combined: .combined
                case .split: .split
                }
            },
            identities: identities.map { identity in
                SweetbriarCore.BridgeIdentity(
                    email: identity.email,
                    displayName: identity.displayName,
                    isDefault: identity.isDefault,
                    sendable: {
                        switch identity.sendable {
                        case .validated: .validated
                        case .provisional: .provisional
                        case .unavailable: .unavailable
                        }
                    }()
                )
            }
        )
    }
}

extension BridgeAccountAdded {
    /// Map the generated record field by field.
    init(_ generated: SweetbriarCore.BridgeAccountAdded) {
        self.init(
            receipt: Receipt(generated.receipt),
            accountId: generated.accountId,
            needsAttention: generated.needsAttention.map { pause -> CredentialPause in
                switch pause {
                case .notStored: .notStored
                case .storeUnavailable: .storeUnavailable
                case .storeRefused: .storeRefused
                case .storeFailed: .storeFailed
                }
            }
        )
    }
}
