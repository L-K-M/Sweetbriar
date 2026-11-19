// The Recovery list (spec/07 `recovery`; spec/04-sync-imap "Epoch
// change"): logical messages with no present occurrence, nothing in it
// deleted. The bridge's own value types, mirroring the core's field by
// field, so nothing generated crosses into the app.

import Foundation
import SweetbriarCore

/// One Recovery row, newest first (spec/07).
public struct Recovery: Sendable, Equatable {
    public var messageId: Int64
    public var accountId: String
    public var mailboxId: Int64
    public var mailboxName: String
    public var state: RecoveryState
    /// The last occurrence's `last_seen_at`.
    public var lastSeenAt: Date
    /// The last occurrence's id, the order's tie-break.
    public var occurrenceId: Int64

    public init(
        messageId: Int64, accountId: String, mailboxId: Int64, mailboxName: String,
        state: RecoveryState, lastSeenAt: Date, occurrenceId: Int64
    ) {
        self.messageId = messageId
        self.accountId = accountId
        self.mailboxId = mailboxId
        self.mailboxName = mailboxName
        self.state = state
        self.lastSeenAt = lastSeenAt
        self.occurrenceId = occurrenceId
    }

    init(_ value: SweetbriarCore.Recovery) {
        self.init(
            messageId: value.messageId,
            accountId: value.accountId,
            mailboxId: value.mailboxId,
            mailboxName: value.mailboxName,
            state: RecoveryState(value.state),
            lastSeenAt: Date.epochMilliseconds(value.lastSeenAtMs),
            occurrenceId: value.occurrenceId
        )
    }
}

/// The occurrence states the Recovery list shows (spec/07).
public enum RecoveryState: Sendable, Equatable {
    case vanished
    case epochStale
    case superseded

    init(_ value: SweetbriarCore.RecoveryState) {
        switch value {
        case .vanished: self = .vanished
        case .epochStale: self = .epochStale
        case .superseded: self = .superseded
        }
    }
}

/// One page of the Recovery list, newest first.
public struct RecoveryPage: Sendable, Equatable {
    public var rows: [Recovery]
    /// The last row's cursor; nil when the page is empty.
    public var after: String?
    public var hasMore: Bool

    public init(rows: [Recovery], after: String?, hasMore: Bool) {
        self.rows = rows
        self.after = after
        self.hasMore = hasMore
    }

    init(_ value: SweetbriarCore.RecoveryPage) {
        self.init(
            rows: value.rows.map(Recovery.init),
            after: value.after,
            hasMore: value.hasMore
        )
    }
}

/// Where a Recovery page starts: the top, or rows strictly older than
/// the cursor's row.
public enum RecoveryAnchor: Sendable, Equatable {
    case top
    case after(cursor: String)
}
