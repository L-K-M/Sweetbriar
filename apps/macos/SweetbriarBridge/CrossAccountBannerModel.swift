// The compose window's cross-account banner (spec/08 "Cross-account
// check", spec/05 "Cross-account check"): the current revision's notice
// and this window's memory-only acknowledgements. The notice is
// re-queried per saved revision, so a changed identity or recipients
// brings the banner back; an answer for an older revision is dropped.

import Foundation
import Observation

/// One compose window's cross-account banner state (spec/08
/// "Cross-account check"). `notice` is the current saved revision's
/// warning; `keepCurrentNotice` remembers its acknowledgement in memory
/// for this window only, so a relaunch asks again (spec/05).
@MainActor
@Observable
public final class CrossAccountBannerModel {
    /// The shown notice, when the current revision has one.
    public private(set) var notice: CrossAccountNotice?
    /// The revision `notice` belongs to; nil when none shows.
    public private(set) var revisionId: String?
    /// Acknowledgements remembered for this window (spec/05: never
    /// persisted). Kept observable so the banner's visibility follows
    /// Keep without a separate callback.
    public private(set) var acknowledgedAcks: Set<String> = []
    /// The newest revision asked about: an answer for an older one is
    /// stale and ignored.
    @ObservationIgnored private var queriedRevisionId: String?
    @ObservationIgnored private var queryTicket: UInt64 = 0

    public init() {}

    /// The banner shows while the current revision's notice is
    /// unacknowledged (spec/08).
    public var isVisible: Bool {
        guard let notice else { return false }
        return !acknowledgedAcks.contains(notice.ack)
    }

    /// Whether this window remembered `ack` via Keep (spec/08). Send
    /// passes it without asking again.
    public func isAcknowledged(_ ack: String) -> Bool {
        acknowledgedAcks.contains(ack)
    }

    /// Keep the current notice's route account (spec/08): remember its
    /// acknowledgement in memory for this window only.
    public func keepCurrentNotice() {
        guard let notice else { return }
        acknowledgedAcks.insert(notice.ack)
    }

    /// Query the notice for `revisionId`. An answer for an older
    /// revision is ignored; a failure hides the banner (Send still
    /// confirms on its own, spec/08).
    public func refresh(
        source: any SendSource, draftId: String, revisionId: String
    ) async {
        let ticket = queryTicket &+ 1
        queryTicket = ticket
        queriedRevisionId = revisionId
        // The previous revision's notice no longer describes the current
        // one: hide until this answer lands.
        notice = nil
        self.revisionId = nil
        do {
            let answer = try await source.crossAccountNotice(
                draftId: draftId, revisionId: revisionId)
            guard ticket == queryTicket, revisionId == queriedRevisionId else { return }
            notice = answer
            self.revisionId = answer == nil ? nil : revisionId
        } catch {
            guard ticket == queryTicket, revisionId == queriedRevisionId else { return }
            notice = nil
            self.revisionId = nil
        }
    }

    /// The revision to query after a save or a load, or nil when the
    /// current base is nil or was already asked about.
    public func nextRevision(toQuery baseRevisionId: String?) -> String? {
        guard let baseRevisionId, baseRevisionId != queriedRevisionId else { return nil }
        return baseRevisionId
    }

    /// The identity the banner's Use button switches to: the source
    /// account's default when it can send, else its first sendable
    /// identity; nil when the account has none (spec/08, 05).
    public static func useIdentity(
        for notice: CrossAccountNotice, in identities: [ComposeIdentity]
    ) -> ComposeIdentity? {
        let account = identities.filter { $0.accountId == notice.sourceAccountId }
        func sendable(_ identity: ComposeIdentity) -> Bool {
            (identity.accountKind == .imap || identity.accountKind == .proton)
                && identity.sendable != .unavailable
        }
        return account.first { $0.isDefault && sendable($0) }
            ?? account.first(where: sendable)
    }
}
