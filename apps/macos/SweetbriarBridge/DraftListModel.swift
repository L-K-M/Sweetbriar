// `DraftListModel`: the sidebar's Drafts (local) list (spec/08
// "Drafts (local)"), free of AppKit so the bridge's tests exercise it.
//
// It reads `drafts()`, one reload at a time; a reload asked for
// meanwhile runs once after it. A failed reload keeps the rows it
// showed. `retire()` ends the list for an old archive: a result that
// arrives later is dropped and no row opens anything.

import Foundation
import Observation

/// The query the Drafts list needs (spec/07 "Other queries").
public protocol DraftListSource: Sendable {
    func drafts() async throws -> [DraftSummary]
}

extension CoreBridge: DraftListSource {}

@MainActor
@Observable
public final class DraftListModel {
    /// A row's sending identity (spec/08 "Drafts (local)").
    public enum From: Equatable, Sendable {
        /// The draft has no identity.
        case none
        /// The compose catalog names it.
        case identity(ComposeIdentity)
        /// The catalog doesn't name it, or couldn't be read: never a
        /// guessed address.
        case unavailable(identityId: String)
    }

    /// One local draft, as the list shows it.
    public struct Row: Identifiable, Equatable, Sendable {
        /// The draft's id.
        public var id: String
        public var from: From
        /// The subject, or "No Subject".
        public var subject: String
        /// To, then Cc, by name or address; Bcc is never shown.
        public var recipients: String
        public var isQueued: Bool
        public var updated: Date

        init(_ summary: DraftSummary, identities: [ComposeIdentity]?) {
            id = summary.draftId
            from =
                if let identityId = summary.identityId {
                    identities?.first { $0.identityId.utf8.elementsEqual(identityId.utf8) }
                        .map(From.identity) ?? .unavailable(identityId: identityId)
                } else {
                    .none
                }
            let subject = summary.subject.trimmingCharacters(in: .whitespacesAndNewlines)
            self.subject = subject.isEmpty ? DraftListModel.noSubject : subject
            recipients = (summary.to + summary.cc).map(Self.label(of:)).joined(separator: ", ")
            isQueued = summary.state == .queued
            updated = Date(timeIntervalSince1970: TimeInterval(summary.updatedAtMs) / 1000)
        }

        private static func label(of recipient: DraftRecipient) -> String {
            guard let name = recipient.name?.trimmingCharacters(in: .whitespaces), !name.isEmpty else {
                return recipient.address
            }
            return name
        }
    }

    public nonisolated static let noSubject = "No Subject"

    public private(set) var rows: [Row] = []
    /// Why the last reload failed, if it did; the rows keep what they
    /// showed before.
    public private(set) var lastError: (any Error)?
    public private(set) var isRetired = false

    private let source: any DraftListSource
    /// Names each row's identity; without it every identity is unavailable.
    private let catalog: (any ComposeCatalogSource)?
    @ObservationIgnored
    private var reloading = false
    @ObservationIgnored
    private var reloadAgain = false

    public init(source: any DraftListSource, catalog: (any ComposeCatalogSource)? = nil) {
        self.source = source
        self.catalog = catalog
    }

    /// Re-read the drafts. A call during a reload makes it run once
    /// more when it ends, and returns at once.
    public func reload() async {
        guard !isRetired else { return }
        guard !reloading else {
            reloadAgain = true
            return
        }
        reloading = true
        defer { reloading = false }
        repeat {
            reloadAgain = false
            await readOnce()
        } while reloadAgain && !isRetired
    }

    /// The draft a row opens: nil once retired or when it isn't listed.
    public func draftId(forRow id: String) -> String? {
        guard !isRetired, rows.contains(where: { $0.id == id }) else { return nil }
        return id
    }

    /// The old archive is going away (spec/08 "Drafts (local)").
    public func retire() {
        isRetired = true
        rows = []
        lastError = nil
    }

    private func readOnce() async {
        do {
            let summaries = try await source.drafts()
            // A catalog that can't be read leaves the identities
            // unavailable; it doesn't fail the list.
            let identities = try? await catalog?.composeCatalog(preferredAccountId: nil).identities
            guard !isRetired else { return }
            rows = summaries.map { Row($0, identities: identities) }
            lastError = nil
        } catch {
            guard !isRetired else { return }
            lastError = error
        }
    }
}
