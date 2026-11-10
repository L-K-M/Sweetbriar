// The store's health (spec/07 `health()`, `subscribe_health`; spec/03
// "Integrity"): whether the store is read-only after an integrity
// finding. The bridge's own value types, mirroring the core's field
// by field, so nothing generated crosses into the app.

import Foundation
import SweetbriarCore

/// The store's health as the app shows it (spec/07).
public struct Health: Sendable, Equatable {
    public var integrity: Integrity

    public init(integrity: Integrity) {
        self.integrity = integrity
    }

    init(_ value: SweetbriarCore.Health) {
        self.init(integrity: Integrity(value.integrity))
    }
}

/// Whether an integrity check found the database damaged (spec/03
/// "Integrity").
public enum Integrity: Sendable, Equatable {
    /// No finding is recorded.
    case sound
    /// A finding latched the store. Queries go on; every command that
    /// writes fails `corrupt`; no account syncs and no background job
    /// runs. Only a restore ends it.
    ///
    /// - `detail`: the finding, or why its record can't be read.
    /// - `detectedAt`: when the check found it; absent when the record
    ///   can't be read.
    /// - `copyPath`: the copy of the database files under `recovery/`,
    ///   once it is published.
    case readOnly(detail: String, detectedAt: Date?, copyPath: String?)

    init(_ value: SweetbriarCore.Integrity) {
        switch value {
        case .sound:
            self = .sound
        case .readOnly(let detail, let detectedAtMs, let copyPath):
            let detectedAt = detectedAtMs.map {
                Date(timeIntervalSince1970: TimeInterval($0) / 1000)
            }
            self = .readOnly(detail: detail, detectedAt: detectedAt, copyPath: copyPath)
        }
    }
}
