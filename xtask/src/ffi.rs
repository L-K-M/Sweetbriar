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
//! `check-ffi` regenerates into a temp dir and diffs the committed
//! files byte for byte; checks the Swift imports the FFI module the
//! modulemap declares; on macOS type-checks the committed Swift
//! against the committed headers; and lints the exported surface: no
//! callback interface or foreign trait may have an async method,
//! because UniFFI 0.32.2's Swift 6 async-callback fix is unreleased
//! (spec/07 "Threading, cancellation and FFI rules"). The lint reads
//! UniFFI's own metadata from the cdylib — the same bytes the
//! bindgen reads — so it can't drift from what the bindings contain.

use std::collections::HashSet;
use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::fs;
use std::path::{Path, PathBuf};

use uniffi_bindgen::macro_metadata;
use uniffi_meta::{Metadata, ObjectImpl, TraitKind};

use crate::{ToolError, cargo, run};

/// The module name the generated modulemap declares (`--module-name`).
const BINDINGS_MODULE_NAME: &str = "SweetbriarCoreFFI";

/// UniFFI's config for the Swift bindings (`--config`): it names
/// the Swift module and the FFI module the Swift imports.
const BINDINGS_CONFIG: &str = "crates/sb-bindgen/swift-bindings.toml";

/// The target the app builds for (spec/08: arm64, macOS 26), which
/// the Swift type-check matches.
const SWIFT_TARGET: &str = "arm64-apple-macos26.0";

/// The modulemap's filename (`--modulemap-filename`).
const MODULEMAP_FILENAME: &str = "module.modulemap";

/// The condition UniFFI's Swift wraps its FFI module import in.
const CAN_IMPORT: &str = "canImport(";

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

/// What `cargo xtask ffi` produces.
pub enum FfiOutput {
    /// The committed bindings only (`--bindings-only`).
    Bindings,
    /// The bindings, then the XCFramework (macOS only).
    BindingsAndXcframework,
}

/// `cargo xtask ffi [--bindings-only]`.
pub fn ffi(output: FfiOutput) -> bool {
    let root = workspace_root();
    let out = temp_dir("sb-ffi-generate");
    let outcome = generate_bindings(&root, &out)
        .and_then(|_| install_bindings(&out, &swift_sources_dir(&root), &headers_dir(&root)))
        .and_then(|()| {
            eprintln!(
                "ffi: bindings installed under {}",
                core_package(&root).display()
            );
            finish(&root, output)
        });
    let _ = fs::remove_dir_all(&out);
    let Err(err) = outcome else {
        return true;
    };
    eprintln!("ffi: {err}");
    false
}

/// `cargo xtask check-ffi`: a fresh generation matches the committed
/// bindings, its Swift resolves its FFI import, the committed Swift
/// type-checks (macOS), and the exported surface has no async
/// foreign callback.
pub fn check_ffi() -> bool {
    let root = workspace_root();
    let out = temp_dir("sb-ffi-check");
    let checked = generate_bindings(&root, &out).map(|library| run_checks(&root, &out, &library));
    let _ = fs::remove_dir_all(&out);
    match checked {
        Ok(true) => {
            eprintln!("check-ffi: bindings match and resolve; no async foreign callbacks");
            true
        }
        Ok(false) => false,
        Err(err) => {
            eprintln!("check-ffi: {err}");
            false
        }
    }
}

/// Every check over a fresh generation in `out`; each failing check
/// prints its findings and then its own summary. True when all pass.
fn run_checks(root: &Path, out: &Path, library: &Path) -> bool {
    let (violations, lint_failure) = match lint_async_foreign_callbacks(library) {
        Ok(violations) => (violations, Vec::new()),
        Err(err) => (Vec::new(), vec![err.to_string()]),
    };
    let checks = [
        (
            binding_drift(out, &swift_sources_dir(root), &headers_dir(root)),
            "file(s) drifted; run `cargo xtask ffi --bindings-only` and commit",
        ),
        (
            unresolved_ffi_imports(out, out),
            "unresolved FFI import(s): the Swift can't see its C declarations",
        ),
        (
            typecheck_swift(root),
            "Swift type-check failure(s) in the committed bindings",
        ),
        (
            violations,
            "async foreign callback(s); spec/07 forbids them in the pilot",
        ),
        (
            lint_failure,
            "lint failure(s): the metadata couldn't be read",
        ),
    ];

    let mut passed = true;
    for (findings, summary) in checks {
        if findings.is_empty() {
            continue;
        }
        passed = false;
        for line in &findings {
            eprintln!("check-ffi: {line}");
        }
        eprintln!("check-ffi: {} {summary}", findings.len());
    }
    passed
}

/// Type-check the committed Swift against the committed headers the
/// way the app compiles it (spec/08): Swift 6, complete strict
/// concurrency, warnings as errors, the app's target. macOS only:
/// the Swift toolchain and SDK the app builds with exist nowhere
/// else; elsewhere it says it skipped.
fn typecheck_swift(root: &Path) -> Vec<String> {
    if !cfg!(target_os = "macos") {
        eprintln!("check-ffi: the Swift type-check needs macOS (macOS CI runs it)");
        return Vec::new();
    }
    let sources = match read_by_extension(&swift_sources_dir(root), "swift") {
        Ok(sources) => sources,
        Err(err) => return vec![err.to_string()],
    };
    let mut paths = Vec::new();
    for (path, _) in &sources {
        match path_string(path) {
            Ok(path) => paths.push(path),
            Err(err) => return vec![err.to_string()],
        }
    }
    let headers = match path_string(&headers_dir(root)) {
        Ok(headers) => headers,
        Err(err) => return vec![err.to_string()],
    };

    let mut args = vec![
        "-typecheck",
        "-swift-version",
        "6",
        "-strict-concurrency=complete",
        "-warnings-as-errors",
        "-target",
        SWIFT_TARGET,
        "-I",
        &headers,
    ];
    args.extend(paths.iter().map(String::as_str));
    match run("swiftc", &args, root, None) {
        Ok(_) => Vec::new(),
        Err(err) => vec![err.to_string()],
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
    let config = path_string(&root.join(BINDINGS_CONFIG))?;
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
            "--config",
            &config,
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
    let library = root
        .join("target/debug")
        .join(format!("{DLL_PREFIX}sb_ffi{DLL_SUFFIX}"));
    if library.is_file() {
        Ok(library)
    } else {
        Err(ToolError(format!(
            "the sb-ffi cdylib is missing at {}; build sb-ffi first",
            library.display()
        )))
    }
}

/// Copy the generated files into their committed homes — `.swift`
/// into `swift_dir`, the header and modulemap into `headers_dir` —
/// and remove committed files no longer generated, so a renamed
/// output leaves no stale copy behind (both dirs hold generated
/// files only; `binding_drift` enforces it). Copy rather than
/// rename: the generation dir may be on another filesystem.
fn install_bindings(
    generated: &Path,
    swift_dir: &Path,
    headers_dir: &Path,
) -> Result<(), ToolError> {
    fs::create_dir_all(swift_dir).map_err(io("create", swift_dir))?;
    fs::create_dir_all(headers_dir).map_err(io("create", headers_dir))?;
    let files = generated_files(generated)?;
    for file in &files {
        let to = if file.ends_with(".swift") {
            swift_dir.join(file)
        } else {
            headers_dir.join(file)
        };
        fs::copy(generated.join(file), &to).map_err(io("copy", &to))?;
    }

    for dir in [swift_dir, headers_dir] {
        for entry in fs::read_dir(dir).map_err(io("read", dir))? {
            let path = entry.map_err(io("read", dir))?.path();
            let generated_now = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| files.iter().any(|file| file == name));
            if generated_now {
                continue;
            }
            fs::remove_file(&path).map_err(io("remove", &path))?;
        }
    }
    Ok(())
}

/// After the bindings: the XCFramework on macOS, a note elsewhere.
fn finish(root: &Path, output: FfiOutput) -> Result<(), ToolError> {
    if let FfiOutput::Bindings = output {
        return Ok(());
    }
    if !cfg!(target_os = "macos") {
        eprintln!(
            "ffi: the XCFramework needs macOS \
             (run `cargo xtask ffi` there or in macOS CI)"
        );
        return Ok(());
    }
    build_xcframework(root)
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
    eprintln!("ffi: XCFramework built at {output}");
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

/// Whether the Swift can see its C declarations: every module the
/// Swift sources import behind `canImport(…)` — how UniFFI's Swift
/// imports its FFI module — must be declared by a modulemap in
/// `headers`, and every header a modulemap names must sit beside it.
/// Swift built against a mismatch finds no `RustBuffer` and no FFI
/// symbol, yet the XCFramework still builds; this catches it on any
/// host. Swift with no FFI import at all is reported too, so the
/// check never passes for want of anything to check. Each entry
/// names the file at fault.
pub fn unresolved_ffi_imports(swift: &Path, headers: &Path) -> Vec<String> {
    let (sources, modulemaps) = match (
        read_by_extension(swift, "swift"),
        read_by_extension(headers, "modulemap"),
    ) {
        (Ok(sources), Ok(modulemaps)) => (sources, modulemaps),
        (Err(err), _) | (_, Err(err)) => return vec![err.to_string()],
    };

    let mut problems = Vec::new();
    let mut declared = HashSet::new();
    for (path, text) in &modulemaps {
        let (modules, named_headers) = modulemap_entries(text);
        declared.extend(modules);
        for header in named_headers {
            if !headers.join(header).is_file() {
                problems.push(format!(
                    "{} names the header {header}, which isn't beside it",
                    path.display()
                ));
            }
        }
    }

    let mut imports = 0;
    for (path, text) in &sources {
        for module in conditional_imports(text) {
            imports += 1;
            if !declared.contains(module) {
                problems.push(format!(
                    "{} imports {module}, which no modulemap in {} declares",
                    path.display(),
                    headers.display()
                ));
            }
        }
    }
    if imports == 0 {
        problems.push(format!(
            "no Swift source in {} imports an FFI module",
            swift.display()
        ));
    }
    problems
}

/// The modules named by `canImport(Name)` in Swift source: UniFFI's
/// generated Swift wraps its FFI import in it
/// (`#if canImport(XFFI) import XFFI #endif`).
fn conditional_imports(swift: &str) -> Vec<&str> {
    swift
        .split(CAN_IMPORT)
        .skip(1)
        .filter_map(|rest| rest.split_once(')').map(|(module, _)| module.trim()))
        .collect()
}

/// A modulemap's declared modules (`module Name {`, also after
/// `framework`) and the headers it names (`header "x.h"`).
fn modulemap_entries(modulemap: &str) -> (Vec<&str>, Vec<&str>) {
    let mut modules = Vec::new();
    let mut headers = Vec::new();
    let mut tokens = modulemap.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "module" => modules.extend(tokens.next().map(|name| name.trim_end_matches('{'))),
            "header" => headers.extend(tokens.next().map(|name| name.trim_matches('"'))),
            _ => {}
        }
    }
    (modules, headers)
}

/// Every file in `dir` with extension `ext`, with its contents,
/// sorted by path.
fn read_by_extension(dir: &Path, ext: &str) -> Result<Vec<(PathBuf, String)>, ToolError> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(io("read", dir))?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|found| found == ext))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).map_err(io("read", &path))?;
            Ok((path, text))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Installing over outdated bindings leaves exactly the fresh
    /// generation behind: changed files replaced, files no longer
    /// generated (a renamed output) removed.
    #[test]
    fn install_replaces_outdated_and_removes_stale_bindings() {
        let root = temp_dir("xtask-ffi-install");
        let _ = fs::remove_dir_all(&root);
        let generated = root.join("generated");
        let swift = root.join("Sources");
        let headers = root.join("Headers");
        for dir in [&generated, &swift, &headers] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(generated.join("Core.swift"), "let a = 2\n").unwrap();
        fs::write(generated.join("CoreFFI.h"), "// new\n").unwrap();
        fs::write(generated.join(MODULEMAP_FILENAME), "module CoreFFI {}\n").unwrap();
        fs::write(swift.join("Core.swift"), "let a = 1\n").unwrap();
        fs::write(swift.join("old.swift"), "let old = 0\n").unwrap();
        fs::write(headers.join("oldFFI.h"), "// old\n").unwrap();

        install_bindings(&generated, &swift, &headers).expect("installs");

        assert_eq!(
            binding_drift(&generated, &swift, &headers),
            Vec::<String>::new()
        );
        let _ = fs::remove_dir_all(&root);
    }
}
