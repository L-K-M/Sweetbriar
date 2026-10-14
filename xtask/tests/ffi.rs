//! Integration tests for `cargo xtask check-ffi` (plan M0.8): the
//! bindings drift comparison, and the async-foreign-callback lint run
//! on a fixture that breaks spec/07. The real bindings pass both in
//! `cargo xtask check-ffi` itself (locally and in macOS CI).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use xtask::ffi::{binding_drift, lint_async_foreign_callbacks};

/// A fresh scratch dir under the system temp dir, unique per test.
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xtask-ffi-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write `files` (name, contents) into `dir`.
fn write_all(dir: &Path, files: &[(&str, &str)]) {
    fs::create_dir_all(dir).unwrap();
    for (name, contents) in files {
        fs::write(dir.join(name), contents).unwrap();
    }
}

/// Identical bindings drift nowhere; a changed line, a file no longer
/// committed and a file no longer generated are each reported, naming
/// the committed path.
#[test]
fn drift_names_each_changed_missing_or_stale_file() {
    let root = scratch("drift");
    let generated = root.join("generated");
    let swift = root.join("Sources");
    let headers = root.join("Headers");
    write_all(
        &generated,
        &[
            ("sb_ffi.swift", "let a = 1\n"),
            ("sb_ffiFFI.h", "// h\n"),
            ("module.modulemap", "m\n"),
        ],
    );
    write_all(&swift, &[("sb_ffi.swift", "let a = 1\n")]);
    write_all(
        &headers,
        &[("sb_ffiFFI.h", "// h\n"), ("module.modulemap", "m\n")],
    );
    assert_eq!(
        binding_drift(&generated, &swift, &headers),
        Vec::<String>::new()
    );

    fs::write(swift.join("sb_ffi.swift"), "let a = 2\n").unwrap();
    fs::remove_file(headers.join("module.modulemap")).unwrap();
    fs::write(headers.join("stale.h"), "// old\n").unwrap();
    let drift = binding_drift(&generated, &swift, &headers);
    assert_eq!(drift.len(), 3, "{drift:?}");
    assert!(
        drift
            .iter()
            .any(|d| d.contains("sb_ffi.swift") && d.contains("drifted")),
        "{drift:?}"
    );
    assert!(
        drift
            .iter()
            .any(|d| d.contains("module.modulemap") && d.contains("not committed")),
        "{drift:?}"
    );
    assert!(
        drift
            .iter()
            .any(|d| d.contains("stale.h") && d.contains("no longer generated")),
        "{drift:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// The fixture, a surface with a foreign-implemented trait whose method
/// is async (spec/07 forbids it), is flagged naming the method. Built
/// in a scratch copy with its own target dir, so it never shares build
/// output with this workspace.
#[test]
fn the_lint_flags_an_async_foreign_callback() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ffi/async-callback");
    let copy = scratch("lint-fixture");
    for name in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(fixture.join(name), copy.join(name)).unwrap();
    }
    fs::create_dir_all(copy.join("src")).unwrap();
    fs::copy(fixture.join("src/lib.rs"), copy.join("src/lib.rs")).unwrap();

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let built = Command::new(cargo)
        .args(["build", "--locked", "--manifest-path"])
        .arg(copy.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", copy.join("target"))
        .status()
        .expect("cargo runs");
    assert!(built.success(), "the fixture didn't build");

    let library = copy.join("target/debug").join(format!(
        "{}async_callback_fixture{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let violations = lint_async_foreign_callbacks(&library).expect("the lint reads the fixture");
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("SlowGetter.get"), "{violations:?}");
    let _ = fs::remove_dir_all(&copy);
}
