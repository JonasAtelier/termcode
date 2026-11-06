//! Git backend for the VS Code-style views: History pages (with `graph`),
//! commit details and hover, the status bar sync state, refs, stash, the
//! commit dialog, Undo Last Commit, remote and branch operations, hunk
//! stage/unstage, blame, merge conflicts and the file Timeline.
//!
//! Every call here:
//! - goes through `git::cmd()`, so pathspecs are literal (`*.c` is a file name);
//! - is argv only, with `--` before paths; a revision starting with `-` is refused;
//! - runs with `LC_ALL=C`, and parses `-z` or `\x1f`-separated output;
//! - caps stdout. Past the cap the pipe is closed and the child is reaped on a
//!   background thread. Nothing here ever sends a signal;
//! - can never open an editor or a terminal prompt. Read-only calls set
//!   `GIT_OPTIONAL_LOCKS=0`; network calls cannot stall forever (`Mode::Net`).
//!
//! All functions block, so call them off the UI thread. Errors are git's own
//! message (hint lines dropped), ready to show. Operations marked
//! **DESTRUCTIVE** need a confirmation in the UI first.

use crate::git;
use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type Res<T> = Result<T, String>;

/// History page size: VS Code shows 100 commits, then "View more".
pub const PAGE: usize = 100;
/// Largest file or blob kept, as in the editor.
const CAP: usize = 1 << 20;
/// Outputs with many records (history pages, big commits, refs).
const LIST_CAP: usize = 16 << 20;
const ERR_CAP: u64 = 64 << 10;
/// ssh for network calls when the user has not chosen one: no password or
/// host-key prompt, and a dead connection is dropped within about 30 s.
const SSH: &str = "ssh -o BatchMode=yes -o ConnectTimeout=15 -o ServerAliveInterval=15 -o ServerAliveCountMax=2";
/// One log record. `\x1e` starts it so `--name-status` fields can follow; the
/// raw message is last because it may contain anything but NUL.
const LOG_FORMAT: &str = "--format=%x1e%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%ct%x1f%D%x1f%B";

// ---------------------------------------------------------------- runner

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Read-only: no optional locks, so a status refresh never fights the user's git.
    Read,
    /// Changes the index, refs or working tree.
    Write,
    /// Talks to a remote.
    Net,
}

fn git(root: &Path, mode: Mode) -> Command {
    let mut c = git::cmd(root);
    // A user's color.ui=always or log.showSignature would garble parsed output.
    c.args(["-c", "color.ui=false", "-c", "log.showSignature=false"]);
    c.env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0").env("GIT_EDITOR", "true");
    if mode != Mode::Read {
        // Its own session, so no controlling tty: a user's ssh, a hook or a credential
        // helper that opens /dev/tty fails at once instead of drawing over the TUI and
        // waiting. setsid only detaches; it sends no signal.
        // SAFETY: setsid is async-signal-safe and the closure touches nothing else.
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    match mode {
        Mode::Read => {
            c.env("GIT_OPTIONAL_LOCKS", "0");
        }
        Mode::Write => {}
        Mode::Net => {
            // HTTP gives up below 1 KB/s for 30 s.
            c.args(["-c", "http.lowSpeedLimit=1000", "-c", "http.lowSpeedTime=30"]);
            if !user_ssh(root) {
                c.env("GIT_SSH_COMMAND", SSH);
            }
        }
    }
    c
}

/// The user chose an ssh command (GIT_SSH_COMMAND, GIT_SSH or core.sshCommand): keep it.
fn user_ssh(root: &Path) -> bool {
    let set = |k| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    set("GIT_SSH_COMMAND")
        || set("GIT_SSH")
        || read(root, &["config", "--get", "core.sshCommand"]).is_ok_and(|o| !o.text().trim().is_empty())
}

struct Out {
    stdout: Vec<u8>,
    /// stdout reached the cap and the rest was dropped.
    truncated: bool,
}

impl Out {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

fn call(root: &Path, mode: Mode, args: &[&str], input: Option<Vec<u8>>, cap: usize) -> Res<Out> {
    let mut c = git(root, mode);
    c.args(args);
    exec(c, args, input, cap)
}

/// One git call, for the Git Output view (VS Code's Git output channel).
#[derive(Clone, Debug)]
pub struct LogEntry {
    /// Unix seconds and milliseconds when it started.
    pub when: (i64, u32),
    /// The command after `git` and its global options.
    pub args: String,
    /// Exit code; None if it was abandoned (cancelled, or output past the cap).
    pub code: Option<i32>,
    pub ms: u128,
    pub stderr: String,
}

const LOG_MAX: usize = 2000;
static LOG: Mutex<VecDeque<LogEntry>> = Mutex::new(VecDeque::new());
static LOG_GEN: AtomicU64 = AtomicU64::new(0);

/// The Git Output log: a generation that changes with every call, and the entries.
pub fn output() -> (u64, Vec<LogEntry>) {
    let log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    (LOG_GEN.load(Ordering::SeqCst), log.iter().cloned().collect())
}

pub fn output_gen() -> u64 {
    LOG_GEN.load(Ordering::SeqCst)
}

fn log(e: LogEntry) {
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if log.len() >= LOG_MAX {
        log.pop_front();
    }
    log.push_back(e);
    LOG_GEN.fetch_add(1, Ordering::SeqCst);
}

thread_local! {
    /// The cancel switch of the operation running on this thread (`with_cancel`).
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Run `f` with `flag` as the cancel switch of every git call it makes on this
/// thread. Setting the flag closes the running call's pipes (git then fails its
/// next write) and makes the call return Err("Cancelled"); nothing is signalled.
pub fn with_cancel<T>(flag: Arc<AtomicBool>, f: impl FnOnce() -> T) -> T {
    CANCEL.set(Some(flag));
    let r = f();
    CANCEL.set(None);
    r
}

/// Run `c` with `input` on stdin; Ok(stdout up to `cap`) or Err(git's message).
/// Both pipes are polled, so a cancel is seen within 100 ms even while git is silent.
fn exec(mut c: Command, args: &[&str], input: Option<Vec<u8>>, cap: usize) -> Res<Out> {
    let start = Instant::now();
    let when = SystemTime::now().duration_since(UNIX_EPOCH).map_or((0, 0), |d| (d.as_secs() as i64, d.subsec_millis()));
    let entry = |code: Option<i32>, stderr: &[u8]| LogEntry {
        when,
        args: args.join(" "),
        code,
        ms: start.elapsed().as_millis(),
        stderr: String::from_utf8_lossy(stderr).lines().take(20).collect::<Vec<_>>().join("\n"),
    };
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("git: {e}"))?;
    if let (Some(mut w), Some(data)) = (child.stdin.take(), input) {
        // Its own thread: git may write output before it has read all its input.
        thread::spawn(move || {
            let _ = w.write_all(&data);
        });
    }
    let cancel = CANCEL.with(|c| c.borrow().clone());
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut buf = vec![0u8; 64 << 10];
    let (mut truncated, mut cancelled) = (false, false);
    while out.is_some() || err.is_some() {
        if cancel.as_ref().is_some_and(|f| f.load(Ordering::SeqCst)) {
            cancelled = true;
            break;
        }
        let fds: Vec<i32> = [out.as_ref().map(|o| o.as_raw_fd()), err.as_ref().map(|e| e.as_raw_fd())].into_iter().flatten().collect();
        let mut polls: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
        // SAFETY: every fd belongs to a pipe end owned by `out` / `err` above.
        let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 100) };
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
        for p in polls.iter().filter(|p| p.revents != 0) {
            let is_out = out.as_ref().is_some_and(|o| o.as_raw_fd() == p.fd);
            let got = if is_out { out.as_mut().map(|o| o.read(&mut buf)) } else { err.as_mut().map(|e| e.read(&mut buf)) };
            match got {
                Some(Ok(0)) | Some(Err(_)) | None => {
                    if is_out {
                        out = None;
                    } else {
                        err = None;
                    }
                }
                Some(Ok(k)) if is_out => {
                    stdout.extend_from_slice(&buf[..k]);
                    if stdout.len() > cap {
                        stdout.truncate(cap);
                        truncated = true;
                    }
                }
                // Past ERR_CAP stderr is read and dropped, so a chatty git never blocks.
                Some(Ok(k)) => stderr.extend_from_slice(&buf[..k.min((ERR_CAP as usize).saturating_sub(stderr.len()))]),
            }
        }
        if truncated {
            break;
        }
    }
    if truncated || cancelled {
        // Our pipe ends close here, so git fails its next write (EPIPE) and exits; reap it elsewhere.
        drop((out, err));
        thread::spawn(move || {
            let _ = child.wait();
        });
        log(entry(None, if cancelled { b"cancelled" } else { b"output past the cap: abandoned" }));
        return if cancelled { Err("Cancelled".into()) } else { Ok(Out { stdout, truncated: true }) };
    }
    let status = child.wait().map_err(|e| format!("git: {e}"))?;
    log(entry(status.code(), &stderr));
    if status.success() {
        return Ok(Out { stdout, truncated: false });
    }
    Err(message(&stderr, &stdout))
}

/// git's error text for the UI: stderr, then stdout (where `commit` and
/// `merge` put "nothing to commit" and "CONFLICT"), without hint lines.
fn message(stderr: &[u8], stdout: &[u8]) -> String {
    let (e, o) = (String::from_utf8_lossy(stderr), String::from_utf8_lossy(stdout));
    let lines: Vec<&str> = e
        .lines()
        .chain(o.lines())
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.starts_with("hint:"))
        .take(10)
        .collect();
    if lines.is_empty() {
        "git failed".into()
    } else {
        lines.join("\n")
    }
}

fn read(root: &Path, args: &[&str]) -> Res<Out> {
    call(root, Mode::Read, args, None, CAP)
}

fn list(root: &Path, args: &[&str]) -> Res<Out> {
    call(root, Mode::Read, args, None, LIST_CAP)
}

fn write(root: &Path, args: &[&str]) -> Res<()> {
    call(root, Mode::Write, args, None, CAP).map(drop)
}

fn net(root: &Path, args: &[&str]) -> Res<()> {
    call(root, Mode::Net, args, None, CAP).map(drop)
}

/// A revision or ref name from the UI; one starting with `-` would be read as an option.
fn rev(r: &str) -> Res<&str> {
    if r.is_empty() || r.starts_with('-') {
        Err(format!("invalid revision {r:?}"))
    } else {
        Ok(r)
    }
}

// ---------------------------------------------------------------- history

/// A `%D` decoration, as VS Code's graph labels it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefLabel {
    /// `HEAD -> branch`: the checked-out branch.
    Current(String),
    /// A detached HEAD points here.
    Head,
    Branch(String),
    /// For example "origin/main". `origin/HEAD` is left out, as in VS Code.
    Remote(String),
    Tag(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    /// First parent first; empty for a root commit.
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    /// Unix seconds.
    pub author_time: i64,
    pub commit_time: i64,
    /// First line of the message.
    pub subject: String,
    /// The rest of the message, without the blank line after the subject.
    pub body: String,
    pub refs: Vec<RefLabel>,
}

/// One page of a paged list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// More follow: show "View more" and ask for `skip + items.len()` next.
    pub more: bool,
}

fn parse_refs(d: &str) -> Vec<RefLabel> {
    d.split(", ")
        .filter_map(|r| {
            if let Some(b) = r.strip_prefix("HEAD -> refs/heads/") {
                Some(RefLabel::Current(b.into()))
            } else if r == "HEAD" {
                Some(RefLabel::Head)
            } else if let Some(t) = r.strip_prefix("tag: refs/tags/") {
                Some(RefLabel::Tag(t.into()))
            } else if let Some(b) = r.strip_prefix("refs/heads/") {
                Some(RefLabel::Branch(b.into()))
            } else {
                r.strip_prefix("refs/remotes/").filter(|b| !b.ends_with("/HEAD")).map(|b| RefLabel::Remote(b.into()))
            }
        })
        .collect()
}

fn parse_commit(rec: &str) -> Option<Commit> {
    let f: Vec<&str> = rec.splitn(9, '\x1f').collect();
    let [hash, short, parents, name, email, at, ct, refs, msg] = f[..] else { return None };
    let (subject, body) = msg.trim_end().split_once('\n').unwrap_or((msg.trim_end(), ""));
    Some(Commit {
        hash: hash.into(),
        short: short.into(),
        parents: parents.split_whitespace().map(String::from).collect(),
        author_name: name.into(),
        author_email: email.into(),
        author_time: at.parse().unwrap_or(0),
        commit_time: ct.parse().unwrap_or(0),
        subject: subject.trim_end().into(),
        body: body.trim_start_matches(['\r', '\n']).into(),
        refs: parse_refs(refs),
    })
}

/// Parse `git log -z LOG_FORMAT [--name-status]`: each commit with the first
/// (status, path) of its name-status. A truncated read loses its last record.
fn parse_log(out: &Out) -> Vec<(Commit, Option<(char, String)>)> {
    let mut v: Vec<(Commit, Option<(char, String)>)> = Vec::new();
    let mut fields = out.stdout.split(|&b| b == 0).map(String::from_utf8_lossy);
    while let Some(f) = fields.next() {
        if let Some(rec) = f.strip_prefix('\x1e') {
            v.extend(parse_commit(rec).map(|c| (c, None)));
        } else if let Some(st) = f.trim_start_matches('\n').chars().next() {
            let mut path = fields.next();
            if matches!(st, 'R' | 'C') {
                path = fields.next();
            }
            if let (Some(last), Some(p)) = (v.last_mut(), path) {
                last.1.get_or_insert((st, p.into_owned()));
            }
        }
    }
    if out.truncated {
        v.pop();
    }
    v
}

/// One page of HEAD's history in `--topo-order`, newest first:
/// `log_page(repo, 0, PAGE)`, then `skip + items.len()` while `more`. Lay the
/// pages out with one `graph::Lanes`. An unborn branch has no history.
pub fn log_page(repo: &Path, skip: usize, limit: usize) -> Res<Page<Commit>> {
    let (s, n) = (format!("--skip={skip}"), format!("--max-count={}", limit + 1));
    let out = match list(repo, &["log", "-z", "--topo-order", "--decorate=full", LOG_FORMAT, &s, &n, "HEAD", "--"]) {
        Ok(o) => o,
        Err(_) if !git::has_head(repo) => return Ok(Page { items: Vec::new(), more: false }),
        Err(e) => return Err(e),
    };
    let mut items: Vec<Commit> = parse_log(&out).into_iter().map(|(c, _)| c).collect();
    let more = items.len() > limit || out.truncated;
    items.truncate(limit);
    Ok(Page { items, more })
}

/// A Timeline entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileCommit {
    pub commit: Commit,
    /// The file's path in this commit; before a rename it is the old name.
    pub path: String,
    /// A M D R C T, or ' ' when git listed no change (a merge).
    pub status: char,
}

/// Timeline: the commits that touched `path`, following renames, newest first.
pub fn file_history(repo: &Path, path: &str, skip: usize, limit: usize) -> Res<Page<FileCommit>> {
    // No --skip: it skips the rename commit too, and then --follow never
    // switches to the old name. Read the skipped part and drop it here.
    // ponytail: page N re-reads pages 0..N; fine for a file's history.
    let n = format!("--max-count={}", skip + limit + 1);
    let args = ["log", "-z", "--follow", "--name-status", "--decorate=full", LOG_FORMAT, &n, "HEAD", "--", path];
    let out = match list(repo, &args) {
        Ok(o) => o,
        Err(_) if !git::has_head(repo) => return Ok(Page { items: Vec::new(), more: false }),
        Err(e) => return Err(e),
    };
    let mut last = path.to_string();
    let mut items: Vec<FileCommit> = parse_log(&out)
        .into_iter()
        .map(|(commit, f)| {
            let (status, p) = f.unwrap_or((' ', last.clone()));
            last.clone_from(&p);
            FileCommit { commit, path: p, status }
        })
        .collect();
    items.drain(..skip.min(items.len()));
    let more = items.len() > limit || out.truncated;
    items.truncate(limit);
    Ok(Page { items, more })
}

// ---------------------------------------------------------------- commit details

/// A file row in commit details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    /// A M D R T (C only if git reports a copy).
    pub status: char,
    pub path: String,
    /// The source of a rename or copy.
    pub orig_path: Option<String>,
    /// Lines added and deleted; None for a binary file.
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

fn parents(repo: &Path, r: &str) -> Res<Vec<String>> {
    let t = read(repo, &["rev-list", "--parents", "-n1", rev(r)?, "--"])?.text();
    let mut it = t.split_whitespace();
    it.next().ok_or("no such commit")?;
    Ok(it.map(String::from).collect())
}

/// The files `hash` changed against its first parent (VS Code shows a merge
/// that way too); a root commit lists every file as added.
pub fn commit_files(repo: &Path, hash: &str) -> Res<Vec<FileChange>> {
    let p = parents(repo, hash)?;
    diff_files(repo, hash, p.first().map(String::as_str))
}

fn diff_files(repo: &Path, hash: &str, parent: Option<&str>) -> Res<Vec<FileChange>> {
    let mut args = vec!["diff-tree", "-r", "-z", "-M", "--raw", "--numstat", "--no-commit-id"];
    match parent {
        Some(p) => args.extend([p, hash]),
        None => args.extend(["--root", hash]),
    }
    args.push("--");
    Ok(parse_raw_numstat(&list(repo, &args)?.stdout))
}

/// `diff-tree -z --raw --numstat`: all raw records (`:modes shas STATUS\0path\0`,
/// two paths for R/C), then numstat records in the same order
/// (`add\tdel\tpath\0`, or `add\tdel\t\0from\0to\0` for a rename).
fn parse_raw_numstat(out: &[u8]) -> Vec<FileChange> {
    let mut fields = out.split(|&b| b == 0).map(|f| String::from_utf8_lossy(f).into_owned()).peekable();
    let mut files = Vec::new();
    while let Some(head) = fields.next_if(|f| f.starts_with(':')) {
        let status = head.rsplit(' ').next().and_then(|s| s.chars().next()).unwrap_or('M');
        let a = fields.next().unwrap_or_default();
        let (path, orig_path) = if matches!(status, 'R' | 'C') { (fields.next().unwrap_or_default(), Some(a)) } else { (a, None) };
        files.push(FileChange { status, path, orig_path, additions: None, deletions: None });
    }
    for f in &mut files {
        let Some(n) = fields.next() else { break };
        let mut it = n.splitn(3, '\t');
        f.additions = it.next().and_then(|s| s.parse().ok());
        f.deletions = it.next().and_then(|s| s.parse().ok());
        if it.next() == Some("") {
            fields.next();
            fields.next();
        }
    }
    files
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blob {
    pub bytes: Vec<u8>,
    /// A NUL in the first 8000 bytes (git's own test).
    pub binary: bool,
    /// Over 1 MB: only the first 1 MB is in `bytes`.
    pub truncated: bool,
}

impl Blob {
    fn new(bytes: Vec<u8>, truncated: bool) -> Blob {
        let binary = bytes[..bytes.len().min(8000)].contains(&0);
        Blob { bytes, binary, truncated }
    }
}

/// `path` as of `rev`. A path that does not exist there is an empty Blob; an
/// unknown `rev` is an error. For a commit's parent side use `commit_sides`.
pub fn file_at(repo: &Path, r: &str, path: &str) -> Res<Blob> {
    let t = read(repo, &["ls-tree", "-z", rev(r)?, "--", path])?.text();
    let Some((meta, _)) = t.split('\0').filter_map(|e| e.split_once('\t')).find(|(_, p)| *p == path) else {
        return Ok(Blob::default());
    };
    let f: Vec<&str> = meta.split(' ').collect();
    match f[..] {
        [_, "blob", sha] => {
            let o = read(repo, &["cat-file", "blob", sha])?;
            Ok(Blob::new(o.stdout, o.truncated))
        }
        [_, "commit", sha] => Ok(Blob::new(format!("Subproject commit {sha}\n").into_bytes(), false)),
        _ => Err(format!("{path} is not a file in {r}")),
    }
}

/// The two sides of a file opened from commit details: the first parent
/// (empty for a root commit or an added file) and the commit (empty for a
/// deleted file). A rename's left side is read from `orig_path`.
pub fn commit_sides(repo: &Path, hash: &str, f: &FileChange) -> Res<(Blob, Blob)> {
    let old = match parents(repo, hash)?.first() {
        Some(p) => file_at(repo, p, f.orig_path.as_deref().unwrap_or(&f.path))?,
        None => Blob::default(),
    };
    Ok((old, file_at(repo, hash, &f.path)?))
}

/// The commit hover card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    /// Author, dates, full message (subject + body) and refs.
    pub commit: Commit,
    pub files: usize,
    pub insertions: u64,
    pub deletions: u64,
}

pub fn commit_hover(repo: &Path, hash: &str) -> Res<Hover> {
    let out = read(repo, &["log", "-1", "-z", "--decorate=full", LOG_FORMAT, rev(hash)?, "--"])?;
    let commit = parse_log(&out).into_iter().next().ok_or("no such commit")?.0;
    let files = diff_files(repo, &commit.hash, commit.parents.first().map(String::as_str))?;
    Ok(Hover {
        files: files.len(),
        insertions: files.iter().filter_map(|f| f.additions).sum(),
        deletions: files.iter().filter_map(|f| f.deletions).sum(),
        commit,
    })
}

// ---------------------------------------------------------------- status bar, refs

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Branch(String),
    /// Full hash.
    Detached(String),
    /// A branch with no commits yet.
    Unborn(String),
}

pub fn head(repo: &Path) -> Res<Head> {
    let line = |args: &[&str]| read(repo, args).ok().map(|o| o.text().trim_end().to_string());
    let short = |r: String| r.strip_prefix("refs/heads/").map(String::from).unwrap_or(r);
    match (line(&["symbolic-ref", "-q", "HEAD"]), line(&["rev-parse", "--verify", "-q", "HEAD"])) {
        (Some(r), Some(_)) => Ok(Head::Branch(short(r))),
        (Some(r), None) => Ok(Head::Unborn(short(r))),
        (None, Some(h)) => Ok(Head::Detached(h)),
        (None, None) => Err("not a git repository".into()),
    }
}

/// The status bar's branch item and sync button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatus {
    pub head: Head,
    /// For example "origin/main". None on a branch: the UI shows "Publish Branch".
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

pub fn sync_status(repo: &Path) -> Res<SyncStatus> {
    let head = head(repo)?;
    let upstream = match head {
        Head::Branch(_) => read(repo, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .ok()
            .map(|o| o.text().trim_end().to_string()),
        _ => None,
    };
    let (mut ahead, mut behind) = (0, 0);
    if upstream.is_some() {
        let t = read(repo, &["rev-list", "--left-right", "--count", "@{u}...HEAD", "--"])?.text();
        let mut n = t.split_whitespace().map(|s| s.parse().unwrap_or(0));
        behind = n.next().unwrap_or(0);
        ahead = n.next().unwrap_or(0);
    }
    Ok(SyncStatus { head, upstream, ahead, behind })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Branch,
    Remote,
    Tag,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefInfo {
    pub kind: RefKind,
    /// "main", "origin/main", "v1.0".
    pub name: String,
    /// The commit it points at (annotated tags are peeled).
    pub hash: String,
    /// A local branch's upstream, for example "origin/main".
    pub upstream: Option<String>,
    /// The checked-out branch.
    pub current: bool,
}

/// Local branches, remote branches and tags, each sorted by name.
pub fn refs(repo: &Path) -> Res<Vec<RefInfo>> {
    let fmt = "--format=%(refname)%1f%(objectname)%1f%(*objectname)%1f%(HEAD)%1f%(upstream:short)%1f%(symref)";
    let t = list(repo, &["for-each-ref", fmt, "refs/heads", "refs/remotes", "refs/tags"])?.text();
    Ok(t.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\x1f').collect();
            let [full, obj, peeled, head, up, symref] = f[..] else { return None };
            if !symref.is_empty() {
                return None; // origin/HEAD
            }
            let (kind, name) = if let Some(n) = full.strip_prefix("refs/heads/") {
                (RefKind::Branch, n)
            } else if let Some(n) = full.strip_prefix("refs/remotes/") {
                (RefKind::Remote, n)
            } else {
                (RefKind::Tag, full.strip_prefix("refs/tags/")?)
            };
            Some(RefInfo {
                kind,
                name: name.into(),
                hash: (if peeled.is_empty() { obj } else { peeled }).into(),
                upstream: (!up.is_empty()).then(|| up.into()),
                current: head == "*",
            })
        })
        .collect())
}

// ---------------------------------------------------------------- stash

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// N of `stash@{N}`, the argument to apply/pop/drop.
    pub index: usize,
    pub hash: String,
    pub time: i64,
    /// "On main: msg", or "WIP on main: abc1234 subject".
    pub message: String,
}

pub fn stash_list(repo: &Path) -> Res<Vec<Stash>> {
    let t = list(repo, &["stash", "list", "-z", "--format=%H%x1f%ct%x1f%gs"])?.text();
    Ok(t.split('\0')
        .filter(|r| !r.is_empty())
        .enumerate()
        .filter_map(|(index, r)| {
            let mut f = r.splitn(3, '\x1f');
            Some(Stash { index, hash: f.next()?.into(), time: f.next()?.parse().ok()?, message: f.next()?.into() })
        })
        .collect())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StashOpts {
    /// Stash untracked files too (`-u`).
    pub include_untracked: bool,
    /// Stash only what is staged (`--staged`).
    pub staged_only: bool,
}

/// Stash the changes. An empty `message` lets git write "WIP on <branch>: ...".
pub fn stash_push(repo: &Path, message: &str, o: StashOpts) -> Res<()> {
    let msg = format!("--message={message}");
    let mut args = vec!["stash", "push"];
    if o.include_untracked {
        args.push("--include-untracked");
    }
    if o.staged_only {
        args.push("--staged");
    }
    if !message.is_empty() {
        args.push(&msg);
    }
    // An explicit pathspec: without one, `stash -u` cleans untracked files with
    // `:/`, which --literal-pathspecs turns into a file name, so they would stay.
    args.extend(["--", "."]);
    if call(repo, Mode::Write, &args, None, CAP)?.text().contains("No local changes to save") {
        return Err("No local changes to stash".into());
    }
    Ok(())
}

pub fn stash_apply(repo: &Path, index: usize) -> Res<()> {
    write(repo, &["stash", "apply", "-q", &format!("stash@{{{index}}}")])
}

/// Apply, then drop unless the apply conflicted (git keeps the stash then).
pub fn stash_pop(repo: &Path, index: usize) -> Res<()> {
    write(repo, &["stash", "pop", "-q", &format!("stash@{{{index}}}")])
}

/// **DESTRUCTIVE**: the stash is gone.
pub fn stash_drop(repo: &Path, index: usize) -> Res<()> {
    write(repo, &["stash", "drop", "-q", &format!("stash@{{{index}}}")])
}

// ---------------------------------------------------------------- commit dialog

#[derive(Clone, Copy, Debug, Default)]
pub struct CommitOpts {
    /// Replace HEAD (`--amend`); prefill the dialog with `last_commit_message`.
    pub amend: bool,
    /// Add a `Signed-off-by:` trailer from user.name and user.email.
    pub signoff: bool,
    /// Commit every tracked change, staged or not (`-a`).
    pub all: bool,
}

/// Commit with `message`, passed on stdin (`-F -`) so no text is ever read
/// as an option. An empty message is refused, and so is a commit with nothing
/// staged ("Nothing staged to commit").
pub fn commit(repo: &Path, message: &str, o: CommitOpts) -> Res<()> {
    if message.trim().is_empty() {
        return Err("Empty commit message".into());
    }
    let mut args = vec!["commit", "-q", "-F", "-"];
    for (on, flag) in [(o.amend, "--amend"), (o.signoff, "--signoff"), (o.all, "--all")] {
        if on {
            args.push(flag);
        }
    }
    call(repo, Mode::Write, &args, Some(message.as_bytes().to_vec()), CAP).map(drop).map_err(|e| {
        let nothing = ["nothing to commit", "nothing added to commit", "no changes added to commit"];
        if nothing.iter().any(|s| e.contains(s)) {
            "Nothing staged to commit".into()
        } else {
            e
        }
    })
}

/// git-gui's Sign Off line, from the committer identity (user.name / user.email).
pub fn signoff_trailer(repo: &Path) -> Res<String> {
    let id = read(repo, &["var", "GIT_COMMITTER_IDENT"])?.text();
    // "Name <email> 1700000000 +0800"
    let end = id.rfind('>').ok_or("no committer identity: set user.name and user.email")?;
    Ok(format!("Signed-off-by: {}", &id[..=end]))
}

pub fn last_commit_message(repo: &Path) -> Res<String> {
    Ok(read(repo, &["log", "-1", "--format=%B", "HEAD", "--"])?.text().trim_end().to_string())
}

/// Undo Last Commit: `reset --soft` to HEAD's first parent, so its changes
/// stay staged. Refuses the initial commit, and a merge unless `allow_merge`
/// (the other parents are dropped; confirm that in the UI).
pub fn undo_last_commit(repo: &Path, allow_merge: bool) -> Res<()> {
    let p = parents(repo, "HEAD")?;
    match p.len() {
        0 => Err("Can't undo the initial commit".into()),
        n if n > 1 && !allow_merge => Err("HEAD is a merge commit".into()),
        _ => write(repo, &["reset", "-q", "--soft", &p[0], "--"]),
    }
}

// ---------------------------------------------------------------- remotes

pub fn fetch(repo: &Path, prune: bool) -> Res<()> {
    net(repo, if prune { &["fetch", "--prune"][..] } else { &["fetch"] })
}

/// `git pull`: merges or rebases as the user's pull.rebase / pull.ff config says.
pub fn pull(repo: &Path) -> Res<()> {
    net(repo, &["pull"])
}

/// Push the current branch to its upstream, never forced: an explicit
/// `refs/heads/<b>:<upstream ref>` to the upstream's remote, so neither a `+`
/// refspec in `remote.*.push` nor push.default can widen or force it. Without
/// an upstream it refuses; the UI offers `publish` instead.
pub fn push(repo: &Path) -> Res<()> {
    let Head::Branch(b) = head(repo)? else { return Err("Not on a branch".into()) };
    let fmt = "--format=%(upstream:remotename)%1f%(upstream:remoteref)";
    let t = read(repo, &["for-each-ref", fmt, &format!("refs/heads/{b}")])?.text();
    let (remote, up) = t.trim_end().split_once('\x1f').unwrap_or(("", ""));
    if remote.is_empty() || up.is_empty() {
        return Err("The branch has no upstream branch; publish it first".into());
    }
    net(repo, &["push", "--", remote, &format!("refs/heads/{b}:{up}")])
}

/// The remote Publish Branch offers first: `origin`, else the only remote.
pub fn default_remote(repo: &Path) -> Option<String> {
    let t = read(repo, &["remote"]).ok()?.text();
    let all: Vec<&str> = t.lines().filter(|l| !l.is_empty()).collect();
    match all[..] {
        _ if all.contains(&"origin") => Some("origin".into()),
        [only] => Some(only.into()),
        _ => None,
    }
}

/// Publish Branch: `push -u <remote> <branch>`, only on the user's request.
pub fn publish(repo: &Path, remote: &str) -> Res<()> {
    let Head::Branch(b) = head(repo)? else { return Err("Not on a branch".into()) };
    net(repo, &["push", "-u", "--", rev(remote)?, &format!("refs/heads/{b}:refs/heads/{b}")])
}

/// Sync: pull, then push.
pub fn sync(repo: &Path) -> Res<()> {
    pull(repo)?;
    push(repo)
}

// ---------------------------------------------------------------- branches

/// Check out a local branch, a tag or a commit; a tag or commit gives a
/// detached HEAD, as in VS Code. git refuses if local changes would be lost.
pub fn checkout(repo: &Path, target: &str) -> Res<()> {
    write(repo, &["checkout", "-q", rev(target)?, "--"])
}

/// Check out a remote branch ("origin/x") as a new local branch tracking it.
pub fn checkout_remote(repo: &Path, remote_branch: &str) -> Res<()> {
    write(repo, &["checkout", "-q", "--track", rev(remote_branch)?, "--"])
}

/// Create `name` from HEAD or `from` and check it out (VS Code's Create Branch).
pub fn create_branch(repo: &Path, name: &str, from: Option<&str>) -> Res<()> {
    let mut args = vec!["checkout", "-q", "-b", rev(name)?];
    if let Some(f) = from {
        args.push(rev(f)?);
    }
    args.push("--");
    write(repo, &args)
}

/// Delete a local branch. Without `force`, git refuses a branch that is not
/// merged. With `force` (`-D`) it is **DESTRUCTIVE**: its commits may be lost.
pub fn delete_branch(repo: &Path, name: &str, force: bool) -> Res<()> {
    write(repo, &["branch", if force { "-D" } else { "-d" }, "--", rev(name)?])
}

/// Merge `r` into the current branch. A conflict is an Err; then see `conflicts`.
pub fn merge(repo: &Path, r: &str) -> Res<()> {
    write(repo, &["merge", "--no-edit", "--", rev(r)?])
}

/// **DESTRUCTIVE**: rebase the current branch onto `onto`, rewriting its commits.
pub fn rebase(repo: &Path, onto: &str) -> Res<()> {
    write(repo, &["rebase", "--", rev(onto)?])
}

/// After the conflicts are resolved and staged.
pub fn rebase_continue(repo: &Path) -> Res<()> {
    write(repo, &["rebase", "--continue"])
}

/// **DESTRUCTIVE**: conflict resolutions made so far are lost.
pub fn rebase_abort(repo: &Path) -> Res<()> {
    write(repo, &["rebase", "--abort"])
}

/// **DESTRUCTIVE**: conflict resolutions made so far are lost.
pub fn merge_abort(repo: &Path) -> Res<()> {
    write(repo, &["merge", "--abort"])
}

// ---------------------------------------------------------------- hunks

/// One changed region, in 0-based lines. A line keeps its `\n` (and `\r`); a
/// last line without `\n` is still a line. To revert it in an editor buffer,
/// replace new lines `new_start..new_start + new_len` with old lines
/// `old_start..old_start + old_len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
}

fn split_lines(b: &[u8]) -> Vec<&[u8]> {
    b.split_inclusive(|&c| c == b'\n').collect()
}

/// The hunks between two versions, compared byte for byte.
pub fn hunks(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    let (a, b) = (split_lines(old), split_lines(new));
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(Instant::now() + Duration::from_secs(2)));
    let mut out: Vec<Hunk> = Vec::new();
    let mut open = false;
    for op in ops {
        let (tag, ra, rb) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            open = false;
            continue;
        }
        match out.last_mut() {
            Some(h) if open => {
                h.old_len = ra.end - h.old_start;
                h.new_len = rb.end - h.new_start;
            }
            _ => out.push(Hunk { old_start: ra.start, old_len: ra.len(), new_start: rb.start, new_len: rb.len() }),
        }
        open = true;
    }
    out
}

/// The two sides a hunk is staged between: the index blob and the working file
/// as git would stage it (clean filters, eol, ident applied). Compute the
/// peek's hunks from these (`hunks(&index, &work)`) so they match `stage_hunk`.
pub fn stage_sides(repo: &Path, path: &str) -> Res<(Vec<u8>, Vec<u8>)> {
    Ok((index_file(repo, path)?, worktree_file(repo, path)?))
}

/// HEAD's blob (empty before the first commit) and the index blob: `hunks` of these unstage.
pub fn unstage_sides(repo: &Path, path: &str) -> Res<(Vec<u8>, Vec<u8>)> {
    let head = if git::has_head(repo) { text_only(file_at(repo, "HEAD", path)?, path)? } else { Vec::new() };
    Ok((head, index_file(repo, path)?))
}

/// Stage one hunk of `hunks(stage_sides)` for `path`. Fails if either side
/// changed since the hunks were computed.
pub fn stage_hunk(repo: &Path, path: &str, h: &Hunk) -> Res<()> {
    let (index, work) = stage_sides(repo, path)?;
    apply_hunk(repo, path, &index, &work, h, false)
}

/// Unstage one hunk of `hunks(unstage_sides)` for `path`.
pub fn unstage_hunk(repo: &Path, path: &str, h: &Hunk) -> Res<()> {
    let (head, index) = unstage_sides(repo, path)?;
    apply_hunk(repo, path, &head, &index, h, true)
}

/// Replace the hunk's lines in the index with the other side's: the working
/// file's (stage) or HEAD's (unstage). The context comes from the index, the
/// side being patched, so a nearby hunk never breaks the patch.
fn apply_hunk(repo: &Path, path: &str, old: &[u8], new: &[u8], h: &Hunk, unstage: bool) -> Res<()> {
    if !hunks(old, new).contains(h) {
        return Err("The file changed; refresh and try again".into());
    }
    let (o, n) = (h.old_start..h.old_start + h.old_len, h.new_start..h.new_start + h.new_len);
    let p = if unstage { patch(path, new, n, old, o) } else { patch(path, old, o, new, n) };
    call(repo, Mode::Write, &["apply", "--cached", "--whitespace=nowarn", "-"], Some(p), CAP).map(drop)
}

/// A one-hunk patch replacing `target[t]` with `src[s]`, with up to 3 lines of
/// context from `target`. Lines keep their bytes; the path is C-quoted.
fn patch(path: &str, target: &[u8], t: Range<usize>, src: &[u8], s: Range<usize>) -> Vec<u8> {
    const CTX: usize = 3;
    let (tl, sl) = (split_lines(target), split_lines(src));
    let (pre, post) = (t.start.saturating_sub(CTX), (t.end + CTX).min(tl.len()));
    let old_len = post - pre;
    let new_len = old_len - t.len() + s.len();
    // An empty side is numbered from the line before it: 0 for an empty file.
    let start = |len: usize| if len == 0 { pre } else { pre + 1 };
    let (a, b) = (quote(&format!("a/{path}")), quote(&format!("b/{path}")));
    let mut p = format!("diff --git {a} {b}\n--- {a}\n+++ {b}\n@@ -{},{old_len} +{},{new_len} @@\n", start(old_len), start(new_len))
        .into_bytes();
    for (sign, lines) in [(b' ', &tl[pre..t.start]), (b'-', &tl[t.clone()]), (b'+', &sl[s]), (b' ', &tl[t.end..post])] {
        for l in lines {
            p.push(sign);
            p.extend_from_slice(l);
            if !l.ends_with(b"\n") {
                p.extend_from_slice(b"\n\\ No newline at end of file\n");
            }
        }
    }
    p
}

/// git's C-style quoting, which `git apply` reads back for any file name.
fn quote(s: &str) -> String {
    let mut q = String::from("\"");
    for b in s.bytes() {
        match b {
            b'"' | b'\\' => {
                q.push('\\');
                q.push(b as char);
            }
            b'\t' => q.push_str("\\t"),
            b'\n' => q.push_str("\\n"),
            0x20..=0x7e => q.push(b as char),
            _ => q.push_str(&format!("\\{b:03o}")),
        }
    }
    q.push('"');
    q
}

fn text_only(b: Blob, path: &str) -> Res<Vec<u8>> {
    if b.binary || b.truncated {
        Err(format!("{path} is binary or over 1 MB; stage the whole file"))
    } else {
        Ok(b.bytes)
    }
}

/// The stage-0 index blob of a regular file.
fn index_file(repo: &Path, path: &str) -> Res<Vec<u8>> {
    let t = read(repo, &["ls-files", "-s", "-z", "--", path])?.text();
    let (meta, _) = t
        .split('\0')
        .filter_map(|e| e.split_once('\t'))
        .find(|(_, p)| *p == path)
        .ok_or_else(|| format!("{path} is not tracked; stage the whole file"))?;
    match meta.split(' ').collect::<Vec<_>>()[..] {
        ["100644" | "100755", sha, "0"] => {
            let o = read(repo, &["cat-file", "blob", sha])?;
            text_only(Blob::new(o.stdout, o.truncated), path)
        }
        [_, _, "0"] => Err(format!("{path} is not a regular file")),
        _ => Err(format!("{path} has conflicts")),
    }
}

fn worktree_file(repo: &Path, path: &str) -> Res<Vec<u8>> {
    let p = repo.join(path);
    // Regular files only: a FIFO would block, a symlink's index blob is its target.
    if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
        return Err(format!("{path} is not a regular file"));
    }
    let mut buf = Vec::new();
    std::fs::File::open(&p).and_then(|f| f.take(CAP as u64 + 1).read_to_end(&mut buf)).map_err(|e| format!("{path}: {e}"))?;
    if buf.len() > CAP {
        return text_only(Blob::new(buf, true), path);
    }
    // As git would stage it: core.autocrlf, eol/text attributes, ident and clean
    // filters (LFS) apply. hash-object stores that blob; cat-file reads it back.
    let at = format!("--path={path}");
    let oid = call(repo, Mode::Write, &["hash-object", "-w", "--stdin", &at], Some(buf), CAP)?.text();
    let o = read(repo, &["cat-file", "blob", oid.trim()])?;
    text_only(Blob::new(o.stdout, o.truncated), path)
}

// ---------------------------------------------------------------- blame

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlameLine {
    /// None: the line is not committed yet (VS Code: "You, not committed yet").
    pub hash: Option<String>,
    pub author: String,
    pub author_email: String,
    /// Unix seconds.
    pub author_time: i64,
    pub summary: String,
}

/// Blame 0-based `line` of the editor buffer `contents`, which may be unsaved.
pub fn blame_line(repo: &Path, path: &str, line: usize, contents: &[u8]) -> Res<BlameLine> {
    let l = format!("{0},{0}", line + 1);
    let args = ["blame", "--porcelain", "-L", &l, "--contents", "-", "--", path];
    let t = call(repo, Mode::Read, &args, Some(contents.to_vec()), CAP)?.text();
    let mut lines = t.lines();
    let hash = lines.next().and_then(|l| l.split(' ').next()).ok_or("no blame output")?;
    let mut b = BlameLine { hash: (!hash.bytes().all(|c| c == b'0')).then(|| hash.into()), ..Default::default() };
    for l in lines.take_while(|l| !l.starts_with('\t')) {
        let (k, v) = l.split_once(' ').unwrap_or((l, ""));
        match k {
            "author" => b.author = v.into(),
            "author-mail" => b.author_email = v.trim_matches(['<', '>']).into(),
            "author-time" => b.author_time = v.parse().unwrap_or(0),
            "summary" => b.summary = v.into(),
            _ => {}
        }
    }
    Ok(b)
}

// ---------------------------------------------------------------- conflicts

#[allow(dead_code)] // backend API; the Merge Changes group reads the same state from git status
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    /// git's status code: UU both modified, AA both added, UD deleted by them,
    /// DU deleted by us, AU added by us, UA added by them, DD both deleted.
    pub code: &'static str,
}

/// Unmerged paths, from the index stages present for each (as `git status` derives them).
#[allow(dead_code)] // backend API; the Merge Changes group reads the same state from git status
pub fn conflicts(repo: &Path) -> Res<Vec<Conflict>> {
    const CODES: [&str; 8] = ["", "DD", "AU", "UD", "UA", "DU", "AA", "UU"];
    let t = list(repo, &["ls-files", "-u", "-z"])?.text();
    let mut v: Vec<(String, usize)> = Vec::new();
    for e in t.split('\0') {
        let Some((meta, path)) = e.split_once('\t') else { continue };
        let bit = match meta.rsplit(' ').next() {
            Some("1") => 1,
            Some("2") => 2,
            Some("3") => 4,
            _ => continue,
        };
        match v.last_mut() {
            Some((p, m)) if p == path => *m |= bit,
            _ => v.push((path.into(), bit)),
        }
    }
    Ok(v.into_iter().map(|(path, m)| Conflict { path, code: CODES[m] }).collect())
}

/// One `<<<<<<<` ... `>>>>>>>` block, in 0-based lines of the text.
/// Accept Current / Incoming / Both = replace lines `start..=end` with the
/// `current` lines, the `incoming` lines, or current then incoming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRegion {
    /// The `<<<<<<<` line.
    pub start: usize,
    /// The `>>>>>>>` line.
    pub end: usize,
    pub current: Range<usize>,
    /// The `|||||||` section of the diff3 / zdiff3 style.
    pub base: Option<Range<usize>>,
    pub incoming: Range<usize>,
    pub current_label: String,
    pub incoming_label: String,
}

/// Complete conflict blocks in `text`; a block left open or broken is skipped.
pub fn parse_conflict_markers(text: &str) -> Vec<ConflictRegion> {
    fn marker<'a>(line: &'a str, m: &str) -> Option<&'a str> {
        let rest = line.strip_prefix(m)?;
        if rest.is_empty() {
            Some("")
        } else {
            rest.strip_prefix(' ')
        }
    }
    let mut out = Vec::new();
    // The open block and its section: 0 current, 1 base, 2 incoming.
    let mut open: Option<(ConflictRegion, u8)> = None;
    for (i, l) in text.lines().enumerate() {
        if let Some(label) = marker(l, "<<<<<<<") {
            let r = i + 1..i + 1;
            let region = ConflictRegion {
                start: i,
                end: i,
                current: r.clone(),
                base: None,
                incoming: r,
                current_label: label.into(),
                incoming_label: String::new(),
            };
            open = Some((region, 0));
            continue;
        }
        let Some((r, section)) = open.as_mut() else { continue };
        let mut done = false;
        if *section < 2 && marker(l, "=======").is_some() {
            match r.base.as_mut() {
                Some(b) => b.end = i,
                None => r.current.end = i,
            }
            r.incoming = i + 1..i + 1;
            *section = 2;
        } else if *section == 0 && marker(l, "|||||||").is_some() {
            r.current.end = i;
            r.base = Some(i + 1..i + 1);
            *section = 1;
        } else if *section == 2 {
            if let Some(label) = marker(l, ">>>>>>>") {
                r.incoming.end = i;
                r.end = i;
                r.incoming_label = label.into();
                done = true;
            }
        }
        if done {
            out.extend(open.take().map(|o| o.0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Lanes;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;

    /// A scratch dir, removed on drop.
    struct Tmp(PathBuf);

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(name: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("herdr-code-gitops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }

    /// Run a `sh -e` script in `dir`; panics with its stderr on failure.
    fn sh(dir: &Path, script: &str) -> String {
        let o = Command::new("sh").arg("-ec").arg(script).current_dir(dir).output().unwrap();
        assert!(o.status.success(), "{script}\n{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    const IDENT: &str = "git config user.name Tester; git config user.email t@example.com; git config commit.gpgsign false";

    /// `<tmp>/r`: an empty repo on `main` with an identity.
    fn repo(t: &Tmp) -> PathBuf {
        sh(&t.0, &format!("git init -q -b main r; cd r; {IDENT}"));
        t.0.join("r")
    }

    fn index_of(r: &Path, path: &str) -> Vec<u8> {
        Command::new("git").arg("-C").arg(r).args(["cat-file", "blob", &format!(":0:{path}")]).output().unwrap().stdout
    }

    #[test]
    fn log_pages_100_100_50_with_refs() {
        let t = tmp("log");
        let r = repo(&t);
        sh(
            &r,
            "i=1; while [ $i -le 250 ]; do git commit -q --allow-empty -m c$i -m \"body $i\"
             if [ $i = 10 ]; then git tag -a v1 -m v1; fi; if [ $i = 20 ]; then git branch old; fi; i=$((i+1)); done
             git update-ref refs/remotes/origin/main HEAD~1",
        );
        let (mut all, mut sizes) = (Vec::new(), Vec::new());
        loop {
            let p = log_page(&r, all.len(), PAGE).unwrap();
            sizes.push((p.items.len(), p.more));
            all.extend(p.items);
            if !p.more {
                break;
            }
        }
        assert_eq!(sizes, [(100, true), (100, true), (50, false)]);
        let want: Vec<String> = sh(&r, "git rev-list --topo-order HEAD").lines().map(String::from).collect();
        assert_eq!(all.iter().map(|c| c.hash.clone()).collect::<Vec<_>>(), want);
        let top = &all[0];
        assert_eq!((top.subject.as_str(), top.body.as_str(), top.author_name.as_str()), ("c250", "body 250", "Tester"));
        assert_eq!(top.author_email, "t@example.com");
        assert!(top.author_time > 0 && top.commit_time >= top.author_time);
        assert_eq!(top.parents, [all[1].hash.clone()]);
        assert!(top.short.len() >= 7 && top.hash.starts_with(&top.short));
        assert_eq!(top.refs, [RefLabel::Current("main".into())]);
        assert_eq!(all[1].refs, [RefLabel::Remote("origin/main".into())]);
        assert_eq!(all[240].refs, [RefLabel::Tag("v1".into())]);
        assert_eq!(all[230].refs, [RefLabel::Branch("old".into())]);
        assert!(all[249].parents.is_empty());
        sh(&r, "git checkout -q --detach HEAD~3");
        assert_eq!(log_page(&r, 0, 1).unwrap().items[0].refs, [RefLabel::Head]);
    }

    #[test]
    fn graph_lanes_survive_page_boundaries() {
        let t = tmp("graph");
        let r = repo(&t);
        sh(
            &r,
            "c() { git commit -q --allow-empty -m $1; }; c A; c B; git checkout -q -b side; c D; c E
             git checkout -q main; c C; git merge -q --no-edit -m M side
             git checkout -q -b f2 HEAD~1; c F; git checkout -q main; git merge -q --no-edit -m M2 f2",
        );
        let whole = log_page(&r, 0, PAGE).unwrap().items;
        let one = Lanes::default().rows(&whole);
        for size in 1..whole.len() {
            let (mut lanes, mut rows) = (Lanes::default(), Vec::new());
            loop {
                let p = log_page(&r, rows.len(), size).unwrap();
                rows.extend(lanes.rows(&p.items));
                if !p.more {
                    break;
                }
            }
            assert_eq!(rows, one, "page size {size}");
        }
        let text: Vec<String> = one.iter().map(|r| r.cells.iter().map(|c| c.glyph).collect()).collect();
        assert!(text[0].starts_with("●─╮"), "{text:?}");
        assert!(text.iter().filter(|l| l.contains('╯')).count() >= 2, "{text:?}");
    }

    #[test]
    fn commit_files_rename_add_delete_root_and_merge() {
        let t = tmp("files");
        let r = repo(&t);
        sh(
            &r,
            "printf 'one\\ntwo\\nthree\\nfour\\n' > keep.txt; echo gone > gone.txt; printf '\\0\\1' > bin.dat
             git add .; git commit -qm root
             git mv keep.txt moved.txt; echo five >> moved.txt; git rm -q gone.txt; echo new > 新檔.txt
             git add .; git commit -qm second",
        );
        let files = commit_files(&r, "HEAD").unwrap();
        let get = |p: &str| files.iter().find(|f| f.path == p).cloned().unwrap();
        let mv = get("moved.txt");
        assert_eq!((mv.status, mv.orig_path.as_deref(), mv.additions, mv.deletions), ('R', Some("keep.txt"), Some(1), Some(0)));
        assert_eq!((get("gone.txt").status, get("gone.txt").deletions), ('D', Some(1)));
        assert_eq!((get("新檔.txt").status, get("新檔.txt").additions), ('A', Some(1)));
        assert_eq!(files.len(), 3);
        let root = commit_files(&r, "HEAD~1").unwrap();
        assert_eq!(root.len(), 3);
        assert!(root.iter().all(|f| f.status == 'A'), "{root:?}");
        let bin = root.iter().find(|f| f.path == "bin.dat").unwrap();
        assert_eq!((bin.additions, bin.deletions), (None, None));
        let (old, new) = commit_sides(&r, "HEAD", &mv).unwrap();
        assert_eq!((old.bytes.as_slice(), new.bytes.as_slice()), (&b"one\ntwo\nthree\nfour\n"[..], &b"one\ntwo\nthree\nfour\nfive\n"[..]));

        sh(
            &r,
            "git checkout -q -b side; echo s > s.txt; git add .; git commit -qm s
             git checkout -q main; echo m > m.txt; git add .; git commit -qm m; git merge -q --no-edit side",
        );
        let m = commit_files(&r, "HEAD").unwrap();
        assert_eq!(m.iter().map(|f| (f.status, f.path.as_str())).collect::<Vec<_>>(), [('A', "s.txt")]);
        let h = commit_hover(&r, "main~2").unwrap();
        assert_eq!((h.commit.subject.as_str(), h.files, h.insertions, h.deletions), ("second", 3, 2, 1));
        assert_eq!(commit_hover(&r, "HEAD").unwrap().commit.refs, [RefLabel::Current("main".into())]);
        assert!(commit_files(&r, "nope").is_err());
    }

    #[test]
    fn file_at_root_binary_missing_and_cap() {
        let t = tmp("fileat");
        let r = repo(&t);
        sh(&r, "printf 'a\\r\\nb' > t.txt; printf 'x\\0y' > b.bin; git add .; git commit -qm root; echo c > c.txt; git add .; git commit -qm two");
        let root = commit_files(&r, "HEAD~1").unwrap();
        let tf = root.iter().find(|f| f.path == "t.txt").unwrap();
        let (old, new) = commit_sides(&r, "HEAD~1", tf).unwrap();
        assert_eq!(old, Blob::default(), "a root commit's parent side is empty");
        assert_eq!((new.bytes.as_slice(), new.binary), (&b"a\r\nb"[..], false));
        let b = file_at(&r, "HEAD", "b.bin").unwrap();
        assert_eq!((b.bytes.as_slice(), b.binary), (&b"x\0y"[..], true));
        assert_eq!(file_at(&r, "HEAD~1", "c.txt").unwrap(), Blob::default());
        assert!(file_at(&r, "nope", "t.txt").is_err());
        assert!(file_at(&r, "--output=x", "t.txt").is_err());
        sh(&r, "head -c 1100000 /dev/zero | tr '\\0' a > big.txt; git add big.txt; git commit -qm big");
        let big = file_at(&r, "HEAD", "big.txt").unwrap();
        assert_eq!((big.truncated, big.bytes.len()), (true, CAP));
    }

    #[test]
    fn branch_create_checkout_delete() {
        let t = tmp("branch");
        let r = repo(&t);
        sh(&r, "git commit -q --allow-empty -m one; git tag -a v1 -m v1");
        create_branch(&r, "feat", None).unwrap();
        assert_eq!(head(&r).unwrap(), Head::Branch("feat".into()));
        sh(&r, "git commit -q --allow-empty -m two");
        checkout(&r, "main").unwrap();
        let e = delete_branch(&r, "feat", false).unwrap_err();
        assert!(e.contains("not fully merged"), "{e}");
        create_branch(&r, "from-tag", Some("v1")).unwrap();
        checkout(&r, "main").unwrap();
        delete_branch(&r, "feat", true).unwrap();
        delete_branch(&r, "from-tag", false).unwrap();
        checkout(&r, "v1").unwrap();
        assert!(matches!(head(&r).unwrap(), Head::Detached(_)));
        let all = refs(&r).unwrap();
        let v1 = sh(&r, "git rev-parse v1^{commit}");
        assert_eq!(
            all.iter().map(|x| (x.kind, x.name.as_str(), x.current)).collect::<Vec<_>>(),
            [(RefKind::Branch, "main", false), (RefKind::Tag, "v1", false)]
        );
        assert_eq!(all[1].hash, v1.trim(), "annotated tags are peeled");
        assert!(create_branch(&r, "-x", None).is_err());
        assert!(checkout(&r, "-f").is_err());
    }

    #[test]
    fn stash_round_trip_with_untracked() {
        let t = tmp("stash");
        let r = repo(&t);
        sh(&r, "echo a > a.txt; echo b > b.txt; git add .; git commit -qm one; echo a2 >> a.txt; echo u > new.txt");
        let u = StashOpts { include_untracked: true, ..Default::default() };
        stash_push(&r, "-m wip", u).unwrap();
        assert!(!r.join("new.txt").exists());
        assert_eq!(fs::read(r.join("a.txt")).unwrap(), b"a\n");
        let l = stash_list(&r).unwrap();
        assert_eq!((l.len(), l[0].index, l[0].message.as_str()), (1, 0, "On main: -m wip"));
        stash_apply(&r, 0).unwrap();
        assert!(r.join("new.txt").exists());
        assert_eq!(stash_list(&r).unwrap().len(), 1);
        sh(&r, "git reset -q --hard; git clean -qfd");
        stash_pop(&r, 0).unwrap();
        assert_eq!(fs::read(r.join("new.txt")).unwrap(), b"u\n");
        assert!(stash_list(&r).unwrap().is_empty());
        stash_push(&r, "", u).unwrap();
        assert!(stash_list(&r).unwrap()[0].message.starts_with("WIP on main:"));
        stash_drop(&r, 0).unwrap();
        assert!(stash_list(&r).unwrap().is_empty());
        assert!(stash_push(&r, "", StashOpts::default()).unwrap_err().contains("No local changes"));
        sh(&r, "echo s >> a.txt; git add a.txt; echo w >> b.txt");
        stash_push(&r, "staged", StashOpts { staged_only: true, ..Default::default() }).unwrap();
        assert_eq!(sh(&r, "git status --porcelain"), " M b.txt\n");
    }

    #[test]
    fn commit_dialog() {
        let t = tmp("commit");
        let r = repo(&t);
        assert_eq!(log_page(&r, 0, PAGE).unwrap(), Page { items: vec![], more: false });
        assert_eq!(head(&r).unwrap(), Head::Unborn("main".into()));
        assert!(commit(&r, " \n\t", CommitOpts::default()).unwrap_err().contains("Empty"));
        assert_eq!(commit(&r, "x", CommitOpts::default()).unwrap_err(), "Nothing staged to commit");
        sh(&r, "echo a > a.txt; git add a.txt");
        let msg = "-n --amend subject\n\nline two\n  indented";
        commit(&r, msg, CommitOpts::default()).unwrap();
        assert_eq!(last_commit_message(&r).unwrap(), msg);
        assert_eq!(commit(&r, "again", CommitOpts::default()).unwrap_err(), "Nothing staged to commit");
        commit(&r, "amended", CommitOpts { amend: true, signoff: true, all: false }).unwrap();
        assert_eq!(last_commit_message(&r).unwrap(), "amended\n\nSigned-off-by: Tester <t@example.com>");
        assert_eq!(log_page(&r, 0, PAGE).unwrap().items.len(), 1);
        sh(&r, "echo b >> a.txt");
        assert_eq!(commit(&r, "x", CommitOpts::default()).unwrap_err(), "Nothing staged to commit");
        commit(&r, "all", CommitOpts { all: true, ..Default::default() }).unwrap();
        assert_eq!(sh(&r, "git status --porcelain"), "");
    }

    #[test]
    fn undo_last_commit_root_normal_merge() {
        let t = tmp("undo");
        let r = repo(&t);
        sh(&r, "echo a > a.txt; git add .; git commit -qm root");
        assert!(undo_last_commit(&r, false).unwrap_err().contains("initial"));
        sh(&r, "echo b > b.txt; git add .; git commit -qm two");
        undo_last_commit(&r, false).unwrap();
        assert_eq!(last_commit_message(&r).unwrap(), "root");
        assert_eq!(sh(&r, "git status --porcelain"), "A  b.txt\n");
        sh(
            &r,
            "git commit -qm two; git checkout -q -b side HEAD~1; echo s > s.txt; git add .; git commit -qm s
             git checkout -q main; git merge -q --no-edit side",
        );
        assert!(undo_last_commit(&r, false).unwrap_err().contains("merge"));
        undo_last_commit(&r, true).unwrap();
        assert_eq!(last_commit_message(&r).unwrap(), "two");
        assert_eq!(sh(&r, "git status --porcelain"), "A  s.txt\n");
    }

    #[test]
    fn stage_and_unstage_one_hunk_byte_exact() {
        let t = tmp("hunk");
        let r = repo(&t);
        let name = "資料 *.txt";
        let mut base: Vec<u8> = (1..=12).map(|i| format!("l{i}\r\n")).collect::<String>().into_bytes();
        base.extend_from_slice(b"end");
        let swap = |b: &[u8], from: &str, to: &str| String::from_utf8(b.to_vec()).unwrap().replacen(from, to, 1).into_bytes();
        let work = swap(&swap(&swap(&base, "l2\r\n", "L2\r\n"), "l4\r\n", "L4\r\nx\r\n"), "end", "END\n");
        fs::write(r.join(name), &base).unwrap();
        sh(&r, "echo same > '資料 a.txt'; git add -A; git commit -qm base");
        fs::write(r.join(name), &work).unwrap();
        let hs = hunks(&base, &work);
        assert_eq!(hs.len(), 3, "{hs:?}");
        stage_hunk(&r, name, &hs[1]).unwrap();
        assert_eq!(index_of(&r, name), swap(&base, "l4\r\n", "L4\r\nx\r\n"));
        stage_hunk(&r, name, &hs[2]).unwrap_err();
        let rest = hunks(&index_of(&r, name), &work);
        assert_eq!(rest.len(), 2);
        stage_hunk(&r, name, &rest[1]).unwrap();
        stage_hunk(&r, name, &hunks(&index_of(&r, name), &work)[0]).unwrap();
        assert_eq!(index_of(&r, name), work);
        // Unstage the first hunk: its context (L4) exists only in the index, not in HEAD.
        unstage_hunk(&r, name, &hunks(&base, &work)[0]).unwrap();
        assert_eq!(index_of(&r, name), swap(&work, "L2\r\n", "l2\r\n"));
        let left = hunks(&base, &index_of(&r, name));
        assert_eq!(left.len(), 2);
        unstage_hunk(&r, name, &left[1]).unwrap();
        unstage_hunk(&r, name, &left[0]).unwrap();
        assert_eq!(index_of(&r, name), base);
        assert_eq!(sh(&r, "git diff --cached --name-only"), "");
        assert!(stage_hunk(&r, "untracked.txt", &hs[0]).is_err());
    }

    #[test]
    fn hunks_cover_edges() {
        assert_eq!(hunks(b"", b"a\n"), [Hunk { old_start: 0, old_len: 0, new_start: 0, new_len: 1 }]);
        assert_eq!(hunks(b"a\nb", b"a\nb\n"), [Hunk { old_start: 1, old_len: 1, new_start: 1, new_len: 1 }]);
        assert_eq!(hunks(b"a\r\n", b"a\n").len(), 1);
        assert!(hunks(b"same\n", b"same\n").is_empty());
        assert_eq!(quote("a/資 \"q\"\\\t"), "\"a/\\350\\263\\207 \\\"q\\\"\\\\\\t\"");
    }

    #[test]
    fn hunk_patch_on_empty_and_emptied_files() {
        let t = tmp("hunk0");
        let r = repo(&t);
        sh(&r, "touch e.txt; printf 'x\\ny\\n' > f.txt; git add .; git commit -qm base; printf 'new' > e.txt; : > f.txt");
        stage_hunk(&r, "e.txt", &hunks(b"", b"new")[0]).unwrap();
        stage_hunk(&r, "f.txt", &hunks(b"x\ny\n", b"")[0]).unwrap();
        assert_eq!((index_of(&r, "e.txt"), index_of(&r, "f.txt")), (b"new".to_vec(), Vec::new()));
        unstage_hunk(&r, "f.txt", &hunks(b"x\ny\n", b"")[0]).unwrap();
        assert_eq!(index_of(&r, "f.txt"), b"x\ny\n");
    }

    #[test]
    fn blame_dirty_buffer() {
        let t = tmp("blame");
        let r = repo(&t);
        sh(&r, "printf 'one\\ntwo\\n' > f.txt; git add .; git commit -qm first");
        let buf = b"one\nnew line\ntwo\n";
        let b0 = blame_line(&r, "f.txt", 0, buf).unwrap();
        assert_eq!((b0.summary.as_str(), b0.author.as_str(), b0.author_email.as_str()), ("first", "Tester", "t@example.com"));
        assert!(b0.hash.is_some() && b0.author_time > 0);
        assert_eq!(blame_line(&r, "f.txt", 1, buf).unwrap().hash, None);
        assert_eq!(blame_line(&r, "f.txt", 2, buf).unwrap().summary, "first");
        assert!(blame_line(&r, "f.txt", 9, buf).is_err());
    }

    #[test]
    fn conflicts_and_markers_with_diff3() {
        let t = tmp("conflict");
        let r = repo(&t);
        sh(
            &r,
            "git config merge.conflictStyle diff3; printf 'a\\nb\\nc\\n' > f.txt; echo x > d.txt; git add .; git commit -qm base
             git checkout -q -b side; printf 'a\\nTHEIRS\\nc\\n' > f.txt; git rm -q d.txt; git commit -qam theirs
             git checkout -q main; printf 'a\\nOURS\\nc\\n' > f.txt; echo y > d.txt; git commit -qam ours",
        );
        let e = merge(&r, "side").unwrap_err();
        assert!(e.contains("CONFLICT"), "{e}");
        let c = conflicts(&r).unwrap();
        assert_eq!(c, [Conflict { path: "d.txt".into(), code: "UD" }, Conflict { path: "f.txt".into(), code: "UU" }]);
        let text = fs::read_to_string(r.join("f.txt")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let g = parse_conflict_markers(&text);
        assert_eq!(g.len(), 1, "{text}");
        assert_eq!((&lines[g[0].current.clone()], &lines[g[0].base.clone().unwrap()], &lines[g[0].incoming.clone()]), (&["OURS"][..], &["b"][..], &["THEIRS"][..]));
        assert_eq!((g[0].start, g[0].end, g[0].current_label.as_str(), g[0].incoming_label.as_str()), (1, 7, "HEAD", "side"));
        merge_abort(&r).unwrap();
        assert!(conflicts(&r).unwrap().is_empty());

        let two_way = "x\r\n<<<<<<< ours\r\n1\r\n2\r\n=======\r\n3\r\n>>>>>>> theirs\r\ny\n<<<<<<< open\nz\n";
        let g = parse_conflict_markers(two_way);
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].start, g[0].end, g[0].current.clone(), g[0].base.clone(), g[0].incoming.clone()), (1, 6, 2..4, None, 5..6));
        assert_eq!((g[0].current_label.as_str(), g[0].incoming_label.as_str()), ("ours", "theirs"));
        assert!(parse_conflict_markers("<<<<<<<< not a marker\n=======\n>>>>>>>\n").is_empty());
    }

    #[test]
    fn fetch_pull_push_sync_publish_against_bare_remote() {
        let t = tmp("remote");
        sh(
            &t.0,
            &format!(
                "git init -q --bare -b main remote.git
                 git init -q -b main a; cd a; {IDENT}; git remote add origin ../remote.git
                 git commit -q --allow-empty -m one; git push -q -u origin main 2>/dev/null; cd ..
                 git clone -q remote.git b; cd b; {IDENT}; git config pull.rebase true"
            ),
        );
        let (a, b) = (t.0.join("a"), t.0.join("b"));
        sh(&a, "git commit -q --allow-empty -m two");
        let s = sync_status(&a).unwrap();
        assert_eq!((s.head, s.upstream.as_deref(), s.ahead, s.behind), (Head::Branch("main".into()), Some("origin/main"), 1, 0));
        push(&a).unwrap();
        assert_eq!(sync_status(&a).unwrap().ahead, 0);

        sh(&b, "git commit -q --allow-empty -m local");
        fetch(&b, false).unwrap();
        let s = sync_status(&b).unwrap();
        assert_eq!((s.ahead, s.behind), (1, 1));
        sync(&b).unwrap();
        let s = sync_status(&b).unwrap();
        assert_eq!((s.ahead, s.behind), (0, 0));
        assert_eq!(parents(&b, "HEAD").unwrap().len(), 1, "pull.rebase=true must not merge");
        pull(&a).unwrap();
        assert_eq!(last_commit_message(&a).unwrap(), "local");

        create_branch(&a, "feature", None).unwrap();
        let s = sync_status(&a).unwrap();
        assert_eq!((s.head, s.upstream), (Head::Branch("feature".into()), None));
        assert!(push(&a).unwrap_err().contains("no upstream"));
        assert_eq!(default_remote(&a).as_deref(), Some("origin"));
        publish(&a, "origin").unwrap();
        assert_eq!(sync_status(&a).unwrap().upstream.as_deref(), Some("origin/feature"));

        let remote = |r: &Path| refs(r).unwrap().into_iter().filter(|x| x.kind == RefKind::Remote).map(|x| x.name).collect::<Vec<_>>();
        fetch(&b, false).unwrap();
        assert_eq!(remote(&b), ["origin/feature", "origin/main"]);
        checkout_remote(&b, "origin/feature").unwrap();
        assert_eq!(sync_status(&b).unwrap().upstream.as_deref(), Some("origin/feature"));
        checkout(&b, "main").unwrap();
        sh(&a, "git push -q origin --delete feature");
        fetch(&b, true).unwrap();
        assert_eq!(remote(&b), ["origin/main"]);
    }

    /// Staging a hunk compares and patches in git's clean form: an `eol=crlf`
    /// file with an LF index, and a `tr a-z A-Z` clean filter.
    #[test]
    fn stage_hunk_applies_clean_conversion() {
        let t = tmp("clean");
        let r = repo(&t);
        sh(
            &r,
            "printf 'c.txt text eol=crlf\nu.txt filter=up\n' > .gitattributes; git config filter.up.clean 'tr a-z A-Z'
             printf 'l1\nl2\nl3\nl4\nl5\nl6\n' > c.txt; printf 'a\nb\nc\nd\ne\n' > u.txt
             git add .; git commit -qm base; rm c.txt; git checkout -- c.txt",
        );
        assert_eq!(fs::read(r.join("c.txt")).unwrap(), b"l1\r\nl2\r\nl3\r\nl4\r\nl5\r\nl6\r\n", "CRLF working tree");
        assert_eq!(index_of(&r, "c.txt"), b"l1\nl2\nl3\nl4\nl5\nl6\n", "LF index");
        fs::write(r.join("c.txt"), "L1\r\nl2\r\nl3\r\nl4\r\nl5\r\nL6\r\n").unwrap();
        let (index, work) = stage_sides(&r, "c.txt").unwrap();
        let hs = hunks(&index, &work);
        assert_eq!(hs.len(), 2, "two hunks, not the whole file: {hs:?}");
        stage_hunk(&r, "c.txt", &hs[0]).unwrap();
        assert_eq!(index_of(&r, "c.txt"), b"L1\nl2\nl3\nl4\nl5\nl6\n", "still LF, one line changed");

        assert_eq!(index_of(&r, "u.txt"), b"A\nB\nC\nD\nE\n");
        fs::write(r.join("u.txt"), "a\nx\nc\nd\ny\n").unwrap();
        let (index, work) = stage_sides(&r, "u.txt").unwrap();
        assert_eq!(work, b"A\nX\nC\nD\nY\n", "the clean filter ran");
        let hs = hunks(&index, &work);
        assert_eq!(hs.len(), 2, "{hs:?}");
        stage_hunk(&r, "u.txt", &hs[1]).unwrap();
        assert_eq!(index_of(&r, "u.txt"), b"A\nB\nC\nD\nY\n");
    }

    /// The user's own ssh (no BatchMode) reading /dev/tty fails at once: git
    /// runs in its own session, without a controlling tty.
    #[test]
    fn user_ssh_cannot_prompt_on_the_tty() {
        let t = tmp("tty");
        let r = repo(&t);
        let probe = t.0.join("ssh-probe");
        let script = t.0.join("fake-ssh.sh");
        fs::write(
            &script,
            format!("#!/bin/sh\nawk '{{print $6}}' /proc/$$/stat > '{}'\nif read -r x < /dev/tty; then echo read-tty >> '{}'; fi\nexit 1\n", probe.display(), probe.display()),
        )
        .unwrap();
        sh(&t.0, &format!("chmod +x '{}'", script.display()));
        sh(&r, &format!("git remote add origin fakehost:repo.git; git config core.sshCommand '{}'", script.display()));
        let t0 = Instant::now();
        let e = fetch(&r, false).unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(10), "returned quickly");
        assert!(e.contains("Could not read from remote") || e.contains("fatal"), "{e}");
        let got = fs::read_to_string(&probe).unwrap();
        let sid: i64 = got.lines().next().unwrap().trim().parse().unwrap();
        // SAFETY: getsid(0) only reads this process's session id.
        let ours = unsafe { libc::getsid(0) } as i64;
        assert_ne!(sid, ours, "ssh ran in another session (git's own)");
        assert!(!got.contains("read-tty"), "no tty to prompt on");
    }

    /// A `+` refspec in remote.*.push must never force: push sends an explicit, plain refspec.
    #[test]
    fn push_never_forces_even_with_a_plus_refspec() {
        let t = tmp("force");
        sh(
            &t.0,
            &format!(
                "git init -q --bare -b main remote.git; git init -q -b main a; cd a; {IDENT}; git remote add origin ../remote.git
                 git commit -q --allow-empty -m one; git push -q -u origin main 2>/dev/null; git commit -q --allow-empty -m two
                 git push -q origin main 2>/dev/null; git reset -q --hard HEAD~1; git commit -q --allow-empty -m diverged
                 git config remote.origin.push '+refs/heads/*:refs/heads/*'"
            ),
        );
        let a = t.0.join("a");
        let before = sh(&t.0, "git -C remote.git rev-parse main");
        assert!(push(&a).is_err(), "a non-fast-forward is refused");
        assert_eq!(sh(&t.0, "git -C remote.git rev-parse main"), before, "remote untouched");
        assert_eq!(default_remote(&t.0.join("a")).as_deref(), Some("origin"));
    }

    #[test]
    fn network_calls_cannot_prompt_or_stall() {
        let t = tmp("net");
        let r = repo(&t);
        let envs = |c: &Command| -> HashMap<String, Option<String>> {
            c.get_envs().map(|(k, v)| (k.to_string_lossy().into(), v.map(|v| v.to_string_lossy().into()))).collect()
        };
        let c = git(&r, Mode::Net);
        let args: Vec<String> = c.get_args().map(|a| a.to_string_lossy().into()).collect();
        assert_eq!(args[0], "--literal-pathspecs");
        for kv in ["http.lowSpeedLimit=1000", "http.lowSpeedTime=30"] {
            assert!(args.windows(2).any(|w| w[0] == "-c" && w[1] == kv), "{args:?}");
        }
        let e = envs(&c);
        assert_eq!((e["GIT_TERMINAL_PROMPT"].as_deref(), e["LC_ALL"].as_deref(), e["GIT_EDITOR"].as_deref()), (Some("0"), Some("C"), Some("true")));
        if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
            assert_eq!(e["GIT_SSH_COMMAND"].as_deref(), Some(SSH));
        }
        sh(&r, "git config core.sshCommand 'ssh -i mykey'");
        assert!(!envs(&git(&r, Mode::Net)).contains_key("GIT_SSH_COMMAND"), "core.sshCommand wins");
        assert_eq!(envs(&git(&r, Mode::Read))["GIT_OPTIONAL_LOCKS"].as_deref(), Some("0"));
        assert!(!envs(&git(&r, Mode::Write)).contains_key("GIT_OPTIONAL_LOCKS"));
    }

    #[test]
    fn star_dot_c_is_a_file_name_and_timeline_follows_renames() {
        let t = tmp("star");
        let r = repo(&t);
        sh(&r, "echo star > '*.c'; echo a > a.c; git add -A; git commit -qm both; echo a2 >> a.c; git commit -qam 'only a.c'");
        let h = file_history(&r, "*.c", 0, PAGE).unwrap();
        assert_eq!(h.items.iter().map(|f| f.commit.subject.as_str()).collect::<Vec<_>>(), ["both"]);
        assert_eq!(file_at(&r, "HEAD", "*.c").unwrap().bytes, b"star\n");
        assert_eq!(blame_line(&r, "*.c", 0, b"star\n").unwrap().summary, "both");
        sh(&r, "echo more >> '*.c'; echo more >> a.c");
        stage_hunk(&r, "*.c", &hunks(b"star\n", b"star\nmore\n")[0]).unwrap();
        assert_eq!(sh(&r, "git diff --cached --name-only"), "*.c\n");

        sh(&r, "git commit -qm more; git mv '*.c' 'new name.c'; git commit -qm rename");
        let h = file_history(&r, "new name.c", 0, PAGE).unwrap();
        let got: Vec<(&str, char, &str)> = h.items.iter().map(|f| (f.commit.subject.as_str(), f.status, f.path.as_str())).collect();
        assert_eq!(got, [("rename", 'R', "new name.c"), ("more", 'M', "*.c"), ("both", 'A', "*.c")]);
        assert!(!h.more);
        let p = file_history(&r, "new name.c", 1, 1).unwrap();
        assert_eq!((p.items[0].commit.subject.as_str(), p.more), ("more", true));
    }
}
