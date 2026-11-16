import Foundation
import Observation

/// The registered `reader.reading_view_senders` setting (spec/02, 08).
public protocol ReadingViewMemorySource: AnyObject, Sendable {
    func readingViewSenders() async throws -> [String]
    func setReadingViewSenders(_ senders: [String]) async throws -> Receipt
}

extension CoreBridge: ReadingViewMemorySource {
    public func readingViewSenders() async throws -> [String] {
        let settings = try await settings()
        guard let setting = settings.first(where: { $0.key == ReadingViewMemory.settingKey }) else {
            throw CoreFailure.unavailable(reason: "The reading view sender list is missing.")
        }
        return ReadingViewMemory.decode(setting.value)
    }

    public func setReadingViewSenders(_ senders: [String]) async throws -> Receipt {
        try await setSetting(key: ReadingViewMemory.settingKey, value: ReadingViewMemory.encode(senders))
    }
}

/// A full list refuses another sender (spec/08).
public enum ReadingViewRefusal: Sendable, Equatable { case listFull }

/// One shared owner of the per-sender Reading view memory. A failed read
/// or write leaves the previous list.
@MainActor @Observable
public final class ReadingViewMemory {
    public nonisolated static let settingKey = "reader.reading_view_senders"
    public static let limit = 1_000

    public private(set) var senders: [String] = []
    private let source: any ReadingViewMemorySource

    public init(source: any ReadingViewMemorySource) {
        self.source = source
    }

    public nonisolated static func decode(_ value: String) -> [String] {
        value.isEmpty ? [] : value.components(separatedBy: "\n")
    }

    public nonisolated static func encode(_ senders: [String]) -> String {
        Array(Set(senders.map { $0.lowercased() })).sorted().joined(separator: "\n")
    }

    public func reload() async {
        if let loaded = try? await source.readingViewSenders() { senders = loaded }
    }

    public func opensReadingView(from address: String?) -> Bool {
        guard let address else { return false }
        return senders.contains(address.lowercased())
    }

    @discardableResult
    public func addSender(_ address: String) async throws -> ReadingViewRefusal? {
        let address = address.lowercased()
        guard !senders.contains(address) else { return nil }
        guard senders.count < Self.limit else { return .listFull }
        try await write(senders + [address])
        return nil
    }

    public func removeSender(_ address: String) async throws {
        try await write(senders.filter { $0 != address.lowercased() })
    }

    private func write(_ list: [String]) async throws {
        _ = try await source.setReadingViewSenders(list)
        senders = Self.decode(Self.encode(list))
    }
}
