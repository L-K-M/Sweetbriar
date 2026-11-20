// Settings → Sending safety across the L1 boundary (spec/02
// "Sending", spec/08 "Settings"): the registered `sending.undo_seconds`
// choices, their shared source and the section's load/write model. The
// change publishes only after its receipt; a failure keeps the last
// value and shows its error.

import Foundation
import Observation

/// The registered `sending.undo_seconds` setting (spec/02): its dotted
/// key, its exact choices and the registry default a failed read falls
/// back to.
public enum SendSafety {
    public static let undoSecondsKey = "sending.undo_seconds"
    public static let undoSecondsChoices = [5, 7, 10, 20, 30]
    public static let fallbackUndoSeconds = 7

    /// The setting's valid value; nil when absent or outside the registry.
    public static func undoSeconds(in settings: [Setting]) -> Int? {
        guard let setting = settings.first(where: { $0.key == undoSecondsKey }),
            let value = Int(setting.value),
            undoSecondsChoices.contains(value)
        else { return nil }
        return value
    }
}

/// Reads and writes registered settings (spec/02); `CoreBridge` adopts
/// it. Separate from the account commands so the sending section and
/// the compose send model share one surface.
public protocol SendSafetySettingsSource: Sendable {
    func settings() async throws -> [Setting]
    func setSetting(key: String, value: String) async throws -> Receipt
}

extension CoreBridge: SendSafetySettingsSource {}

/// The Sending safety section's logic (spec/08 "Settings"; spec/02):
/// load the registered undo delay and write one of its choices. Nil is
/// unknown, never a guessed default; a failed read keeps the last value
/// and shows its error.
@MainActor
@Observable
public final class SendSafetySettingsModel {
    public private(set) var undoSeconds: Int?
    public private(set) var isLoading = false
    public private(set) var isSaving = false
    public private(set) var lastError: (any Error)?

    private let source: any SendSafetySettingsSource
    /// Bumped by each successful write, so a load that began before it
    /// can't put back the value it replaced.
    private var writes = 0

    public init(source: any SendSafetySettingsSource) {
        self.source = source
    }

    @discardableResult
    public func load() async -> Bool {
        isLoading = true
        lastError = nil
        defer { isLoading = false }
        let writesBefore = writes
        do {
            let settings = try await source.settings()
            guard writes == writesBefore else { return true }
            guard let value = SendSafety.undoSeconds(in: settings) else {
                lastError = CoreFailure.unavailable(
                    reason: "The \(SendSafety.undoSecondsKey) setting is missing or invalid.")
                return false
            }
            undoSeconds = value
            return true
        } catch {
            lastError = error
            return false
        }
    }

    /// Write one registered choice. The value publishes only after the
    /// core's receipt; a refusal keeps the last value and shows why.
    @discardableResult
    public func setUndoSeconds(_ value: Int) async -> Bool {
        guard SendSafety.undoSecondsChoices.contains(value), !isSaving else { return false }
        isSaving = true
        lastError = nil
        defer { isSaving = false }
        do {
            _ = try await source.setSetting(
                key: SendSafety.undoSecondsKey, value: String(value))
            writes += 1
            undoSeconds = value
            return true
        } catch {
            lastError = error
            return false
        }
    }
}
