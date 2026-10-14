//! `cargo xtask ffi` and `cargo xtask check-ffi` (plan M0.8): the
//! Swift bindings pipeline over `sb-ffi`.
//!
//! `ffi` builds `sb-ffi` for the host (so the cdylib with UniFFI's
//! metadata exists), runs `sb-bindgen` over it and installs the
//! generated Swift under `apps/macos/SweetbriarCore/Sources/`, the C
//! header and modulemap under `apps/macos/SweetbriarCore/Headers/`.
//! On macOS it then builds the release staticlib and wraps it in the
//! XCFramework the Swift package links (M0.9); elsewhere the full
//! command generates the bindings and stops, saying the XCFramework
//! needs macOS.
//!
//! `check-ffi` regenerates into a temp dir, diffs the committed
//! files byte for byte, and lints the exported surface: no callback
//! interface or foreign trait may have an async method, because
//! UniFFI 0.32.2's Swift 6 async-callback fix is unreleased
//! (spec/07 "Threading, cancellation and FFI rules"). The lint reads
//! UniFFI's own metadata from the cdylib — the same bytes the
//! bindgen reads — so it can't drift from what the bindings contain.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use uniffi_bindgen::macro_metadata;
use uniffi_meta::{Metadata, ObjectImpl, TraitKind};

use crate::{ToolError, run};

/// The module name the generated modulemap declares (`--module-name`).
const BINDINGS_MODULE_NAME: &str = "SweetbriarCoreFFI";

/// The modulemap's filename (`--modulemap-filename`).
const MODULEMAP_FILENAME: &str = "module.modulemap";

/// The Swift package that carries the generated bindings (spec/01).
fn core_package(root: &Path) -> PathBuf {
    root.join("apps/macos/SweetbriarCore")
}

/// Where the generated `.swift` files are committed.
pub fn swift_sources_dir(root: &Path) -> PathBuf {
    core_package(root).join("Sources/SweetbriarCore")
}

/// Where the generated header and modulemap are committed.
pub fn headers_dir(root: &Path) -> PathBuf {
    core_package(root).join("Headers")
}

/// Where `xtask ffi` builds the XCFramework on macOS (gitignored).
fn xcframework_path(root: &Path) -> PathBuf {
    core_package(root).join("SweetbriarCoreFFI.xcframework")
}

/// `cargo xtask ffi [--bindings-only]`.
pub fn ffi(bindings_only: bool) -> bool {
    let root = workspace_root();
    let out = temp_dir("sb-ffi-generate");
    let outcome = generate_bindings(&root, &out)
        .map(|_| ())
        .and_then(|()| install_bindings(&root, &out))
        .and_then(|()| finish(&root, bindings_only));
    let _ = fs::remove_dir_all(&out);
    match outcome {
        Ok(()) => {
            eprintln!(
                "ffi: bindings installed under {}",
                core_package(&root).display()
            );
            true
        }
        Err(err) => {
            eprintln!("ffi: {err}");
            false
        }
    }
}

/// `cargo xtask check-ffi`: no drift between the committed bindings
/// and a fresh generation, and no async foreign callbacks in the
/// exported surface.
pub fn check_ffi() -> bool {
    let root = workspace_root();
    let out = temp_dir("sb-ffi-check");
    let checked = generate_bindings(&root, &out).map(|library| {
        let drift = binding_drift(&out, &swift_sources_dir(&root), &headers_dir(&root));
        for line in &drift {
            eprintln!("check-ffi: {line}");
        }
        match lint_async_foreign_callbacks(&library) {
            Ok(violations) => {
                for line in &violations {
                    eprintln!("check-ffi: {line}");
                }
                (drift, violations)
            }
            Err(err) => (vec![err.to_string()], Vec::new()),
        }
    });
    let _ = fs::remove_dir_all(&out);
    match checked {
        Ok((drift, violations)) if drift.is_empty() && violations.is_empty() => {
            eprintln!("check-ffi: bindings match; no async foreign callbacks");
            true
        }
        Ok((drift, violations)) => {
            if drift.is_empty() {
                eprintln!(
                    "check-ffi: {} async foreign callback(s); spec/07 forbids them in the pilot",
                    violations.len()
                );
            } else {
                eprintln!(
                    "check-ffi: {} file(s) drifted; run `cargo xtask ffi --bindings-only` and commit",
                    drift.len()
                );
            }
            false
        }
        Err(err) => {
            eprintln!("check-ffi: {err}");
            false
        }
    }
}

/// Build `sb-ffi` for the host and generate the Swift bindings into
/// `out`, returning the cdylib the metadata was read from (the lint
/// re-reads it).
pub fn generate_bindings(root: &Path, out: &Path) -> Result<PathBuf, ToolError> {
    run(&cargo(), &["build", "-p", "sb-ffi"], root, None)?;
    let library = host_cdylib(root)?;
    let out_dir = path_string(out)?;
    let lib = path_string(&library)?;
    run(
        &cargo(),
        &[
            "run",
            "-p",
            "sb-bindgen",
            "--",
            "--swift-sources",
            "--headers",
            "--modulemap",
            // A plain `module`, not `--xcframework`'s `framework module`:
            // the XCFramework wraps a static library and a headers dir,
            // where Swift needs a non-framework module map.
            "--module-name",
            BINDINGS_MODULE_NAME,
            "--modulemap-filename",
            MODULEMAP_FILENAME,
            &lib,
            &out_dir,
        ],
        root,
        None,
    )?;
    Ok(library)
}

/// The host cdylib `cargo build -p sb-ffi` leaves in `target/debug`.
fn host_cdylib(root: &Path) -> Result<PathBuf, ToolError> {
    let ext = match std::env::consts::OS {
        "macos" => "dylib",
        "windows" => "dll",
        _ => "so",
    };
    let library = root.join("target/debug").join(format!("libsb_ffi.{ext}"));
    if library.is_file() {
        Ok(library)
    } else {
        Err(ToolError(format!(
            "the sb-ffi cdylib is missing at {}; build sb-ffi first",
            library.display()
        )))
    }
}

/// Copy the generated files into their committed homes: `.swift`
/// under `Sources/SweetbriarCore`, the header and modulemap under
/// `Headers`. Copy-and-remove rather than rename: the generation
/// dir may be on another filesystem.
fn install_bindings(root: &Path, generated: &Path) -> Result<(), ToolError> {
    let swift_dir = swift_sources_dir(root);
    let headers = headers_dir(root);
    fs::create_dir_all(&swift_dir).map_err(io("create", &swift_dir))?;
    fs::create_dir_all(&headers).map_err(io("create", &headers))?;
    for file in generated_files(generated)? {
        let to = if file.ends_with(".swift") {
            swift_dir.join(&file)
        } else {
            headers.join(&file)
        };
        fs::copy(generated.join(&file), &to).map_err(io("copy", &to))?;
    }
    Ok(())
}

/// After the bindings: the XCFramework on macOS, a note elsewhere.
fn finish(root: &Path, bindings_only: bool) -> Result<(), ToolError> {
    if bindings_only {
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        build_xcframework(root)
    } else {
        eprintln!(
            "ffi: bindings generated; the XCFramework needs macOS \
             (run `cargo xtask ffi` there or in macOS CI)"
        );
        Ok(())
    }
}

/// Build the release staticlib for the app's architecture and wrap
/// it, with the committed headers, in the XCFramework (plan M0.8;
/// macOS only — `xcodebuild` exists nowhere else).
fn build_xcframework(root: &Path) -> Result<(), ToolError> {
    run(
        &cargo(),
        &[
            "build",
            "-p",
            "sb-ffi",
            "--release",
            "--target",
            "aarch64-apple-darwin",
        ],
        root,
        None,
    )?;
    let library = root.join("target/aarch64-apple-darwin/release/libsb_ffi.a");
    let output = xcframework_path(root);
    if output.exists() {
        fs::remove_dir_all(&output).map_err(io("remove", &output))?;
    }
    let library = path_string(&library)?;
    let headers = path_string(&headers_dir(root))?;
    let output = path_string(&output)?;
    run(
        "xcodebuild",
        &[
            "-create-xcframework",
            "-library",
            &library,
            "-headers",
            &headers,
            "-output",
            &output,
        ],
        root,
        None,
    )?;
    Ok(())
}

/// The generated files that must be committed, sorted: everything
/// the bindgen wrote except cargo leftovers. An empty list is an
/// error — a generation that produced nothing would silently pass
/// the drift check.
fn generated_files(generated: &Path) -> Result<Vec<String>, ToolError> {
    let mut files: Vec<String> = fs::read_dir(generated)
        .map_err(io("read", generated))?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            (name != "Cargo.lock" && !name.starts_with('.')).then_some(name)
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(ToolError(format!(
            "the bindgen wrote nothing into {}",
            generated.display()
        )));
    }
    Ok(files)
}

/// Drift between a fresh generation and the committed bindings:
/// every generated file must be committed byte for byte in the
/// directory its kind belongs to, and every committed file must
/// still be generated. Each entry names the committed path.
pub fn binding_drift(
    generated: &Path,
    committed_swift: &Path,
    committed_headers: &Path,
) -> Vec<String> {
    let mut drift = Vec::new();
    let Ok(files) = generated_files(generated) else {
        return vec![format!(
            "cannot read the generated bindings under {}",
            generated.display()
        )];
    };
    for name in &files {
        let (dir, label) = if name.ends_with(".swift") {
            (committed_swift, "Swift sources")
        } else {
            (committed_headers, "headers")
        };
        let committed = dir.join(name);
        let Ok(generated_bytes) = fs::read(generated.join(name)) else {
            drift.push(format!("could not read the generated {name}"));
            continue;
        };
        match fs::read(&committed) {
            Ok(committed_bytes) if committed_bytes == generated_bytes => {}
            Ok(_) => drift.push(format!(
                "{} {name} drifted from the generated bindings: {}",
                label,
                committed.display()
            )),
            Err(_) => drift.push(format!(
                "{name} is generated but not committed where it belongs: {}",
                committed.display()
            )),
        }
    }
    for dir in [committed_swift, committed_headers] {
        let Ok(entries) = fs::read_dir(dir) else {
            drift.push(format!(
                "cannot read the committed bindings in {}",
                dir.display()
            ));
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let Some(name) = entry.file_name().into_string().ok() else {
                continue;
            };
            if !files.contains(&name) {
                drift.push(format!(
                    "{name} is committed but no longer generated: {}",
                    dir.join(&name).display()
                ));
            }
        }
    }
    drift
}

/// The async-foreign-callback lint (spec/07): every callback
/// interface, and every trait a foreign side may implement, must
/// carry only synchronous methods. Returns one line per violating
/// method.
///
/// Reads UniFFI's metadata straight from the cdylib — the same
/// symbol-table bytes `sb-bindgen` generates from — so what the
/// lint sees is exactly what the bindings contain. A method counts
/// as foreign-implementable when its trait is a callback interface
/// or a trait of kind `ForeignOnly`/`Both`; `RustOnly` traits never
/// reach Swift.
pub fn lint_async_foreign_callbacks(library: &Path) -> Result<Vec<String>, ToolError> {
    let bytes = fs::read(library).map_err(io("read", library))?;
    let items = macro_metadata::extract_from_bytes(&bytes).map_err(|err| {
        ToolError(format!(
            "extract UniFFI metadata from {}: {err}",
            library.display()
        ))
    })?;

    let mut foreign_traits: HashSet<String> = HashSet::new();
    for item in &items {
        match item {
            Metadata::CallbackInterface(meta) => {
                foreign_traits.insert(meta.name.clone());
            }
            // A trait the foreign side may implement (kind
            // `ForeignOnly` or `Both`) is a foreign callback trait;
            // `RustOnly` traits never reach Swift, and plain structs
            // aren't traits. A new UniFFI kind fails this match at
            // compile time — the lint stays exhaustive.
            Metadata::Object(meta) => match meta.imp {
                ObjectImpl::Trait(TraitKind::ForeignOnly | TraitKind::Both) => {
                    foreign_traits.insert(meta.name.clone());
                }
                ObjectImpl::Trait(TraitKind::RustOnly) | ObjectImpl::Struct => {}
            },
            _ => {}
        }
    }

    let mut violations = Vec::new();
    for item in &items {
        if let Metadata::TraitMethod(meta) = item
            && meta.is_async
            && foreign_traits.contains(&meta.trait_name)
        {
            violations.push(format!(
                "async foreign callback: {}.{} (spec/07 forbids async callback traits in the pilot)",
                meta.trait_name, meta.name
            ));
        }
    }
    Ok(violations)
}

/// A fresh scratch directory under the system temp dir, named for
/// the run so parallel invocations can't collide. Callers remove it.
fn temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{label}-{}", std::process::id()))
}

/// The workspace root: this crate lives at `<workspace>/xtask`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <workspace>/xtask")
        .to_path_buf()
}

/// The cargo binary to run (`$CARGO` under other cargo tooling).
fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// A path as a String for `run`, which takes `&[&str]`.
fn path_string(path: &Path) -> Result<String, ToolError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ToolError(format!("{} is not valid UTF-8", path.display())))
}

/// An I/O failure message naming the operation and the path.
fn io<'a, 'b>(what: &'a str, path: &'b Path) -> impl Fn(std::io::Error) -> ToolError + use<'a, 'b> {
    move |err| ToolError(format!("cannot {what} {}: {err}", path.display()))
}
