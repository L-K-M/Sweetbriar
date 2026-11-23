import Foundation
import Testing

@testable import SweetbriarBridge

/// spec/09 "In the app": the inventory names each source's mailbox,
/// message count and size, the first eight, then how many more.
@Suite
struct ImportInventoryTests {
    private let locale = Locale(identifier: "en_US")

    private func source(_ ordinal: UInt64, messages: UInt64 = 2, bytes: UInt64 = 100) -> ImportSource {
        ImportSource(
            ordinal: ordinal, path: "/tmp/\(ordinal).mbox", mailbox: "Box \(ordinal)",
            kind: .mbox, messages: messages, bytes: bytes, sha256: "", needsVariant: false,
            variant: nil, messagesDone: 0
        )
    }

    private func size(_ bytes: Int64) -> String {
        bytes.formatted(.byteCount(style: .file).locale(locale))
    }

    @Test
    func eachSourceShowsItsMailboxCountAndSizeInInventoryOrder() {
        let lines = ImportInventory.lines(
            [source(1, messages: 1, bytes: 2_048), source(0, messages: 1_204, bytes: 3_200_000)],
            locale: locale
        )
        #expect(
            lines == [
                "Box 0: 1,204 messages, \(size(3_200_000))",
                "Box 1: 1 message, \(size(2_048))",
            ])
    }

    @Test
    func pastEightTheRestAreCounted() {
        #expect(ImportInventory.lines([], locale: locale).isEmpty)
        let eight = ImportInventory.lines((0..<8).map { source($0) }, locale: locale)
        #expect(eight.count == 8)
        #expect(eight.last == "Box 7: 2 messages, \(size(100))")
        let nine = ImportInventory.lines((0..<9).map { source($0) }, locale: locale)
        #expect(nine.count == 9)
        #expect(nine.last == "and 1 more source")
        let twelve = ImportInventory.lines((0..<12).reversed().map { source($0) }, locale: locale)
        #expect(twelve.first == "Box 0: 2 messages, \(size(100))")
        #expect(twelve.last == "and 4 more sources")
    }
}
