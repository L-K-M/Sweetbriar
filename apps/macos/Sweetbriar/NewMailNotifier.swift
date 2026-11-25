import Foundation
import SweetbriarBridge
import UserNotifications

/// One new-mail notification's text (spec/10 "Notifications"): the
/// finished title, subtitle, body and userInfo, already redacted by
/// the formatter.
extension Notification.Name {
    /// New mail arrived in some account; compose windows reread their
    /// recipient check (spec/08 "Recipient check").
    static let sweetbriarMailArrived = Notification.Name("ch.lkmc.Sweetbriar.mailArrived")
}

struct NewMailNotification: Sendable, Equatable {
    var title: String
    var subtitle: String?
    var body: String
    var userInfo: [String: String]
}

/// The one message's headers a preview shows (spec/10).
struct MessagePreview: Sendable, Equatable {
    var sender: String?
    var subject: String?
}

/// Builds one arrival's notification text (spec/10). Pure Foundation
/// only: no notification center, no I/O, so tests drive it directly.
/// The account's display name always appears; sender and subject only
/// for a single message whose preview was read.
enum NewMailNotificationText {
    static func make(
        accountName: String, mail: NewMail, preview: MessagePreview?
    ) -> NewMailNotification {
        let userInfo = [
            "accountId": mail.accountId,
            "count": String(mail.count),
        ]
        guard let preview, mail.count == 1 else {
            let summary = mail.count == 1 ? "1 new message" : "\(mail.count) new messages"
            return NewMailNotification(
                title: accountName, subtitle: nil, body: summary, userInfo: userInfo)
        }
        let subject = preview.subject.flatMap { $0.isEmpty ? nil : $0 }
        return NewMailNotification(
            title: subject ?? "New message",
            subtitle: preview.sender.flatMap { $0.isEmpty ? nil : $0 },
            body: accountName,
            userInfo: userInfo)
    }
}

/// Posts one new-mail notification (spec/10). Tests substitute a fake.
protocol NewMailPoster: Sendable {
    /// Ask for notification authorization on the first arrival; later
    /// calls while it is denied only re-read the user's decision.
    func requestAuthorization() async -> Bool
    /// Show one notification.
    func post(_ notification: NewMailNotification) async
}

/// Posts through `UNUserNotificationCenter` (spec/10).
struct UNUserNotificationCenterPoster: NewMailPoster {
    func requestAuthorization() async -> Bool {
        do {
            return try await UNUserNotificationCenter.current()
                .requestAuthorization(options: [.alert, .sound])
        } catch {
            return false
        }
    }

    func post(_ notification: NewMailNotification) async {
        let content = UNMutableNotificationContent()
        content.title = notification.title
        content.subtitle = notification.subtitle ?? ""
        content.body = notification.body
        content.userInfo = notification.userInfo
        let request = UNNotificationRequest(
            identifier: UUID().uuidString, content: content, trigger: nil)
        do {
            try await UNUserNotificationCenter.current().add(request)
        } catch {
            // A notification that can't be shown is not worth an alert
            // of its own; the next arrival is a fresh chance.
            print("new-mail notification: \(error.localizedDescription)")
        }
    }
}

/// Watches one account's Inbox arrivals and posts a notification for
/// each (spec/10). The app starts it once the archive session is open
/// and stops it with the archive or at termination.
@MainActor
final class NewMailNotifier {
    /// spec/10's previews setting key and its on value.
    static let previewsSettingKey = "notifications.previews"
    private static let previewsOn = "on"
    private static let fallbackAccountName = "Sweetbriar"

    private let source: any NewMailSource
    private let poster: any NewMailPoster
    private var loop: Task<Void, Never>?
    private var authorized = false

    init(source: any NewMailSource, poster: any NewMailPoster) {
        self.source = source
        self.poster = poster
    }

    deinit {
        loop?.cancel()
    }

    /// Start the pull loop once; a second call is a no-op.
    func start() {
        guard loop == nil else { return }
        loop = Task { [weak self] in await self?.pull() }
    }

    /// Cancel the loop and wait for the in-flight `next` to end, so no
    /// notification is posted after the archive closed.
    func stop() async {
        let running = loop
        loop = nil
        running?.cancel()
        await running?.value
    }

    private func pull() async {
        do {
            let stream = try await source.subscribeNewMail()
            while !Task.isCancelled {
                let mail = try await stream.next()
                guard !Task.isCancelled else { return }
                await handle(mail)
            }
        } catch is CancellationError {
            // stop() cancelled the loop.
        } catch {
            // The subscription is done (the archive is closing or the
            // core went away): stop rather than spin on a dead stream,
            // but leave a trace, since notifications stay off for the
            // rest of the session.
            print("new-mail subscription ended: \(error.localizedDescription)")
        }
    }

    /// Handle one arrival. Tests drive this directly so a previews
    /// read is observable without a notification center.
    func handle(_ mail: NewMail) async {
        // Compose windows reread their recipient check (spec/08).
        NotificationCenter.default.post(name: .sweetbriarMailArrived, object: nil)
        let previewEnabled = await previewsEnabled()
        var preview: MessagePreview?
        if previewEnabled, mail.messageIds.count == 1 {
            preview = await loadPreview(messageId: mail.messageIds[0])
        }
        let accountName = await accountName(for: mail.accountId)
        let text = NewMailNotificationText.make(
            accountName: accountName, mail: mail, preview: preview)
        if !authorized {
            // The first call may prompt; once the user decided, the
            // call only reports the current decision, so a later
            // grant in System Settings takes effect without a relaunch.
            authorized = await poster.requestAuthorization()
        }
        guard authorized else { return }
        await poster.post(text)
    }

    /// spec/10: previews only when the setting reads "on"; a missing
    /// setting, any other value or a failure is off.
    private func previewsEnabled() async -> Bool {
        guard let settings = try? await source.settings(),
            let setting = settings.first(where: { $0.key == Self.previewsSettingKey })
        else { return false }
        return setting.value == Self.previewsOn
    }

    /// The account's display name from `accounts()`; a read failure
    /// still names the app rather than dropping the notification.
    private func accountName(for accountId: String) async -> String {
        guard let accounts = try? await source.accounts(),
            let account = accounts.first(where: { $0.accountId == accountId })
        else { return Self.fallbackAccountName }
        return account.displayName
    }

    /// Read one message's headers for a preview; a failed read falls
    /// back to the no-preview text (spec/10).
    private func loadPreview(messageId: Int64) async -> MessagePreview? {
        guard let message = try? await source.getMessage(messageId: messageId) else { return nil }
        let sender = message.headers.from.first
        let name = sender?.name.flatMap { $0.isEmpty ? nil : $0 }
        return MessagePreview(sender: name ?? sender?.address, subject: message.headers.subject)
    }
}
