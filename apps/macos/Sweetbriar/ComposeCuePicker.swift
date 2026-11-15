import AppKit
import SweetbriarBridge
import SwiftUI

/// Native radio controls always display the acknowledged model value. AppKit
/// changes a radio's state before its action, so restore that value before
/// starting the receipt-aware command.
struct ComposeCuePicker: NSViewRepresentable {
    let mode: ComposeCueMode?
    let enabled: Bool
    let choose: @MainActor (ComposeCueMode) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSStackView {
        let standard = NSButton(
            radioButtonWithTitle: "Standard", target: context.coordinator,
            action: #selector(Coordinator.selected(_:)))
        let strong = NSButton(
            radioButtonWithTitle: "Strong", target: context.coordinator,
            action: #selector(Coordinator.selected(_:)))
        standard.setAccessibilityIdentifier("composeCueStandard")
        strong.setAccessibilityIdentifier("composeCueStrong")
        let stack = NSStackView(views: [standard, strong])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        stack.setAccessibilityRole(.radioGroup)
        stack.setAccessibilityLabel("Compose identity cue")
        stack.setAccessibilityIdentifier("composeCuePicker")
        context.coordinator.standard = standard
        context.coordinator.strong = strong
        return stack
    }

    func updateNSView(_ stack: NSStackView, context: Context) {
        context.coordinator.mode = mode
        context.coordinator.choose = choose
        context.coordinator.standard?.isEnabled = enabled
        context.coordinator.strong?.isEnabled = enabled
        context.coordinator.showAcknowledgedMode()
    }

    @MainActor
    final class Coordinator: NSObject {
        weak var standard: NSButton?
        weak var strong: NSButton?
        var mode: ComposeCueMode?
        var choose: (@MainActor (ComposeCueMode) -> Void)?

        func showAcknowledgedMode() {
            standard?.state = mode == .standard ? .on : .off
            strong?.state = mode == .strong ? .on : .off
        }

        @objc func selected(_ sender: NSButton) {
            guard sender === standard || sender === strong else { return }
            let proposed: ComposeCueMode = sender === strong ? .strong : .standard
            showAcknowledgedMode()
            choose?(proposed)
        }
    }
}
