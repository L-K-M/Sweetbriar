//! `sb-cli` — L1. Developer/ops CLI over the same service API as the
//! app (spec/07): the Linux-testable "app". Depends on `sb-service`
//! only; arguments are hand-parsed (no argument-parsing dependency,
//! per plan M0.7's dependency rules).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use sb_service::{
    Anchor, BackupGeneration, BackupState, BackupVerdict, Core, CoreConfig, CoreError,
    ExportFormat, ExportJobState, ExportScopeInput, ImportJob, ImportJobState, ImportTarget,
    MboxVariant, WindowSpec, open_core,
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
    SettingsGet {
        key: Option<String>,
    },
    /// `settings set <key> <value>`: one command under a fresh
    /// idempotency key from the service.
    SettingsSet {
        key: String,
        value: String,
    },
    /// `import [--variant mboxo|mboxrd] <path>...`: one import job,
    /// inventory to completion, with `variant` for ambiguous sources.
    Import {
        variant: Option<MboxVariant>,
        paths: Vec<PathBuf>,
    },
    /// `export mailbox <mailbox_id> <destination> [--format eml|mboxrd]`:
    /// one export job, capture to completed output.
    /// `export account <account_id> <destination> [--format ...]`: the
    /// account's mailboxes.
    /// `export status <job_id>`: the durable record, printed as JSON-ish
    /// fields.
    /// `export cancel <job_id>`: durably cancel a live job.
    Export {
        scope: CliExportScope,
        format: ExportFormat,
        destination: String,
    },
    ExportStatus {
        job_id: String,
    },
    ExportCancel {
        job_id: String,
    },
    /// `backup list`: the recorded backup generations, newest first.
    BackupList,
}

/// The scope word after `export`, with the id it names.
enum CliExportScope {
    Mailbox(i64),
    Account(String),
}

/// The usage text a usage error prints.
const USAGE: &str = "usage: sb-cli [--data-root <path>] settings get [<key>]
       sb-cli [--data-root <path>] settings set <key> <value>
       sb-cli [--data-root <path>] import [--variant mboxo|mboxrd] <path>...
       sb-cli [--data-root <path>] export mailbox <mailbox_id> <destination> [--format eml|mboxrd]
       sb-cli [--data-root <path>] export account <account_id> <destination> [--format eml|mboxrd]
       sb-cli [--data-root <path>] export status <job_id>
       sb-cli [--data-root <path>] export cancel <job_id>
       sb-cli [--data-root <path>] backup list";

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
    if word == "export" {
        return export_from(args);
    }
    if word == "backup" {
        return backup_from(args);
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

/// `backup`'s arguments: the `list` subcommand, and nothing after it.
fn backup_from(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("backup needs a subcommand".to_owned());
    };
    if subcommand != "list" {
        return Err(format!("unknown backup subcommand '{subcommand}'"));
    }
    if args.next().is_some() {
        return Err("backup list takes no arguments".to_owned());
    }
    Ok(Command::BackupList)
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

/// `export`'s arguments: a scope word with its id and a destination,
/// then an optional `--format`; `status`/`cancel` take just the job id.
fn export_from(args: &mut impl Iterator<Item = String>) -> Result<Command, String> {
    let Some(subcommand) = args.next() else {
        return Err("export needs a subcommand".to_owned());
    };
    match subcommand.as_str() {
        "status" | "cancel" => {
            let rest: Vec<String> = args.collect();
            if rest.len() != 1 || rest[0].starts_with("--") {
                return Err(format!("export {subcommand} takes exactly one job id"));
            }
            return Ok(if subcommand == "status" {
                Command::ExportStatus {
                    job_id: rest[0].clone(),
                }
            } else {
                Command::ExportCancel {
                    job_id: rest[0].clone(),
                }
            });
        }
        "mailbox" | "account" => {}
        other => return Err(format!("unknown export subcommand '{other}'")),
    }
    let Some(id) = args.next() else {
        return Err(format!("export {subcommand} needs its id"));
    };
    let scope = match subcommand.as_str() {
        "mailbox" => CliExportScope::Mailbox(
            id.parse::<i64>()
                .map_err(|_| "mailbox_id must be an integer".to_owned())?,
        ),
        _ => CliExportScope::Account(id),
    };
    let mut destination = None;
    let mut format = None;
    while let Some(arg) = args.next() {
        if arg == "--format" {
            let chosen = match args.next().as_deref() {
                Some("eml") => ExportFormat::Eml,
                Some("mboxrd") => ExportFormat::Mboxrd,
                _ => return Err("--format takes eml or mboxrd".to_owned()),
            };
            if format.replace(chosen).is_some() {
                return Err("--format was given twice".to_owned());
            }
            continue;
        }
        if arg.starts_with("--") {
            return Err(format!("unknown export option '{arg}'"));
        }
        if destination.replace(arg).is_some() {
            return Err("export takes exactly one destination".to_owned());
        }
    }
    let Some(destination) = destination else {
        return Err("export needs a destination path".to_owned());
    };
    Ok(Command::Export {
        scope,
        format: format.unwrap_or(ExportFormat::Mboxrd),
        destination,
    })
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
        Command::Export {
            scope,
            format,
            destination,
        } => export(&core, scope, *format, destination),
        Command::ExportStatus { job_id } => export_status(&core, job_id),
        Command::ExportCancel { job_id } => export_cancel(&core, job_id),
        Command::BackupList => backup_list(&core),
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

/// `export` (spec/09 "Export file output"): fence the scope at the
/// generation the CLI just read (the window's, or `mailboxes`'s for
/// an account), start the job, wait and report. A `Conflict` at
/// capture means the generation moved; the job is `Failed` with the
/// reason in its checkpoint.
fn export(
    core: &Core,
    scope: &CliExportScope,
    format: ExportFormat,
    destination: &str,
) -> Result<(), u8> {
    // Persist an absolute identity, without requiring the output to exist.
    let absolute_destination = std::path::absolute(destination).map_err(|error| {
        eprintln!("sb-cli: cannot resolve the export destination: {error}");
        ERROR_EXIT
    })?;
    let destination = absolute_destination.to_str().ok_or_else(|| {
        eprintln!("sb-cli: the export destination must be a UTF-8 path");
        ERROR_EXIT
    })?;
    let scope = match scope {
        CliExportScope::Mailbox(mailbox_id) => {
            let window =
                block_on(core.open_window(WindowSpec::Mailbox(*mailbox_id), Anchor::Top, 1))
                    .map_err(error_exit)?;
            ExportScopeInput::Mailbox {
                mailbox_id: *mailbox_id,
                generation: window.generation,
            }
        }
        CliExportScope::Account(account_id) => {
            let mailboxes = block_on(core.mailboxes(account_id)).map_err(error_exit)?;
            ExportScopeInput::Account {
                account_id: account_id.clone(),
                generation: mailboxes.generation,
            }
        }
    };
    let key = core.new_idempotency_key();
    let started = block_on(core.start_export(&key, scope, format, destination.to_owned()))
        .map_err(error_exit)?;
    println!("job {}", started.job_id);
    let mut last_state = None;
    let job = loop {
        let job = block_on(core.export_job(&started.job_id)).map_err(error_exit)?;
        if job.state.is_finished() {
            break job;
        }
        if last_state != Some(job.state) {
            eprintln!(
                "sb-cli: export {} is {:?} ({} capture chunks)",
                started.job_id, job.state, job.captured_chunks
            );
            last_state = Some(job.state);
        }
        std::thread::sleep(IMPORT_POLL);
    };
    println!("{:?}: {} capture chunks", job.state, job.captured_chunks);
    if job.state != ExportJobState::Completed {
        eprintln!(
            "sb-cli: the export did not complete; checkpoint: {}",
            job.checkpoint_json
        );
        return Err(ERROR_EXIT);
    }
    println!("manifest: {}/sweetbriar-metadata.json", destination);
    Ok(())
}

/// `export status`: the durable record — state, sealed request hash,
/// checkpoint and captured chunk count.
fn export_status(core: &Core, job_id: &str) -> Result<(), u8> {
    let job = block_on(core.export_job(job_id)).map_err(error_exit)?;
    println!("state: {:?}", job.state);
    println!("request: {}", job.request_json);
    println!("checkpoint: {}", job.checkpoint_json);
    println!("captured_chunks: {}", job.captured_chunks);
    Ok(())
}

/// `export cancel`: the durable cancel; a live worker stops at its
/// next progress check. A finished job is `Conflict`.
fn export_cancel(core: &Core, job_id: &str) -> Result<(), u8> {
    let receipt =
        block_on(core.cancel_job(&core.new_idempotency_key(), job_id)).map_err(error_exit)?;
    println!("{}", receipt.result_json);
    Ok(())
}

/// `backup list`: one line per recorded generation, newest first. A
/// store without any prints nothing and says so on stderr.
fn backup_list(core: &Core) -> Result<(), u8> {
    let generations = block_on(core.list_backups()).map_err(error_exit)?;
    if generations.is_empty() {
        eprintln!("sb-cli: no backups are recorded");
    }
    for generation in &generations {
        println!("{}", backup_line(generation));
    }
    Ok(())
}

/// One generation as tab-separated fields: its id, state, start, size,
/// the weekly check's verdict and its directory. The directory comes
/// last, since only a path can hold a tab.
fn backup_line(generation: &BackupGeneration) -> String {
    let state = match generation.state {
        BackupState::InProgress => "in progress",
        BackupState::Complete => "complete",
        BackupState::Failed => "failed",
    };
    let size = match (generation.total_bytes, generation.object_count) {
        (Some(bytes), Some(objects)) => format!("{bytes} bytes in {objects} objects"),
        _ => "-".to_owned(),
    };
    let check = match generation.verification {
        None => "not checked".to_owned(),
        Some(verification) => {
            let verdict = match verification.verdict {
                BackupVerdict::Intact => "intact",
                BackupVerdict::Damaged => "damaged",
                BackupVerdict::Missing => "missing",
            };
            format!("{verdict} {}", utc(verification.checked_at_ms))
        }
    };
    format!(
        "{}\t{state}\t{}\t{size}\t{check}\t{}",
        generation.generation_id,
        utc(generation.started_at_ms),
        generation.location.display()
    )
}

/// Milliseconds since the epoch as an RFC 3339 UTC time to the second.
fn utc(millis: i64) -> String {
    const DAY: i64 = 86_400;
    let seconds = millis.div_euclid(1_000);
    let (days, second) = (seconds.div_euclid(DAY), seconds.rem_euclid(DAY));
    // Days since 1970-01-01 to a civil date, in 400-year eras that
    // start on a 1 March (Hinnant's `civil_from_days`).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second / 3_600,
        second % 3_600 / 60,
        second % 60
    )
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sb_service::BackupVerification;

    use super::*;

    #[test]
    fn times_print_as_utc_to_the_second() {
        for (millis, shown) in [
            (0, "1970-01-01T00:00:00Z"),
            (-1, "1969-12-31T23:59:59Z"),
            (999, "1970-01-01T00:00:00Z"),
            // A leap day, and the days around a century's end.
            (1_709_210_096_000, "2024-02-29T12:34:56Z"),
            (951_782_400_000, "2000-02-29T00:00:00Z"),
            (4_107_542_399_000, "2100-02-28T23:59:59Z"),
            (4_107_542_400_000, "2100-03-01T00:00:00Z"),
            (1_794_182_400_000, "2026-11-09T00:00:00Z"),
            (1_798_761_599_999, "2026-12-31T23:59:59Z"),
        ] {
            assert_eq!(utc(millis), shown, "{millis}");
        }
    }

    #[test]
    fn a_generation_prints_as_one_tab_separated_line() {
        let complete = BackupGeneration {
            generation_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
            state: BackupState::Complete,
            started_at_ms: 1_794_182_400_000,
            completed_at_ms: Some(1_794_182_401_000),
            location: PathBuf::from("/data/store/backups/00000000-0000-4000-8000-0000000000b1"),
            object_count: Some(3),
            total_bytes: Some(4_096),
            verification: Some(BackupVerification {
                verdict: BackupVerdict::Damaged,
                checked_at_ms: 1_794_268_800_000,
            }),
        };
        assert_eq!(
            backup_line(&complete),
            "00000000-0000-4000-8000-0000000000b1\tcomplete\t2026-11-09T00:00:00Z\t\
             4096 bytes in 3 objects\tdamaged 2026-11-10T00:00:00Z\t\
             /data/store/backups/00000000-0000-4000-8000-0000000000b1"
        );

        let running = BackupGeneration {
            state: BackupState::InProgress,
            completed_at_ms: None,
            object_count: None,
            total_bytes: None,
            verification: None,
            ..complete.clone()
        };
        assert_eq!(
            backup_line(&running),
            "00000000-0000-4000-8000-0000000000b1\tin progress\t2026-11-09T00:00:00Z\t-\t\
             not checked\t/data/store/backups/00000000-0000-4000-8000-0000000000b1"
        );
        for (verdict, shown) in [
            (BackupVerdict::Intact, "intact"),
            (BackupVerdict::Missing, "missing"),
        ] {
            let checked = BackupGeneration {
                state: BackupState::Failed,
                verification: Some(BackupVerification {
                    verdict,
                    checked_at_ms: 0,
                }),
                ..complete.clone()
            };
            let line = backup_line(&checked);
            assert!(line.contains("\tfailed\t"), "{line}");
            assert!(
                line.contains(&format!("\t{shown} 1970-01-01T00:00:00Z\t")),
                "{line}"
            );
        }
    }
}
