//! Integration tests for `cargo xtask check-layers`.
//!
//! Every fixture under `tests/layers/<case>/` is a small workspace
//! that really compiles — the check drives `cargo metadata`,
//! `cargo check` and `cargo +nightly rustdoc` on it. Each test copies
//! its fixture to a unique scratch dir (PID + counter) so parallel
//! tests never share directories; each copy checks into its own
//! `target/check-layers` — a shared `CARGO_TARGET_DIR` is unsafe:
//! identical crates produce identical artifact hashes, so parallel
//! builds would swap `.rmeta`/`.d` files under each other.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use xtask::layers::{Report, Violation, check_at, check_workspace};

/// `tests/layers/<case>` inside the xtask package.
fn fixture(case: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/layers")
        .join(case)
}

/// `<tmp>/layers-check-<pid>` — per-process scratch root.
fn scratch() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| std::env::temp_dir().join(format!("layers-check-{}", std::process::id())))
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh scratch copy of `case`'s fixture.
fn copy(case: &str) -> PathBuf {
    let root = scratch().join(format!("{}-{case}", NEXT.fetch_add(1, Ordering::Relaxed)));
    copy_dir(&fixture(case), &root, true);
    root
}

/// Copies the fixture to a fresh dir and runs the whole check on it.
fn check(case: &str) -> Report {
    let root = copy(case);
    check_workspace(&root).unwrap_or_else(|e| panic!("{case}: checker errored: {e}"))
}

/// A fixture's own `target/` is never copied (it can only be stale
/// build output); anything else named `target` is fixture data.
fn copy_dir(from: &Path, to: &Path, top: bool) {
    for entry in std::fs::read_dir(from).expect("fixture dir") {
        let entry = entry.expect("dir entry");
        if top && entry.file_name() == "target" {
            continue;
        }
        let dest = to.join(entry.file_name());
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            copy_dir(&entry.path(), &dest, false);
        } else if kind.is_symlink() {
            // Symlink fixtures must stay symlinks — a copy of the
            // target's contents would hide the escape they test.
            std::fs::create_dir_all(to).expect("mkdir");
            let link = std::fs::read_link(entry.path()).expect("readlink");
            std::os::unix::fs::symlink(link, &dest).expect("symlink");
        } else {
            std::fs::create_dir_all(to).expect("mkdir");
            std::fs::copy(entry.path(), &dest).expect("copy");
        }
    }
}

fn render(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asserts that one violation sits at `file`:`line` with `text` in
/// its message.
fn expect_violation(report: &Report, case: &str, file: &str, line: usize, text: &str) {
    let all = render(&report.violations);
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.file.ends_with(file) && v.line == line && v.message.contains(text)),
        "{case}: expected a `{file}:{line}` violation mentioning `{text}` in:\n{all}"
    );
}

/// Every spec/01 negative fixture fails with the stated violation:
/// (case, workspace-relative file, line, required message text).
#[test]
fn negative_fixtures_fail() {
    let cases: &[(&str, &str, usize, &str)] = &[
        // A skip-layer dependency edge (L3 -> L5, manifest level).
        (
            "skip-layer",
            "crates/sb-engine/Cargo.toml",
            7,
            "sb_engine (L3) -> sb_store (L5): first-party dependency on `sb_store` \
             is not allowed by layers.toml",
        ),
        // `storage = { package = "sb-store" }` hides the real crate.
        (
            "renamed-dep",
            "crates/sb-resources/Cargo.toml",
            7,
            "renamed first-party dependency `storage`",
        ),
        // `pub use sb_store;` re-exports the whole crate.
        (
            "crate-reexport",
            "crates/sb-resources/src/lib.rs",
            2,
            "sb_resources (L4) -> sb_store (L5): re-exports module `sb_store`",
        ),
        // `pub extern crate sb_store;` re-exports it just the same.
        (
            "extern-crate",
            "crates/sb-resources/src/lib.rs",
            2,
            "sb_resources (L4) -> sb_store (L5): re-exports module `sb_store`",
        ),
        // `pub extern crate sb_engine;` in an L2 crate.
        (
            "extern-service",
            "crates/sb-service/src/lib.rs",
            2,
            "sb_service (L2) -> sb_engine (L3): re-exports module `sb_engine`",
        ),
        // `pub use sb_store::inner;` re-exports a module.
        (
            "module-reexport",
            "crates/sb-resources/src/lib.rs",
            2,
            "re-exports module `sb_store::inner` of `sb_store`",
        ),
        // `pub use sb_store::*;` re-exports a glob.
        (
            "glob-reexport",
            "crates/sb-resources/src/lib.rs",
            2,
            "sb_resources (L4) -> sb_store (L5): glob re-export of `sb_store`",
        ),
        // `sb-service` re-exports nothing from other crates.
        (
            "service-reexport",
            "crates/sb-service/src/lib.rs",
            2,
            "sb_service (L2) -> sb_engine (L3): re-exports `sb_engine::Engine`; \
             sb-service re-exports nothing",
        ),
        // Rule 3 keys on the L2 class, not the lib target's spelling.
        (
            "service-renamed-lib",
            "crates/sb-service/src/lib.rs",
            2,
            "service_api (L2) -> sb_engine (L3): re-exports `sb_engine::Engine`; \
             sb-service re-exports nothing",
        ),
        // A `pub use` of another crate's `#[macro_export]` macro.
        (
            "macro-reexport",
            "crates/sb-resources/src/lib.rs",
            2,
            "re-exports macro `sb_store::store_log` of `sb_store`",
        ),
        // … and of attribute and derive proc macros.
        (
            "proc-macro",
            "crates/sb-resources/src/lib.rs",
            2,
            "re-exports macro `sb_pm::store_attr` of `sb_pm`",
        ),
        (
            "proc-macro",
            "crates/sb-resources/src/lib.rs",
            3,
            "re-exports macro `sb_pm::Store` of `sb_pm`",
        ),
        // `#[doc(hidden)]` doesn't hide a re-export from rustdoc.
        (
            "hidden-reexport",
            "crates/sb-resources/src/lib.rs",
            3,
            "re-exports module `sb_store::inner`",
        ),
        // `#[path]` escaping the crate's directory.
        (
            "path-escapes",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // `include!()` whose target leaves the crate's directory.
        (
            "include-escapes",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/inc.rs` lies outside the crate's directory",
        ),
        // `.rs` data read with `include_str!` is a source too.
        (
            "str-escape",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/data.rs` lies outside the crate's directory",
        ),
        // An OUT_DIR-shaped path a crate has no build script for.
        (
            "forged-out-dir",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/target/fake/build/sb-store-forge/out/external.rs` \
             lies outside the crate's directory",
        ),
        // Every target is checked: a `tests/` `include!` escape.
        (
            "test-escape",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // … an `examples/` `include!` escape …
        (
            "example-escape",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // … a `benches/` `include!` escape …
        (
            "bench-escape",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // … and a `#[path]` escape under a `#[cfg(test)]` module.
        (
            "cfgtest-escape",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // `xtask` sources are boundary-checked like any member.
        (
            "xtask-escape",
            "xtask/Cargo.toml",
            1,
            "xtask: source `outside.rs` lies outside the crate's directory",
        ),
        // A build script's `include!` is checked from its real .d file.
        (
            "buildrs-escape",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // rustdoc runs with all features, so a feature-gated re-export
        // is seen.
        (
            "feature-gate",
            "crates/sb-resources/src/lib.rs",
            3,
            "re-exports module `sb_store::inner` of `sb_store`",
        ),
        // `--all-features` on the dep-info build sees a `#[cfg]`-gated
        // `#[path]` escape too.
        (
            "feature-path",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // A workspace member `layers.toml` doesn't classify fails.
        (
            "unclassified",
            "crates/weird/Cargo.toml",
            1,
            "weird: not classified in layers.toml",
        ),
        // The declaring line is found on a `dep.workspace = true` key.
        (
            "workspace-dep",
            "crates/sb-engine/Cargo.toml",
            7,
            "sb_engine (L3) -> sb_store (L5): first-party dependency on `sb_store` \
             is not allowed by layers.toml",
        ),
        // A `#[cfg(not(feature))]` re-export shows under default
        // features — both configurations run.
        (
            "not-feature",
            "crates/sb-resources/src/lib.rs",
            3,
            "re-exports module `sb_store`",
        ),
        // A version-only dep naming a member (resolved via `[patch]`)
        // is not a path dep; the `[patch]` entry is a violation too.
        (
            "patched-version",
            "crates/sb-resources/Cargo.toml",
            7,
            "first-party dependency `sb_store` must be a path dependency",
        ),
        (
            "patched-version",
            "Cargo.toml",
            6,
            "sb_store: named by a `[patch]` entry",
        ),
        // An unused `[replace]` naming a member is still banned.
        (
            "replace-entry",
            "Cargo.toml",
            6,
            "sb_store: named by a `[replace]` entry",
        ),
        // An `.inc` symlink to an outside `.rs` resolves before the
        // extension rule.
        (
            "inc-symlink",
            "crates/sb-store/Cargo.toml",
            1,
            "sb_store: source `crates/outside.rs` lies outside the crate's directory",
        ),
        // Target-table deps: the bare-table entry lands on its `name =`
        // line, the `[target.….dependencies.name]` header on its own.
        (
            "target-deps",
            "crates/sb-engine/Cargo.toml",
            7,
            "sb_engine (L3) -> sb_store (L5): first-party dependency on `sb_store` \
             is not allowed by layers.toml",
        ),
        (
            "target-deps",
            "crates/sb-engine/Cargo.toml",
            9,
            "sb_engine (L3) -> sb_mime (L5): first-party dependency on `sb_mime` \
             is not allowed by layers.toml",
        ),
    ];
    for &(case, file, line, text) in cases {
        let report = check(case);
        assert!(
            !report.violations.is_empty(),
            "{case}: expected violations, got none"
        );
        expect_violation(&report, case, file, line, text);
    }
}

/// Every spec/01 positive fixture passes clean.
#[test]
fn positive_fixtures_pass() {
    for case in [
        // A legal adjacent item re-export (sb-resources → sb-store).
        "positive",
        // `pub use` inside a private module isn't public API.
        "private-exports",
        // A glob of a private local module exposes only local items.
        "local-glob",
        // `env!()` dep-info comments aren't make rules.
        "env-dep",
        // Non-`.rs` data read with `include_str!` is exempt.
        "data-file",
        // Sources generated into the crate's real `OUT_DIR`.
        "generated-code",
        // A glob of an enum's variants is bounded and legal.
        "enum-glob",
        // A package built as dep and build-dep has two real OUT_DIRs.
        "feature-variant",
        // Primitive/std re-exports carry `id: null` — not violations.
        "primitive-export",
        // `pub extern crate self as api` aliases the crate itself.
        "extern-self",
    ] {
        let report = check(case);
        assert!(
            report.violations.is_empty(),
            "{case}: expected clean, got:\n{}",
            render(&report.violations)
        );
    }
}

/// A `CARGO_TARGET_DIR` that is a symlink into a directory without a
/// literal `target` component still locates the real `OUT_DIR`.
#[test]
fn generated_code_under_symlinked_target() {
    let case = "generated-code";
    let root = copy(case);
    let real = scratch().join("real-target");
    let link = scratch().join("link-target");
    std::fs::create_dir_all(&real).expect("real target dir");
    std::os::unix::fs::symlink(&real, &link).expect("symlink target");
    let report = check_at(&root, &root.join("layers.toml"), &link)
        .unwrap_or_else(|e| panic!("{case}: checker errored: {e}"));
    assert!(
        report.violations.is_empty(),
        "{case}: expected clean under a symlinked target, got:\n{}",
        render(&report.violations)
    );
}

/// A `[build] target` in `.cargo/config.toml` relocates rustdoc JSON
/// and dep-info under the triple; the check follows it.
#[test]
fn configured_native_target() {
    let root = copy("positive");
    let out = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("rustc -vV");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let host = stdout
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .expect("rustc -vV reports host");
    let cargo = root.join(".cargo");
    std::fs::create_dir_all(&cargo).expect("mkdir .cargo");
    std::fs::write(
        cargo.join("config.toml"),
        format!("[build]\ntarget = \"{host}\"\n"),
    )
    .expect("write .cargo/config.toml");
    let report =
        check_workspace(&root).unwrap_or_else(|e| panic!("native-target: checker errored: {e}"));
    assert!(
        report.violations.is_empty(),
        "native-target: expected clean, got:\n{}",
        render(&report.violations)
    );
}

/// Inherited `RUSTDOCFLAGS=--document-private-items` must not widen
/// the checked API: the check clears it (env vars supersede
/// `[build] rustdocflags` too) and visits only `public` items, so a
/// private module's `pub use` stays invisible. Runs the built binary
/// so the flag is set for the child process only.
#[test]
fn inherited_rustdocflags_are_cleared() {
    let case = "private-exports";
    let root = copy(case);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("check-layers")
        .arg(&root)
        .env("RUSTDOCFLAGS", "--document-private-items")
        .output()
        .expect("run xtask check-layers");
    assert!(
        out.status.success(),
        "{case}: expected clean under inherited RUSTDOCFLAGS, got:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `check-layers <relative root>` joins `target/check-layers` onto the
/// canonical root once — not `<root>/<root>/target/…`.
#[test]
fn relative_root_is_not_doubled() {
    let case = "positive";
    let root = copy(case);
    let name = root.file_name().expect("fixture dir name").to_owned();
    let parent = root.parent().expect("fixture parent").to_path_buf();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("check-layers")
        .arg(&name)
        .current_dir(&parent)
        .output()
        .expect("run xtask check-layers");
    assert!(
        out.status.success(),
        "{case}: expected clean under a relative root, got:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !root.join(&name).exists(),
        "{case}: doubled root directory created"
    );
}

/// A failed `cargo check --message-format=json` reports the compiler's
/// own message (its rendered JSON text on stdout), not just cargo's
/// "could not compile" summary on stderr. The error sits in the
/// top-level crate under `cfg(not(doc))`, so every rustdoc pass stays
/// green and the dep-info build is what fails.
#[test]
fn check_failure_reports_compiler_message() {
    let case = "positive";
    let root = copy(case);
    let lib = root.join("crates/sb-engine/src/lib.rs");
    let mut src = std::fs::read_to_string(&lib).expect("lib.rs");
    src.push_str("\n#[cfg(not(doc))]\nconst BAD: u8 = MISSING;\n");
    std::fs::write(&lib, src).expect("write lib.rs");
    let err = match check_workspace(&root) {
        Err(err) => err,
        Ok(_) => panic!("{case}: broken build must error"),
    };
    let err = err.to_string();
    assert!(
        err.contains("MISSING") && err.contains("sb-engine/src/lib.rs"),
        "{case}: expected the rendered rustc diagnostic, got: {err}"
    );
}

/// The real workspace must satisfy its own `layers.toml`.
#[test]
fn real_workspace_passes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <workspace>/xtask")
        .to_path_buf();
    let report = check_workspace(&root).expect("real workspace checks");
    assert!(
        report.violations.is_empty(),
        "real workspace should be clean:\n{}",
        render(&report.violations)
    );
}
