// Read and star across the L1 boundary (spec/07 "Flags"; spec/08
// "Message list" Keyboard and "Reader" Marking read): the flags
// `set_flags` changes, the list's toggle, the `reader.mark_read`
// setting and the reader's mark-read timer.

import Foundation
import Observation

/// A flag `set_flags` changes, by its intent payload name (spec/02).
public enum MessageFlag: Sendable, Equatable {
    case seen
    case flagged

    public var name: String {
        switch self {
        case .seen: "\\Seen"
        case .flagged: "\\Flagged"
        }
    }
}

/// `set_flags` and the settings the reader's timer reads; `CoreBridge`
/// adopts it.
public protocol MessageFlagsSource: Sendable {
    func setFlags(
        idempotencyKey: String, selection: MessageSelection, add: [String], remove: [String]
    ) async throws -> String?
    func settings() async throws -> [Setting]
}

extension CoreBridge: MessageFlagsSource {}

/// One selected row as the flag commands see it.
public struct FlagTarget: Sendable, Equatable {
    public let occurrenceId: Int64
    public let accountId: String
    public let unread: Bool
    public let flagged: Bool

    public init(occurrenceId: Int64, accountId: String, unread: Bool, flagged: Bool) {
        self.occurrenceId = occurrenceId
        self.accountId = accountId
        self.unread = unread
        self.flagged = flagged
    }

    public init(_ row: ListRow) {
        self.init(
            occurrenceId: row.occurrenceId, accountId: row.accountId, unread: row.unread,
            flagged: row.flagged)
    }
}

/// One `set_flags` call of a toggle: an account's selected occurrences.
public struct FlagCall: Sendable, Equatable {
    public let accountId: String
    public let occurrenceIds: [Int64]
}

/// `⇧⌘U` and `⇧⌘L` on a selection (spec/08 "Message list" Keyboard):
/// when any row is unread (unstarred) every row becomes read (starred),
/// otherwise unread (unstarred). One call per account, in the order the
/// selection first names it.
public struct FlagToggle: Sendable, Equatable {
    public let flag: MessageFlag
    /// Whether the call adds the flag; `false` removes it.
    public let set: Bool
    public let calls: [FlagCall]

    /// The toggle for `targets`; nil for an empty selection.
    public static func plan(_ flag: MessageFlag, targets: [FlagTarget]) -> FlagToggle? {
        guard !targets.isEmpty else { return nil }
        let set: Bool
        switch flag {
        case .seen: set = targets.contains { $0.unread }
        case .flagged: set = targets.contains { !$0.flagged }
        }
        var order: [String] = []
        var ids: [String: [Int64]] = [:]
        for target in targets {
            if ids[target.accountId] == nil { order.append(target.accountId) }
            ids[target.accountId, default: []].append(target.occurrenceId)
        }
        let calls = order.map { FlagCall(accountId: $0, occurrenceIds: ids[$0] ?? []) }
        return FlagToggle(flag: flag, set: set, calls: calls)
    }

    /// Send every call; each commits its own local effect, so a refused
    /// one changes nothing while the others keep theirs (spec/08). The
    /// refusals, in call order.
    public func run(source: any MessageFlagsSource) async -> [any Error] {
        var failures: [any Error] = []
        let names = [flag.name]
        for call in calls {
            do {
                _ = try await source.setFlags(
                    idempotencyKey: UUID().uuidString,
                    selection: .occurrenceIds(call.occurrenceIds),
                    add: set ? names : [], remove: set ? [] : names)
            } catch {
                failures.append(error)
            }
        }
        return failures
    }
}

/// The registered `reader.mark_read` setting (spec/02): when the reader
/// marks a shown unread message read.
public enum MarkRead: Sendable, Equatable {
    case immediately
    case after(seconds: Int)
    case manual

    public static let key = "reader.mark_read"
    /// The registry's choices, in Settings' order.
    public static let choices: [MarkRead] = [
        .immediately, .after(seconds: 2), .after(seconds: 5), .after(seconds: 10), .manual,
    ]
    public static let fallback = MarkRead.immediately

    /// The stored value: `immediately`, `2`, `5`, `10` or `manual`.
    public var value: String {
        switch self {
        case .immediately: "immediately"
        case .after(let seconds): String(seconds)
        case .manual: "manual"
        }
    }

    public var title: String {
        switch self {
        case .immediately: "Immediately"
        case .after(let seconds): "After \(seconds) seconds"
        case .manual: "Manually"
        }
    }

    public init?(value: String) {
        guard let choice = Self.choices.first(where: { $0.value == value }) else { return nil }
        self = choice
    }

    /// The setting's valid value; nil when absent or outside the registry.
    public static func setting(in settings: [Setting]) -> MarkRead? {
        settings.first { $0.key == key }.flatMap { MarkRead(value: $0.value) }
    }
}

/// Settings → Reading's logic (spec/08 "Reader"): load `reader.mark_read`
/// and write one of its choices. The value publishes only after the
/// receipt; a failure keeps the last value and shows why.
@MainActor
@Observable
public final class ReadingSettingsModel {
    public private(set) var markRead: MarkRead?
    public private(set) var isLoading = false
    public private(set) var isSaving = false
    public private(set) var lastError: (any Error)?

    private let source: any SendSafetySettingsSource
    /// Bumped by each successful write, so a load that began before it
    /// can't put back the value it replaced.
    private var writes = 0

    public init(source: any SendSafetySettingsSource) {
        self.source = source
    }

    @discardableResult
    public func load() async -> Bool {
        isLoading = true
        lastError = nil
        defer { isLoading = false }
        let writesBefore = writes
        do {
            let settings = try await source.settings()
            guard writes == writesBefore else { return true }
            guard let value = MarkRead.setting(in: settings) else {
                lastError = CoreFailure.unavailable(
                    reason: "The \(MarkRead.key) setting is missing or invalid.")
                return false
            }
            markRead = value
            return true
        } catch {
            lastError = error
            return false
        }
    }

    @discardableResult
    public func setMarkRead(_ value: MarkRead) async -> Bool {
        guard !isSaving else { return false }
        isSaving = true
        lastError = nil
        defer { isSaving = false }
        do {
            _ = try await source.setSetting(key: MarkRead.key, value: value.value)
            writes += 1
            markRead = value
            return true
        } catch {
            lastError = error
            return false
        }
    }
}

/// The reader's mark-read timer (spec/08 "Reader" Marking read): a shown
/// unread message of a Proton account is marked read as
/// `reader.mark_read` says. Each `show` cancels the pending mark, so
/// another selection, a multi-row selection, a closed pane or marking
/// the message unread ends it; a message marked unread while shown
/// isn't marked again until it is shown again.
@MainActor
public final class MarkReadTimer {
    public typealias Sleep = @Sendable (Duration) async throws -> Void

    /// The last mark's refusal, or the setting's unreadable value; the
    /// row stays unread.
    public private(set) var lastError: (any Error)?
    /// Called after each mark that failed.
    public var onError: (@MainActor (any Error) -> Void)?

    private let source: any MessageFlagsSource
    private let sleep: Sleep
    private var pending: Task<Void, Never>?

    public init(
        source: any MessageFlagsSource,
        sleep: @escaping Sleep = { try await Task.sleep(for: $0) }
    ) {
        self.source = source
        self.sleep = sleep
    }

    /// The reader shows `target`, a Proton account's message, or nil for
    /// nothing, several rows or another account kind.
    public func show(_ target: FlagTarget?) {
        cancel()
        guard let target, target.unread else { return }
        let source = source
        let sleep = sleep
        pending = Task { [weak self] in
            do {
                let settings = try await source.settings()
                guard let setting = MarkRead.setting(in: settings) else {
                    throw CoreFailure.unavailable(
                        reason: "The \(MarkRead.key) setting is missing or invalid.")
                }
                switch setting {
                case .manual: return
                case .immediately: break
                case .after(let seconds): try await sleep(.seconds(seconds))
                }
                try Task.checkCancellation()
                _ = try await source.setFlags(
                    idempotencyKey: UUID().uuidString,
                    selection: .occurrenceIds([target.occurrenceId]),
                    add: [MessageFlag.seen.name], remove: [])
            } catch is CancellationError {
            } catch {
                guard let self, !Task.isCancelled else { return }
                lastError = error
                onError?(error)
            }
        }
    }

    /// End a pending mark without marking.
    public func cancel() {
        pending?.cancel()
        pending = nil
    }

    /// The pending mark, for tests that wait for it to settle.
    public var pendingTask: Task<Void, Never>? { pending }
}
