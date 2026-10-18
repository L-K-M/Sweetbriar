// `SidebarModel`: what the main window's sidebar lists (spec/08
// "Sidebar"), free of AppKit so the bridge's Linux tests exercise it.
//
//   Unified                      ← fixed: Inbox, Flagged, then entries
//     Inbox      unified(inbox)    whose steps haven't landed, disabled
//     Flagged    flagged
//     Drafts (local) ┐
//     Out Box        ├ unavailable, with a note
//     Saved Views    ┘
//   <account>                    ← one section per account, in order
//     <mailbox>  mailbox(id)     ← folders, labels, virtual; a mailbox
//                                  that isn't selectable opens nothing
//
// It reads `accounts()`, then each account's `mailboxes()`, one reload
// at a time; a reload asked for meanwhile runs next. `generation` is
// the oldest list generation the sections show, so a list that saw a
// newer one can ask for a reload with `noteGeneration`.

import Observation

/// The sidebar queries a model needs from the core (spec/07 "Other
/// queries"). `CoreBridge` adopts it; tests substitute a fake.
public protocol SidebarSource: Sendable {
    func accounts() async throws -> [Account]
    func mailboxes(accountId: String) async throws -> Mailboxes
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
    }

    /// What choosing an entry does.
    public enum Target: Equatable, Sendable {
        /// Open a list over this window.
        case window(WindowSpec)
        /// Nothing yet: shown disabled, with why.
        case unavailable(note: String)
    }

    /// The unified section's id, also its entries' prefix.
    public static let unifiedSectionID = "unified"
    /// The Unified Inbox entry's id: the main window's first choice.
    public static let unifiedInboxID = "\(unifiedSectionID)/inbox"
    /// The note on a mailbox that isn't selectable (spec/08).
    static let notSelectableNote = "This mailbox holds no messages of its own."
    /// The note on entries whose steps haven't landed (spec/08).
    static let notYetNote = "Not available yet."

    /// The unified section, then one per account.
    public private(set) var sections: [Section] = [SidebarModel.unifiedSection]

    /// The oldest list generation the account sections show; 0 before
    /// the first reload.
    public private(set) var generation: UInt64 = 0

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
            sections = [Self.unifiedSection] + accountSections
            generation = oldest ?? generation
            lastError = nil
        } catch {
            lastError = error
        }
    }

    /// The unified section: Inbox and Flagged open windows; Drafts
    /// (local), Out Box and saved views wait for their steps (spec/08).
    private static let unifiedSection = Section(
        id: unifiedSectionID,
        title: "Unified",
        entries: [
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
                id: "\(unifiedSectionID)/drafts",
                title: "Drafts (local)",
                unread: nil,
                target: .unavailable(note: notYetNote)
            ),
            Entry(
                id: "\(unifiedSectionID)/outbox",
                title: "Out Box",
                unread: nil,
                target: .unavailable(note: notYetNote)
            ),
            Entry(
                id: "\(unifiedSectionID)/views",
                title: "Saved Views",
                unread: nil,
                target: .unavailable(note: notYetNote)
            ),
        ]
    )

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
            }
        )
    }
}
