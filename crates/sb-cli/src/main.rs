//! `sb-cli` — L1. Developer/ops CLI over the same service API as the
//! app (spec/07): the Linux-testable "app". Depends on `sb-service`
//! only; arguments are hand-parsed (no argument-parsing dependency,
//! per plan M0.7's dependency rules).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sb_service::{
    Core, CoreConfig, CoreError, ImportJob, ImportJobState, ImportTarget, MboxVariant, open_core,
};

/// How a usage failure exits: everything the user mistyped (plan M0.7).
const USAGE_EXIT: u8 = 2;

/// How a core failure exits, with the error printed to stderr.
const ERROR_EXIT: u8 = 1;

/// How often `import` polls its job; shells poll `import_job` (spec/09).
const IMPORT_POLL: Duration = Duration::from_millis(100);
/// How long `import` waits for a finished job's report to be written.
const REPORT_PATIENCE: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    let invocation = match parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(problem) => {
            eprintln!("sb-cli: {problem}");
            eprint!("\n{USAGE}");
            return ExitCode::from(USAGE_EXIT);
        }
    };
    match run(invocation) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

/// One parsed command line: an optional `--data-root` and a command.
struct Invocation {
    data_root: Option<PathBuf>,
    command: Command,
}

/// The CLI's commands: the settings query and command (plan M0.7) and
/// `import` (plan M1.2, spec/09).
enum Command {
    /// `settings get [<key>]`: every setting, or one.
    SettingsGet { key: Option<String> },
    /// `settings set <key> <value>`: one command under a fresh
    /// idempotency key from the service.
    SettingsSet { key: String, value: String },
    /// `import [--variant mboxo|mboxrd] <path>...`: one import job,
    /// inventory to completion, with `variant` for ambiguous sources.
    Import {
        variant: Option<MboxVariant>,
        paths: Vec<PathBuf>,
    },
}

/// The usage text a usage error prints.
const USAGE: &str = "usage: sb-cli [--data-root <path>] settings get [<key>]
       sb-cli [--data-root <path>] settings set <key> <value>
       sb-cli [--data-root <path>] import [--variant mboxo|mboxrd] <path>...";

/// Parse the arguments after the program name: options, then exactly
/// one command with its fixed shape. Anything else is a usage error
/// naming the problem.
fn parse(mut args: impl Iterator<Item = String>) -> Result<Invocation, String> {
    let mut data_root = None;
    let word = loop {
        match args.next() {
            None => return Err("missing command".to_owned()),
            Some(option) if option == "--data-root" => {
                let Some(value) = args.next() else {
                    return Err("--data-root needs a path".to_owned());
                };
                if data_root.replace(PathBuf::from(value)).is_some() {
                    return Err("--data-root was given twice".to_owned());
                }
            }
            Some(option) if option.starts_with("--") => {
                return Err(format!("unknown option '{option}'"));
            }
            Some(word) => break word,
        }
    };
    let command = command_from(word, &mut args)?;
    Ok(Invocation { data_root, command })
}

/// Parse the command word and everything after it; each command takes
/// a fixed argument count, so what is left over decides.
fn command_from(word: String, args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    if word == "import" {
        return import_from(args);
    }
    if word != "settings" {
        return Err(format!("unknown command '{word}'"));
    }
    let Some(subcommand) = args.next() else {
        return Err("settings needs a subcommand".to_owned());
    };
    let rest: Vec<String> = args.collect();
    match (subcommand.as_str(), rest.len()) {
        ("get", 0) => Ok(Command::SettingsGet { key: None }),
        ("get", 1) => Ok(Command::SettingsGet {
            key: Some(rest[0].clone()),
        }),
        ("get", _) => Err("settings get takes at most one key".to_owned()),
        ("set", 2) => Ok(Command::SettingsSet {
            key: rest[0].clone(),
            value: rest[1].clone(),
        }),
        ("set", _) => Err("settings set takes <key> <value>".to_owned()),
        (other, _) => Err(format!("unknown settings subcommand '{other}'")),
    }
}

/// `import`'s arguments: an optional `--variant`, then at least one path.
/// An unknown `--` option is an error, not a path; after `--`, every
/// argument is a path.
fn import_from(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    let mut variant = None;
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        if arg == "--" {
            paths.extend(args.by_ref().map(PathBuf::from));
            break;
        }
        if arg.starts_with("--") && arg != "--variant" {
            return Err(format!("unknown import option '{arg}'"));
        }
        if arg != "--variant" {
            paths.push(PathBuf::from(arg));
            continue;
        }
        let chosen = match args.next().as_deref() {
            Some("mboxo") => MboxVariant::Mboxo,
            Some("mboxrd") => MboxVariant::Mboxrd,
            _ => return Err("--variant takes mboxo or mboxrd".to_owned()),
        };
        if variant.replace(chosen).is_some() {
            return Err("--variant was given twice".to_owned());
        }
    }
    if paths.is_empty() {
        return Err("import needs at least one path".to_owned());
    }
    Ok(Command::Import { variant, paths })
}

/// Open the core over the chosen data root (the platform's default
/// unless `--data-root` says otherwise), run the command, then shut
/// the core down before exiting — the CLI never leaves a store open
/// or a lease held.
fn run(invocation: Invocation) -> Result<(), u8> {
    let mut config = match CoreConfig::platform_default() {
        Ok(config) => config,
        Err(err) => return failed(err),
    };
    if let Some(data_root) = invocation.data_root {
        config.data_root = data_root;
    }
    let core = match block_on(open_core(config)) {
        Ok(core) => core,
        Err(err) => return failed(err),
    };
    let outcome = match &invocation.command {
        Command::SettingsGet { key } => get(&core, key.as_deref()),
        Command::SettingsSet { key, value } => set(&core, key, value),
        Command::Import { variant, paths } => import(&core, *variant, paths),
    };
    block_on(core.shutdown());
    outcome
}

/// `settings get`: every setting as `key=value` lines, or the one
/// asked for — a key the registry doesn't hold is `InvalidInput`, as
/// for `set`.
fn get(core: &Core, key: Option<&str>) -> Result<(), u8> {
    let settings = match block_on(core.settings()) {
        Ok(settings) => settings,
        Err(err) => return failed(err),
    };
    if let Some(wanted) = key {
        match settings.iter().find(|setting| setting.key == wanted) {
            Some(setting) => println!("{}={}", setting.key, setting.value),
            None => {
                return failed(CoreError::InvalidInput {
                    field: "key".to_owned(),
                    message: format!("unknown setting key '{wanted}'"),
                });
            }
        }
    } else {
        for setting in &settings {
            println!("{}={}", setting.key, setting.value);
        }
    }
    Ok(())
}

/// `settings set`: one command under an idempotency key the service
/// makes (the CLI has no UUID source of its own), printing the
/// receipt's result JSON.
fn set(core: &Core, key: &str, value: &str) -> Result<(), u8> {
    let idempotency_key = core.new_idempotency_key();
    match block_on(core.set_setting(&idempotency_key, key, value)) {
        Ok(receipt) => {
            println!("{}", receipt.result_json);
            Ok(())
        }
        Err(err) => failed(err),
    }
}

/// `import` (spec/09): start a job, print its inventory, confirm with
/// `variant` for every ambiguous source, then wait and print the totals
/// and the report. Only a completed job exits 0. Without a needed
/// variant the job is cancelled: nothing is guessed.
fn import(core: &Core, variant: Option<MboxVariant>, paths: &[PathBuf]) -> Result<(), u8> {
    let key = core.new_idempotency_key();
    let target = ImportTarget::LocalArchive;
    let started = block_on(core.start_import(&key, paths.to_vec(), target)).map_err(error_exit)?;
    println!("job {}", started.job_id);
    let job = wait(core, &started.job_id, |state| {
        state != ImportJobState::Inventory
    })?;
    if job.state != ImportJobState::AwaitingConfirmation {
        return outcome(&with_report(core, job)?);
    }
    print_inventory(&job);

    let ambiguous: Vec<u64> = job
        .sources
        .iter()
        .filter(|source| source.needs_variant)
        .map(|source| source.ordinal)
        .collect();
    let variants = match variant {
        Some(chosen) => ambiguous.iter().map(|&ordinal| (ordinal, chosen)).collect(),
        None if ambiguous.is_empty() => Vec::new(),
        None => {
            let cancelled = block_on(core.cancel_job(&core.new_idempotency_key(), &job.id));
            eprintln!(
                "sb-cli: sources {ambiguous:?} have >>From lines, which mboxo and mboxrd decode differently; rerun with --variant mboxo or --variant mboxrd"
            );
            return match cancelled {
                Ok(_) => {
                    eprintln!("sb-cli: the import was cancelled");
                    Err(USAGE_EXIT)
                }
                Err(err) => Err(error_exit(err)),
            };
        }
    };
    let key = core.new_idempotency_key();
    block_on(core.confirm_import(&key, &job.id, variants)).map_err(error_exit)?;
    let job = wait(core, &job.id, ImportJobState::is_finished)?;
    outcome(&with_report(core, job)?)
}

/// The finished `job` once its report is written, or as it stands
/// after `REPORT_PATIENCE`: a finished state alone doesn't mean the
/// report exists (spec/09).
fn with_report(core: &Core, mut job: ImportJob) -> Result<ImportJob, u8> {
    let deadline = Instant::now() + REPORT_PATIENCE;
    while job.report_path.is_none() && Instant::now() < deadline {
        std::thread::sleep(IMPORT_POLL);
        job = block_on(core.import_job(&job.id)).map_err(error_exit)?;
    }
    Ok(job)
}

/// Poll the job until `done` accepts its state.
fn wait(core: &Core, id: &str, done: impl Fn(ImportJobState) -> bool) -> Result<ImportJob, u8> {
    loop {
        let job = block_on(core.import_job(id)).map_err(error_exit)?;
        if done(job.state) {
            return Ok(job);
        }
        std::thread::sleep(IMPORT_POLL);
    }
}

fn print_inventory(job: &ImportJob) {
    let messages: u64 = job.sources.iter().map(|source| source.messages).sum();
    let bytes: u64 = job.sources.iter().map(|source| source.bytes).sum();
    println!(
        "inventory: {} sources, {messages} messages, {bytes} bytes",
        job.sources.len()
    );
    for source in &job.sources {
        let choice = if source.needs_variant {
            ", needs a variant"
        } else {
            ""
        };
        println!(
            "  [{}] {} -> {}: {} messages, {} bytes{choice}",
            source.ordinal,
            source.path.display(),
            source.mailbox,
            source.messages,
            source.bytes
        );
    }
    if job.problem_count == 0 {
        return;
    }
    println!("problems: {}", job.problem_count);
    for problem in &job.problems {
        println!(
            "  {:?} {}: {}",
            problem.kind,
            problem.path.display(),
            problem.detail
        );
    }
    let unlisted = job.problem_count - job.problems.len() as u64;
    if unlisted > 0 {
        println!("  and {unlisted} more, counted in the report");
    }
}

/// Print a finished job's totals and report; only completion succeeds.
fn outcome(job: &ImportJob) -> Result<(), u8> {
    let totals = &job.totals;
    println!(
        "{:?}: {} imported of {} processed, {} duplicates, {} unparseable",
        job.state, totals.imported, totals.processed, totals.duplicates, totals.rejected
    );
    println!(
        "read state: {} from headers, {} by default, {} with incomplete headers",
        totals.read_from_headers, totals.read_default, totals.flags_incomplete
    );
    if let Some(failure) = &job.failure {
        eprintln!("sb-cli: the import failed: {failure}");
    }
    match &job.report_path {
        Some(report) => println!("report: {}", report.display()),
        None => eprintln!("sb-cli: the report isn't written yet; the next open writes it"),
    }
    match job.state {
        ImportJobState::Completed => Ok(()),
        _ => Err(ERROR_EXIT),
    }
}

/// Report a core failure and the exit code it earns.
fn failed(err: CoreError) -> Result<(), u8> {
    Err(error_exit(err))
}

fn error_exit(err: CoreError) -> u8 {
    eprintln!("sb-cli: {err}");
    ERROR_EXIT
}

/// Drive one future to completion on the calling thread: a minimal
/// executor with a thread-park waker, as in the test support, since
/// the CLI takes no async dependency — the engine owns the one
/// runtime (spec/01), and the futures it returns are runtime-neutral.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    /// Wakes by unparking the blocked main thread. `Thread::unpark`
    /// leaves a permit, so an unpark that lands before the `park`
    /// below cannot be lost.
    struct ThreadParker(std::thread::Thread);

    impl Wake for ThreadParker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadParker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}
