// Filing across the L1 boundary (spec/07 "Messages"; spec/08 "Keyboard
// filing"): the five actions, the selection each is invoked on, one
// core call per account, and the toast that holds a move's undo
// tokens. The shell shows what this model answers; the core alone
// decides what may be filed.
//
//   action + selection ──► one call per account ──► refusals
//                               │ succeeded
//                               ▼
//                     toast (tokens) ──10 s or next──► gone
//                               │ Undo
//                               ▼
//                          undo(token) each

import Foundation
import Observation

/// The filing commands and the setting that ranks the palette;
/// `CoreBridge` adopts it.
public protocol FilingSource: Sendable {
    func moveMessages(
        idempotencyKey: String, selection: MessageSelection, toMailboxId: Int64
    ) async throws -> String?
    func deleteToTrash(idempotencyKey: String, selection: MessageSelection) async throws -> String?
    func addLabel(
        idempotencyKey: String, selection: MessageSelection, labelMailboxId: Int64
    ) async throws
    func removeLabel(
        idempotencyKey: String, selection: MessageSelection, labelMailboxId: Int64
    ) async throws
    func undo(idempotencyKey: String, undoToken: String) async throws
    func settings() async throws -> [Setting]
    func setSetting(key: String, value: String) async throws -> Receipt
}

extension CoreBridge: FilingSource {}

/// What one filing command does.
public enum FilingAction: Sendable, Equatable {
    case archive
    case trash
    case move(to: Mailbox)
    case addLabel(Mailbox)
    case removeLabel(Mailbox)

    /// The mailbox the user chose, which fixes the one account.
    var chosenMailbox: Mailbox? {
        switch self {
        case .archive, .trash: nil
        case .move(let mailbox), .addLabel(let mailbox), .removeLabel(let mailbox): mailbox
        }
    }

    /// What was done to `count` rows, for the toast.
    func summary(count: Int) -> String {
        let messages = count == 1 ? "1 message" : "\(count.formatted(.number)) messages"
        return switch self {
        case .archive: "Archived \(messages)"
        case .trash: "Moved \(messages) to Trash"
        case .move(let mailbox): "Moved \(messages) to \(mailbox.name)"
        case .addLabel(let label): "Added \(label.name) to \(messages)"
        case .removeLabel(let label): "Removed \(label.name) from \(messages)"
        }
    }
}

/// One account's part of a selection: one core call.
public struct FilingPart: Sendable, Equatable {
    public let accountId: String
    public let selection: MessageSelection
    /// The rows selected.
    public let count: Int

    public init(accountId: String, selection: MessageSelection, count: Int) {
        self.accountId = accountId
        self.selection = selection
        self.count = count
    }
}

/// A selection as captured when a command was invoked: a part per
/// account, in the order the list first names it.
public struct FilingSelection: Sendable, Equatable {
    /// The most occurrence ids one selection carries (spec/07
    /// "Selections").
    public static let explicitLimit = 1_000

    public let parts: [FilingPart]

    public init(parts: [FilingPart]) {
        self.parts = parts
    }

    /// The account of a one-account selection; nil for several.
    public var onlyAccountId: String? {
        parts.count == 1 ? parts.first?.accountId : nil
    }
}

/// The shell's own refusals (spec/08 "Keyboard filing").
public enum FilingRefusal: Error, Sendable, Equatable, LocalizedError {
    case listChanged
    case tooManyRows
    case severalAccounts
    case anotherAccount(mailbox: String)
    case noArchive

    public var errorDescription: String? {
        switch self {
        case .listChanged:
            "The list changed. Select the messages again."
        case .tooManyRows:
            """
            Select up to 1,000 messages. Larger selections work in a mailbox \
            with all of its messages selected.
            """
        case .severalAccounts:
            """
            The selected messages are in several accounts. Select messages \
            of one account: messages can't move between accounts.
            """
        case .anotherAccount(let mailbox):
            "\(mailbox) belongs to another account. Messages can't move between accounts."
        case .noArchive:
            "This account has no Archive folder. Use Move to… to choose a folder."
        }
    }
}

/// Why one account's call, or a whole action, changed nothing.
public struct FilingFailure: Sendable {
    /// Nil when the action was refused before any account's call.
    public let accountId: String?
    public let error: any Error

    public init(accountId: String? = nil, error: any Error) {
        self.accountId = accountId
        self.error = error
    }

    /// The explanation to show: the core's own words, without the name
    /// of the argument they refused.
    public var explanation: String {
        switch error as? CoreFailure {
        case .invalidInput(_, let message): Self.sentence(message)
        case .conflict(let detail): Self.sentence(detail)
        case .resetRequired: FilingRefusal.listChanged.localizedDescription
        default: error.localizedDescription
        }
    }

    private static func sentence(_ text: String) -> String {
        guard let first = text.first, let last = text.last else { return text }
        let capitalised = first.uppercased() + text.dropFirst()
        return ".!?".contains(last) ? capitalised : capitalised + "."
    }
}

/// What the last action did, and how to undo it.
public struct FilingToast: Sendable, Equatable, Identifiable {
    public let id: UUID
    public let message: String
    let tokens: [String]

    /// Labels and moves that planned nothing offer no Undo.
    public var canUndo: Bool { !tokens.isEmpty }
}

/// The main window's filing: runs actions, holds the toast and records
/// the palette's recent mailboxes.
@MainActor
@Observable
public final class FilingModel {
    public typealias Sleep = @Sendable (Duration) async throws -> Void

    /// How long the toast and its Undo are offered (spec/08).
    public static let toastDuration = Duration.seconds(10)

    public private(set) var toast: FilingToast?
    /// `filing.recent_mailboxes` as last read or written, newest first.
    public private(set) var recentMailboxIds: [Int64] = []

    private let source: any FilingSource
    private let sleep: Sleep
    @ObservationIgnored private var expiry: Task<Void, Never>?
    /// The last recording, which the next waits for.
    @ObservationIgnored private var recording: Task<Void, Never>?
    @ObservationIgnored private var retired = false

    public init(
        source: any FilingSource,
        sleep: @escaping Sleep = { try await Task.sleep(for: $0) }
    ) {
        self.source = source
        self.sleep = sleep
    }

    /// The archive is being replaced: nothing is offered or recorded
    /// any more.
    public func retire() {
        retired = true
        dismissToast()
    }

    public func dismissToast() {
        expiry?.cancel()
        expiry = nil
        toast = nil
    }

    /// Run `action` on `selection`, with each account's mailboxes as
    /// the sidebar last read them. Answers what was refused: the other
    /// accounts' calls still run and keep their effect.
    public func perform(
        _ action: FilingAction, on selection: FilingSelection, mailboxes: [String: [Mailbox]]
    ) async -> [FilingFailure] {
        guard !retired else { return [] }
        if let chosen = action.chosenMailbox {
            guard let account = selection.onlyAccountId else {
                return [FilingFailure(error: FilingRefusal.severalAccounts)]
            }
            guard account == chosen.accountId else {
                return [FilingFailure(error: FilingRefusal.anotherAccount(mailbox: chosen.name))]
            }
        }

        var tokens: [String] = []
        var filed = 0
        var failures: [FilingFailure] = []
        for part in selection.parts {
            guard !retired, !Task.isCancelled else { break }
            do {
                let token = try await call(action, part: part, mailboxes: mailboxes)
                if let token { tokens.append(token) }
                filed += part.count
            } catch is CancellationError {
                break
            } catch {
                failures.append(FilingFailure(accountId: part.accountId, error: error))
            }
        }

        guard !retired, filed > 0 else { return failures }
        offer(FilingToast(id: UUID(), message: action.summary(count: filed), tokens: tokens))
        if let chosen = action.chosenMailbox {
            await record(chosen.mailboxId)
        }
        return failures
    }

    /// Undo the toast `id`: each of its tokens once. A toast already
    /// expired, replaced or undone calls nothing.
    public func undo(_ id: FilingToast.ID) async -> [FilingFailure] {
        guard !retired, let toast, toast.id == id, toast.canUndo else { return [] }
        dismissToast()
        var failures: [FilingFailure] = []
        for token in toast.tokens {
            do {
                try await source.undo(idempotencyKey: UUID().uuidString, undoToken: token)
            } catch is CancellationError {
                break
            } catch {
                failures.append(FilingFailure(error: error))
            }
        }
        return failures
    }

    /// Read the palette's ranking. A failed or invalid read keeps the
    /// last one, since it is only a hint.
    public func loadRecentMailboxes() async {
        guard let settings = try? await source.settings(), !retired,
            let recent = RecentMailboxes.setting(in: settings)
        else { return }
        recentMailboxIds = recent
    }

    private func call(
        _ action: FilingAction, part: FilingPart, mailboxes: [String: [Mailbox]]
    ) async throws -> String? {
        let key = UUID().uuidString
        switch action {
        case .archive:
            guard let archive = FilingDestinations.archive(in: mailboxes[part.accountId] ?? [])
            else { throw FilingRefusal.noArchive }
            return try await source.moveMessages(
                idempotencyKey: key, selection: part.selection, toMailboxId: archive.mailboxId)
        case .trash:
            return try await source.deleteToTrash(idempotencyKey: key, selection: part.selection)
        case .move(let mailbox):
            return try await source.moveMessages(
                idempotencyKey: key, selection: part.selection, toMailboxId: mailbox.mailboxId)
        case .addLabel(let label):
            try await source.addLabel(
                idempotencyKey: key, selection: part.selection, labelMailboxId: label.mailboxId)
            return nil
        case .removeLabel(let label):
            try await source.removeLabel(
                idempotencyKey: key, selection: part.selection, labelMailboxId: label.mailboxId)
            return nil
        }
    }

    private func offer(_ next: FilingToast) {
        expiry?.cancel()
        toast = next
        let sleep = sleep
        expiry = Task { [weak self] in
            do {
                try await sleep(Self.toastDuration)
            } catch {
                return
            }
            guard !Task.isCancelled, let self, toast?.id == next.id else { return }
            toast = nil
            expiry = nil
        }
    }

    /// Put `mailboxId` first in the setting, after any recording still
    /// running so neither is lost. A failure keeps the previous ranking
    /// and never undoes the filing (spec/08).
    private func record(_ mailboxId: Int64) async {
        let previous = recording
        let task = Task { [weak self] in
            await previous?.value
            guard let self else { return }
            await writeRecent(mailboxId)
        }
        recording = task
        await task.value
    }

    private func writeRecent(_ mailboxId: Int64) async {
        guard !retired, let settings = try? await source.settings(), !retired,
            let stored = RecentMailboxes.setting(in: settings)
        else { return }
        let recent = RecentMailboxes.recording(mailboxId, in: stored)
        do {
            _ = try await source.setSetting(
                key: RecentMailboxes.key, value: RecentMailboxes.encode(recent))
        } catch {
            return
        }
        guard !retired else { return }
        recentMailboxIds = recent
    }
}
