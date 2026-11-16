// `ProtonSignInModel` (plan M2.14a, spec/07 "Proton sign-in"): each
// step in order to done; a wrong password returns to credentials with a
// new idempotency key; a wrong code or mailbox password stays on its
// step; secrets are zeroed once sent; signing in again keeps the
// username and passes the account id; one call runs at a time, and an
// answer that lands after a cancel changes nothing.

import Foundation
import Testing
@testable import SweetbriarBridge

/// Holds a call until the test releases it.
final class Hold: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Never>?
    private var released = false

    func wait() async {
        await withCheckedContinuation { continuation in
            let resumeNow = lock.withLock { () -> Bool in
                if released { return true }
                self.continuation = continuation
                return false
            }
            if resumeNow { continuation.resume() }
        }
    }

    func release() {
        let waiting = lock.withLock { () -> CheckedContinuation<Void, Never>? in
            released = true
            defer { continuation = nil }
            return continuation
        }
        waiting?.resume()
    }
}

/// A scripted `ProtonSignInSource` recording what the model asked. A
/// call past the script is a test failure, never an invented answer.
/// A hold applies to the first such call only.
final class FakeProtonSignInSource: ProtonSignInSource, @unchecked Sendable {
    private let lock = NSLock()
    private var beginHold: Hold?
    private var continueHold: Hold?
    private var _begins: [(key: String, params: ProtonSignInRequest)] = []
    private var _continues: [(signInId: String, step: ProtonSignInStep)] = []
    private var _cancels: [String] = []
    private var beginAnswers: [Result<BeginProtonSignIn, CoreFailure>]
    private var continueAnswers: [Result<ProtonSignInState, CoreFailure>]

    init(
        beginAnswers: [Result<BeginProtonSignIn, CoreFailure>] = [],
        continueAnswers: [Result<ProtonSignInState, CoreFailure>] = [],
        beginHold: Hold? = nil,
        continueHold: Hold? = nil
    ) {
        self.beginAnswers = beginAnswers
        self.continueAnswers = continueAnswers
        self.beginHold = beginHold
        self.continueHold = continueHold
    }

    var begins: [(key: String, params: ProtonSignInRequest)] { lock.withLock { _begins } }
    var continues: [(signInId: String, step: ProtonSignInStep)] { lock.withLock { _continues } }
    var cancels: [String] { lock.withLock { _cancels } }

    func beginProtonSignIn(
        idempotencyKey: String, params: ProtonSignInRequest
    ) async throws -> BeginProtonSignIn {
        var hold: Hold?
        let answer: Result<BeginProtonSignIn, CoreFailure> = lock.withLock {
            _begins.append((idempotencyKey, params))
            hold = beginHold
            beginHold = nil
            if beginAnswers.isEmpty {
                Issue.record("an unscripted begin")
                return .failure(.unavailable(reason: "unscripted"))
            }
            return beginAnswers.removeFirst()
        }
        await hold?.wait()
        return try answer.get()
    }

    func continueProtonSignIn(
        signInId: String, step: ProtonSignInStep
    ) async throws -> ProtonSignInState {
        var hold: Hold?
        let answer: Result<ProtonSignInState, CoreFailure> = lock.withLock {
            _continues.append((signInId, step))
            hold = continueHold
            continueHold = nil
            if continueAnswers.isEmpty {
                Issue.record("an unscripted continue")
                return .failure(.unavailable(reason: "unscripted"))
            }
            return continueAnswers.removeFirst()
        }
        await hold?.wait()
        return try answer.get()
    }

    func cancelProtonSignIn(signInId: String) async throws {
        lock.withLock { _cancels.append(signInId) }
    }

    func disconnectAccount(accountId: String) async throws {}
}

@MainActor
@Test func eachStepLeadsToTheDoneState() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [
            .success(.needsMailboxPassword),
            .success(.needsHumanVerification(
                url: "https://proton.example/verify", methods: ["captcha"]
            )),
            .success(.complete(accountId: "account-1")),
        ]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)

    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    #expect(model.step == .secondFactor)
    #expect(model.inlineError == nil)
    #expect(password.allSatisfy { $0 == 0 }, "the password is cleared once sent")
    #expect(source.begins.count == 1)
    #expect(source.begins[0].params.displayName == "Owner")
    #expect(source.begins[0].params.username == "owner@example.org")
    #expect(source.begins[0].params.password == Data("synthetic-proton-password".utf8))
    #expect(source.begins[0].params.accountId == nil)

    await model.submitCode("123456")
    #expect(model.step == .mailboxPassword)
    #expect(source.continues.last?.step == .secondFactor(code: "123456"))

    var mailboxPassword = Data("synthetic-mailbox-password".utf8)
    await model.submitMailboxPassword(&mailboxPassword)
    #expect(model.step == .humanVerification(url: "https://proton.example/verify", methods: ["captcha"]))
    #expect(source.continues.last?.step == .mailboxPassword(
        bytes: Data("synthetic-mailbox-password".utf8)
    ))
    #expect(mailboxPassword.allSatisfy { $0 == 0 }, "the mailbox password is cleared once sent")

    await model.submitVerification(token: "hv-token", tokenType: "captcha")
    #expect(model.step == .done(accountID: "account-1"))
    #expect(source.continues.last?.step == .humanVerification(
        token: "hv-token", tokenType: "captcha"
    ))
    #expect(source.cancels.isEmpty, "a completed conversation is not cancelled")
}

@MainActor
@Test func aWrongPasswordReturnsToCredentialsWithANewKey() async {
    let source = FakeProtonSignInSource(beginAnswers: [
        .failure(.invalidInput(field: "password", message: "Proton refused the password")),
        .success(.open(signInId: "sign-in-2", state: .needsSecondFactor)),
    ])
    let model = ProtonSignInModel(source: source)
    var password = Data("wrong-password".utf8)

    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    #expect(model.step == .credentials)
    #expect(model.inlineError == .password("Proton refused the password"))
    #expect(password.allSatisfy { $0 == 0 })
    let firstKey = source.begins[0].key

    var retry = Data("right-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &retry)

    #expect(model.step == .secondFactor)
    #expect(model.inlineError == nil)
    #expect(source.begins.count == 2)
    #expect(source.begins[1].key != firstKey, "the next begin uses a new idempotency key")
}

@MainActor
@Test func aWrongCodeKeepsTheStepAndShowsTheInlineError() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [
            .failure(.invalidInput(field: "second_factor", message: "Proton refused the code")),
            .success(.needsMailboxPassword),
        ]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    await model.submitCode("000000")
    #expect(model.step == .secondFactor)
    #expect(model.inlineError == .secondFactor("Proton refused the code"))

    await model.submitCode("123456")
    #expect(model.step == .mailboxPassword)
    #expect(model.inlineError == nil, "the next answer clears the refusal")
}

@MainActor
@Test func aWrongMailboxPasswordKeepsTheMailboxPasswordStep() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsMailboxPassword))],
        continueAnswers: [
            .failure(.invalidInput(
                field: "mailbox_password", message: "Proton refused the mailbox password"
            )),
            .success(.complete(accountId: "account-1")),
        ]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)
    #expect(model.step == .mailboxPassword)

    var wrong = Data("wrong-mailbox-password".utf8)
    await model.submitMailboxPassword(&wrong)
    #expect(model.step == .mailboxPassword)
    #expect(model.inlineError == .mailboxPassword("Proton refused the mailbox password"))
    #expect(wrong.allSatisfy { $0 == 0 })

    var right = Data("synthetic-mailbox-password".utf8)
    await model.submitMailboxPassword(&right)
    #expect(model.step == .done(accountID: "account-1"))
    #expect(model.inlineError == nil)
    #expect(right.allSatisfy { $0 == 0 })
}

@MainActor
@Test func theHumanVerificationStepCarriesTheUrlAndMethods() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(
            signInId: "sign-in-1",
            state: .needsHumanVerification(
                url: "https://proton.example/verify", methods: ["captcha", "email"]
            )
        ))],
        continueAnswers: [.success(.complete(accountId: "account-1"))]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    #expect(model.step == .humanVerification(
        url: "https://proton.example/verify", methods: ["captcha", "email"]
    ))

    await model.submitVerification(token: "hv-token", tokenType: "captcha")
    #expect(model.step == .done(accountID: "account-1"))
    #expect(source.continues.last?.step == .humanVerification(
        token: "hv-token", tokenType: "captcha"
    ))
}

@MainActor
@Test func anAccountRefusalShowsOnTheAccountField() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [.failure(.invalidInput(
            field: "account", message: "this account needs a security key"
        ))]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    await model.submitCode("123456")
    #expect(model.step == .secondFactor)
    #expect(model.inlineError == .account("this account needs a security key"))
}

@MainActor
@Test func anUnavailableRefusalShowsProtonsMessage() async {
    let source = FakeProtonSignInSource(beginAnswers: [
        .failure(.unavailable(reason: "Proton is unreachable"))
    ])
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)

    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    #expect(model.step == .credentials)
    #expect(model.inlineError == .unavailable("Proton is unreachable"))
    #expect(password.allSatisfy { $0 == 0 })
}

@MainActor
@Test func anUnavailableRefusalMidConversationKeepsTheStep() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [
            .failure(.unavailable(reason: "Proton is unreachable")),
            .success(.needsMailboxPassword),
        ]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    await model.submitCode("123456")
    #expect(model.step == .secondFactor)
    #expect(model.inlineError == .unavailable("Proton is unreachable"))

    await model.submitCode("123456")
    #expect(model.step == .mailboxPassword, "the conversation is still answerable")
    #expect(model.inlineError == nil)
}

@MainActor
@Test func aCancelDuringBeginDropsTheConversationItOpened() async {
    let hold = Hold()
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        beginHold: hold
    )
    let model = ProtonSignInModel(source: source)
    let begin = Task { @MainActor in
        var password = Data("synthetic-proton-password".utf8)
        await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)
    }
    #expect(await eventually { source.begins.count == 1 })
    #expect(model.isBusy)

    await model.cancel()
    #expect(!model.isBusy)
    hold.release()
    await begin.value

    #expect(model.step == .credentials)
    #expect(model.inlineError == nil)
    #expect(source.cancels == ["sign-in-1"], "the conversation it opened anyway is dropped")
    await model.submitCode("123456")
    #expect(source.continues.isEmpty, "the cancelled conversation takes no answers")
}

@MainActor
@Test func aCancelDuringAStepIgnoresItsLateAnswer() async {
    let hold = Hold()
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [.success(.complete(accountId: "account-1"))],
        continueHold: hold
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    let submit = Task { @MainActor in await model.submitCode("123456") }
    #expect(await eventually { source.continues.count == 1 })
    await model.cancel()
    hold.release()
    await submit.value

    #expect(model.step == .credentials, "the late answer does not resurrect the conversation")
    #expect(model.inlineError == nil)
    #expect(source.cancels == ["sign-in-1"])
}

@MainActor
@Test func aSecondCallWhileOneRunsIsIgnored() async {
    let hold = Hold()
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        beginHold: hold
    )
    let model = ProtonSignInModel(source: source)
    let first = Task { @MainActor in
        var password = Data("synthetic-proton-password".utf8)
        await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)
    }
    #expect(await eventually { source.begins.count == 1 })

    var again = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &again)
    #expect(source.begins.count == 1, "a double submit starts no second conversation")
    #expect(again.allSatisfy { $0 == 0 }, "an ignored submit still clears its password")

    hold.release()
    await first.value
    #expect(model.step == .secondFactor)
    #expect(!model.isBusy)
}

@MainActor
@Test func cancelCallsTheSourceForAnOpenConversation() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))]
    )
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)
    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    await model.cancel()
    #expect(source.cancels == ["sign-in-1"])
    #expect(model.step == .credentials)

    let idle = ProtonSignInModel(source: source)
    await idle.cancel()
    #expect(source.cancels.count == 1, "no conversation, no cancel call")
}

@MainActor
@Test func signingInAgainPassesTheAccountIdAndKeepsTheUsername() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))]
    )
    let model = ProtonSignInModel(
        source: source, accountId: "account-1", username: "owner@example.org"
    )
    #expect(model.isSigningInAgain)
    #expect(model.username == "owner@example.org")

    var password = Data("synthetic-proton-password".utf8)
    await model.begin(
        displayName: "Owner", username: "someone-else@example.org", password: &password
    )

    let request = source.begins[0].params
    #expect(request.accountId == "account-1")
    #expect(request.username == "owner@example.org", "the fixed username is kept")
}

@MainActor
@Test func aReplayedCompletedKeyStillShowsTheDoneState() async {
    let source = FakeProtonSignInSource(beginAnswers: [
        .success(.complete(receipt: Receipt(
            command: "begin_proton_sign_in",
            resultJson: "{\"account_id\":\"account-9\"}",
            createdAtMs: 0
        )))
    ])
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)

    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    #expect(model.step == .done(accountID: "account-9"))
    #expect(model.inlineError == nil)
}

@MainActor
@Test func aReceiptNamingNoAccountIsAnError() async {
    let source = FakeProtonSignInSource(beginAnswers: [
        .success(.complete(receipt: Receipt(
            command: "begin_proton_sign_in", resultJson: "{}", createdAtMs: 0
        )))
    ])
    let model = ProtonSignInModel(source: source)
    var password = Data("synthetic-proton-password".utf8)

    await model.begin(displayName: "Owner", username: "owner@example.org", password: &password)

    #expect(model.step == .credentials, "never done with an empty account id")
    #expect(model.inlineError == .unavailable("The sign-in finished without naming its account."))
}

@Test func aStepNeverPrintsItsSecrets() {
    let steps: [ProtonSignInStep] = [
        .secondFactor(code: "synthetic-code"),
        .mailboxPassword(bytes: Data("synthetic-mailbox".utf8)),
        .humanVerification(token: "synthetic-token", tokenType: "captcha"),
    ]
    for step in steps {
        for printed in [String(describing: step), String(reflecting: step)] {
            #expect(!printed.contains("synthetic"), "\(printed)")
            #expect(printed.contains("[redacted]"))
        }
    }
}

@Test func theRequestNeverPrintsThePassword() {
    let request = ProtonSignInRequest(
        displayName: "Owner", username: "owner@example.org",
        password: Data("synthetic-secret".utf8), accountId: nil
    )
    let printed = String(describing: request)
    #expect(!printed.contains("synthetic-secret"))
    #expect(printed.contains("[redacted]"))
}
