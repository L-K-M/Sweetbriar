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
    /// One local draft, as the list shows it.
    public struct Row: Identifiable, Equatable, Sendable {
        /// The draft's id.
        public var id: String
        /// The subject, or "No Subject".
        public var subject: String
        /// To, then Cc, by name or address; Bcc is never shown.
        public var recipients: String
        public var isQueued: Bool
        public var updated: Date

        init(_ summary: DraftSummary) {
            id = summary.draftId
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
    @ObservationIgnored
    private var reloading = false
    @ObservationIgnored
    private var reloadAgain = false

    public init(source: any DraftListSource) {
        self.source = source
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
            guard !isRetired else { return }
            rows = summaries.map(Row.init)
            lastError = nil
        } catch {
            guard !isRetired else { return }
            lastError = error
        }
    }
}
