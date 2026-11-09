// `BridgeAccountSetupModel` (plan M2.3, spec/10): Bridge's defaults
// prefilled; no submission without a trusted certificate; a presented
// fingerprint trusted only once confirmed and dropped when the IMAP
// endpoint changes; one idempotency key for every attempt; a field
// failure kept for its row; an account added without its password.

import Foundation
import Testing
@testable import SweetbriarBridge

/// A scripted `AccountSetupSource` recording what the model asked.
final class FakeAccountSetupSource: AccountSetupSource, @unchecked Sendable {
    private let lock = NSLock()
    private var _requests: [(key: String, request: BridgeAccountRequest)] = []
    private var _probes: [(port: UInt16, security: EndpointSecurity)] = []
    private var answers: [Result<BridgeAccountAdded, CoreFailure>]
    private let presented: Result<String, CoreFailure>
    private let pemFingerprint: Result<String, CoreFailure>
    private var _pemReads = 0
    /// When set, `bridgeCertificate` waits until the stream yields.
    private let probeGate: AsyncStream<Void>?

    init(
        answers: [Result<BridgeAccountAdded, CoreFailure>] = [],
        presented: Result<String, CoreFailure> = .success(String(repeating: "ab", count: 32)),
        pemFingerprint: Result<String, CoreFailure> = .success(String(repeating: "cd", count: 32)),
        probeGate: AsyncStream<Void>? = nil
    ) {
        self.answers = answers
        self.presented = presented
        self.pemFingerprint = pemFingerprint
        self.probeGate = probeGate
    }

    var pemReads: Int { lock.withLock { _pemReads } }

    var requests: [(key: String, request: BridgeAccountRequest)] {
        lock.withLock { _requests }
    }

    var probes: [(port: UInt16, security: EndpointSecurity)] {
        lock.withLock { _probes }
    }

    func addBridgeAccount(
        idempotencyKey: String, request: BridgeAccountRequest
    ) async throws -> BridgeAccountAdded {
        let answer: Result<BridgeAccountAdded, CoreFailure> = lock.withLock {
            _requests.append((idempotencyKey, request))
            return answers.isEmpty ? .success(Self.added()) : answers.removeFirst()
        }
        return try answer.get()
    }

    func bridgeCertificate(port: UInt16, security: EndpointSecurity) async throws -> String {
        lock.withLock { _probes.append((port, security)) }
        if let probeGate {
            for await _ in probeGate { break }
        }
        return try presented.get()
    }

    func certificateFingerprint(pem: Data) throws -> String {
        lock.withLock { _pemReads += 1 }
        return try pemFingerprint.get()
    }

    static func added(needsAttention: CredentialPause? = nil) -> BridgeAccountAdded {
        BridgeAccountAdded(
            receipt: Receipt(command: "add_bridge_account", resultJson: "{}", createdAtMs: 0),
            accountId: "account-1",
            needsAttention: needsAttention
        )
    }
}

@MainActor
private func filled(_ source: FakeAccountSetupSource) -> BridgeAccountSetupModel {
    let model = BridgeAccountSetupModel(source: source, idempotencyKey: "sheet-key")
    model.username = "owner@example.org"
    model.password = "bridge-generated"
    return model
}

@MainActor
@Test func bridgeDefaultsArePrefilled() {
    let model = BridgeAccountSetupModel(source: FakeAccountSetupSource())
    #expect(model.imapPort == 1143)
    #expect(model.smtpPort == 1025)
    #expect(model.imapSecurity == .startTls)
    #expect(model.smtpSecurity == .startTls)
    #expect(model.certificate == .none)
    #expect(!model.canSubmit)
}

@MainActor
@Test func noSubmissionWithoutATrustedCertificate() async {
    let source = FakeAccountSetupSource()
    let model = filled(source)
    #expect(model.missing == ["a trusted certificate"])

    await model.readPresentedCertificate()
    #expect(model.certificate == .presented(fingerprint: String(repeating: "ab", count: 32)))
    #expect(model.trustedFingerprint == nil, "presented is not trusted")
    #expect(!model.canSubmit)
    await model.submit()
    #expect(source.requests.isEmpty, "nothing is submitted untrusted")

    model.confirmPresented()
    #expect(model.trustedFingerprint == String(repeating: "ab", count: 32))
    #expect(model.canSubmit)
}

@MainActor
@Test func changingTheImapEndpointDropsAPresentedFingerprint() async {
    let source = FakeAccountSetupSource()
    let model = filled(source)
    await model.readPresentedCertificate()
    model.confirmPresented()
    #expect(source.probes.map(\.port) == [1143])

    model.imapPort = 1144
    #expect(model.certificate == .none)

    await model.readPresentedCertificate()
    model.imapSecurity = .tls
    #expect(model.certificate == .none)

    // An imported cert.pem does not depend on the endpoint.
    model.importCertificate(pem: Data("-----BEGIN CERTIFICATE-----".utf8))
    model.imapPort = 1145
    #expect(model.certificate == .imported(fingerprint: String(repeating: "cd", count: 32)))
}

@MainActor
@Test func submissionCarriesTheFieldsAndOneKey() async {
    let source = FakeAccountSetupSource(answers: [
        .failure(.unavailable(reason: "Bridge did not answer")),
        .success(FakeAccountSetupSource.added()),
    ])
    let model = filled(source)
    model.importCertificate(pem: Data())

    await model.submit()
    #expect(model.phase == .editing)
    #expect(model.failure == .unavailable(reason: "Bridge did not answer"))
    #expect(model.password == "bridge-generated", "kept for the retry")

    await model.submit()
    #expect(model.phase == .added(accountId: "account-1", needsAttention: nil))
    #expect(model.password.isEmpty, "cleared once the account exists")

    let requests = source.requests
    #expect(requests.map(\.key) == ["sheet-key", "sheet-key"], "one key for every attempt")
    let request = requests[1].request
    #expect(request.username == "owner@example.org")
    #expect(request.password == Data("bridge-generated".utf8))
    #expect(request.imapPort == 1143)
    #expect(request.imapSecurity == .startTls)
    #expect(request.smtpPort == 1025)
    #expect(request.certificateSha256 == String(repeating: "cd", count: 32))
    #expect(request.identities == [
        BridgeIdentity(
            email: "owner@example.org", displayName: "",
            isDefault: true, sendable: .provisional
        )
    ], "no From name unless the user gives one")
    #expect(!String(describing: request).contains("bridge-generated"), "never printed")
}

@MainActor
@Test func aFieldFailureIsKeptForItsRow() async {
    let source = FakeAccountSetupSource(answers: [
        .failure(.invalidInput(field: "password", message: "Bridge refused the username or the Bridge password"))
    ])
    let model = filled(source)
    model.importCertificate(pem: Data())
    await model.submit()

    #expect(model.failure(for: "password") == "Bridge refused the username or the Bridge password")
    #expect(model.failure(for: "pin") == nil)
    #expect(model.phase == .editing)
}

@MainActor
@Test func anAccountAddedWithoutItsPasswordSaysSo() async {
    let source = FakeAccountSetupSource(answers: [
        .success(FakeAccountSetupSource.added(needsAttention: .storeUnavailable))
    ])
    let model = filled(source)
    model.importCertificate(pem: Data())
    await model.submit()

    #expect(model.phase == .added(accountId: "account-1", needsAttention: .storeUnavailable))
    #expect(BridgeAccountSetupModel.explain(.storeUnavailable).contains("Keychain"))
}

@MainActor
@Test func aRefusedCertificateFileShowsWhy() {
    let source = FakeAccountSetupSource(
        pemFingerprint: .failure(.invalidInput(
            field: "certificate",
            message: "the file holds a private key; choose the cert.pem Bridge exported, not key.pem"
        ))
    )
    let model = filled(source)
    model.importCertificate(pem: Data())
    #expect(model.certificate == .none)
    #expect(model.certificateError?.contains("key.pem") == true)
}

@MainActor
@Test func keyPemIsRefusedUnreadAndCertPemIsRead() throws {
    let source = FakeAccountSetupSource()
    let model = filled(source)
    let folder = FileManager.default.temporaryDirectory
        .appendingPathComponent("bridge-setup-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: folder) }

    // Named key.pem: refused before it is opened, so even an unreadable
    // path fails the same way.
    model.importCertificate(fileAt: folder.appendingPathComponent("key.pem"))
    #expect(model.certificate == .none)
    #expect(model.certificateError?.contains("never reads key.pem") == true)

    let large = folder.appendingPathComponent("big.pem")
    try Data(count: BridgeAccountSetupModel.maxCertificateFileBytes + 1).write(to: large)
    model.importCertificate(fileAt: large)
    #expect(model.certificate == .none)
    #expect(model.certificateError?.contains("too large") == true)

    let cert = folder.appendingPathComponent("cert.pem")
    try Data("-----BEGIN CERTIFICATE-----\n".utf8).write(to: cert)
    model.importCertificate(fileAt: cert)
    #expect(model.certificate == .imported(fingerprint: String(repeating: "cd", count: 32)))
    #expect(model.certificateError == nil)
}

@Test func fingerprintsDisplayAsOpensslPrintsThem() {
    #expect(BridgeAccountSetupModel.displayFingerprint("f921c0") == "F9:21:C0")
    #expect(
        BridgeAccountSetupModel.displayFingerprint(String(repeating: "ab", count: 32))
            == Array(repeating: "AB", count: 32).joined(separator: ":")
    )
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func aStaleProbeNeverReplacesAnImportedCertificate() async {
    let (gate, release) = AsyncStream<Void>.makeStream()
    let source = FakeAccountSetupSource(probeGate: gate)
    let model = filled(source)

    let reading = Task { await model.readPresentedCertificate() }
    while source.probes.isEmpty { await Task.yield() }
    // The user imports cert.pem while the probe is still waiting.
    model.importCertificate(pem: Data())
    release.yield()
    release.finish()
    await reading.value

    #expect(model.certificate == .imported(fingerprint: String(repeating: "cd", count: 32)))
}

@MainActor
@Test(.timeLimit(.minutes(1)))
func onlyARegularCertificateFileIsRead() throws {
    let source = FakeAccountSetupSource()
    let model = filled(source)
    let folder = FileManager.default.temporaryDirectory
        .appendingPathComponent("bridge-files-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: folder) }

    // A FIFO would block a read forever: refused without opening it.
    let fifo = folder.appendingPathComponent("bridge.pem")
    #expect(mkfifo(fifo.path, 0o600) == 0)
    model.importCertificate(fileAt: fifo)
    #expect(model.certificateError == "Choose the cert.pem file Bridge exported.")

    // A cert.pem that links to key.pem is still key.pem: refused unread.
    let key = folder.appendingPathComponent("key.pem")
    try Data("-----BEGIN PRIVATE KEY-----\n".utf8).write(to: key)
    let link = folder.appendingPathComponent("cert.pem")
    try FileManager.default.createSymbolicLink(at: link, withDestinationURL: key)
    model.importCertificate(fileAt: link)
    #expect(model.certificateError?.contains("never reads key.pem") == true)
    #expect(source.pemReads == 0, "neither file reached the parser")
    #expect(model.certificate == .none)

    // A link named key.pem is refused by its own name too, even when
    // it points at a harmless certificate.
    let real = folder.appendingPathComponent("bridge-cert.pem")
    try Data("-----BEGIN CERTIFICATE-----\n".utf8).write(to: real)
    let named = folder.appendingPathComponent("key.pem.d")
    try FileManager.default.createDirectory(at: named, withIntermediateDirectories: false)
    let keyLink = named.appendingPathComponent("key.pem")
    try FileManager.default.createSymbolicLink(at: keyLink, withDestinationURL: real)
    model.importCertificate(fileAt: keyLink)
    #expect(model.certificateError?.contains("never reads key.pem") == true)
    #expect(source.pemReads == 0, "the linked certificate was not read either")
}

@MainActor
@Test func theFromNameIsTheUsersTrimmed() async {
    let source = FakeAccountSetupSource()
    let model = filled(source)
    model.senderName = "  Owner Name  "
    model.importCertificate(pem: Data())
    await model.submit()
    #expect(source.requests.first?.request.identities.first?.displayName == "Owner Name")
}
