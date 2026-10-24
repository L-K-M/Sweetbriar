# MIME, mbox and query fuzzing

Requires C and C++ compilers. Install `cargo-fuzz` 0.13.2, then run from
this directory:

```sh
mkdir -p corpus/mime
cargo fuzz run -O --target x86_64-unknown-linux-gnu --fuzz-dir . \
  mime corpus/mime ../fixtures/eml -- \
  -max_total_time=60 -max_len=1000000 -rss_limit_mb=2048 -timeout=10
```

For the mbox splitter, use its own writable corpus and import seeds:

```sh
mkdir -p corpus/mbox
cargo fuzz run -O --target x86_64-unknown-linux-gnu --fuzz-dir . \
  mbox corpus/mbox ../fixtures/import -- \
  -max_total_time=60 -max_len=1000000 -rss_limit_mb=2048 -timeout=10
```

The splitter target checks inventory, offsets and both variants' bytes
against a whole-line reference decoder. Each target runs for 60 seconds
in CI, with separate corpus caches and finding artifacts.

For the query parser, use its own writable corpus and search seeds:

```sh
mkdir -p corpus/query
cargo fuzz run -O --target x86_64-unknown-linux-gnu --fuzz-dir . \
  query corpus/query ../fixtures/search -- \
  -max_total_time=60 -max_len=1000000 -rss_limit_mb=2048 -timeout=10
```

Each seed is one complete query input; no prefix byte selects a front
end. The target tries UTF-8 text under several explicit fixed contexts —
the `UTC` zone with a fixed 2026 clock, plus other fixed or deliberately
invalid defaults — and the v1 JSON envelope, checking parse/format/parse
equivalence. Bare dates consume the context's zone; bare `within:` also
consumes its clock. The fixed `UTC` 2026 context supplies a valid baseline.
Absolute dates with `@zone`, and `within:` with `@zone@clock`, carry their
full context and behave identically under every default.
`fixtures/search` names seeds `valid-*` (must parse under their
extension's front end) or `invalid-*` (must be rejected).

`-O` builds the real release profile: without it cargo-fuzz injects
debug_assertions and overflow-checks into every crate, so dependencies
assert on states their release builds recover from (mail-parser's
`debug_assert!("Invalid part ID")` is one — the minimised reproducer is
committed as `fixtures/eml/fuzz-rfc822-partid.eml`). The gate verifies
the shipped build; AddressSanitizer stays enabled either way.

`--target` pins the host triple: cargo-fuzz defaults to the platform it
was itself compiled for, and prebuilt musl binaries would otherwise try
to build a musl fuzz target, which AddressSanitizer does not support.

The pinned nightly applies only here. Cargo-fuzz's default AddressSanitizer
stays enabled. The first corpus is writable; committed synthetic fixtures
supply read-only seeds.
Caught parser panics are findings, not successful error handling.

Reproduce a finding with the same `run` command and the artifact path as
its only corpus argument; `cargo fuzz tmin` with the same flags minimizes
it. Commit the minimized input as a permanent `fixtures/search` seed and
cover it with a unit regression beside the parser tests.

CI limits inputs to 1 MB; unit tests cover the larger parser ceilings.
Refresh this workspace's lockfile when dependencies change. Its separate
licence check allows NCSA only for `libfuzzer-sys`, which is never linked
into the app.
