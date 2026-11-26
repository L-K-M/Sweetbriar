import Foundation
import Observation
import SweetbriarBridge

/// Settings → Sending's allowances (spec/08 "Recipient check"): every
/// allowance by account, each removable, which brings the block back.
@MainActor
@Observable
final class RecipientAllowancesModel {
    /// One account's allowances, in the core's order.
    struct Group: Equatable {
        let accountId: String
        let accountName: String
        let allowances: [RecipientAllowance]
    }

    private let source: any RecipientFenceSource
    private let accountNames: @MainActor () async -> [String: String]
    private(set) var groups: [Group] = []
    private(set) var isLoading = false
    private var listLoadSucceeded = false
    /// A successful load confirmed the list is empty.
    var showsEmptyState: Bool { listLoadSucceeded && !isLoading && groups.isEmpty }
    private(set) var removing: RecipientAllowance?
    /// The last remove's failure; a failed removal outranks a stale
    /// list read, since the user's action is what they need to know.
    private(set) var removeError: String?
    /// The last list read's failure.
    private(set) var loadError: String?
    /// The most useful failure to show.
    var error: String? { removeError ?? loadError }
    /// The newest load: an older one that finishes later is dropped, so
    /// a removed row never comes back.
    @ObservationIgnored private var loadTicket: UInt64 = 0

    init(source: any RecipientFenceSource, accountNames: @escaping @MainActor () async -> [String: String]) {
        self.source = source
        self.accountNames = accountNames
    }

    func load() async {
        loadTicket &+= 1
        let mine = loadTicket
        isLoading = true
        defer { if mine == loadTicket { isLoading = false } }
        do {
            let allowances = try await source.recipientAllowances()
            let names = await accountNames()
            guard mine == loadTicket else { return }
            var order: [String] = []
            var byAccount: [String: [RecipientAllowance]] = [:]
            for allowance in allowances {
                if byAccount[allowance.accountId] == nil { order.append(allowance.accountId) }
                byAccount[allowance.accountId, default: []].append(allowance)
            }
            groups = order.map {
                Group(accountId: $0, accountName: names[$0] ?? $0, allowances: byAccount[$0] ?? [])
            }
            listLoadSucceeded = true
            loadError = nil
        } catch {
            guard mine == loadTicket else { return }
            listLoadSucceeded = false
            loadError = "Couldn't load the allowed addresses: \(error.localizedDescription)"
        }
    }

    /// Remove one allowance; the block applies again. The list reloads
    /// either way, so it never shows a stale row.
    func remove(_ allowance: RecipientAllowance) async {
        guard removing == nil else { return }
        removing = allowance
        defer { removing = nil }
        do {
            _ = try await source.removeRecipientAllowance(accountId: allowance.accountId, address: allowance.address)
            removeError = nil
        } catch {
            removeError = "Couldn't remove \(allowance.address): \(error.localizedDescription)"
        }
        let failure = removeError
        await load()
        if let failure { removeError = failure }
    }
}
