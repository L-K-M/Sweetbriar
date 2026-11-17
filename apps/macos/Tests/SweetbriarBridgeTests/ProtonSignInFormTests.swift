// `ProtonSignInForm` (plan M2.14b, spec/08 "Proton sign-in"): each
// step's secret field is empty once the step is sent, whatever Proton
// answers; the rest of what was typed stays; codes paste with spaces;
// signing in again fixes the username.

import Foundation
import Testing
@testable import SweetbriarBridge

private let password = "synthetic-proton-password"

@MainActor
private func filled(_ source: FakeProtonSignInSource) -> ProtonSignInForm {
    let form = ProtonSignInForm(source: source)
    form.displayName = "Owner"
    form.username = " owner@example.org "
    form.password = password
    return form
}

@MainActor
@Test func eachStepEmptiesItsSecretFieldOnceSent() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [
            .success(.needsMailboxPassword),
            .success(.complete(accountId: "account-1")),
        ]
    )
    let form = filled(source)
    #expect(form.canSubmit)

    await form.submit()
    #expect(form.password.isEmpty)
    #expect(form.model.step == .secondFactor)
    #expect(source.begins.first?.params.password == Data(password.utf8))
    #expect(source.begins.first?.params.username == "owner@example.org")

    form.code = "123 456"
    #expect(form.canSubmit)
    await form.submit()
    #expect(form.code.isEmpty)
    #expect(source.continues.first?.step == .secondFactor(code: "123456"))

    form.mailboxPassword = "synthetic-mailbox-password"
    await form.submit()
    #expect(form.mailboxPassword.isEmpty)
    #expect(source.continues.last?.step == .mailboxPassword(bytes: Data("synthetic-mailbox-password".utf8)))
    #expect(form.model.step == .done(accountID: "account-1"))
    #expect(!form.canSubmit)
}

@MainActor
@Test func aRefusalKeepsWhatWasTypedExceptTheSecret() async {
    let source = FakeProtonSignInSource(beginAnswers: [
        .failure(.invalidInput(field: "password", message: "wrong password"))
    ])
    let form = filled(source)

    await form.submit()

    #expect(form.password.isEmpty)
    #expect(form.displayName == "Owner")
    #expect(form.username == " owner@example.org ")
    #expect(form.model.inlineError == .password("wrong password"))
    #expect(!form.canSubmit)
}

@MainActor
@Test func aWrongCodeEmptiesTheCodeAndKeepsTheStep() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))],
        continueAnswers: [.failure(.invalidInput(field: "second_factor", message: "wrong code"))]
    )
    let form = filled(source)
    await form.submit()
    form.code = "000000"

    await form.submit()

    #expect(form.code.isEmpty)
    #expect(form.model.step == .secondFactor)
    #expect(form.model.inlineError == .secondFactor("wrong code"))
}

@MainActor
@Test func eachStepNeedsItsField() async {
    let form = ProtonSignInForm(source: FakeProtonSignInSource())
    #expect(!form.canSubmit)
    form.displayName = "Owner"
    form.username = "owner@example.org"
    #expect(!form.canSubmit)
    form.password = "x"
    #expect(form.canSubmit)
    form.displayName = "   "
    #expect(!form.canSubmit)
}

@Test func aPastedCodeKeepsItsSixDigits() {
    #expect(ProtonSignInForm.normalizedCode("123 456") == "123456")
    #expect(ProtonSignInForm.normalizedCode("123-456\n") == "123456")
    #expect(ProtonSignInForm.normalizedCode("1234567") == "123456")
    #expect(ProtonSignInForm.normalizedCode("12a٣4") == "124")
}

@MainActor
@Test func verificationIsSentOnlyOnItsStep() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(
            signInId: "sign-in-1",
            state: .needsHumanVerification(url: "https://verify.proton.me/?methods=captcha", methods: ["captcha"])
        ))],
        continueAnswers: [.success(.complete(accountId: "account-1"))]
    )
    let form = filled(source)
    await form.verify(token: "early", tokenType: "captcha")
    #expect(source.continues.isEmpty)

    await form.submit()
    await form.verify(token: "token-1", tokenType: "captcha")

    #expect(source.continues.map { $0.step } == [.humanVerification(token: "token-1", tokenType: "captcha")])
    #expect(form.model.step == .done(accountID: "account-1"))
}

@MainActor
@Test func cancelEmptiesEverySecretField() async {
    let source = FakeProtonSignInSource(
        beginAnswers: [.success(.open(signInId: "sign-in-1", state: .needsSecondFactor))]
    )
    let form = filled(source)
    await form.submit()
    form.code = "123"
    form.mailboxPassword = "typed"
    form.password = "typed"

    await form.cancel()

    #expect(form.password.isEmpty && form.code.isEmpty && form.mailboxPassword.isEmpty)
    #expect(source.cancels == ["sign-in-1"])
}

@MainActor
@Test func signingInAgainFixesTheUsernameAndKeepsTheName() async {
    let account = Account(
        accountId: "account-1", kind: .proton, displayName: "Work", state: .needsAttention,
        stateReason: nil, sortOrder: 0, username: "owner@example.org"
    )
    let source = FakeProtonSignInSource(beginAnswers: [.success(.open(signInId: "s", state: .needsSecondFactor))])
    let form = ProtonSignInForm(source: source, signingInAgain: account)
    #expect(form.displayName == "Work" && form.username == "owner@example.org")
    #expect(form.model.isSigningInAgain)
    form.username = "someone-else@example.org"
    form.password = password

    await form.submit()

    #expect(source.begins.first?.params.username == "owner@example.org")
    #expect(source.begins.first?.params.accountId == "account-1")
}
