// `CoreBridge`: the L1 boundary (spec/08). It owns the generated
// `Core`, gives every call a fresh `CancelToken` wired to the calling
// task's cancellation, and publishes settings snapshots through the
// owner's `FrameBatcher`. App code never imports `SweetbriarCore`.

import Foundation
import SweetbriarCore

/// A live core. `open` it, call it, `shutdown()` before exit.
///
/// Every call runs inside `withTaskCancellationHandler`: cancelling
/// the caller's task cancels the call's `CancelToken`, and a
/// `Cancelled` from the core comes back to the caller as Swift's
/// `CancellationError` (spec/07). `shutdown()` stays uncancellable.
public final class CoreBridge: Sendable {
    private let core: SweetbriarCore.Core
    private let snapshots: FrameBatcher<[Setting]>

    private init(core: SweetbriarCore.Core, snapshots: FrameBatcher<[Setting]>) {
        self.core = core
        self.snapshots = snapshots
    }

    /// Open the core under explicit roots.
    public static func open(
        config: CoreConfig,
        snapshots: FrameBatcher<[Setting]>
    ) async throws -> CoreBridge {
        let token = SweetbriarCore.CancelToken()
        let core = try await withCancellation(token: token) {
            try await SweetbriarCore.openCore(
                token: token,
                config: SweetbriarCore.CoreConfig(
                    dataRoot: config.dataRoot,
                    cacheRoot: config.cacheRoot
                )
            )
        }
        return CoreBridge(core: core, snapshots: snapshots)
    }

    /// Open the core under the platform's default roots.
    public static func open(
        snapshots: FrameBatcher<[Setting]>
    ) async throws -> CoreBridge {
        let config: SweetbriarCore.CoreConfig
        do {
            config = try SweetbriarCore.platformDefaultConfig()
        } catch let error as SweetbriarCore.CoreError {
            throw CoreFailure(error)
        }
        return try await open(
            config: CoreConfig(
                dataRoot: config.dataRoot,
                cacheRoot: config.cacheRoot
            ),
            snapshots: snapshots
        )
    }

    /// Every registered setting with its effective value. The snapshot
    /// also goes to the batcher for `@MainActor` observers.
    public func settings() async throws -> [Setting] {
        let token = SweetbriarCore.CancelToken()
        let read = try await Self.withCancellation(token: token) {
            try await self.core.settings(token: token)
        }
        let snapshot = read.map(Setting.init)
        snapshots.post(snapshot)
        return snapshot
    }

    /// Write one setting under a fresh `UUID` idempotency key.
    ///
    /// Once the core has begun the command it runs to completion
    /// (spec/07): the receipt comes back even if the token is
    /// cancelled meanwhile. The changed snapshot is then published on
    /// a token of its own, so a caller cancelled after the commit
    /// can't leave observers showing the old value; a failed re-read
    /// leaves them until the next snapshot.
    @discardableResult
    public func setSetting(key: String, value: String) async throws -> Receipt {
        let token = SweetbriarCore.CancelToken()
        let receipt = try await Self.withCancellation(token: token) {
            try await self.core.setSetting(
                token: token,
                idempotencyKey: UUID().uuidString,
                key: key,
                value: value
            )
        }
        let fresh = try? await core.settings(token: SweetbriarCore.CancelToken())
        if let fresh {
            snapshots.post(fresh.map(Setting.init))
        }
        return Receipt(
            command: receipt.command,
            resultJson: receipt.resultJson,
            createdAtMs: receipt.createdAtMs
        )
    }

    /// Close the core. Uncancellable by spec/07: it waits for calls in
    /// flight, so the caller just awaits it.
    public func shutdown() async {
        await core.shutdown()
    }

    /// Run `work` with `token` bound to the surrounding task's
    /// cancellation, and translate core errors into the bridge's
    /// public surface: `Cancelled` becomes `CancellationError`, the
    /// rest `CoreFailure`.
    private static func withCancellation<T>(
        token: SweetbriarCore.CancelToken,
        _ work: () async throws -> T
    ) async throws -> T {
        try await withTaskCancellationHandler {
            do {
                return try await work()
            } catch let error as SweetbriarCore.CoreError {
                if case .Cancelled = error {
                    throw CancellationError()
                }
                throw CoreFailure(error)
            }
        } onCancel: {
            token.cancel()
        }
    }
}

extension Setting {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.Setting) {
        self.init(
            key: generated.key,
            value: generated.value,
            isDefault: generated.isDefault
        )
    }
}
