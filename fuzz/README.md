# MIME fuzzing

Requires a C++ compiler. Install `cargo-fuzz` 0.13.2, then run from this
directory:

```sh
mkdir -p corpus/mime
cargo fuzz run -O --target x86_64-unknown-linux-gnu --fuzz-dir . \
  mime corpus/mime ../fixtures/eml -- \
  -max_total_time=60 -max_len=1000000 -rss_limit_mb=2048 -timeout=10
```

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

CI limits inputs to 1 MB; unit tests cover the larger parser ceilings.
Refresh this workspace's lockfile when dependencies change. Its separate
licence check allows NCSA only for `libfuzzer-sys`, which is never linked
into the app.
