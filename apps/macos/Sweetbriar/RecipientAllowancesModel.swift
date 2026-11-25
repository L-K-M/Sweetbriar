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
    private(set) var removing: RecipientAllowance?
    private(set) var error: String?

    init(source: any RecipientFenceSource, accountNames: @escaping @MainActor () async -> [String: String]) {
        self.source = source
        self.accountNames = accountNames
    }

    func load() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let allowances = try await source.recipientAllowances()
            let names = await accountNames()
            var order: [String] = []
            var byAccount: [String: [RecipientAllowance]] = [:]
            for allowance in allowances {
                if byAccount[allowance.accountId] == nil { order.append(allowance.accountId) }
                byAccount[allowance.accountId, default: []].append(allowance)
            }
            groups = order.map {
                Group(accountId: $0, accountName: names[$0] ?? $0, allowances: byAccount[$0] ?? [])
            }
            error = nil
        } catch {
            self.error = "Couldn't load the allowed addresses: \(error.localizedDescription)"
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
            error = nil
        } catch {
            self.error = "Couldn't remove \(allowance.address): \(error.localizedDescription)"
        }
        let failure = error
        await load()
        if let failure { error = failure }
    }
}
