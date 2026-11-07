//! `xtask` — tool (outside the L1–L6 graph). Sweetbriar's task runner.
//!
//! `cargo xtask ci` mirrors `.github/workflows/ci.yml` on Linux: every
//! gate except the Docker-based Dovecot suite (its own `dovecot` job;
//! `cargo xtask dovecot`, or `ci --docker` to fold it in) and the
//! macOS-only checks, which `.github/workflows/macos.yml` covers.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use xtask::ffi::FfiOutput;

/// The generated notices file the drift check diffs against.
const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES.md";
/// cargo-about's Handlebars template, at the repo root.
const NOTICES_TEMPLATE: &str = "notices.hbs";

type Step = fn(&Path) -> bool;

fn main() -> ExitCode {
    let Some(task) = env::args().nth(1) else {
        usage();
        return ExitCode::FAILURE;
    };

    match task.as_str() {
        "ci" => ci(),
        "dovecot" => dovecot(),
        "notices" => notices(),
        "check-layers" => check_layers(),
        "ffi" => ffi(),
        "check-ffi" => check_ffi(),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

/// `cargo xtask ffi [--bindings-only]`: generate the Swift bindings
/// (and, on macOS, build the XCFramework).
fn ffi() -> ExitCode {
    let flags: Vec<String> = env::args().skip(2).collect();
    let output = match flags.as_slice() {
        [] => FfiOutput::BindingsAndXcframework,
        [flag] if flag == "--bindings-only" => FfiOutput::Bindings,
        _ => {
            usage();
            return ExitCode::FAILURE;
        }
    };
    if xtask::ffi::ffi(output) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `cargo xtask check-ffi`: bindings drift + async-callback lint.
fn check_ffi() -> ExitCode {
    if env::args().nth(2).is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    if xtask::ffi::check_ffi() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The full Linux CI gate, in the same order as `ci.yml`; first failure
/// stops the run. `--docker` appends the Dovecot protocol suite
/// (spec/11), which `ci.yml` runs as its own `dovecot` job.
fn ci() -> ExitCode {
    let with_docker = match env::args().nth(2).as_deref() {
        None => false,
        Some("--docker") => true,
        _ => {
            usage();
            return ExitCode::FAILURE;
        }
    };

    let root = workspace_root();
    let steps: [(&str, Step); 7] = [
        ("fmt", step_fmt),
        ("clippy", step_clippy),
        ("test", step_test),
        ("doc", step_doc),
        ("check-layers", step_check_layers),
        ("deny", step_deny),
        ("notices-drift", step_notices_drift),
    ];

    for (name, step) in steps {
        eprintln!("==> cargo xtask ci: {name}");
        if step(&root) {
            continue;
        }
        eprintln!("==> cargo xtask ci: step `{name}` failed; stopping");
        return ExitCode::FAILURE;
    }

    if with_docker && !step_dovecot(&root) {
        eprintln!("==> cargo xtask ci: step `dovecot` failed; stopping");
        return ExitCode::FAILURE;
    }

    eprintln!("==> cargo xtask ci: all steps passed");
    ExitCode::SUCCESS
}

/// `cargo xtask dovecot`: the spec/11 protocol suite against a real
/// Dovecot. Fails — never skips — when Docker or the container can't
/// be reached.
fn dovecot() -> ExitCode {
    if env::args().nth(2).is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    if step_dovecot(&workspace_root()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Regenerates `THIRD_PARTY_NOTICES.md` at the repo root.
fn notices() -> ExitCode {
    let root = workspace_root();
    if generate_notices(&root, &root.join(NOTICES_FILE)) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn step_fmt(root: &Path) -> bool {
    run(&mut cargo(root, &["fmt", "--all", "--check"]))
}

fn step_clippy(root: &Path) -> bool {
    run(&mut cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ))
}

fn step_test(root: &Path) -> bool {
    run(&mut cargo(root, &["test", "--workspace"]))
}

fn step_doc(root: &Path) -> bool {
    let mut cmd = cargo(root, &["doc", "--workspace", "--no-deps"]);
    cmd.env("RUSTDOCFLAGS", "-D warnings");
    run(&mut cmd)
}

/// `cargo xtask check-layers [root]`: the layering gate, run on its
/// own. An optional workspace root lets the fixtures under
/// `xtask/tests/layers/` be checked by hand.
fn check_layers() -> ExitCode {
    let root = env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(workspace_root);
    if run_check_layers(&root) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The `ci` step: same checker, folded into the step loop.
fn step_check_layers(root: &Path) -> bool {
    run_check_layers(root)
}

/// Checks the workspace's sources against `layers.toml` and prints
/// each violation (`file:line: <from> (Lx) -> <to> (Ly): <why>`).
fn run_check_layers(root: &Path) -> bool {
    match xtask::layers::check_workspace(root) {
        Ok(report) if report.violations.is_empty() => {
            eprintln!("check-layers: all layer boundaries hold");
            true
        }
        Ok(report) => {
            for violation in &report.violations {
                eprintln!("{violation}");
            }
            eprintln!("check-layers: {} violation(s)", report.violations.len());
            false
        }
        Err(error) => {
            eprintln!("check-layers: {error}");
            false
        }
    }
}

fn step_deny(root: &Path) -> bool {
    run(&mut cargo(root, &["deny", "check"]))
}

/// `dovecot/dovecot:2.4.5`, resolved to its multi-arch manifest
/// digest (spec/11 pins the image; a floating tag would drift).
const DOVECOT_IMAGE: &str =
    "dovecot/dovecot:2.4.5@sha256:43c14def08a57f25107593cbcf64a0a6fbae84a7b9de7aa038513c9b21e91914";
/// The rootless 2.4 image's cleartext IMAP listener.
const DOVECOT_CONTAINER_PORT: &str = "31143";
/// `USER_PASSWORD={PLAIN}dovecot` unlocks the image's static passdb:
/// any user name authenticates with it.
const DOVECOT_PASSWORD: &str = "dovecot";
/// How long the suite waits for Dovecot's listener after `docker run`.
const DOVECOT_READY_TIMEOUT: Duration = Duration::from_secs(60);
const DOVECOT_READY_POLL: Duration = Duration::from_millis(500);

/// The test identity staged for the container: the image reads its
/// TLS material from `/etc/dovecot/ssl/tls.{crt,key}`, so the suite
/// pins the same certificate the fake server presents.
const DOVECOT_CERT_SOURCE: &str = "crates/sb-test-support/certs";

/// Removes the container and staged TLS directory on drop.
struct ContainerGuard {
    name: String,
    ssl_dir: PathBuf,
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        let mut rm = Command::new("docker");
        run(rm.arg("rm").arg("-f").arg(&self.name));
        let _ = std::fs::remove_dir_all(&self.ssl_dir);
    }
}

/// The `dovecot` step: start the pinned image with an ephemeral
/// loopback port and the committed test certificate staged at the
/// image's TLS path, wait for its listener, then run the committed
/// `dovecot_operation_sequence` test pointed at it. Missing Docker,
/// an unpullable image or a dead listener all fail the step — the
/// suite fails rather than skipping (spec/11).
fn step_dovecot(root: &Path) -> bool {
    let name = format!("sb-dovecot-{}", std::process::id());

    // TLS material named for the image's defaults, staged so the
    // repository files stay untouched.
    let ssl_dir = env::temp_dir().join(format!("{name}-ssl"));
    let certs = root.join(DOVECOT_CERT_SOURCE);
    let staged = std::fs::create_dir_all(&ssl_dir)
        .and_then(|()| std::fs::copy(certs.join("imap-localhost.pem"), ssl_dir.join("tls.crt")))
        .and_then(|_| {
            std::fs::copy(
                certs.join("imap-localhost-key.pem"),
                ssl_dir.join("tls.key"),
            )
        });
    // These are public, committed test fixtures. The image runs as vmail,
    // while files copied through the Docker API are owned by root. Make the
    // staged fixtures readable independently of the client's checkout mode.
    #[cfg(unix)]
    let staged = staged.and_then(|_| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssl_dir, std::fs::Permissions::from_mode(0o755))?;
        for name in ["tls.crt", "tls.key"] {
            std::fs::set_permissions(ssl_dir.join(name), std::fs::Permissions::from_mode(0o644))?;
        }
        Ok(())
    });
    if let Err(err) = staged {
        eprintln!(
            "dovecot: cannot stage TLS material in {}: {err}",
            ssl_dir.display()
        );
        return false;
    }

    let mut docker_run = Command::new("docker");
    // Copy through the Docker API instead of bind-mounting a client path:
    // Docker Desktop/Colima and remote daemons cannot see every host temp dir.
    let created = run(docker_run
        .arg("create")
        .arg("--name")
        .arg(&name)
        .arg("-e")
        .arg(format!("USER_PASSWORD={{PLAIN}}{DOVECOT_PASSWORD}"))
        .arg("-p")
        .arg(format!("127.0.0.1::{DOVECOT_CONTAINER_PORT}"))
        .arg(DOVECOT_IMAGE));
    if !created {
        let _ = std::fs::remove_dir_all(&ssl_dir);
        return false;
    }
    let guard = ContainerGuard {
        name: name.clone(),
        ssl_dir,
    };

    let mut copy = Command::new("docker");
    if !run(copy
        .arg("cp")
        .arg(guard.ssl_dir.join("."))
        .arg(format!("{name}:/etc/dovecot/ssl")))
    {
        return false;
    }
    let mut start = Command::new("docker");
    if !run(start.arg("start").arg(&name)) {
        return false;
    }

    let mut docker_port = Command::new("docker");
    let port_output = docker_port
        .arg("port")
        .arg(&name)
        .arg(format!("{DOVECOT_CONTAINER_PORT}/tcp"))
        .output();
    let Ok(output) = port_output else {
        eprintln!("dovecot: `docker port` failed to run");
        return false;
    };
    if !output.status.success() {
        eprintln!("dovecot: `docker port` exited {}", output.status);
        return false;
    }
    // `docker port` prints `127.0.0.1:NNNNN` per family; take the first.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(addr) = stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    else {
        eprintln!("dovecot: `docker port` printed no mapping: {stdout:?}");
        return false;
    };
    eprintln!("dovecot: container {name} serves IMAP on {addr}");

    let ready = std::time::Instant::now();
    let mut up = false;
    while ready.elapsed() < DOVECOT_READY_TIMEOUT {
        match addr.rsplit_once(':').and_then(|(host, port)| {
            port.parse::<u16>()
                .ok()
                .map(|port| (host.to_string(), port))
        }) {
            Some((host, port)) => {
                if dovecot_greets(&host, port) {
                    up = true;
                    break;
                }
            }
            None => {
                eprintln!("dovecot: cannot parse {addr} as host:port");
                return false;
            }
        }
        std::thread::sleep(DOVECOT_READY_POLL);
    }
    if !up {
        eprintln!("dovecot: no listener on {addr} after {DOVECOT_READY_TIMEOUT:?}");
        return false;
    }

    let mut suite = cargo(
        root,
        &[
            "test",
            "-p",
            "sb-imap",
            "--test",
            "spike_operations",
            "dovecot_operation_sequence",
            "--",
            "--exact",
            "--nocapture",
        ],
    );
    suite
        .env("SB_IMAP_DOVECOT", addr)
        .env("SB_IMAP_DOVECOT_USER", "spike")
        .env("SB_IMAP_DOVECOT_PASSWORD", DOVECOT_PASSWORD);
    if !run(&mut suite) {
        return false;
    }

    // M2.2a2's STARTTLS transport: setup, ID and best-effort LOGOUT
    // through `Connection::connect_starttls`.
    let mut setup = cargo(
        root,
        &[
            "test",
            "-p",
            "sb-imap",
            "--test",
            "connection",
            "dovecot_starttls_setup_id_logout",
            "--",
            "--exact",
            "--nocapture",
        ],
    );
    setup
        .env("SB_IMAP_DOVECOT", addr)
        .env("SB_IMAP_DOVECOT_USER", "spike")
        .env("SB_IMAP_DOVECOT_PASSWORD", DOVECOT_PASSWORD);
    if !run(&mut setup) {
        return false;
    }
    let mut mailboxes = cargo(
        root,
        &[
            "test",
            "-p",
            "sb-imap",
            "--test",
            "mailboxes",
            "dovecot_mailbox_observations",
            "--",
            "--exact",
            "--nocapture",
        ],
    );
    mailboxes
        .env("SB_IMAP_DOVECOT", addr)
        .env("SB_IMAP_DOVECOT_USER", "spike")
        .env("SB_IMAP_DOVECOT_PASSWORD", DOVECOT_PASSWORD);
    run(&mut mailboxes)
}

/// Docker's port proxy accepts a connection before the container's
/// listener exists, so readiness is an IMAP greeting, not a connect.
fn dovecot_greets(host: &str, port: u16) -> bool {
    use std::io::Read;

    let Ok(mut stream) = std::net::TcpStream::connect((host, port)) else {
        return false;
    };
    if stream.set_read_timeout(Some(DOVECOT_READY_POLL)).is_err() {
        return false;
    }
    let mut greeting = [0u8; 4];
    stream.read_exact(&mut greeting).is_ok() && &greeting == b"* OK"
}

/// Regenerates the notices into a temp file and diffs it against the
/// committed one; any drift is an error with a remediation hint.
fn step_notices_drift(root: &Path) -> bool {
    let tmp = env::temp_dir().join(format!("sb-notices-{}.md", std::process::id()));
    if !generate_notices(root, &tmp) {
        return false;
    }

    let mut diff = Command::new("diff");
    let clean = run(diff.current_dir(root).arg("-u").arg(NOTICES_FILE).arg(&tmp));
    let _ = std::fs::remove_file(&tmp);
    if clean {
        return true;
    }

    eprintln!(
        "{NOTICES_FILE} is stale or missing. Regenerate it with \
         `cargo xtask notices` and commit the result."
    );
    false
}

fn generate_notices(root: &Path, out: &Path) -> bool {
    let mut cmd = cargo(root, &["about", "generate", NOTICES_TEMPLATE]);
    run(cmd.arg("--output-file").arg(out))
}

/// A `cargo` invocation pinned to the workspace root so `xtask` works
/// from any directory.
fn cargo(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root).args(args);
    cmd
}

/// Runs a command to completion, echoing it first; false on nonzero
/// exit or a spawn error.
fn run(cmd: &mut Command) -> bool {
    eprintln!("+ {}", describe(cmd));
    match cmd.status() {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("failed to spawn: {err}");
            false
        }
    }
}

fn describe(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&arg.to_string_lossy());
    }
    out
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <workspace>/xtask")
        .to_path_buf()
}

fn usage() {
    eprintln!(
        "usage: cargo xtask <ci [--docker]|dovecot|notices|check-layers|ffi [--bindings-only]|check-ffi>"
    );
}
