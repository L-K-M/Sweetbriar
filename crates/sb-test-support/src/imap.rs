//! The scripted fake IMAP server (spec/11): an in-process listener on
//! `127.0.0.1` answering client commands from a [`Script`] of
//! [`Step`]s — expected commands and canned response bytes — plus the
//! fault knobs spec/11 lists: [`Reply::Drop`] after command N or after
//! a tagged OK; one script per connection for a UIDVALIDITY change
//! between sessions; unsolicited `EXPUNGE`/`FETCH` lines moving
//! messages behind the client's back; partial FETCH results;
//! [`Reply::Delay`]; and the profile's `with_capabilities`.
//!
//! Command framing uses `imap-codec`'s [`Fragmentizer`], the codec the
//! client crate builds on, so the server sees exactly the boundaries
//! the client encodes, synchronizing literals included (`{n}` gets a
//! `+` first). Responses are scripted text, so a script can send bytes
//! no encoder would produce — the malformed ENVELOPE fault needs that.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use imap_codec::fragmentizer::{FragmentInfo, Fragmentizer, LiteralAnnouncement};
use imap_codec::imap_types::core::LiteralMode;

use crate::imap_cert;

/// One message's framing cap — what the real client's fragmentizer allows.
const MAX_MESSAGE_SIZE: u32 = 50 * 1024 * 1024;

/// Sent for each synchronizing literal: no LITERAL+ without a `+` first.
const SYNC_LITERAL_CONTINUATION: &str = "+ Ready for additional command text\r\n";

/// How long the accept and connection threads sleep between shutdown
/// checks; keeps `Drop` prompt without a self-connect.
const SHUTDOWN_POLL: Duration = Duration::from_millis(10);

/// Which command a step answers: a case-insensitive token-prefix match.
#[derive(Clone, Debug)]
pub struct Expect(String);

impl Expect {
    /// Normalized like a received command, so `LIST "" *` and
    /// `EXAMINE "All Mail"` match as a client sends them.
    pub fn command(text: impl Into<String>) -> Self {
        Self(normalize_line(text.into().as_bytes()))
    }

    fn matches(&self, command_tokens: &[String]) -> bool {
        let mut tokens = command_tokens.iter();
        self.0.split_whitespace().all(|want| {
            tokens
                .next()
                .is_some_and(|got| want.eq_ignore_ascii_case(got))
        })
    }
}

/// One scripted exchange: matched `expect` runs every `send` in order.
#[derive(Clone, Debug)]
pub struct Step {
    pub expect: Expect,
    pub send: Vec<Reply>,
}

impl Step {
    /// `Step { expect, send }` without the struct noise.
    pub fn on(expect: Expect, send: Vec<Reply>) -> Self {
        Self { expect, send }
    }
}

/// A response or action inside a step, in wire order.
#[derive(Clone, Debug)]
pub enum Reply {
    /// `* <text>\r\n`: an untagged data or status response.
    Untagged(String),
    /// `* <head> {n}\r\n<literal><tail>\r\n`: an untagged response
    /// carrying a literal; the server fills `{n}` in.
    UntaggedLiteral {
        head: String,
        bytes: Vec<u8>,
        tail: String,
    },
    /// `+ <text>\r\n`: a continuation response, e.g. `+ idling`.
    Continuation(String),
    /// `<tag> OK <text>\r\n`; tagless lines like `DONE` take the last
    /// tagged command's tag.
    TaggedOk(String),
    /// `<tag> NO <text>\r\n`.
    TaggedNo(String),
    /// `<tag> BAD <text>\r\n`.
    TaggedBad(String),
    /// Verbatim bytes — the knob for responses no encoder would emit
    /// (a malformed ENVELOPE); the script supplies the CRLF.
    Raw(Vec<u8>),
    /// Pause before the next send.
    Delay(Duration),
    /// STARTTLS: queued sends go out in cleartext, then the socket is
    /// wrapped in the committed test certificate.
    UpgradeTls,
    /// Close the connection without another byte.
    Drop,
}

impl Reply {
    pub fn untagged(text: impl Into<String>) -> Self {
        Self::Untagged(text.into())
    }

    pub fn ok(text: impl Into<String>) -> Self {
        Self::TaggedOk(text.into())
    }
}

/// A connection's canned conversation: the greeting, then the steps.
#[derive(Clone, Debug)]
pub struct Script {
    /// Sent on connect, verbatim; the full `* OK ...` greeting line.
    pub greeting: String,
    /// Steps consumed in order of first match: each command takes the
    /// earliest remaining step it satisfies, so an `EXAMINE` for a
    /// second mailbox needn't sit behind unrelated steps.
    pub steps: Vec<Step>,
    /// Responses for a command no step matches (tagged BAD by default).
    pub fallback: Vec<Reply>,
}

impl Script {
    /// A script with the default greeting and `steps`.
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            greeting: "* OK Fake IMAP server ready\r\n".to_string(),
            steps,
            fallback: vec![Reply::TaggedBad(
                "no scripted answer for this command".to_string(),
            )],
        }
    }

    /// Overrides the greeting line (CRLF included).
    pub fn greeting(mut self, line: impl Into<String>) -> Self {
        self.greeting = line.into();
        self
    }
}

/// One command line the server saw, in arrival order.
#[derive(Clone, Debug)]
pub struct ReceivedCommand {
    pub conn: usize,
    pub line: String,
}

#[derive(Default)]
struct Shared {
    received: Mutex<Vec<ReceivedCommand>>,
    unexpected: AtomicUsize,
}

/// The listening server: `serve` starts it, `Drop` joins its threads.
pub struct FakeImapServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl FakeImapServer {
    /// Binds `127.0.0.1` on an ephemeral port and accepts one
    /// connection per script in `scripts`; the last script repeats
    /// for further connections, so `serve(vec![s])` answers a client
    /// that reconnects. `scripts` must not be empty.
    pub fn serve(scripts: Vec<Script>) -> io::Result<Self> {
        assert!(!scripts.is_empty(), "serve needs at least one script");

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        let shutdown = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::default());
        let conns: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));

        let accept = {
            let (shutdown, shared, conns) = (shutdown.clone(), shared.clone(), conns.clone());
            thread::spawn(move || {
                let mut index = 0;
                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        return;
                    }
                    let (stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(SHUTDOWN_POLL);
                            continue;
                        }
                        Err(_) => return,
                    };
                    let script = scripts
                        .get(index)
                        .unwrap_or_else(|| scripts.last().expect("nonempty"))
                        .clone();
                    let (shutdown, shared) = (shutdown.clone(), shared.clone());
                    let conn = thread::spawn(move || {
                        let _ = run_connection(stream, script, shared, shutdown, index);
                    });
                    conns.lock().expect("conns").push(conn);
                    index += 1;
                }
            })
        };

        Ok(Self {
            port,
            shutdown,
            shared,
            accept: Some(accept),
            conns,
        })
    }

    /// The bound port on `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Every command line received so far, in order.
    pub fn received(&self) -> Vec<ReceivedCommand> {
        self.shared.received.lock().expect("received").clone()
    }

    /// Commands that hit the fallback step — script deviations a
    /// test asserts as zero.
    pub fn unexpected_count(&self) -> usize {
        self.shared.unexpected.load(Ordering::Relaxed)
    }

    /// Stops accepting and drops live connections without `Drop`.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for FakeImapServer {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let mut conns = self.conns.lock().expect("conns");
        for conn in conns.drain(..) {
            let _ = conn.join();
        }
    }
}

/// The connection's socket: cleartext, or TLS after [`Reply::UpgradeTls`]
/// via the crate's own [`imap_cert::TlsIo`].
enum Wire {
    Plain(TcpStream),
    Tls(Box<imap_cert::TlsIo<rustls::ServerConnection>>),
}

impl Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(stream) => stream.read(buf),
            Wire::Tls(stream) => stream.read(buf),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        match self {
            Wire::Plain(stream) => stream.write_all(buf),
            Wire::Tls(stream) => stream.write_all(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(stream) => stream.flush(),
            Wire::Tls(stream) => stream.flush(),
        }
    }

    /// STARTTLS: drives the handshake to completion over the
    /// cleartext socket, then wraps it.
    fn upgrade_tls(self, shutdown: &AtomicBool) -> io::Result<Self> {
        let Wire::Plain(mut tcp) = self else {
            return Err(io::Error::other("second STARTTLS on one connection"));
        };
        let mut conn =
            rustls::ServerConnection::new(imap_cert::server_config()).map_err(io::Error::other)?;
        while conn.is_handshaking() && !shutdown.load(Ordering::Relaxed) {
            match conn.complete_io(&mut tcp) {
                Ok(_) => {}
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Wire::Tls(Box::new(imap_cert::TlsIo::new(conn, tcp))))
    }

    fn sock(&self) -> &TcpStream {
        match self {
            Wire::Plain(tcp) => tcp,
            Wire::Tls(tls) => tls.sock(),
        }
    }

    /// Poll interval so reads notice shutdown.
    fn set_poll_timeout(&self) -> io::Result<()> {
        self.sock().set_read_timeout(Some(SHUTDOWN_POLL))
    }

    /// Closes the socket for [`Reply::Drop`].
    fn close(&self) {
        let _ = self.sock().shutdown(Shutdown::Both);
    }
}

/// One scripted connection's state: wire, framing, and the tag the
/// last tagged command carried (tagless lines like `DONE` reuse it).
struct Conn {
    wire: Wire,
    fragmentizer: Fragmentizer,
    last_tag: Option<String>,
}

impl Conn {
    /// Appends `<tag> <status> <text>\r\n` to the pending output.
    fn tagged(&self, status: &str, text: &str, pending: &mut Vec<u8>) {
        let tag = self.last_tag.as_deref().unwrap_or("*");
        pending.extend(format!("{tag} {status} {text}\r\n").as_bytes());
    }
}

/// Writes the step's pending response bytes in one call, keeping them
/// inside one segment/record.
fn flush_pending(wire: &mut Wire, pending: &mut Vec<u8>) -> io::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    wire.write_all(pending)?;
    pending.clear();
    wire.flush()
}

/// One command as the server consumed it.
struct Command {
    /// The first line, normalized (quotes dropped, whitespace
    /// collapsed) for matching and the trace.
    line: String,
    /// `line` split on whitespace, tag excluded.
    tokens: Vec<String>,
    tag: Option<String>,
}

/// The run of one connection: greeting, then command/step loops
/// until the client leaves, a [`Reply::Drop`] fires, or shutdown.
fn run_connection(
    stream: TcpStream,
    script: Script,
    shared: Arc<Shared>,
    shutdown: Arc<AtomicBool>,
    index: usize,
) -> io::Result<()> {
    let mut conn = Conn {
        wire: Wire::Plain(stream),
        fragmentizer: Fragmentizer::new(MAX_MESSAGE_SIZE),
        last_tag: None,
    };
    conn.wire.set_poll_timeout()?;
    conn.wire.write_all(script.greeting.as_bytes())?;

    let mut steps = script.steps;
    while !shutdown.load(Ordering::Relaxed) {
        let Some(command) = read_command(&mut conn, &shutdown)? else {
            return Ok(()); // EOF or shutdown
        };
        if command.tag.is_some() {
            conn.last_tag.clone_from(&command.tag);
        }
        shared
            .received
            .lock()
            .expect("received")
            .push(ReceivedCommand {
                conn: index,
                line: command.line,
            });

        let position = steps
            .iter()
            .position(|step| step.expect.matches(&command.tokens));
        let sends = match position {
            Some(at) => steps.remove(at).send,
            None => {
                shared.unexpected.fetch_add(1, Ordering::Relaxed);
                script.fallback.clone()
            }
        };

        // Consecutive response bytes go out in one write: a real
        // server emits a multi-line answer as one segment, and a
        // client that treats the first line as terminal (io-imap ends
        // LOGOUT on `* BYE`) must see the rest in the same read.
        let mut pending: Vec<u8> = Vec::new();
        for send in sends {
            match send {
                Reply::Untagged(text) => pending.extend(format!("* {text}\r\n").as_bytes()),
                Reply::UntaggedLiteral { head, bytes, tail } => {
                    pending.extend(format!("* {head} {{{}}}\r\n", bytes.len()).as_bytes());
                    pending.extend(&bytes);
                    pending.extend(format!("{tail}\r\n").as_bytes());
                }
                Reply::Continuation(text) => pending.extend(format!("+ {text}\r\n").as_bytes()),
                Reply::TaggedOk(text) => conn.tagged("OK", &text, &mut pending),
                Reply::TaggedNo(text) => conn.tagged("NO", &text, &mut pending),
                Reply::TaggedBad(text) => conn.tagged("BAD", &text, &mut pending),
                Reply::Raw(bytes) => pending.extend(&bytes),
                Reply::Delay(pause) => {
                    flush_pending(&mut conn.wire, &mut pending)?;
                    thread::sleep(pause);
                }
                Reply::UpgradeTls => {
                    flush_pending(&mut conn.wire, &mut pending)?;
                    conn.wire = conn.wire.upgrade_tls(&shutdown)?;
                }
                Reply::Drop => {
                    let _ = flush_pending(&mut conn.wire, &mut pending);
                    conn.wire.close();
                    return Ok(());
                }
            }
        }
        flush_pending(&mut conn.wire, &mut pending)?;
    }
    Ok(())
}

/// Reads one complete command message through the fragmentizer —
/// literals included, each synchronizing one answered with `+`.
fn read_command(conn: &mut Conn, shutdown: &AtomicBool) -> io::Result<Option<Command>> {
    let mut line: Option<String> = None;
    let mut tag: Option<String> = None;
    let mut buf = [0u8; 8192];

    loop {
        match conn.fragmentizer.progress() {
            Some(info) => {
                if let FragmentInfo::Line { announcement, .. } = info {
                    if line.is_none() {
                        let bytes = conn.fragmentizer.fragment_bytes(info).to_vec();
                        line = Some(normalize_line(&bytes));
                        tag = conn
                            .fragmentizer
                            .decode_tag()
                            .map(|t| t.as_ref().to_string());
                    }
                    if matches!(
                        announcement,
                        Some(LiteralAnnouncement {
                            mode: LiteralMode::Sync,
                            ..
                        })
                    ) {
                        conn.wire.write_all(SYNC_LITERAL_CONTINUATION.as_bytes())?;
                    }
                }
                if conn.fragmentizer.is_message_complete() {
                    let line = line.unwrap_or_default();
                    let tokens = line
                        .split_whitespace()
                        .skip(usize::from(tag.is_some()))
                        .map(str::to_string)
                        .collect();
                    return Ok(Some(Command { line, tokens, tag }));
                }
            }
            None => {
                if shutdown.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                match conn.wire.read(&mut buf) {
                    Ok(0) => return Ok(None),
                    Ok(n) => conn.fragmentizer.enqueue_bytes(&buf[..n]),
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::TimedOut => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

/// A command's first line for matching and tracing: up to the first
/// CRLF, quotes dropped, whitespace collapsed.
fn normalize_line(line: &[u8]) -> String {
    let end = line
        .iter()
        .position(|b| matches!(b, b'\r' | b'\n'))
        .unwrap_or(line.len());
    String::from_utf8_lossy(&line[..end])
        .split_whitespace()
        .map(|word| word.trim_matches('"'))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
