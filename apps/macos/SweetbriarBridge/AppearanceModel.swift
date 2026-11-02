// `AppearanceModel`: the shell's resolved appearance (spec/07 "Other
// queries", "Subscriptions"; spec/02 "Appearance storage") — every
// account's and identity's color, monogram and rendering tokens —
// and the v1 palette the pickers index. `follow` loads
// `appearance()`, then pulls its subscription: an invalidation
// re-reads on the same stream, `resetRequired` re-reads and
// resubscribes at the new generation. Free of AppKit so the bridge's
// tests exercise it.

import Observation
import SweetbriarCore

/// `appearance()`'s answer (spec/07): every account and identity at
/// one committed appearance generation — `subscribeAppearance`'s mark.
public struct Appearance: Sendable, Equatable {
    /// The committed appearance generation this answers for.
    public var generation: UInt64
    /// Every account, in sidebar order.
    public var accounts: [AccountAppearance]
    /// Every identity, ordered by account then id.
    public var identities: [IdentityAppearance]

    public init(
        generation: UInt64,
        accounts: [AccountAppearance],
        identities: [IdentityAppearance]
    ) {
        self.generation = generation
        self.accounts = accounts
        self.identities = identities
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.Appearance) {
        self.init(
            generation: value.generation, accounts: value.accounts.map(AccountAppearance.init),
            identities: value.identities.map(IdentityAppearance.init)
        )
    }
}

/// An account's resolved appearance (spec/07): the color, monogram
/// and tokens the shell draws, the collision flag, the color's
/// source, and the revision the setter expects.
public struct AccountAppearance: Sendable, Equatable {
    public var accountId: String
    /// The resolved color, `#rrggbb`.
    public var colorSrgb: String
    /// The tokens the shell draws over the row (spec/02).
    public var tokens: AppearanceTokens
    /// The stored symbol; `nil` derives `monogram` from the name.
    public var symbol: String?
    /// What the account displays: `symbol` or the name's first letter.
    public var monogram: String
    /// Another account shows the same monogram — label this row.
    public var collides: Bool
    /// Where the color came from (spec/02): a preset or a custom pick.
    public var assignment: AppearanceAssignment
    /// The record's revision; the setter requires it.
    public var revision: Int64

    public init(
        accountId: String,
        colorSrgb: String,
        tokens: AppearanceTokens,
        symbol: String?,
        monogram: String,
        collides: Bool,
        assignment: AppearanceAssignment,
        revision: Int64
    ) {
        self.accountId = accountId
        self.colorSrgb = colorSrgb
        self.tokens = tokens
        self.symbol = symbol
        self.monogram = monogram
        self.collides = collides
        self.assignment = assignment
        self.revision = revision
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.AccountAppearance) {
        self.init(
            accountId: value.accountId, colorSrgb: value.colorSrgb,
            tokens: AppearanceTokens(value.tokens), symbol: value.symbol,
            monogram: value.monogram, collides: value.collides,
            assignment: AppearanceAssignment(value.assignment), revision: value.revision
        )
    }
}

/// An identity's resolved appearance (spec/07): the effective color
/// and monogram — its overrides where set, its account's where
/// inherited — the overrides themselves, and the record revision the
/// setter expects (`nil` when no record exists).
public struct IdentityAppearance: Sendable, Equatable {
    public var identityId: String
    public var accountId: String
    /// The color the identity displays, `#rrggbb`.
    public var colorSrgb: String
    /// The stored override; `nil` displays the account's color.
    public var colorOverrideSrgb: String?
    /// The tokens over the effective `colorSrgb` (spec/02).
    public var tokens: AppearanceTokens
    /// The stored symbol override; `nil` inherits from the account.
    public var symbolOverride: String?
    /// What the identity displays: `symbolOverride`, else its
    /// account's symbol, else a mark derived from this identity's name.
    public var monogram: String
    /// The record's revision; `nil` means no record exists.
    public var revision: Int64?

    public init(
        identityId: String,
        accountId: String,
        colorSrgb: String,
        colorOverrideSrgb: String?,
        tokens: AppearanceTokens,
        symbolOverride: String?,
        monogram: String,
        revision: Int64?
    ) {
        self.identityId = identityId
        self.accountId = accountId
        self.colorSrgb = colorSrgb
        self.colorOverrideSrgb = colorOverrideSrgb
        self.tokens = tokens
        self.symbolOverride = symbolOverride
        self.monogram = monogram
        self.revision = revision
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.IdentityAppearance) {
        self.init(
            identityId: value.identityId, accountId: value.accountId,
            colorSrgb: value.colorSrgb, colorOverrideSrgb: value.colorOverrideSrgb,
            tokens: AppearanceTokens(value.tokens), symbolOverride: value.symbolOverride,
            monogram: value.monogram, revision: value.revision
        )
    }
}

/// The rendering tokens derived from one stored color (spec/02
/// "Rendering tokens"), for both appearances.
public struct AppearanceTokens: Sendable, Equatable {
    /// The light appearance's tokens.
    public var light: AppearanceVariant
    /// The dark appearance's tokens.
    public var dark: AppearanceVariant

    public init(light: AppearanceVariant, dark: AppearanceVariant) {
        self.light = light
        self.dark = dark
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.AppearanceTokens) {
        self.init(light: AppearanceVariant(value.light), dark: AppearanceVariant(value.dark))
    }
}

/// One appearance's tokens: accent, strip fill, text ink and edge,
/// each `#rrggbb`.
public struct AppearanceVariant: Sendable, Equatable {
    /// The saturated accent: the list rail and monograms.
    public var markerSrgb: String
    /// The identity strip's opaque fill.
    public var stripSrgb: String
    /// The strip's text and content ink.
    public var foregroundSrgb: String
    /// The Increase-Contrast edge and separator.
    public var outlineSrgb: String

    public init(
        markerSrgb: String,
        stripSrgb: String,
        foregroundSrgb: String,
        outlineSrgb: String
    ) {
        self.markerSrgb = markerSrgb
        self.stripSrgb = stripSrgb
        self.foregroundSrgb = foregroundSrgb
        self.outlineSrgb = outlineSrgb
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.AppearanceVariant) {
        self.init(
            markerSrgb: value.markerSrgb, stripSrgb: value.stripSrgb,
            foregroundSrgb: value.foregroundSrgb, outlineSrgb: value.outlineSrgb
        )
    }
}

/// Where an account's color came from (spec/02 `assignment`): a
/// palette-v1 slot or an explicit custom pick.
public enum AppearanceAssignment: Sendable, Equatable {
    /// The v1 preset at this slot — `palette`'s index.
    case paletteV1(slot: UInt8)
    /// A color the owner picked, whatever preset it may equal.
    case custom

    /// Map the generated enum case by case.
    init(_ value: SweetbriarCore.AppearanceAssignment) {
        switch value {
        case .paletteV1(let slot):
            self = .paletteV1(slot: slot)
        case .custom:
            self = .custom
        }
    }
}

/// One v1 palette preset (spec/02 "The palette"): its slot — what a
/// palette pick carries — display name and `#rrggbb` color.
public struct PaletteColor: Sendable, Equatable {
    /// The preset's index in `palette`.
    public var slot: UInt8
    /// The picker's display name.
    public var name: String
    /// The preset's `#rrggbb` color.
    public var colorSrgb: String

    public init(slot: UInt8, name: String, colorSrgb: String) {
        self.slot = slot
        self.name = name
        self.colorSrgb = colorSrgb
    }

    /// Map the generated record field by field.
    init(_ value: SweetbriarCore.PaletteColor) {
        self.init(slot: value.slot, name: value.name, colorSrgb: value.colorSrgb)
    }
}

/// What an appearance subscription reports (spec/07).
public enum AppearanceEvent: Sendable, Equatable {
    /// Committed appearance changed; re-read `appearance()`.
    case invalidated(generation: UInt64)
    /// Fallen too far behind: re-read `appearance()` and subscribe
    /// again at its generation.
    case resetRequired
}

/// The appearance queries a model needs from the core (spec/07).
/// `CoreBridge` adopts it; tests substitute a fake.
public protocol AppearanceSource: Sendable {
    /// `appearance()`: the resolved snapshot and its generation mark.
    func appearance() async throws -> Appearance
    /// `subscribe_appearance(generation)`: committed appearance
    /// changes from `generation`, pulled one `next` at a time.
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource
    /// The v1 account palette, a constant (spec/02).
    func palette() -> [PaletteColor]
}

/// The appearance stream as the model pulls it (spec/07): one
/// `next` at a time.
public protocol AppearanceEventSource: Sendable {
    func next() async throws -> AppearanceEvent
}

extension CoreBridge: AppearanceSource {
    /// The appearance stream, marked at the caller's last
    /// `appearance()` generation.
    public func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        try await subscribeAppearance(generation: generation)
    }
}

/// The appearance the app draws with, kept current by `follow`
/// (spec/07, spec/08): the snapshot that answers `appearance()`,
/// re-read on every invalidation and resubscribed on a reset.
@MainActor
@Observable
public final class AppearanceModel {
    /// The last snapshot the follow loop published, if any.
    public private(set) var appearance: Appearance?

    /// The last read or pull's failure, if any.
    public private(set) var lastError: (any Error)?

    /// The v1 palette the pickers index (spec/02): a constant.
    public var palette: [PaletteColor] { source.palette() }

    private let source: any AppearanceSource
    @ObservationIgnored
    private var following = false

    public init(source: any AppearanceSource) {
        self.source = source
    }

    /// Load `appearance()`, then pull its subscription until the
    /// calling task is cancelled or a read or pull fails: an
    /// `invalidated` re-reads on the same stream; `resetRequired`
    /// re-reads and resubscribes at the answer's generation. One
    /// follow at a time — a second call while one runs returns at
    /// once.
    public func follow() async {
        guard !following else { return }
        following = true
        defer { following = false }
        do {
            outer: while !Task.isCancelled {
                let snapshot = try await readOnce()
                let events = try await source.appearanceEvents(generation: snapshot.generation)
                while !Task.isCancelled {
                    switch try await events.next() {
                    case .invalidated:
                        _ = try await readOnce()
                    case .resetRequired:
                        continue outer
                    }
                }
            }
        } catch is CancellationError {
        } catch {
            lastError = error
        }
    }

    /// Read `appearance()` and publish it, clearing the last failure.
    private func readOnce() async throws -> Appearance {
        let snapshot = try await source.appearance()
        appearance = snapshot
        lastError = nil
        return snapshot
    }
}
