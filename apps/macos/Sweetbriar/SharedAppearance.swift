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
    private var task: Task<Void, Never>?

    func model(source: any AppearanceSource) -> AppearanceModel {
        if let current { return current }
        let model = AppearanceModel(source: source)
        current = model
        task = Task { await model.follow() }
        return model
    }

    deinit {
        task?.cancel()
    }
}
