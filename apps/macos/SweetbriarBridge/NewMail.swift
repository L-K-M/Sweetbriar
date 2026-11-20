// New-mail arrivals across the L1 boundary (spec/07
// `subscribe_new_mail`, spec/10 "Notifications"): one account's Inbox
// arrivals, which the shell turns into notifications. App code never
// imports `SweetbriarCore`.

import Foundation
import SweetbriarCore

/// One account's arrivals since the subscription's last answer
/// (spec/07). `count` is every arrival; `messageIds` the newest 20,
/// so the shell summarizes the rest and none is dropped.
public struct NewMail: Sendable, Equatable {
    public var accountId: String
    public var count: UInt32
    public var messageIds: [Int64]

    public init(accountId: String, count: UInt32, messageIds: [Int64]) {
        self.accountId = accountId
        self.count = count
        self.messageIds = messageIds
    }
}

/// The new-mail stream (spec/07): one in-flight `next` at a time, in
/// memory only.
public protocol NewMailStream: Sendable {
    /// One account's arrivals since the last answer. A second
    /// concurrent call is `invalidInput`; a cancelled call moves
    /// nothing, so the next call still sees what it missed.
    func next() async throws -> NewMail
}

/// The calls the new-mail notifier needs (spec/07, spec/10):
/// subscribe, the previews setting, the account list and one message's
/// headers. `CoreBridge` adopts it; tests substitute a fake.
public protocol NewMailSource: Sendable {
    func subscribeNewMail() async throws -> any NewMailStream
    func settings() async throws -> [Setting]
    func accounts() async throws -> [Account]
    func getMessage(messageId: Int64) async throws -> ReaderMessage
}

/// The new-mail stream (spec/07): one in-flight `next` at a time,
/// in-memory only. Cancelling the calling task cancels the call, and
/// a pending call ends with `unavailable` once the core shuts down.
public final class NewMailSubscription: NewMailStream, Sendable {
    private let subscription: SweetbriarCore.NewMailSubscription

    /// Built only from the generated subscription the core hands
    /// back; app code cannot make one.
    init(_ subscription: SweetbriarCore.NewMailSubscription) {
        self.subscription = subscription
    }

    public func next() async throws -> NewMail {
        let token = SweetbriarCore.CancelToken()
        let mail = try await CoreBridge.withCancellation(token: token) {
            try await self.subscription.next(token: token)
        }
        return NewMail(
            accountId: mail.accountId,
            count: mail.count,
            messageIds: mail.messageIds
        )
    }
}
