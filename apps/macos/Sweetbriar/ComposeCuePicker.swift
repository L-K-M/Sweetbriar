import AppKit
import SweetbriarBridge
import SwiftUI

extension ComposeCueMode {
    fileprivate var title: String {
        switch self {
        case .wash: "Color Wash"
        case .standard: "Standard"
        case .strong: "Strong"
        case .minimal: "Minimal"
        }
    }

    fileprivate var radioIdentifier: String {
        "composeCue\(title.replacingOccurrences(of: " ", with: ""))"
    }

    fileprivate var spokenLabel: String {
        switch self {
        case .wash: "Color Wash, colors the header and the window behind the text"
        case .standard: "Standard, colors the identity strip"
        case .strong: "Strong, colors the identity strip and the upper window"
        case .minimal: "Minimal, colors only the monogram"
        }
    }
}

/// Native radio controls always display the acknowledged model value. AppKit
/// changes a radio's state before its action, so restore that value before
/// starting the receipt-aware command. Each sits beside its mode's preview.
struct ComposeCuePicker: NSViewRepresentable {
    let mode: ComposeCueMode?
    let enabled: Bool
    let preview: IdentityStripView.Content
    let choose: @MainActor (ComposeCueMode) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSGridView {
        let coordinator = context.coordinator
        let rows = ComposeCueMode.allCases.map { mode -> [NSView] in
            let radio = NSButton(
                radioButtonWithTitle: mode.title, target: coordinator,
                action: #selector(Coordinator.selected(_:)))
            radio.setAccessibilityIdentifier(mode.radioIdentifier)
            radio.setAccessibilityLabel(mode.spokenLabel)
            let picture = ComposeCuePreview(mode: mode)
            coordinator.radios[mode] = radio
            coordinator.previews[mode] = picture
            return [radio, picture]
        }
        // A grid keeps the radios siblings, so they stay one group.
        let grid = NSGridView(views: rows)
        grid.rowSpacing = 8
        grid.columnSpacing = 12
        for row in 0..<grid.numberOfRows { grid.row(at: row).yPlacement = .center }
        grid.setContentHuggingPriority(.required, for: .vertical)
        grid.setAccessibilityRole(.radioGroup)
        grid.setAccessibilityLabel("Compose identity cue")
        grid.setAccessibilityIdentifier("composeCuePicker")
        return grid
    }

    func updateNSView(_ grid: NSGridView, context: Context) {
        let coordinator = context.coordinator
        coordinator.mode = mode
        coordinator.choose = choose
        for radio in coordinator.radios.values { radio.isEnabled = enabled }
        for picture in coordinator.previews.values { picture.content = preview }
        coordinator.showAcknowledgedMode()
    }

    @MainActor
    final class Coordinator: NSObject {
        var radios: [ComposeCueMode: NSButton] = [:]
        var previews: [ComposeCueMode: ComposeCuePreview] = [:]
        var mode: ComposeCueMode?
        var choose: (@MainActor (ComposeCueMode) -> Void)?

        func showAcknowledgedMode() {
            for (candidate, radio) in radios { radio.state = candidate == mode ? .on : .off }
        }

        @objc func selected(_ sender: NSButton) {
            guard let proposed = radios.first(where: { $0.value === sender })?.key else { return }
            showAcknowledgedMode()
            choose?(proposed)
        }
    }
}
