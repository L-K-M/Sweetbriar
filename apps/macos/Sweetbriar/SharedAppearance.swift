import SweetbriarBridge

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
