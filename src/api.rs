//! termcode's own socket: a private Unix socket for each running termcode
//! ($XDG_RUNTIME_DIR/termcode/<pid>.sock, its folder 0700, the socket 0600), one JSON
//! request a line, one reply. The shells in its terminals know it (TERMCODE_SOCKET), so
//! `termcode PATH` typed in one opens PATH in this termcode instead of a second one
//! inside it: a file of this folder in place, another folder by switching to it (the
//! way a reload does: the shells go on).
//!
//! It is also termcode's API (doc/api.md): JSON-RPC 2.0, a request a line, several on
//! one connection. Commands, state, edits, terminals, the screen; `termcode api` is its
//! command line and skills/termcode/SKILL.md the guide for agents.
//!
//! Safety: only this user's processes get in (the folder 0700, the socket 0600, and each
//! peer's uid checked). Reading is free; changing files and running things (commands,
//! terminals, the screen) is asked of the user once per client, the process behind the
//! connection past shells and `termcode api` (`Gate`). `termcode.api` turns that to
//! read-only or off.

use crate::panel::App;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;
use std::time::Duration;

/// Where a terminal's shell finds its termcode.
pub const ENV: &str = "TERMCODE_SOCKET";
/// A request this long at most (a line).
const MAX: u64 = 64 << 10;

static PATH: OnceLock<PathBuf> = OnceLock::new();
/// The read end of the pipe the socket's thread wakes the event loop with (-1: none).
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// This termcode's socket, for its shells.
pub fn path() -> Option<&'static Path> {
    PATH.get().map(PathBuf::as_path)
}

/// The event loop waits on this too: a request is answered at once, not at its next tick.
pub fn wake_fd() -> Option<i32> {
    Some(WAKE.load(Ordering::Relaxed)).filter(|&fd| fd >= 0)
}

type Request = (Value, Sender<Value>, Client);

/// Connections served at once at most; more are turned away.
const CONNECTIONS: usize = 16;
/// Requests waiting at once at most (for the screen, output, the user's answer).
const WAITS: usize = 64;

pub struct Server {
    rx: Receiver<Request>,
    wake: std::io::PipeReader,
    path: PathBuf,
    /// Connections watching events, and the state they were last told of.
    watchers: Vec<(Vec<String>, Sender<Value>)>,
    /// To hand requests in from elsewhere (stdin): the queue and the loop's wake-up.
    tx: Sender<Request>,
    poke: std::io::PipeWriter,
    last: Option<Snapshot>,
    /// Requests answered later: when the next frame is drawn, a terminal prints a match,
    /// the user answers whether a client may.
    waits: Vec<(Wait, Sender<Value>)>,
}

/// Who asks: the first process behind the connection that is not a shell or `termcode
/// api` (an agent, a script, an editor), as (pid, its start time) and how it shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Client {
    pub key: (i32, u64),
    pub label: String,
    /// A shell of this termcode's own terminal, typed in by the user.
    pub own: bool,
    /// An extension termcode started (its id): its own views (`view.*`), and what the
    /// user granted when installing it.
    pub ext: Option<String>,
    pub granted: Vec<Tier>,
}

/// What a request may do: read, change files (`Edit`), run things (`Run`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    Read,
    Edit,
    Run,
}

/// The user's answers, per client and tier: allowed while that process runs, or refused.
#[derive(Default)]
pub struct Gate {
    answers: std::collections::HashMap<((i32, u64), Tier), bool>,
    /// Allowed for every client (TERMCODE_API_ALLOW, set by whoever starts termcode).
    allowed: Vec<Tier>,
    /// The answer to the prompt shown (`PAct::Api`): 0 deny, 1 once, 2 while it runs.
    pub answer: Option<u8>,
    /// The prompt for a client is up.
    pub asking: bool,
    /// A `ui.prompt` answered: its number, the button picked.
    pub picked: Option<(u64, usize)>,
    /// A `ui.input` / `ui.pick` answered: its number, the row picked (pick), the text.
    pub typed: Option<(u64, Option<usize>, String)>,
    /// The `ui.prompt` up, by number (another prompt over it, or Esc: no answer).
    asked: Option<u64>,
    next_ask: u64,
}

impl Gate {
    pub fn from_env() -> Self {
        let allowed = std::env::var("TERMCODE_API_ALLOW").unwrap_or_default().split(',').filter_map(|t| match t.trim() {
            "edit" => Some(Tier::Edit),
            "run" => Some(Tier::Run),
            _ => None,
        }).collect();
        Gate { allowed, ..Gate::default() }
    }

    /// Whether `c` may do `t` now: yes, no, or (None) the user is to be asked.
    pub fn check(&self, mode: &str, c: &Client, t: Tier) -> Option<bool> {
        match (mode, t) {
            // An extension: reading, and what its install granted, whatever the API's mode.
            _ if c.ext.is_some() && (t == Tier::Read || c.granted.contains(&t)) => Some(true),
            ("off", _) => Some(false),
            (_, Tier::Read) => Some(true),
            ("read-only", _) => Some(false),
            _ if c.own || self.allowed.contains(&t) => Some(true),
            _ => self.answers.get(&(c.key, t)).copied(),
        }
    }
}

/// An answer that waits, for request `id`, until `until` at most.
pub struct Wait {
    id: Value,
    until: std::time::Instant,
    what: Waiting,
}

enum Waiting {
    /// The next frame's text.
    Screen,
    /// Terminal `term` printing a match of `re` after byte `mark` of its output (looked
    /// at again only when it printed more than `seen`).
    Output { term: usize, re: fancy_regex::Regex, mark: u64, seen: Option<u64> },
    /// The user's answer: may `client` do `tier` (`req`, done once allowed)? `shown`:
    /// the prompt is up for it; `once`: allowed for this request only.
    Grant { client: Client, tier: Tier, req: Value, shown: bool, once: bool },
    /// `ui.prompt` number `n`, from `who`: its question and buttons; `shown` once up.
    Ask { n: u64, who: String, text: String, buttons: Vec<String>, shown: bool },
    /// An API call an extension serves, passed on (`ext::forward`): its reply, by key.
    Ext { key: String },
    /// `ui.input` / `ui.pick` number `n` the same: a picker's box, or its rows.
    Input { n: u64, who: String, text: String, value: String, items: Option<Vec<(String, String)>>, shown: bool },
}

/// A request's answer: now, or later (see `Wait`), or (events.watch) now and then events
/// on the same connection.
pub enum Reply {
    Now(Value),
    /// Boxed: a wait is large next to a reply.
    Later(Box<Wait>),
    Watch(Value, Vec<String>),
}

/// What watchers are told about, as last seen: each document's version and dirtiness, the
/// active tab and cursor, problems per file, Git, each terminal's and serial port's output end.
#[derive(Clone, Default, PartialEq)]
struct Snapshot {
    docs: std::collections::BTreeMap<String, (u64, bool)>,
    active: Option<String>,
    cursor: Option<(String, usize, usize)>,
    diags: std::collections::BTreeMap<String, (usize, usize)>,
    git: (String, usize),
    terms: Vec<u64>,
}

fn snapshot(app: &App) -> Snapshot {
    use crate::panel::Doc;
    let docs = app.docs.iter().filter_map(|(k, d)| match d {
        Doc::Text(b) if k.starts_with("file:") => Some((k.clone(), (b.version(), b.dirty()))),
        _ => None,
    });
    let tab = app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active));
    let cursor = tab.filter(|t| t.key.starts_with("file:")).map(|t| (t.key.clone(), t.view.cursor.line + 1, t.view.cursor.col + 1));
    let diags = app.docs.keys().filter(|k| k.starts_with("file:")).map(|k| {
        let all = app.lints.all(k);
        let errors = all.iter().filter(|d| d.3 == crate::lint::Sev::Error).count();
        (k.clone(), (errors, all.len() - errors))
    });
    Snapshot {
        docs: docs.collect(),
        active: tab.map(|t| t.key.clone()),
        cursor,
        diags: diags.filter(|(_, (e, w))| e + w > 0).collect(),
        git: (app.branch.clone(), app.status.len()),
        terms: app.terms.iter().map(|t| t.log.end()).collect(),
    }
}

/// What changed between `a` and `b`, as events (topic, its fields).
fn changes(app: &App, a: &Snapshot, b: &Snapshot) -> Vec<(&'static str, Value)> {
    let path = |k: &str| k.strip_prefix("file:").unwrap_or(k).to_string();
    let mut out = Vec::new();
    for (k, &(ver, dirty)) in &b.docs {
        match a.docs.get(k) {
            None => {
                out.push(("file.opened", json!({"path": path(k)})));
                // Opened by an edit (an API's, a refactor's): changed as well.
                if dirty {
                    out.push(("buffer.changed", json!({"path": path(k), "version": ver, "dirty": dirty})));
                }
            }
            Some(&(v0, d0)) => {
                if v0 != ver {
                    out.push(("buffer.changed", json!({"path": path(k), "version": ver, "dirty": dirty})));
                }
                if d0 && !dirty {
                    out.push(("file.saved", json!({"path": path(k)})));
                }
            }
        }
    }
    out.extend(a.docs.keys().filter(|k| !b.docs.contains_key(*k)).map(|k| ("file.closed", json!({"path": path(k)}))));
    if a.active != b.active {
        out.push(("tab", json!({"key": b.active})));
    }
    if let Some((k, line, col)) = &b.cursor {
        if a.cursor != b.cursor {
            out.push(("cursor", json!({"path": path(k), "line": line, "col": col})));
        }
    }
    for (k, v) in b.diags.iter().filter(|(k, v)| a.diags.get(*k) != Some(*v)).chain(a.diags.iter().filter(|(k, _)| !b.diags.contains_key(*k)).map(|(k, _)| (k, &(0, 0)))) {
        out.push(("diagnostics", json!({"path": path(k), "errors": v.0, "warnings": v.1})));
    }
    if a.git != b.git {
        out.push(("git", json!({"branch": b.git.0, "changes": b.git.1})));
    }
    for (i, &end) in b.terms.iter().enumerate() {
        let from = a.terms.get(i).copied().unwrap_or(end);
        if end > from {
            if let Some(t) = app.terms.get(i) {
                out.push(("terminal.output", json!({"index": i, "text": tail(t.log.since(from), 16 << 10)})));
            }
        }
    }
    out
}

/// Watchers told what changed since they were last (their topics only); gone ones dropped.
fn notify(app: &mut App) {
    if app.api.as_ref().is_none_or(|s| s.watchers.is_empty()) {
        return;
    }
    let now = snapshot(app);
    let before = app.api.as_mut().unwrap().last.replace(now.clone()).unwrap_or_else(|| now.clone());
    if before == now {
        return;
    }
    let events = changes(app, &before, &now);
    let srv = app.api.as_mut().unwrap();
    srv.watchers.retain(|(topics, tx)| {
        events.iter().filter(|(t, _)| topics.is_empty() || topics.iter().any(|x| x == t)).all(|(t, v)| {
            let mut params = v.clone();
            params["topic"] = json!(t);
            tx.send(json!({"jsonrpc": "2.0", "method": "event", "params": params})).is_ok()
        })
    });
}

/// An event (an extension's) to the watchers of `topic`; how many took it.
fn emit(app: &mut App, topic: &str, params: &Value) -> usize {
    let Some(srv) = app.api.as_mut() else { return 0 };
    let mut ev = params.clone();
    ev["topic"] = json!(topic);
    let mut n = 0;
    srv.watchers.retain(|(topics, tx)| {
        if !(topics.is_empty() || topics.iter().any(|t| t == topic)) {
            return true;
        }
        n += 1;
        tx.send(json!({"jsonrpc": "2.0", "method": "event", "params": ev})).is_ok()
    });
    n
}

/// The running termcode's server: its socket for its shells, its pipe for the event loop.
/// (Only `panel::run` arms one: tests' servers stay to themselves.)
pub fn arm(s: &Server) {
    let _ = PATH.set(s.path.clone());
    WAKE.store(s.wake.as_raw_fd(), Ordering::Relaxed);
}

/// The socket for this process in `dir` (made 0700), its requests to the event loop.
pub fn start(dir: &Path) -> Option<Server> {
    let _ = std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700).create(dir);
    let meta = fs::metadata(dir).ok()?;
    // SAFETY: geteuid has no preconditions.
    if meta.permissions().mode() & 0o077 != 0 || std::os::unix::fs::MetadataExt::uid(&meta) != unsafe { libc::geteuid() } {
        return None; // never a socket others can reach, nor in a folder of theirs
    }
    let p = dir.join(format!("{}.sock", std::process::id()));
    let _ = fs::remove_file(&p); // this pid's before a reload, or a crash's
    // Those of termcodes gone without cleaning up (killed hard, the power cut).
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.strip_suffix(".sock").and_then(|pid| pid.parse::<u32>().ok()).is_some_and(|pid| !process_alive(pid)) {
            let _ = fs::remove_file(e.path());
        }
    }
    let listener = UnixListener::bind(&p).ok()?;
    // (bind made it with the umask's mode: no one else gets in, the folder being 0700.)
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).ok()?;
    let (tx, rx) = channel();
    let (wake, poke) = std::io::pipe().ok()?;
    let (keep_tx, keep_poke) = (tx.clone(), poke.try_clone().ok()?);
    // SAFETY: the pipe's read end is open; reads from it must not block the loop.
    unsafe {
        let fd = wake.as_raw_fd();
        libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
    }
    let open = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("api".into())
        .spawn(move || {
            for s in listener.incoming().flatten() {
                // Only this user's processes (the socket's mode says so; this makes sure).
                let Some((pid, uid)) = peer(&s) else { continue };
                // SAFETY: geteuid has no preconditions.
                if uid != unsafe { libc::geteuid() } {
                    continue;
                }
                // Each connection its own thread: a long wait holds up no one else.
                if open.load(Ordering::Relaxed) >= CONNECTIONS {
                    let _ = (&s).write_all(b"{\"jsonrpc\": \"2.0\", \"id\": null, \"error\": {\"code\": -32000, \"message\": \"too many connections\"}}\n");
                    continue;
                }
                let (tx, mine) = (tx.clone(), open.clone());
                let Ok(mut poke) = poke.try_clone() else { continue };
                open.fetch_add(1, Ordering::Relaxed);
                let spawned = std::thread::Builder::new().name("api-conn".into()).spawn(move || {
                    serve(s, &tx, &mut poke, &client_of(pid));
                    mine.fetch_sub(1, Ordering::Relaxed);
                });
                if spawned.is_err() {
                    open.fetch_sub(1, Ordering::Relaxed);
                }
            }
            drop(poke);
        })
        .ok()?;
    Some(Server { rx, wake, path: p, waits: Vec::new(), watchers: Vec::new(), last: None, tx: keep_tx, poke: keep_poke })
}

/// Whether a process still exists, without sending it a signal.
pub(crate) fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    if pid == 0 { return false }
    #[cfg(target_os = "macos")]
    {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
        let mut len = 0;
        // SAFETY: sysctl first sizes, then fills an allocated byte buffer. A failed
        // query is treated as alive, so cleanup never removes a live process's files.
        unsafe {
            if libc::sysctl(mib.as_mut_ptr(), 4, std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 { return true }
            let mut info = vec![0u8; len];
            libc::sysctl(mib.as_mut_ptr(), 4, info.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 || len > 0
        }
    }
    #[cfg(not(target_os = "macos"))]
    { Path::new(&format!("/proc/{pid}")).exists() }
}

/// The connecting process: its pid and uid (SO_PEERCRED).
/// Whether process `pid` was started, at any depth, from this termcode.
pub fn descends(pid: i32) -> bool {
    let me = std::process::id() as i32;
    let mut p = pid;
    for _ in 0..64 {
        if p == me {
            return true;
        }
        match proc_stat(p) {
            Some((_, parent, _)) if parent > 1 => p = parent,
            _ => return false,
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
fn peer(s: &UnixStream) -> Option<(i32, u32)> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: an open socket, and a buffer of the size given.
    let r = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len) };
    (r == 0).then_some((cred.pid, cred.uid))
}

#[cfg(target_os = "macos")]
fn peer(s: &UnixStream) -> Option<(i32, u32)> {
    let (mut uid, mut gid, mut pid) = (0, 0, 0i32);
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    // SAFETY: an open socket and valid output buffers of the specified sizes.
    let ok = unsafe {
        libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) == 0
            && libc::getsockopt(s.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &mut len) == 0
    };
    (ok && len as usize == std::mem::size_of_val(&pid) && pid > 0).then_some((pid, uid))
}

#[cfg(target_os = "macos")]
pub(crate) fn proc_stat(pid: i32) -> Option<(String, i32, u64)> {
    // SAFETY: proc_bsdinfo is plain data and proc_pidinfo receives its exact size.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    if n != size { return None }
    let name: Vec<u8> = info.pbi_comm.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    Some((String::from_utf8_lossy(&name).into_owned(), info.pbi_ppid as i32, info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec))
}

/// A process's arguments (KERN_PROCARGS2: argc, the executable's path, NULs, then argv).
#[cfg(target_os = "macos")]
pub(crate) fn cmdline(pid: i32) -> Vec<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut len = 0;
    // SAFETY: the first call only sizes; the second fills a buffer of that many bytes.
    let mut buf = unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; len];
        if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) != 0 {
            return Vec::new();
        }
        buf
    };
    buf.truncate(len);
    parse_procargs(&buf)
}

#[cfg(any(target_os = "macos", test))]
fn parse_procargs(buf: &[u8]) -> Vec<String> {
    let Some((argc, rest)) = buf.split_first_chunk::<4>() else { return Vec::new() };
    let argc = usize::try_from(i32::from_ne_bytes(*argc)).unwrap_or(0);
    let Some(path_end) = rest.iter().position(|&b| b == 0) else { return Vec::new() };
    let args = &rest[path_end..];
    let start = args.iter().position(|&b| b != 0).unwrap_or(args.len());
    args[start..].split(|&b| b == 0).take(argc).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
}

/// A process's name, parent and start time (/proc/PID/stat).
#[cfg(not(target_os = "macos"))]
pub(crate) fn proc_stat(pid: i32) -> Option<(String, i32, u64)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (head, rest) = stat.rsplit_once(')')?;
    let comm = head.split_once('(')?.1.to_string();
    let f: Vec<&str> = rest.split_whitespace().collect();
    Some((comm, f.get(1)?.parse().ok()?, f.get(19)?.parse().ok()?))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn cmdline(pid: i32) -> Vec<String> {
    fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default().split(|&b| b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()
}

/// Who is behind the process `pid` that connected (see `Client`).
pub fn client_of(pid: i32) -> Client {
    let me = std::process::id() as i32;
    let mut p = pid;
    for _ in 0..32 {
        if p == me {
            return Client { key: (me, 0), label: "this termcode's terminal".into(), own: true, ..Client::default() };
        }
        let Some((comm, parent, start)) = proc_stat(p) else { break };
        let args = cmdline(p);
        let go_on = matches!(comm.as_str(), "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "mksh" | "tcsh" | "csh" | "env" | "timeout" | "xargs")
            || (comm == "termcode" && args.get(1).is_some_and(|a| a == "api"));
        if !go_on {
            let mut label = args.join(" ");
            if label.chars().count() > 80 {
                label = label.chars().take(79).collect::<String>() + "…";
            }
            return Client { key: (p, start), label: format!("{} (pid {p})", if label.is_empty() { comm } else { label }), ..Client::default() };
        }
        p = parent;
    }
    Client { key: (pid, 0), label: format!("pid {pid}"), ..Client::default() }
}

/// One connection: request lines in, each answered in turn, until it closes or idles 5 s.
fn serve(s: UnixStream, tx: &Sender<Request>, poke: &mut std::io::PipeWriter, client: &Client) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    // Gone: its end closed (looked at without waiting).
    let gone = || {
        let _ = s.set_nonblocking(true);
        let mut b = [0u8; 1];
        let closed = matches!((&s).read(&mut b), Ok(0));
        let _ = s.set_nonblocking(false);
        closed
    };
    serve_io(&mut BufReader::new(&s), &mut &s, &gone, tx, poke, client, None);
}

/// Request lines from `input`, each answered on `out` in turn; a watching one (events.watch)
/// then only carries its events, until `gone` or a write fails. With `events` (an
/// extension's stdin, shared), the events go there instead and requests go on.
#[allow(clippy::too_many_arguments)]
fn serve_io(input: &mut dyn BufRead, out: &mut dyn Write, gone: &dyn Fn() -> bool, tx: &Sender<Request>, poke: &mut std::io::PipeWriter, client: &Client, events: Option<&Sender<String>>) {
    loop {
        let mut line = String::new();
        let reply = match input.take(MAX).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.trim().is_empty() => continue,
            Ok(_) => match serde_json::from_str::<Value>(&line) {
                // An extension's reply to a call passed to it (no method, our "t…" id).
                Ok(r) if events.is_some() && r.get("method").is_none() && r["id"].as_str().is_some_and(|k| k.starts_with('t')) => {
                    let key = r["id"].as_str().unwrap_or_default().to_string();
                    let mut replies = EXT_REPLIES.lock().unwrap_or_else(|e| e.into_inner());
                    if replies.len() < 1000 {
                        replies.push((key, r));
                    }
                    drop(replies);
                    let _ = poke.write_all(b"!");
                    continue;
                }
                Ok(req) => {
                    // A wait's own timeout, the user's answer, and some to spare.
                    let secs = req["params"]["timeout"].as_f64().unwrap_or(0.0).clamp(0.0, 3600.0) + ASK.as_secs_f64() + 10.0;
                    let (rtx, rrx) = channel();
                    let _ = tx.send((req.clone(), rtx, client.clone()));
                    let _ = poke.write_all(b"!");
                    let reply = rrx.recv_timeout(Duration::from_secs_f64(secs)).unwrap_or_else(|_| failure(&req, "termcode did not answer"));
                    if let Some(ev_out) = events.filter(|_| req["method"] == "events.watch" && reply.get("result").is_some()) {
                        let ev_out = ev_out.clone();
                        std::thread::spawn(move || {
                            while let Ok(ev) = rrx.recv() {
                                if ev_out.send(ev.to_string()).is_err() {
                                    break;
                                }
                            }
                        });
                    // Watching: from now on this connection only carries the events.
                    } else if req["method"] == "events.watch" && reply.get("result").is_some() {
                        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
                            return;
                        }
                        loop {
                            match rrx.recv_timeout(Duration::from_secs(1)) {
                                Ok(ev) => {
                                    if writeln!(out, "{ev}").and_then(|()| out.flush()).is_err() {
                                        return;
                                    }
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if gone() => return,
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                    }
                    reply
                }
                Err(e) => json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("not JSON: {e}")}}),
            },
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            return;
        }
    }
}

/// The API on this process's stdin and stdout too (`--headless --api-stdio`), for the
/// program that started it: trusted as its own terminal is.
pub fn serve_stdio(srv: &Server) {
    let (tx, poke) = (srv.tx.clone(), srv.poke.try_clone());
    let Ok(mut poke) = poke else { return };
    let me = Client { key: (std::process::id() as i32, 0), label: "stdin".into(), own: true, ..Client::default() };
    let _ = std::thread::Builder::new().name("api-stdio".into()).spawn(move || {
        let stdin = std::io::stdin();
        serve_io(&mut stdin.lock(), &mut std::io::stdout(), &|| false, &tx, &mut poke, &me, None);
        // stdin closed: the starter is gone, termcode with it.
        END.store(true, Ordering::SeqCst);
    });
}

/// An extension: `cmd` started, its stdout's requests answered as a connection's (as
/// `client`, keyed by the child's pid), its stdin fed by the Sender returned (whole
/// lines: the replies, its events, what termcode tells it). `done` is set when it ends.
pub fn host(app: &App, mut cmd: std::process::Command, mut client: Client, done: std::sync::Arc<AtomicBool>) -> Result<(Sender<String>, u32), String> {
    use std::process::Stdio;
    let srv = app.api.as_ref().ok_or("termcode's API is not running")?;
    let (tx, mut poke) = (srv.tx.clone(), srv.poke.try_clone().map_err(|e| e.to_string())?);
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    client.key = (pid as i32, proc_stat(pid as i32).map_or(0, |s| s.2));
    let (otx, orx) = channel::<String>();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    // Its stdin from one thread: lines whole, in order, never blocking the screen; CLOSE
    // closes it (its replies' sender lives as long as it, so the channel alone never ends).
    std::thread::Builder::new().name("ext-out".into()).spawn(move || {
        for line in orx {
            if line == CLOSE || writeln!(stdin, "{line}").and_then(|()| stdin.flush()).is_err() {
                break;
            }
        }
    }).map_err(|e| e.to_string())?;
    let events = otx.clone();
    std::thread::Builder::new().name("ext-in".into()).spawn(move || {
        let mut out = Lines { buf: Vec::new(), to: events.clone() };
        serve_io(&mut BufReader::new(stdout), &mut out, &|| false, &tx, &mut poke, &client, Some(&events));
        let _ = child.wait();
        done.store(true, Ordering::SeqCst);
        let _ = poke.write_all(b"!");
    }).map_err(|e| e.to_string())?;
    Ok((otx, pid))
}

/// Sent to an extension's stdin thread: its stdin closed (the extension then ends).
pub const CLOSE: &str = "\u{0}close";

/// A writer whose flushed text goes, as lines, to an extension's stdin thread.
struct Lines {
    buf: Vec<u8>,
    to: Sender<String>,
}

impl Write for Lines {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        for l in text.lines().filter(|l| !l.is_empty()) {
            self.to.send(l.to_string()).map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        }
        Ok(())
    }
}

/// Extensions' replies to the calls passed to them, by key, until their waits take them.
static EXT_REPLIES: std::sync::Mutex<Vec<(String, Value)>> = std::sync::Mutex::new(Vec::new());

/// Set when the stdio client closed stdin (`--api-stdio`): headless ends.
pub static END: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long a client waits for the user's answer.
const ASK: Duration = Duration::from_secs(120);

/// The requests that came (from the event loop): each answered, put to wait, or put to
/// the user; the waits that came true, were answered or ran out answered. True when
/// something changed on screen.
pub fn tick(app: &mut App) -> bool {
    let Some(srv) = app.api.as_mut() else { return false };
    let mut buf = [0u8; 64];
    while matches!((&srv.wake).read(&mut buf), Ok(n) if n > 0) {}
    let reqs: Vec<Request> = srv.rx.try_iter().collect();
    let mut changed = !reqs.is_empty();
    let mut waits = std::mem::take(&mut app.api.as_mut().unwrap().waits);
    for (req, reply, client) in reqs {
        if waits.len() >= WAITS {
            let _ = reply.send(failure(&req, "too many requests waiting"));
            continue;
        }
        if let Some(w) = admit(app, &req, &client, &reply) {
            waits.push((w, reply));
        }
    }
    // The user's answer to the prompt up (3: closed with Esc, no for this request).
    let answer = app.api_gate.answer.take().or_else(|| (app.api_gate.asking && app.prompt.is_none()).then_some(3));
    if let Some(a) = answer {
        app.api_gate.asking = false;
        if let Some((w, _)) = waits.iter_mut().find(|(w, _)| matches!(w.what, Waiting::Grant { shown: true, .. })) {
            if let Waiting::Grant { client, tier, shown, once, .. } = &mut w.what {
                *shown = false;
                if a == 3 {
                    w.until = std::time::Instant::now();
                } else if a == 1 {
                    *once = true; // this request only
                } else {
                    app.api_gate.answers.insert((client.key, *tier), a == 2);
                }
            }
        }
        changed = true;
    }
    let now = std::time::Instant::now();
    let mut later = Vec::new();
    waits.retain_mut(|(w, reply)| {
        let done = match &mut w.what {
            Waiting::Output { term, re, mark, seen } => match app.terms.get(*term) {
                None => Some(Err("the terminal closed".to_string())),
                Some(t) if *seen == Some(t.log.end()) => None,
                Some(t) => {
                    *seen = Some(t.log.end());
                    let text = t.log.since(*mark);
                    re.find(text).ok().flatten().map(|m| Ok(json!({"matched": m.as_str(), "output": tail(text, 4096)})))
                }
            },
            Waiting::Screen => None,
            Waiting::Ext { key } => {
                let mut replies = EXT_REPLIES.lock().unwrap_or_else(|e| e.into_inner());
                replies.iter().position(|(k, _)| k == key).map(|i| {
                    let r = replies.remove(i).1;
                    match r.get("error") {
                        Some(e) => Err(e["message"].as_str().unwrap_or("failed").to_string()),
                        None => Ok(r["result"].clone()),
                    }
                })
            }
            Waiting::Input { n, items, shown, .. } => match app.api_gate.typed.take() {
                Some((tn, row, text)) if tn == *n => {
                    app.api_gate.asked = None;
                    Some(Ok(if items.is_some() { json!({"index": row, "label": text}) } else { json!({"text": text}) }))
                }
                other => {
                    app.api_gate.typed = other;
                    // Closed without an answer (Esc, a click outside it): null.
                    (*shown && app.api_gate.asked == Some(*n) && app.gitui.cmds.picker.is_none()).then(|| {
                        app.api_gate.asked = None;
                        Ok(if items.is_some() { json!({"index": null, "label": null}) } else { json!({"text": null}) })
                    })
                }
            },
            Waiting::Ask { n, buttons, shown, .. } => match app.api_gate.picked {
                Some((pn, b)) if pn == *n => {
                    app.api_gate.picked = None;
                    app.api_gate.asked = None;
                    Some(Ok(json!({"button": buttons.get(b), "index": b})))
                }
                // Closed without a pick (Esc, or another window over it): no answer.
                _ if *shown && app.api_gate.asked == Some(*n) && app.prompt.is_none() => {
                    app.api_gate.asked = None;
                    Some(Ok(json!({"button": null})))
                }
                _ => None,
            },
            Waiting::Grant { client, tier, req, once, .. } => {
                match app.api_gate.check(&app.settings.api, client, *tier) {
                    _ if *once => {
                        later.push((req.clone(), client.clone(), reply.clone()));
                        return false;
                    }
                    Some(true) => {
                        later.push((req.clone(), client.clone(), reply.clone()));
                        return false;
                    }
                    Some(false) => Some(Err(format!("{} refused: the user said no", req["method"].as_str().unwrap_or("it")))),
                    None => None,
                }
            }
        };
        let done = done.or_else(|| (now >= w.until).then(|| Err(if matches!(w.what, Waiting::Grant { .. }) { "the user did not answer".to_string() } else { "timed out".to_string() })));
        // A question given up on while up: its window goes with it.
        if let (Some(Err(_)), Waiting::Ask { n, .. } | Waiting::Input { n, .. }) = (&done, &w.what) {
            if app.api_gate.asked == Some(*n) {
                app.api_gate.asked = None;
                app.prompt = None;
                app.gitui.cmds.picker = None;
            }
        }
        let keep = done.is_none();
        if let Some(r) = done {
            let _ = reply.send(envelope(&w.id, r));
        }
        keep
    });
    // Allowed now: done, as if they had just come.
    for (req, client, reply) in later {
        match respond(app, &req, &client, true) {
            Reply::Now(v) => drop(reply.send(v)),
            Reply::Later(w) => waits.push((*w, reply)),
            Reply::Watch(id, topics) => watch(app, id, topics, reply),
        }
        changed = true;
    }
    // A client's question, when no window is up.
    if !app.api_gate.asking && app.api_gate.asked.is_none() && app.prompt.is_none() {
        if let Some((w, _)) = waits.iter_mut().find(|(w, _)| matches!(w.what, Waiting::Ask { shown: false, .. })) {
            if let Waiting::Ask { n, who, text, buttons, shown } = &mut w.what {
                *shown = true;
                app.api_gate.asked = Some(*n);
                app.prompt = Some(crate::panel::Prompt {
                    text: vec![format!("{who} asks:"), text.clone()],
                    buttons: buttons.iter().enumerate().map(|(i, b)| (b.clone(), crate::panel::PAct::ApiPick(*n, i))).collect(),
                    sel: 0,
                });
                changed = true;
            }
        }
    }
    // A client's line to type or row to pick, when nothing else is up.
    if !app.api_gate.asking && app.api_gate.asked.is_none() && app.prompt.is_none() && app.gitui.cmds.picker.is_none() {
        if let Some((w, _)) = waits.iter_mut().find(|(w, _)| matches!(w.what, Waiting::Input { shown: false, .. })) {
            if let Waiting::Input { n, who, text, value, items, shown } = &mut w.what {
                *shown = true;
                app.api_gate.asked = Some(*n);
                let title = format!("{who}: {text}");
                match items {
                    Some(items) => {
                        let rows = items.iter().enumerate().map(|(i, (label, detail))| crate::gitcmds::Item { label: label.clone(), detail: detail.clone(), pick: crate::gitcmds::Pick::Api(*n, i) }).collect();
                        crate::gitcmds::open_picker(app, &title, rows, None);
                    }
                    None => {
                        crate::gitcmds::open_picker(app, &title, Vec::new(), Some(crate::gitcmds::Input::Api(*n)));
                        if let Some(p) = app.gitui.cmds.picker.as_mut() {
                            p.query = value.clone();
                        }
                    }
                }
                changed = true;
            }
        }
    }
    // The next client to ask about, when no prompt is up.
    if !app.api_gate.asking && app.prompt.is_none() {
        if let Some((w, _)) = waits.iter_mut().find(|(w, _)| matches!(w.what, Waiting::Grant { .. })) {
            if let Waiting::Grant { client, tier, req, shown, .. } = &mut w.what {
                *shown = true;
                app.api_gate.asking = true;
                app.prompt = Some(ask(client, *tier, req));
                changed = true;
            }
        }
    }
    app.api.as_mut().unwrap().waits = waits;
    notify(app);
    changed
}

/// The prompt asking the user whether `c` may do `t`.
fn ask(c: &Client, t: Tier, req: &Value) -> crate::panel::Prompt {
    use crate::panel::PAct;
    let what = match t {
        Tier::Edit => "open and change files",
        _ => "run commands and use the terminals (type in them, read them and the screen)",
    };
    let method = req["method"].as_str().unwrap_or_default();
    let mut detail = req["params"].to_string();
    if detail.chars().count() > 70 {
        detail = detail.chars().take(69).collect::<String>() + "…";
    }
    crate::panel::Prompt {
        text: vec![format!("{} asks to {what}.", c.label), format!("First: {method} {detail}"), "Allow lets it while it runs; Deny refuses it until termcode restarts.".into()],
        buttons: vec![("Allow".into(), PAct::Api(2)), ("Allow once".into(), PAct::Api(1)), ("Deny".into(), PAct::Api(0))],
        sel: 2,
    }
}

/// A request in: answered now (None), or its wait (the screen, output, the user).
fn admit(app: &mut App, req: &Value, client: &Client, reply: &Sender<Value>) -> Option<Wait> {
    match respond(app, req, client, false) {
        Reply::Now(v) => {
            let _ = reply.send(v);
            None
        }
        Reply::Later(w) => Some(*w),
        Reply::Watch(id, topics) => {
            watch(app, id, topics, reply.clone());
            None
        }
    }
}

/// A connection watching `topics`: told so, then each change (`notify`).
fn watch(app: &mut App, id: Value, topics: Vec<String>, reply: Sender<Value>) {
    let now = snapshot(app);
    let Some(srv) = app.api.as_mut() else { return };
    let _ = reply.send(envelope(&id, Ok(json!({"watching": if topics.is_empty() { crate::rpc::TOPICS.iter().map(|t| t.to_string()).collect() } else { topics.clone() }}))));
    srv.last.get_or_insert(now);
    srv.watchers.push((topics, reply));
}

/// The frame just drawn: the requests for the screen answered with its text.
pub fn drawn(app: &mut App, buf: &ratatui::buffer::Buffer) {
    let Some(srv) = app.api.as_mut() else { return };
    if !srv.waits.iter().any(|(w, _)| matches!(w.what, Waiting::Screen)) {
        return;
    }
    let text = screen_text(buf);
    srv.waits.retain(|(w, reply)| {
        if matches!(w.what, Waiting::Screen) {
            let _ = reply.send(envelope(&w.id, Ok(json!({"text": text, "cols": buf.area.width, "rows": buf.area.height}))));
            return false;
        }
        true
    });
}

/// Whether requests wait (the event loop then wakes often enough to time them out).
pub fn waiting(app: &App) -> bool {
    app.api.as_ref().is_some_and(|s| !s.waits.is_empty())
}

/// A frame as text, row by row: pictures (escapes, placeholders) as blanks.
pub fn screen_text(buf: &ratatui::buffer::Buffer) -> String {
    let w = usize::from(buf.area.width.max(1));
    buf.content
        .chunks(w)
        .map(|row| {
            let line: String = row.iter().map(|c| c.symbol()).map(|s| if s.contains('\x1b') || s.starts_with('\u{10EEEE}') { " " } else { s }).collect();
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tail(s: &str, n: usize) -> &str {
    let mut from = s.len().saturating_sub(n);
    while !s.is_char_boundary(from) {
        from += 1;
    }
    &s[from..]
}

/// The answer to `req` from `client`: `open` and `ping` as `termcode PATH` sends them;
/// anything with `"jsonrpc"` as JSON-RPC 2.0, once the gate lets it (`allowed`: it did).
pub fn respond(app: &mut App, req: &Value, client: &Client, allowed: bool) -> Reply {
    if req.get("jsonrpc").is_none() {
        return Reply::Now(handle(app, req));
    }
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req["method"].as_str().unwrap_or_default();
    // A method an extension serves: its tier as its manifest says.
    let served = crate::ext::api_owner(app, method);
    let tier = served.map_or_else(|| crate::rpc::tier(method, &req["params"]), |(_, t)| t);
    if !allowed {
        match app.api_gate.check(&app.settings.api, client, tier) {
            Some(true) => {}
            Some(false) if app.settings.api == "off" => return Reply::Now(envelope(&id, Err("the API is off (termcode.api in settings)".into()))),
            Some(false) if app.settings.api == "read-only" => return Reply::Now(envelope(&id, Err(format!("{method} refused: the API is read-only (termcode.api in settings)")))),
            Some(false) => return Reply::Now(envelope(&id, Err(format!("{method} refused: the user said no")))),
            // Headless: no one to ask; only what its starter allowed (TERMCODE_API_ALLOW).
            None if crate::panel::HEADLESS.load(Ordering::SeqCst) => {
                return Reply::Now(envelope(&id, Err(format!("{method} refused: a headless termcode has no one to ask; start it with TERMCODE_API_ALLOW=edit,run"))));
            }
            None => return Reply::Later(Box::new(Wait { id, until: std::time::Instant::now() + ASK, what: Waiting::Grant { client: client.clone(), tier, req: req.clone(), shown: false, once: false } })),
        }
    }
    // A pager's window: only for programs started in this termcode's terminals (git log
    // typed there), so nothing else can put a window over the user's work.
    if method == "pager.feed" && !client.own && !descends(client.key.0) {
        return Reply::Now(envelope(&id, Err("pager.feed: only from this termcode's terminals".into())));
    }
    if tier != Tier::Read && !client.own && client.ext.is_none() {
        app.msg = format!("API: {} — {method}", client.label);
    }
    // An extension's own panes.
    if method.starts_with("view.") {
        return Reply::Now(envelope(&id, crate::ext::call(app, client, method, &req["params"])));
    }
    // An extension's event, in a namespace it serves, to whoever watches it.
    if method == "events.emit" {
        let topic = req["params"]["topic"].as_str().unwrap_or_default().to_string();
        if !crate::ext::may_emit(app, client, &topic) {
            return Reply::Now(envelope(&id, Err(format!("events.emit: {topic:?} is not in a namespace this extension serves"))));
        }
        return Reply::Now(envelope(&id, Ok(json!({"to": emit(app, &topic, &req["params"])}))));
    }
    // Served by an extension: passed to it, its reply awaited.
    if let Some((i, _)) = served {
        let wait = req["params"]["timeout"].as_f64().unwrap_or(0.0).clamp(0.0, 3600.0) + 30.0;
        return match crate::ext::forward(app, i, method, &req["params"], &client.label) {
            Ok(key) => Reply::Later(Box::new(Wait { id, until: std::time::Instant::now() + Duration::from_secs_f64(wait), what: Waiting::Ext { key } })),
            Err(e) => Reply::Now(envelope(&id, Err(e))),
        };
    }
    match crate::rpc::call(app, method, &req["params"]) {
        Ok(crate::rpc::Out::Now(v)) => Reply::Now(envelope(&id, Ok(v))),
        Ok(crate::rpc::Out::Screen) => Reply::Later(Box::new(Wait { id, until: std::time::Instant::now() + Duration::from_secs(5), what: Waiting::Screen })),
        Ok(crate::rpc::Out::Output { term, re, mark, timeout }) => {
            Reply::Later(Box::new(Wait { id, until: std::time::Instant::now() + timeout, what: Waiting::Output { term, re, mark, seen: None } }))
        }
        Ok(crate::rpc::Out::Watch(topics)) => Reply::Watch(id, topics),
        Ok(crate::rpc::Out::Notify(text)) => {
            let who = client.label.split(" (pid").next().unwrap_or(&client.label).to_string();
            app.msg = format!("{who}: {text}");
            Reply::Now(envelope(&id, Ok(json!({"shown": true}))))
        }
        Ok(crate::rpc::Out::Input { text, value, items, timeout }) => {
            let n = app.api_gate.next_ask;
            app.api_gate.next_ask += 1;
            let who = client.label.split(" (pid").next().unwrap_or(&client.label).to_string();
            Reply::Later(Box::new(Wait { id, until: std::time::Instant::now() + timeout, what: Waiting::Input { n, who, text, value, items, shown: false } }))
        }
        Ok(crate::rpc::Out::Ask { text, buttons, timeout }) => {
            let n = app.api_gate.next_ask;
            app.api_gate.next_ask += 1;
            let who = client.label.split(" (pid").next().unwrap_or(&client.label).to_string();
            Reply::Later(Box::new(Wait { id, until: std::time::Instant::now() + timeout, what: Waiting::Ask { n, who, text, buttons, shown: false } }))
        }
        Err(e) => Reply::Now(envelope(&id, Err(e))),
    }
}

fn envelope(id: &Value, r: Result<Value, String>) -> Value {
    match r {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
        Err(e) => {
            let code = if e.starts_with("unknown method") { -32601 } else { -32000 };
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": e}})
        }
    }
}

/// `req` failed with `e`, in its own protocol.
fn failure(req: &Value, e: &str) -> Value {
    if req.get("jsonrpc").is_some() {
        envelope(&req.get("id").cloned().unwrap_or(Value::Null), Err(e.to_string()))
    } else {
        json!({"ok": false, "error": e})
    }
}

/// `termcode PATH`'s protocol: {"method": "open"|"ping", …} → {"ok": …}.
pub fn handle(app: &mut App, req: &Value) -> Value {
    match req["method"].as_str() {
        Some("ping") => json!({"ok": true, "root": app.root}),
        Some("open") => open(app, req),
        m => json!({"ok": false, "error": format!("unknown method {m:?}")}),
    }
}

/// `open`: {root, file?, line?, col?} as the CLI resolved them. This folder: the file
/// opened. Another: the switch to it, unless something would be lost or cut short.
fn open(app: &mut App, req: &Value) -> Value {
    let Some(root) = req["root"].as_str().map(PathBuf::from) else { return json!({"ok": false, "error": "no root"}) };
    let file = req["file"].as_str().map(PathBuf::from);
    let (line, col) = (req["line"].as_u64().unwrap_or(1), req["col"].as_u64().unwrap_or(1));
    // A file of this workspace opens here, whatever root it would have on its own (a
    // subfolder, out of git).
    if root == app.root || file.as_ref().is_some_and(|f| f.starts_with(&app.root)) {
        let Some(f) = file else { return json!({"ok": true, "note": "already open here"}) };
        app.request(&format!("open:{}:{line}:{col}", f.display()));
        return json!({"ok": true, "note": format!("opened {}", f.display())});
    }
    if app.tab {
        return json!({"ok": false, "tab": true}); // a herdr Code tab: that folder's own tab
    }
    // A git write (an index refresh just after opening a repository) takes a moment:
    // waited for, a few seconds at most, rather than refused.
    let since = std::time::Instant::now();
    while app.gitui.writes() > 0 && since.elapsed() < Duration::from_secs(3) {
        crate::scm::drain(app);
        std::thread::sleep(Duration::from_millis(20));
    }
    if let Some(why) = app.busy_for_switch() {
        return json!({"ok": false, "error": format!("{why}: termcode stays in {}", app.root.display())});
    }
    let arg = match &file {
        Some(f) => format!("{}:{line}:{col}", f.display()),
        None => root.display().to_string(),
    };
    app.reroot = Some(arg);
    (app.reload, app.quit) = (true, true);
    json!({"ok": true, "note": format!("switched to {}", root.display())})
}

/// What the CLI did with a forwarded `termcode PATH`.
#[derive(Debug, PartialEq)]
pub enum Forward {
    Done(String),
    /// The termcode there is a herdr tab: open PATH as a tab instead.
    Tab,
    Refused(String),
    /// No termcode answers there (a stale variable): start one here.
    Unreachable,
}

/// `termcode PATH` from a terminal of a termcode: PATH sent to it. Mid-switch (the
/// socket refusing, or closing unanswered, for the few milliseconds termcode takes to
/// start again in another folder) it is sent again, two seconds at most.
pub fn forward(sock: &Path, root: &Path, file: Option<&(PathBuf, usize, usize)>) -> Forward {
    let since = std::time::Instant::now();
    loop {
        match send_open(sock, root, file) {
            Some(f) => return f,
            None if sock.exists() && since.elapsed() < Duration::from_secs(2) => std::thread::sleep(Duration::from_millis(20)),
            None => return Forward::Unreachable,
        }
    }
}

/// One try: None when no termcode took it (none listening, or one gone mid-request).
fn send_open(sock: &Path, root: &Path, file: Option<&(PathBuf, usize, usize)>) -> Option<Forward> {
    let s = UnixStream::connect(sock).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    let mut req = json!({"method": "open", "root": root});
    if let Some((f, l, c)) = file {
        (req["file"], req["line"], req["col"]) = (json!(f), json!(l), json!(c));
    }
    (&s).write_all(format!("{req}\n").as_bytes()).ok()?;
    let mut line = String::new();
    match BufReader::new(&s).read_line(&mut line) {
        Ok(0) => return None, // closed unanswered: the termcode before a switch
        Ok(_) => {}
        Err(_) => return Some(Forward::Refused("termcode did not answer".into())),
    }
    let v: Value = serde_json::from_str(&line).unwrap_or_default();
    Some(match (v["ok"].as_bool(), v["tab"].as_bool()) {
        (Some(true), _) => Forward::Done(v["note"].as_str().unwrap_or("done").to_string()),
        (_, Some(true)) => Forward::Tab,
        _ => Forward::Refused(v["error"].as_str().unwrap_or("refused").to_string()),
    })
}

const CLI_HELP: &str = "usage: termcode api [--to PID|FOLDER] METHOD [JSON]
       termcode api ls        the running termcodes: pid and folder
       termcode api --skill   the guide for agents (SKILL.md)
Calls a running termcode's API (JSON-RPC; doc/api.md) and prints the result as JSON.
Which termcode: --to, else $TERMCODE_SOCKET (its own terminal), else the one open on
this folder (or above it), else the only one running.
  termcode api hello
  termcode api command.run '{\"id\": \"view.toggleMinimap\"}'
  termcode api state.get '{\"path\": \"/ui/screen\"}'";

/// `termcode api …`.
pub fn cli(args: &[String]) -> Result<(), String> {
    let mut to = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{CLI_HELP}");
                return Ok(());
            }
            "--skill" => {
                print!("{}", include_str!("../skills/termcode/SKILL.md"));
                return Ok(());
            }
            "--to" => to = Some(it.next().ok_or("--to: a pid or a folder")?.clone()),
            _ => rest.push(a.clone()),
        }
    }
    let running = running();
    let [method, more @ ..] = rest.as_slice() else { return Err(CLI_HELP.into()) };
    if method == "ls" {
        for (pid, root, _) in &running {
            println!("{pid}\t{}", root.display());
        }
        return Ok(());
    }
    let params: Value = match more {
        [] => json!({}),
        [j] => serde_json::from_str(j).map_err(|e| format!("not JSON: {e}"))?,
        _ => return Err(CLI_HELP.into()),
    };
    let sock = pick(&running, to.as_deref())?;
    let req = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    if method == "events.watch" {
        return watch_cli(&sock, &req);
    }
    let reply = rpc(&sock, &req)?;
    if let Some(e) = reply.get("error") {
        return Err(e["message"].as_str().unwrap_or("failed").to_string());
    }
    println!("{}", serde_json::to_string_pretty(&reply["result"]).unwrap_or_default());
    Ok(())
}

/// `termcode api events.watch`: the events, a JSON line each, until termcode goes or
/// the command is stopped.
fn watch_cli(sock: &Path, req: &Value) -> Result<(), String> {
    let s = UnixStream::connect(sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    (&s).write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut out = std::io::stdout();
    for line in BufReader::new(&s).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&line).unwrap_or_default();
        if let Some(e) = v.get("error") {
            return Err(e["message"].as_str().unwrap_or("failed").to_string());
        }
        let shown = if v.get("result").is_some() { v["result"].clone() } else { v["params"].clone() };
        if writeln!(out, "{shown}").and_then(|()| out.flush()).is_err() {
            return Ok(()); // the reader went away (| head)
        }
    }
    Ok(())
}

/// One request over `sock`, its reply.
pub fn rpc(sock: &Path, req: &Value) -> Result<Value, String> {
    let s = UnixStream::connect(sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    let secs = req["params"]["timeout"].as_f64().unwrap_or(0.0).clamp(0.0, 3600.0) + 15.0;
    let _ = s.set_read_timeout(Some(Duration::from_secs_f64(secs)));
    (&s).write_all(format!("{req}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).map_err(|e| format!("no answer: {e}"))?;
    serde_json::from_str(&line).map_err(|e| format!("bad answer: {e}"))
}

/// The termcodes running (that answer): pid, folder, socket.
pub(crate) fn running() -> Vec<(u32, PathBuf, PathBuf)> {
    let mut out: Vec<_> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.strip_suffix(".sock")?.parse::<u32>().ok()?;
            let hello = rpc(&e.path(), &json!({"jsonrpc": "2.0", "id": 0, "method": "hello", "params": {"timeout": 0}})).ok()?;
            Some((pid, PathBuf::from(hello["result"]["root"].as_str()?), e.path()))
        })
        .collect();
    out.sort();
    out
}

/// The socket to talk to (see CLI_HELP).
pub(crate) fn pick(running: &[(u32, PathBuf, PathBuf)], to: Option<&str>) -> Result<PathBuf, String> {
    let list = || running.iter().map(|(pid, root, _)| format!("  {pid}  {}", root.display())).collect::<Vec<_>>().join("\n");
    let within = |dir: &Path| running.iter().filter(|(_, root, _)| dir.starts_with(root)).max_by_key(|(_, root, _)| root.as_os_str().len()).map(|r| r.2.clone());
    if let Some(to) = to {
        if let Ok(pid) = to.parse::<u32>() {
            return running.iter().find(|r| r.0 == pid).map(|r| r.2.clone()).ok_or_else(|| format!("no termcode {pid}; running:\n{}", list()));
        }
        let dir = fs::canonicalize(to).map_err(|e| format!("{to}: {e}"))?;
        return within(&dir).ok_or_else(|| format!("no termcode open on {to}; running:\n{}", list()));
    }
    if let Some(s) = std::env::var_os(ENV).filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(s));
    }
    if let Some(s) = std::env::current_dir().ok().and_then(|d| within(&d)) {
        return Ok(s);
    }
    match running {
        [one] => Ok(one.2.clone()),
        [] => Err("no termcode is running".into()),
        _ => Err(format!("which termcode? --to PID|FOLDER; running:\n{}", list())),
    }
}

/// The folder the sockets live in: the user's runtime directory, else termcode's state.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_absolute()).map_or_else(|| crate::data_dir(true).join("run"), |d| d.join("termcode"))
}

/// The socket goes with the process (a reload binds it again).
pub fn remove() {
    if let Some(p) = PATH.get() {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn procargs_give_argv_without_the_executable_path_or_environment() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/local/bin/termcode\0\0\0\0termcode\0api\0a b\0HOME=/h\0");
        assert_eq!(parse_procargs(&buf), ["termcode", "api", "a b"]);
        assert!(parse_procargs(&buf[..2]).is_empty());
        #[cfg(target_os = "macos")]
        assert_eq!(cmdline(std::process::id() as i32).first(), std::env::args().next().as_ref());
    }

    #[test]
    fn socket_peer_identity_and_process_liveness_are_preserved() {
        let dir = std::env::temp_dir().join(format!("termcode-peer-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("peer.sock")).unwrap();
        let _client = UnixStream::connect(dir.join("peer.sock")).unwrap();
        let (server, _) = listener.accept().unwrap();
        assert_eq!(peer(&server), Some((std::process::id() as i32, unsafe { libc::geteuid() })));
        assert!(process_alive(std::process::id()));
        assert!(process_alive(1));
        assert!(!process_alive(u32::MAX));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_watching_connection_gets_each_change_as_an_event() {
        let dir = std::env::temp_dir().join(format!("termcode-api-watch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("a.txt"), "hello\n").unwrap();
        fs::write(dir.join("b.txt"), "other\n").unwrap();
        let mut app = App::new(fs::canonicalize(&dir).unwrap(), None);
        app.open_file(0, "a.txt", false);
        app.api = start(&dir.join("run"));
        let sock = dir.join(format!("run/{}.sock", std::process::id()));
        let s = UnixStream::connect(&sock).unwrap();
        (&s).write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"events.watch\",\"params\":{\"topics\":[\"buffer.changed\",\"file.saved\"]}}\n").unwrap();
        let mut lines = BufReader::new(s.try_clone().unwrap());
        lines.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut next = |app: &mut App| -> Value {
            for _ in 0..300 {
                tick(app);
                let mut line = String::new();
                if lines.read_line(&mut line).is_ok_and(|n| n > 0) {
                    return serde_json::from_str(&line).unwrap();
                }
            }
            panic!("nothing came");
        };
        let first = next(&mut app);
        assert_eq!(first["result"]["watching"], json!(["buffer.changed", "file.saved"]), "{first}");
        // An edit, then a save: an event each, on the same connection.
        crate::rpc::call(&mut app, "buffer.edit", &json!({"path": "a.txt", "changes": [{"start": {"line": 1}, "text": "say "}]})).ok();
        let ev = next(&mut app);
        assert_eq!((ev["method"].as_str(), ev["params"]["topic"].as_str(), ev["params"]["path"].as_str()), (Some("event"), Some("buffer.changed"), Some("a.txt")), "{ev}");
        app.save_doc("file:a.txt").unwrap();
        let ev = next(&mut app);
        assert_eq!(ev["params"]["topic"], "file.saved", "{ev}");
        // An edit on a file not open yet: changed too (not only opened).
        crate::rpc::call(&mut app, "buffer.edit", &json!({"path": "b.txt", "changes": [{"start": {"line": 1}, "text": "x"}]})).ok();
        let ev = next(&mut app);
        assert_eq!((ev["params"]["topic"].as_str(), ev["params"]["path"].as_str()), (Some("buffer.changed"), Some("b.txt")), "{ev}");
        // An unknown topic is refused.
        assert!(crate::rpc::call(&mut app, "events.watch", &json!({"topics": ["nope"]})).is_err());
        drop(app);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clients_notify_ask_the_user_and_mark_lines() {
        let dir = std::env::temp_dir().join(format!("termcode-api-ui-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("a.c"), "int main() {\n    return 0;\n}\n").unwrap();
        let mut app = App::new(fs::canonicalize(&dir).unwrap(), None);
        app.api = start(&dir.join("run"));
        let sock = dir.join(format!("run/{}.sock", std::process::id()));
        let ask = |app: &mut App, req: &str, pick: Option<usize>| -> Value {
            let s = UnixStream::connect(&sock).unwrap();
            (&s).write_all(format!("{req}\n").as_bytes()).unwrap();
            let mut lines = BufReader::new(s.try_clone().unwrap());
            lines.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
            for _ in 0..300 {
                tick(app);
                // The user's click on the question's window.
                if let (Some(b), Some(p)) = (pick, app.prompt.as_ref()) {
                    if let Some(crate::panel::PAct::ApiPick(n, i)) = p.buttons.get(b).map(|x| x.1.clone()) {
                        app.prompt = None;
                        app.api_gate.picked = Some((n, i));
                    }
                }
                let mut line = String::new();
                if lines.read_line(&mut line).is_ok_and(|n| n > 0) {
                    return serde_json::from_str(&line).unwrap();
                }
            }
            panic!("no reply to {req}");
        };
        // notify: on the status bar, with who said it.
        let r = ask(&mut app, r#"{"jsonrpc":"2.0","id":1,"method":"ui.notify","params":{"text":"tests passed"}}"#, None);
        assert_eq!(r["result"]["shown"], true);
        assert!(app.msg.ends_with(": tests passed"), "{}", app.msg);
        // prompt: a window with the buttons; the one clicked is the answer.
        let r = ask(&mut app, r#"{"jsonrpc":"2.0","id":2,"method":"ui.prompt","params":{"text":"Flash the board?","buttons":["Flash","Not now"]}}"#, Some(1));
        assert_eq!((r["result"]["button"].as_str(), r["result"]["index"].as_u64()), (Some("Not now"), Some(1)), "{r}");
        // Closed with Esc: no answer.
        let s = UnixStream::connect(&sock).unwrap();
        (&s).write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ui.prompt\",\"params\":{\"text\":\"Sure?\"}}\n").unwrap();
        for _ in 0..50 {
            tick(&mut app);
            if app.prompt.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.prompt.as_ref().is_some_and(|p| p.text.iter().any(|t| t == "Sure?")));
        app.prompt = None; // Esc
        let mut lines = BufReader::new(s);
        lines.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut line = String::new();
        for _ in 0..100 {
            tick(&mut app);
            if lines.read_line(&mut line).is_ok_and(|n| n > 0) {
                break;
            }
        }
        assert!(line.contains("\"button\":null"), "{line}");
        // decorate: marks on lines, as problems (underline, hover, scrollbar); clear.
        let r = crate::rpc::call(&mut app, "ui.decorate", &json!({"path": "a.c", "marks": [{"line": 2, "severity": "warning", "note": "always 0"}]}));
        assert!(r.is_ok());
        let marks = app.lints.all("file:a.c");
        assert_eq!(marks.len(), 1);
        assert_eq!((marks[0].0, marks[0].1, marks[0].3, marks[0].4), (1, 4, crate::lint::Sev::Warning, "always 0"), "the line's text, past its indent");
        assert_eq!(app.lints.at("file:a.c", 1, 6).map(|h| h.1[0].1.clone()).as_deref(), Some("always 0"), "its note on hover");
        assert!(crate::rpc::call(&mut app, "ui.clear", &json!({})).is_ok());
        assert!(app.lints.all("file:a.c").is_empty());
        drop(app);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_gate_reads_freely_asks_before_changes_and_keeps_the_answers() {
        let agent = Client { key: (42, 7), label: "agent (pid 42)".into(), ..Client::default() };
        let mut g = Gate::default();
        assert_eq!(g.check("ask", &agent, Tier::Read), Some(true));
        assert_eq!(g.check("ask", &agent, Tier::Edit), None, "asked");
        g.answers.insert(((42, 7), Tier::Edit), true);
        g.answers.insert(((42, 7), Tier::Run), false);
        assert_eq!(g.check("ask", &agent, Tier::Edit), Some(true));
        assert_eq!(g.check("ask", &agent, Tier::Run), Some(false), "a no stays");
        let same_pid_later = Client { key: (42, 8), ..agent.clone() };
        assert_eq!(g.check("ask", &same_pid_later, Tier::Edit), None, "another process with that pid: asked again");
        assert_eq!(g.check("read-only", &agent, Tier::Edit), Some(false));
        assert_eq!(g.check("read-only", &agent, Tier::Read), Some(true));
        assert_eq!(g.check("off", &agent, Tier::Read), Some(false));
        let own = Client { key: (1, 0), label: String::new(), own: true, ..Client::default() };
        assert_eq!(g.check("ask", &own, Tier::Run), Some(true), "the user's own terminal");
        assert_eq!(g.check("off", &own, Tier::Read), Some(false));
        let g = Gate { allowed: vec![Tier::Edit], ..Gate::default() };
        assert_eq!((g.check("ask", &agent, Tier::Edit), g.check("ask", &agent, Tier::Run)), (Some(true), None));
    }

    #[test]
    fn a_client_is_the_process_behind_shells_and_termcode_api() {
        assert!(client_of(std::process::id() as i32).own, "this termcode itself");
        // A shell this process started: its parent (here, this termcode) is behind it.
        let mut sh = std::process::Command::new("sh").args(["-c", "sleep 0.5; true"]).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(client_of(sh.id() as i32).own, "a shell of termcode's own");
        let _ = sh.wait();
        let mut other = std::process::Command::new("sleep").arg("0.5").spawn().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let c = client_of(other.id() as i32);
        assert!(!c.own && c.label.starts_with("sleep 0.5 (pid "), "{c:?}");
        let _ = other.wait(); // (both end by themselves: termcode sends no signals)
    }

    #[test]
    fn a_forwarded_open_reaches_the_event_loop_and_its_answer_comes_back() {
        let dir = std::env::temp_dir().join(format!("termcode-api-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("run/999999999.sock"), "").unwrap(); // a termcode long gone
        fs::write(dir.join("run/1.sock"), "").unwrap(); // pid 1 runs: not ours to judge
        let srv = start(&dir.join("run")).expect("a socket");
        assert!(!dir.join("run/999999999.sock").exists() && dir.join("run/1.sock").exists(), "only the dead's");
        let sock = dir.join(format!("run/{}.sock", std::process::id()));
        assert_eq!(fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(dir.join("run")).unwrap().permissions().mode() & 0o777, 0o700);
        // The event loop's side: answer what comes.
        let loop_side = std::thread::spawn(move || {
            let (req, reply, _) = srv.rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!((req["method"].as_str(), req["file"].as_str(), req["line"].as_u64()), (Some("open"), Some("/w/a.c"), Some(7)));
            reply.send(json!({"ok": true, "note": "opened /w/a.c"})).unwrap();
        });
        let got = forward(&sock, Path::new("/w"), Some(&(PathBuf::from("/w/a.c"), 7, 1)));
        loop_side.join().unwrap();
        assert_eq!(got, Forward::Done("opened /w/a.c".into()));
        assert_eq!(forward(&dir.join("none.sock"), Path::new("/w"), None), Forward::Unreachable, "a stale variable: start one here");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_opens_here_switches_folders_and_refuses_what_would_be_lost() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = std::env::temp_dir().join(format!("termcode-api-open-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (here, there) = (dir.join("here"), dir.join("there"));
        fs::create_dir_all(&here).unwrap();
        fs::create_dir_all(&there).unwrap();
        fs::write(here.join("a.c"), "int a;\n").unwrap();
        let mut app = App::new(fs::canonicalize(&here).unwrap(), None);
        let root = app.root.clone();
        let req = |root: &Path, file: Option<&Path>| {
            let mut v = json!({"method": "open", "root": root, "line": 1, "col": 1});
            if let Some(f) = file {
                v["file"] = json!(f);
            }
            v
        };
        // This folder: the file opens in place.
        let r = handle(&mut app, &req(&root, Some(&root.join("a.c"))));
        assert_eq!(r["ok"], true);
        assert!(app.groups[app.ag].tabs.iter().any(|t| t.key == "file:a.c"));
        assert_eq!(handle(&mut app, &req(&root, None))["note"], "already open here");
        // A file of this workspace whose own root would be a subfolder: here too.
        fs::create_dir_all(root.join("main")).unwrap();
        fs::write(root.join("main/b.c"), "int b;\n").unwrap();
        assert_eq!(handle(&mut app, &req(&root.join("main"), Some(&root.join("main/b.c"))))["ok"], true);
        assert!(app.reroot.is_none() && app.groups[app.ag].tabs.iter().any(|t| t.key == "file:main/b.c"));
        // Another folder with an unsaved file: refused, termcode stays.
        app.focus = crate::panel::Focus::Editor;
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let r = handle(&mut app, &req(&there, None));
        assert!(r["error"].as_str().unwrap().contains("1 unsaved file: save it first") && app.reroot.is_none() && !app.quit, "{r}");
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        // Saved: the switch, as a launch there.
        let r = handle(&mut app, &req(&there, None));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!((app.reroot.as_deref(), app.reload, app.quit), (Some(there.to_str().unwrap()), true, true));
        // A herdr Code tab: that folder's own tab instead.
        let mut tab = App::new(fs::canonicalize(&here).unwrap(), None);
        tab.tab = true;
        assert_eq!(handle(&mut tab, &req(&there, None))["tab"], true);
        assert_eq!(handle(&mut tab, &json!({"method": "bogus"}))["ok"], false);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_socket_folder_others_can_enter_is_refused() {
        let dir = std::env::temp_dir().join(format!("termcode-api-shared-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(start(&dir).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }
}
