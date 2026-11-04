import SweetbriarBridge

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

    deinit {
        task?.cancel()
    }
}
