//! `sb-cli` — L1. Developer/ops CLI over the same service API as the
//! app (spec/07): the Linux-testable "app". Depends on `sb-service`
//! only; arguments are hand-parsed (no argument-parsing dependency,
//! per plan M0.7's dependency rules).

use std::path::PathBuf;
use std::process::ExitCode;

use sb_service::{Core, CoreConfig, CoreError, open_core};

/// How a usage failure exits: everything the user mistyped (plan M0.7).
const USAGE_EXIT: u8 = 2;

/// How a core failure exits, with the error printed to stderr.
const ERROR_EXIT: u8 = 1;

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

/// The CLI's commands (plan M0.7): the settings query and the settings
/// command.
enum Command {
    /// `settings get [<key>]`: every setting, or one.
    SettingsGet { key: Option<String> },
    /// `settings set <key> <value>`: one command under a fresh
    /// idempotency key from the service.
    SettingsSet { key: String, value: String },
}

/// The usage text a usage error prints.
const USAGE: &str = "usage: sb-cli [--data-root <path>] settings get [<key>]
       sb-cli [--data-root <path>] settings set <key> <value>";

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

/// Report a core failure and the exit code it earns.
fn failed(err: CoreError) -> Result<(), u8> {
    eprintln!("sb-cli: {err}");
    Err(ERROR_EXIT)
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
