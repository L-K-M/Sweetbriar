import Foundation
import Observation

// `SidebarModel`: what the main window's sidebar lists (spec/08
// "Sidebar"), free of AppKit so the bridge's Linux tests exercise it.
//
//   Unified                      ← fixed: Inbox, Flagged, Drafts, Out
//     Inbox      unified(inbox)    Box, then the saved views in
//     Flagged    flagged           saved_views() order, each opening
//     Drafts (local) drafts        View(id); Drafts is the local list
//     Out Box        outBox
//     <saved view>   view(id)
//   <account>                    ← one section per account, in order
//     <mailbox>  mailbox(id)     ← folders, labels, virtual; a mailbox
//                                  that isn't selectable opens nothing
//
// It reads `accounts()`, then each account's `mailboxes()`, one reload
// at a time; a reload asked for meanwhile runs next. `generation` is
// the oldest list generation the sections show, so a list that saw a
// newer one can ask for a reload with `noteGeneration`.

/// The sidebar queries a model needs from the core (spec/07 "Other
/// queries"). `CoreBridge` adopts it; tests substitute a fake.
public protocol SidebarSource: Sendable {
    func accounts() async throws -> [Account]
    func mailboxes(accountId: String) async throws -> Mailboxes
    func savedViews() async throws -> [SavedView]
    func upsertSavedView(idempotencyKey: String, params: SavedViewParams) async throws -> Receipt
    func deleteSavedView(idempotencyKey: String, viewId: String) async throws -> Receipt
}

extension CoreBridge: SidebarSource {}

@MainActor
@Observable
public final class SidebarModel {
    /// One group of entries under a heading.
    public struct Section: Identifiable, Equatable, Sendable {
        /// `unified`, or the account's id.
        public var id: String
        public var title: String
        public var entries: [Entry]
        /// The account an account section lists; `nil` for unified.
        public var account: Account?

        public init(id: String, title: String, entries: [Entry], account: Account? = nil) {
            self.id = id
            self.title = title
            self.entries = entries
            self.account = account
        }
    }

    /// One row under a heading.
    public struct Entry: Identifiable, Equatable, Sendable {
        /// Unique across the sidebar: the section's id, then the
        /// entry's.
        public var id: String
        public var title: String
        /// Unread messages, for mailboxes; nil where none is shown.
        public var unread: UInt64?
        public var target: Target

        public init(id: String, title: String, unread: UInt64?, target: Target) {
            self.id = id
            self.title = title
            self.unread = unread
            self.target = target
        }
    }

    /// What choosing an entry does.
    public enum Target: Equatable, Sendable {
        /// Open a list over this window.
        case window(WindowSpec)
        /// Show the local drafts list (spec/08 "Drafts (local)").
        case drafts
        /// Show the Out Box (spec/08 "Out Box").
        case outBox
        /// Nothing yet: shown disabled, with why.
        case unavailable(note: String)
    }

    /// The unified section's id, also its entries' prefix.
    public static let unifiedSectionID = "unified"
    /// The Unified Inbox entry's id: the main window's first choice.
    public static let unifiedInboxID = "\(unifiedSectionID)/inbox"
    /// The Drafts (local) entry's id.
    public static let unifiedDraftsID = "\(unifiedSectionID)/drafts"
    /// A saved view's entry id, stable across reloads.
    public static func savedViewEntryID(_ viewId: String) -> String {
        "\(unifiedSectionID)/view/\(viewId)"
    }
    /// The note on a mailbox that isn't selectable (spec/08).
    static let notSelectableNote = "This mailbox holds no messages of its own."

    /// The unified section, then one per account.
    public private(set) var sections: [Section] = [SidebarModel.unifiedSection([])]

    /// Account context comes from the loaded section, never by splitting an
    /// opaque entry identifier. Unified, removed and unknown entries have none.
    public func accountId(forEntry entryId: String) -> String? {
        sections.first {
            $0.id != Self.unifiedSectionID && $0.entries.contains { $0.id == entryId }
        }?.id
    }

    /// The Proton account a section heading or entry belongs to, which
    /// "Sign in again" opens the sign-in sheet for (spec/08); `nil` for
    /// every other row.
    public func protonAccount(forRow rowId: String) -> Account? {
        let section = sections.first { section in
            section.id == rowId || section.entries.contains { $0.id == rowId }
        }
        guard let account = section?.account, account.kind == .proton else { return nil }
        return account
    }

    /// The oldest list generation the account sections show; 0 before
    /// the first reload.
    public private(set) var generation: UInt64 = 0

    /// The saved views the unified section lists, in `saved_views()`
    /// order.
    public private(set) var savedViews: [SavedView] = []

    /// Why the last reload failed, if it did; the sections keep what
    /// they showed before.
    public private(set) var lastError: (any Error)?

    private let source: any SidebarSource
    @ObservationIgnored
    private var reloading = false
    @ObservationIgnored
    private var reloadAgain = false

    public init(source: any SidebarSource) {
        self.source = source
    }

    /// The entry with `id`, if listed.
    public func entry(id: String) -> Entry? {
        for section in sections {
            if let entry = section.entries.first(where: { $0.id == id }) {
                return entry
            }
        }
        return nil
    }

    /// The saved view `id` names, if listed.
    public func savedView(id: String) -> SavedView? {
        savedViews.first { $0.viewId == id }
    }

    /// Create a view from a search (spec/08 Search): the query and sort
    /// are stored, the scope is not part of a saved view (07 Views).
    @discardableResult
    public func saveView(
        name: String, queryJSON: String, sort: SavedViewSort
    ) async throws -> SavedView {
        let receipt = try await source.upsertSavedView(
            idempotencyKey: UUID().uuidString,
            params: SavedViewParams(viewId: nil, name: name, queryJson: queryJSON, sort: sort)
        )
        guard let viewId = receipt.savedViewID else {
            throw CoreFailure.internalError(detail: "The saved view's receipt named no view id.")
        }
        try await reloadSavedViews()
        guard let view = savedView(id: viewId) else {
            throw CoreFailure.internalError(detail: "The saved view vanished after writing.")
        }
        return view
    }

    /// Rename a view, keeping its stored query and sort (spec/08).
    @discardableResult
    public func renameView(id: String, to name: String) async throws -> SavedView {
        guard let view = savedView(id: id) else {
            throw CoreFailure.notFound(what: "saved view \(id)")
        }
        _ = try await source.upsertSavedView(
            idempotencyKey: UUID().uuidString,
            params: SavedViewParams(
                viewId: id, name: name, queryJson: view.queryJson, sort: view.sort
            )
        )
        try await reloadSavedViews()
        guard let renamed = savedView(id: id) else {
            throw CoreFailure.internalError(detail: "The renamed view vanished after writing.")
        }
        return renamed
    }

    /// Delete a view; an unknown id is `NotFound` (spec/07 Views).
    public func deleteView(id: String) async throws {
        _ = try await source.deleteSavedView(idempotencyKey: UUID().uuidString, viewId: id)
        try await reloadSavedViews()
    }

    /// Re-read the saved views after a write (spec/08); a reload in
    /// flight runs again, as it may have listed before the write.
    private func reloadSavedViews() async throws {
        if reloading { reloadAgain = true }
        updateSavedViews(try await source.savedViews())
    }

    /// Re-read every account and its mailboxes. One reload runs at a
    /// time: a call during one makes it run again once it ends, and
    /// returns at once.
    public func reload() async {
        guard !reloading else {
            reloadAgain = true
            return
        }
        reloading = true
        defer { reloading = false }
        repeat {
            reloadAgain = false
            await readOnce()
        } while reloadAgain
    }

    /// A list saw `listGeneration`: reload if the sections show an
    /// older one.
    public func noteGeneration(_ listGeneration: UInt64) async {
        guard listGeneration > generation else { return }
        await reload()
    }

    private func readOnce() async {
        do {
            let accounts = try await source.accounts()
            var accountSections: [Section] = []
            var oldest: UInt64?
            for account in accounts {
                let list = try await source.mailboxes(accountId: account.accountId)
                oldest = min(oldest ?? list.generation, list.generation)
                accountSections.append(Self.section(for: account, list.mailboxes))
            }
            let views = try await source.savedViews()
            updateSavedViews(views)
            sections = [Self.unifiedSection(views)] + accountSections
            generation = oldest ?? generation
            lastError = nil
        } catch {
            lastError = error
        }
    }

    /// Relist the saved views after a write, without re-reading accounts.
    private func updateSavedViews(_ views: [SavedView]) {
        savedViews = views
        guard !sections.isEmpty else { return }
        sections[0] = Self.unifiedSection(views)
    }

    /// The unified section: Inbox and Flagged open windows, Drafts
    /// (local) the drafts list, Out Box the Out Box (spec/08); each
    /// saved view opens `View(id)` (07).
    private static func unifiedSection(_ views: [SavedView]) -> Section {
        let fixed: [Entry] = [
            Entry(
                id: unifiedInboxID,
                title: "Inbox",
                unread: nil,
                target: .window(.unified(.inbox))
            ),
            Entry(
                id: "\(unifiedSectionID)/flagged",
                title: "Flagged",
                unread: nil,
                target: .window(.flagged)
            ),
            Entry(
                id: unifiedDraftsID,
                title: "Drafts (local)",
                unread: nil,
                target: .drafts
            ),
            Entry(
                id: "\(unifiedSectionID)/outbox",
                title: "Out Box",
                unread: nil,
                target: .outBox
            ),
        ]
        var entries = fixed
        for view in views {
            entries.append(
                Entry(
                    id: savedViewEntryID(view.viewId),
                    title: view.name,
                    unread: nil,
                    target: .window(.view(view.viewId))
                )
            )
        }
        return Section(id: unifiedSectionID, title: "Unified", entries: entries)
    }

    /// One account's section: its mailboxes in the core's order,
    /// named as stored (spec/08: imported mailboxes are flat).
    private static func section(for account: Account, _ mailboxes: [Mailbox]) -> Section {
        Section(
            id: account.accountId,
            title: account.displayName,
            entries: mailboxes.map { mailbox in
                Entry(
                    id: "\(account.accountId)/\(mailbox.mailboxId)",
                    title: mailbox.name,
                    unread: mailbox.unread,
                    target: mailbox.selectable
                        ? .window(.mailbox(mailbox.mailboxId))
                        : .unavailable(note: notSelectableNote)
                )
            },
            account: account
        )
    }
}
