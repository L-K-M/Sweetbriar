import AppKit
import SweetbriarBridge

func appearanceColor(_ hex: String) -> NSColor {
    guard hex.count == 7, hex.first == "#", let value = UInt32(hex.dropFirst(), radix: 16) else {
        return .labelColor
    }
    return NSColor(srgbRed: CGFloat((value >> 16) & 255) / 255,
                   green: CGFloat((value >> 8) & 255) / 255,
                   blue: CGFloat(value & 255) / 255, alpha: 1)
}

/// Owns one subscription for all windows using the app's core.
@MainActor
final class SharedAppearance {
    private var current: AppearanceModel?
    private var sourceID: ObjectIdentifier?
    private var task: Task<Void, Never>?

    func model(source: any AppearanceSource) -> AppearanceModel {
        let identity = ObjectIdentifier(source)
        if sourceID == identity, let current { return current }
        task?.cancel()
        sourceID = identity
        let model = AppearanceModel(source: source)
        current = model
        task = Task { await model.follow() }
        return model
    }

    /// A restored store can have a lower generation. Retire the old follower
    /// before handing replacement views a fresh model instead of filtering it out.
    func reset() async {
        let previous = task
        task = nil
        previous?.cancel()
        await previous?.value
        current = nil
        sourceID = nil
    }

    deinit {
        task?.cancel()
    }
}
