import Foundation
import SweetbriarBridge
import Testing
@testable import Sweetbriar

/// The new-mail notification text and the notifier's previews read
/// (spec/10 "Notifications"): the account's name is always there,
/// sender and subject only for one message whose headers were read,
/// and a missing, unknown or failed previews setting means off.
@MainActor
@Suite("New mail notifications")
struct NewMailNotificationTests {
    private static let accountName = "Owner"
    private static let sender = "Alice Example"
    private static let subject = "Quarterly Secret Subject"

    @Test func previewsOffHidesTheSenderAndSubject() {
        let mail = Self.mail(count: 1)
        let text = NewMailNotificationText.make(
            accountName: Self.accountName, mail: mail, preview: nil)

        #expect(Self.allFields(text).contains { $0.contains(Self.accountName) })
        for field in Self.allFields(text) {
            #expect(!field.contains(Self.sender))
            #expect(!field.contains(Self.subject))
        }
    }

    @Test func previewsOnShowTheSenderAndSubjectForOneMessage() {
        let mail = Self.mail(count: 1)
        let text = NewMailNotificationText.make(
            accountName: Self.accountName, mail: mail,
            preview: MessagePreview(sender: Self.sender, subject: Self.subject))

        #expect(text.title.contains(Self.subject))
        #expect(text.subtitle?.contains(Self.sender) == true)
        #expect(text.body.contains(Self.accountName))
    }

    @Test func severalMessagesSummarizeTheCountWithoutAPreview() {
        let mail = Self.mail(count: 3)
        let text = NewMailNotificationText.make(
            accountName: Self.accountName, mail: mail,
            preview: MessagePreview(sender: Self.sender, subject: Self.subject))

        #expect(text.title.contains(Self.accountName))
        #expect(text.body.contains("3 new messages"))
        for field in Self.allFields(text) {
            #expect(!field.contains(Self.sender))
            #expect(!field.contains(Self.subject))
        }
    }

    @Test func aCountBeyondTheNewestIdsSummarizesTheFullCount() {
        let ids = (1...20).map { Int64($0) }
        let text = NewMailNotificationText.make(
            accountName: Self.accountName, mail: Self.mail(count: 25, messageIds: ids),
            preview: MessagePreview(sender: Self.sender, subject: Self.subject))

        #expect(text.body.contains("25 new messages"))
        #expect(text.userInfo["count"] == "25")
        for field in Self.allFields(text) {
            #expect(!field.contains(Self.sender))
            #expect(!field.contains(Self.subject))
        }
    }

    @Test func aDeniedAuthorizationIsReadAgainOnTheNextArrival() async {
        let source = FakeNewMailSource(
            settings: Self.previews("off"), accounts: [Self.account()], message: nil)
        let poster = FakePoster(grants: [false, true])
        let notifier = NewMailNotifier(source: source, poster: poster)

        await notifier.handle(Self.mail(count: 1))
        #expect(await poster.notifications.isEmpty, "denied: nothing posts")

        await notifier.handle(Self.mail(count: 1))
        await notifier.handle(Self.mail(count: 1))
        #expect(await poster.notifications.count == 2, "granted later in System Settings")
        #expect(await poster.authorizationRequests == 2, "a grant is not re-read")
    }

    @Test func previewsOnShowsTheHeadersAndAsksAuthorizationOnce() async throws {
        let source = FakeNewMailSource(
            settings: Self.previews("on"),
            accounts: [Self.account()],
            message: Self.message())
        let poster = FakePoster()
        let notifier = NewMailNotifier(source: source, poster: poster)

        await notifier.handle(Self.mail(count: 1))
        await notifier.handle(Self.mail(count: 1))

        let posted = try #require(await poster.notifications.first)
        #expect(posted.title.contains(Self.subject))
        #expect(posted.subtitle?.contains(Self.sender) == true)
        #expect(await poster.notifications.count == 2)
        #expect(await poster.authorizationRequests == 1, "one ask serves every arrival")
    }

    @Test func anUnknownOrMissingPreviewsSettingMeansOff() async throws {
        let cases: [[Setting]?] = [
            nil,
            [],
            Self.previews("sometimes"),
        ]
        for settings in cases {
            let source = FakeNewMailSource(
                settings: settings ?? [], accounts: [Self.account()], message: Self.message())
            let poster = FakePoster()
            let notifier = NewMailNotifier(source: source, poster: poster)

            await notifier.handle(Self.mail(count: 1))

            let posted = try #require(await poster.notifications.first)
            for field in Self.allFields(posted) {
                #expect(!field.contains(Self.sender))
                #expect(!field.contains(Self.subject))
            }
            #expect(await poster.authorizationRequests == 1)
        }
    }

    @Test func aFailedPreviewsReadMeansOff() async throws {
        let source = FakeNewMailSource(
            settings: Self.previews("on"),
            accounts: [Self.account()],
            message: Self.message())
        await source.setSettingsError(CoreFailure.unavailable(reason: "store closed"))
        let poster = FakePoster()
        let notifier = NewMailNotifier(source: source, poster: poster)

        await notifier.handle(Self.mail(count: 1))

        let posted = try #require(await poster.notifications.first)
        for field in Self.allFields(posted) {
            #expect(!field.contains(Self.sender))
            #expect(!field.contains(Self.subject))
        }
    }

    @Test func aFailedPreviewReadFallsBackToTheNoPreviewText() async throws {
        let source = FakeNewMailSource(
            settings: Self.previews("on"),
            accounts: [Self.account()],
            message: Self.message())
        await source.setMessageError(CoreFailure.notFound(what: "message"))
        let poster = FakePoster()
        let notifier = NewMailNotifier(source: source, poster: poster)

        await notifier.handle(Self.mail(count: 1))

        let posted = try #require(await poster.notifications.first)
        for field in Self.allFields(posted) {
            #expect(!field.contains(Self.sender))
            #expect(!field.contains(Self.subject))
        }
        #expect(Self.allFields(posted).contains { $0.contains(Self.accountName) })
    }

    @Test func aFailingSubscribeNeverAsksForAuthorization() async {
        let source = FakeNewMailSource(settings: [], accounts: [], message: nil)
        let poster = FakePoster()
        let notifier = NewMailNotifier(source: source, poster: poster)

        notifier.start()
        // The fixture's subscribeNewMail() throws, so no arrival and no
        // permission request: the app never asks at launch.
        await notifier.stop()
        #expect(await poster.authorizationRequests == 0)
    }

    @Test func theLoopPostsEachArrivalUntilStopped() async throws {
        let source = FakeNewMailSource(
            settings: Self.previews("off"), accounts: [Self.account()], message: nil,
            stream: FakeNewMailStream(mails: [Self.mail(count: 1)]))
        let poster = FakePoster()
        let notifier = NewMailNotifier(source: source, poster: poster)

        notifier.start()
        try #require(await poster.waitForNotifications(1))
        await notifier.stop()

        #expect(await poster.authorizationRequests == 1)
        let posted = try #require(await poster.notifications.first)
        #expect(Self.allFields(posted).contains { $0.contains(Self.accountName) })
    }

    private static func allFields(_ notification: NewMailNotification) -> [String] {
        [notification.title, notification.body, notification.subtitle ?? ""]
            + notification.userInfo.values
    }

    private static func previews(_ value: String) -> [Setting] {
        [Setting(key: "notifications.previews", value: value, isDefault: false)]
    }

    private static func mail(count: UInt32, messageIds: [Int64]? = nil) -> NewMail {
        NewMail(
            accountId: "proton-1", count: count,
            messageIds: messageIds ?? (count == 1 ? [7] : [9, 8, 7]))
    }

    private static func account() -> Account {
        Account(
            accountId: "proton-1", kind: .proton, displayName: Self.accountName,
            state: .active, stateReason: nil, sortOrder: 1,
            username: "owner@example.test")
    }

    private static func message() -> ReaderMessage {
        ReaderMessage(
            messageId: 7,
            headers: ReaderHeaders(
                from: [ReaderAddress(name: Self.sender, address: "alice@example.test")],
                to: [], cc: [], date: nil, subject: Self.subject,
                messageId: "<7@example.test>"),
            accountId: "proton-1", accountDisplayName: Self.accountName,
            occurrences: [], threadId: nil, tags: [], note: nil,
            bodyAvailability: .notAcquired)
    }
}

private actor FakeNewMailSource: NewMailSource {
    private var settingsList: [Setting]
    private var settingsError: (any Error)?
    private var accountsList: [Account]
    private var message: ReaderMessage?
    private var messageError: (any Error)?
    private let stream: (any NewMailStream)?

    init(
        settings: [Setting], accounts: [Account], message: ReaderMessage?,
        stream: (any NewMailStream)? = nil
    ) {
        self.settingsList = settings
        self.accountsList = accounts
        self.message = message
        self.stream = stream
    }

    func setSettingsError(_ error: (any Error)?) { settingsError = error }
    func setMessageError(_ error: (any Error)?) { messageError = error }

    func subscribeNewMail() async throws -> any NewMailStream {
        guard let stream else {
            throw CoreFailure.unavailable(reason: "no stream in this fixture")
        }
        return stream
    }

    func settings() async throws -> [Setting] {
        if let settingsError { throw settingsError }
        return settingsList
    }

    func accounts() async throws -> [Account] { accountsList }

    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        if let messageError { throw messageError }
        guard let message else { throw CoreFailure.notFound(what: "message") }
        return message
    }
}

private actor FakePoster: NewMailPoster {
    private(set) var authorizationRequests = 0
    private(set) var notifications: [NewMailNotification] = []
    /// The answers to successive requests; granted once they run out.
    private var grants: [Bool]

    init(grants: [Bool] = []) {
        self.grants = grants
    }

    func requestAuthorization() async -> Bool {
        authorizationRequests += 1
        return grants.isEmpty ? true : grants.removeFirst()
    }

    func post(_ notification: NewMailNotification) async {
        notifications.append(notification)
    }

    /// Wait for `count` posts; the loop runs on the main actor, so the
    /// test's await lets it proceed.
    func waitForNotifications(_ count: Int) async -> Bool {
        let deadline = ContinuousClock.now + .seconds(5)
        while notifications.count < count {
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return true
    }
}

/// A stream that yields its mails, then waits until cancelled.
private actor FakeNewMailStream: NewMailStream {
    private var mails: [NewMail]

    init(mails: [NewMail]) { self.mails = mails }

    func next() async throws -> NewMail {
        if !mails.isEmpty { return mails.removeFirst() }
        try await Task.sleep(for: .seconds(3_600))
        throw CancellationError()
    }
}
