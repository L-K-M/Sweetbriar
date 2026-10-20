# Remove the unmaintained template-config parser

## Goal

Restore the strict dependency gate after RUSTSEC-2026-0374 without an advisory
ignore or a broad dependency/toolchain update. Keep UniFFI 0.32.2 and the
committed Swift interface unchanged. The owner authorized this separate
maintenance backport on 2026-10-20.

## Evidence and ownership

- `cargo deny check` rejects basic-toml 0.1.10 as unmaintained. It enters via
  askama_derive 0.16.1's config feature, used by uniffi_bindgen 0.32.2.
- Latest published Askama/UniFFI remain affected; RustSec has no patched
  basic-toml release. Upstream Askama issue 594 requests maintained TOML.
- Backport only Askama's config parser dependency/call and compatibility proof
  from upstream v0.16.1 (9ae4d5bdf4352b35e34106e38944c90d7a7ede3e). Preserve
  template behavior, borrowed config strings and error handling. No runtime
  mail/store/API behavior or persistent user data changes.
- The reviewed fork is `https://github.com/BigBoyDevBox/askama`. Cargo must pin
  the tested immutable revision; source policy allows only that named repository.
  Revision: `da7f1d08c1690c576e3a7fd0a0f3436bc64e0440` (61 additions, 3 deletions
  against the release tag, including borrowed-config and malformed-TOML tests).
  The org fork was unavailable to this credential; the authenticated user's
  fork is authorized and available. No arbitrary Git source allowance.

## Deliverables

- A minimal maintained-TOML backport with actual config/template tests.
- Exact Cargo patch revision, lockfile and regenerated notices. Existing
  UniFFI versions, MPL exceptions and strict advisory/ban/license rules stay.
- No local patch path, mutable branch/tag pin, blanket ignore, vendored source
  dump or unrelated dependency update. Include backport and integration diffs
  in the review context and the 1,450 additions-plus-deletions budget.

## Acceptance

- Backport config tests exercise real borrowed syntax/escaper/dir settings;
  invalid TOML still fails explicitly. No parser result is mocked.
- `cargo tree` shows basic-toml absent; fresh `cargo deny check` passes.
- `cargo xtask ffi --bindings-only` and `cargo xtask check-ffi` pass on Linux
  where supported; regenerated committed bindings are byte-identical.
- `cargo xtask ci` passes; current named ci and macos pass with completed review.
  macOS supplies the Swift/XCFramework/app checks unavailable on Linux.

## Compatibility and rollback

No mixed-version store migration is needed: this changes build-time template
configuration only. Retain the release baseline and reviewed patch provenance.
Reverting the patch/source entry and restoring the prior lock is mechanically
reversible but reintroduces the advisory, so it is not an implicit acceptable
rollback. Replace the fork with a fixed registry release in its own update PR
when upstream ships it, with the same binding/gate proof.
