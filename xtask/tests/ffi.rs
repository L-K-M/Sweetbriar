//! Integration tests for `cargo xtask check-ffi` (plan M0.8): the
//! bindings drift comparison, the FFI import check (on fixtures and
//! on the committed bindings), and the async-foreign-callback lint
//! run on a fixture that breaks spec/07. The real bindings pass all
//! of them in `cargo xtask check-ffi` itself (locally and in macOS
//! CI).

use std::fs;
use std::path::{Path, PathBuf};

use xtask::ffi::{
    LibraryKind, binding_drift, build_library, headers_dir, lint_async_foreign_callbacks,
    swift_sources_dir, unresolved_ffi_imports,
};

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

/// A generated file committed in the other kind's directory is
/// stale there, though its name is generated: a Swift file belongs
/// under the Swift sources only, the rest under the headers only.
#[test]
fn drift_names_a_generated_file_in_the_wrong_directory() {
    let root = scratch("wrong-dir");
    let generated = root.join("generated");
    let swift = root.join("Sources");
    let headers = root.join("Headers");
    let files = [
        ("sb_ffi.swift", "let a = 1\n"),
        ("sb_ffiFFI.h", "// h\n"),
        ("module.modulemap", "m\n"),
    ];
    write_all(&generated, &files);
    write_all(&swift, &[files[0], files[1]]);
    write_all(&headers, &[files[0], files[1], files[2]]);

    let drift = binding_drift(&generated, &swift, &headers);
    assert_eq!(drift.len(), 2, "{drift:?}");
    let misplaced = [swift.join("sb_ffiFFI.h"), headers.join("sb_ffi.swift")];
    for path in misplaced {
        assert!(
            drift
                .iter()
                .any(|d| d.contains(&path.display().to_string()) && d.contains("not here")),
            "{drift:?}"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// Swift that imports the module its modulemap declares, whose
/// header sits beside it, resolves; an import no modulemap declares
/// and a header nobody committed are each reported, naming the file.
#[test]
fn an_undeclared_import_or_a_missing_header_is_reported() {
    let root = scratch("imports");
    let swift = root.join("Sources");
    let headers = root.join("Headers");
    write_all(
        &swift,
        &[(
            "Core.swift",
            "#if canImport(CoreFFI)\nimport CoreFFI\n#endif\n",
        )],
    );
    write_all(
        &headers,
        &[
            (
                "module.modulemap",
                "module CoreFFI {\n    header \"CoreFFI.h\"\n    export *\n}",
            ),
            ("CoreFFI.h", "// h\n"),
        ],
    );
    assert_eq!(
        unresolved_ffi_imports(&swift, &headers),
        Vec::<String>::new()
    );

    write_all(
        &headers,
        &[(
            "module.modulemap",
            "module OtherFFI {\n    header \"Other.h\"\n}",
        )],
    );
    let problems = unresolved_ffi_imports(&swift, &headers);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems
            .iter()
            .any(|p| p.contains("Core.swift") && p.contains("CoreFFI")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("module.modulemap") && p.contains("Other.h")),
        "{problems:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// Swift with no FFI import at all is reported too: the check must
/// never pass because it found nothing to check.
#[test]
fn swift_without_an_ffi_import_is_reported() {
    let root = scratch("no-import");
    let swift = root.join("Sources");
    let headers = root.join("Headers");
    write_all(&swift, &[("Core.swift", "import Foundation\n")]);
    write_all(&headers, &[("module.modulemap", "module CoreFFI {\n}")]);
    let problems = unresolved_ffi_imports(&swift, &headers);
    assert_eq!(problems.len(), 1, "{problems:?}");
    let _ = fs::remove_dir_all(&root);
}

/// The committed bindings resolve: the Swift imports the module the
/// committed modulemap declares. A mismatch still builds an
/// XCFramework, but Swift then can't see `RustBuffer` or any FFI
/// symbol (M0.8 review).
#[test]
fn the_committed_swift_imports_the_committed_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <workspace>/xtask");
    assert_eq!(
        unresolved_ffi_imports(&swift_sources_dir(root), &headers_dir(root)),
        Vec::<String>::new()
    );
}

/// The fixture, a surface with a foreign-implemented trait whose method
/// is async (spec/07 forbids it), is flagged naming the method. Built
/// in a scratch copy into a custom target dir, so it never shares build
/// output with this workspace — and `build_library` must find the
/// cdylib where Cargo put it, not the stale one left at
/// `<dir>/target/debug`, the path an overridden `CARGO_TARGET_DIR`
/// makes stale (M0.8 review).
#[test]
fn the_lint_flags_an_async_foreign_callback() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ffi/async-callback");
    let copy = scratch("lint-fixture");
    for name in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(fixture.join(name), copy.join(name)).unwrap();
    }
    fs::create_dir_all(copy.join("src")).unwrap();
    fs::copy(fixture.join("src/lib.rs"), copy.join("src/lib.rs")).unwrap();

    // A stale library where the default target dir would hold it.
    let default_debug = copy.join("target/debug");
    fs::create_dir_all(&default_debug).unwrap();
    fs::write(
        default_debug.join(format!(
            "{}async_callback_fixture{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        )),
        b"stale",
    )
    .unwrap();

    let custom_target = copy.join("custom-target");
    let library = build_library(
        &copy,
        &["build", "--locked"],
        Some(&custom_target),
        "async_callback_fixture",
        LibraryKind::Cdylib,
    )
    .expect("the fixture builds");
    assert!(
        fs::canonicalize(&library)
            .unwrap()
            .starts_with(fs::canonicalize(&custom_target).unwrap()),
        "{} isn't under the target dir the build used",
        library.display()
    );

    let violations = lint_async_foreign_callbacks(&library).expect("the lint reads the fixture");
    assert_eq!(violations.len(), 1, "{violations:?}");
    assert!(violations[0].contains("SlowGetter.get"), "{violations:?}");
    let _ = fs::remove_dir_all(&copy);
}
