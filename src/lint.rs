//! Problems, as VS Code's squiggles: shell scripts through `shellcheck`, C/C++ through
//! the compiler and flags `compile_commands.json` (or a built kernel's `.o.cmd` files)
//! give the file, else `clang` with guessed include paths; a compiler that is not
//! installed (a cross gcc) is stood in for by `clang --target`; Rust's syntax in-process
//! (syn). Each run checks the buffer's text (unsaved edits included) in the background,
//! one run at a time, after a typing pause: a reminder while typing, no language server.
//! Results stay on their lines while other lines are edited: a problem whose line text
//! changed hides until the next check.

use crate::editor::Buffer;
use crate::nav::{self, Lang};
use crate::panel::{App, Doc};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Typing pause before a shell script is checked again.
const SHELL_PAUSE: Duration = Duration::from_millis(500);
/// Before a C/C++ file is (a compiler run, heavier), and a Rust file's syntax.
const C_PAUSE: Duration = Duration::from_millis(800);
const RUST_PAUSE: Duration = Duration::from_millis(400);
/// A run still going after this frees the slot for other files (a cold disk, a huge
/// file); its answer is still taken when it comes, and it is not started again.
const GIVE_UP: Duration = Duration::from_secs(30);
/// How far a problem looks for its line after lines above it were added or removed.
const DRIFT: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Sev {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diag {
    pub line: usize,
    /// Byte range in the line; empty at a line's end.
    pub start: usize,
    pub end: usize,
    pub sev: Sev,
    pub msg: String,
    /// The line as checked, to find it again after edits elsewhere.
    text: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
    Shell,
    C,
    Rust,
}

#[derive(Default, Debug, PartialEq)]
struct Found {
    diags: Vec<Diag>,
    /// Why some or all of the file went unchecked (no shellcheck, a header not found).
    hint: Option<String>,
}

struct Checked {
    ver: u64,
    diags: Vec<Diag>,
    /// Each problem's line in the current text (`placed_ver`), None while hidden.
    placed: Vec<Option<usize>>,
    placed_ver: u64,
}

struct Job {
    key: String,
    ver: u64,
    since: Instant,
    rx: Receiver<Found>,
}

pub struct Lints {
    /// Off in unit tests unless a test turns it on (no compiler runs behind them).
    pub on: bool,
    files: HashMap<String, Checked>,
    /// The version last seen per document and when it changed (the typing pause).
    seen: HashMap<String, (u64, Instant)>,
    job: Option<Job>,
    /// A run past GIVE_UP: its answer, when it comes.
    late: Option<Job>,
    due: Option<Instant>,
    told: HashSet<String>,
    /// Marks an API client put on a document's lines (`ui.decorate`), shown as problems.
    pub marks: HashMap<String, Vec<Diag>>,
    /// What `cargo check` found after a save (types, borrows), per document, placed as
    /// the others; the run going; workspaces to check again; each Rust document's saved
    /// version seen; workspaces checked this session.
    saved: HashMap<String, Checked>,
    cargo: Option<CargoRun>,
    cargo_due: Vec<PathBuf>,
    cargo_seen: HashMap<String, u64>,
    cargo_done: HashSet<PathBuf>,
    /// Rust workspaces asked about (true: `cargo check` on save), kept in the state folder.
    cargo_ok: Option<HashMap<String, bool>>,
}

/// A `cargo check` running for a workspace.
struct CargoRun {
    root: PathBuf,
    since: Instant,
    rx: Receiver<Result<Vec<(PathBuf, Diag)>, String>>,
}

/// `cargo check` runs only with this much memory free (it can take a lot, briefly).
const CARGO_MIN_MB: u64 = 1500;
/// A `cargo check` still going after this is let go (it ends on its own).
const CARGO_GIVE_UP: Duration = Duration::from_secs(600);

impl Default for Lints {
    fn default() -> Self {
        Lints {
            on: !cfg!(test),
            files: HashMap::new(),
            seen: HashMap::new(),
            job: None,
            late: None,
            due: None,
            told: HashSet::new(),
            marks: HashMap::new(),
            saved: HashMap::new(),
            cargo: None,
            cargo_due: Vec::new(),
            cargo_seen: HashMap::new(),
            cargo_done: HashSet::new(),
            cargo_ok: None,
        }
    }
}

impl Diag {
    /// A mark of an API client's: bytes `start..end` of `line`, its severity and note.
    pub fn mark(line: usize, start: usize, end: usize, sev: Sev, msg: String) -> Diag {
        Diag { line, start, end, sev, msg, text: String::new() }
    }
}

impl Lints {
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.cargo.is_some()
    }

    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    /// The problems shown in `key`'s editor: (line, start, end, severity).
    pub fn shown(&self, key: &str) -> Vec<(usize, usize, usize, Sev)> {
        self.all(key).into_iter().map(|(l, s, e, sev, _)| (l, s, e, sev)).collect()
    }

    /// Every problem shown in `key`'s editor: (line, start, end, severity, message).
    pub fn all(&self, key: &str) -> Vec<(usize, usize, usize, Sev, &str)> {
        let checked = self.files.get(key).into_iter().chain(self.saved.get(key)).flat_map(|c| c.diags.iter().zip(&c.placed)).filter_map(|(d, l)| Some(((*l)?, d.start, d.end, d.sev, d.msg.as_str())));
        let marks = self.marks.get(key).into_iter().flatten().map(|d| (d.line, d.start, d.end, d.sev, d.msg.as_str()));
        checked.chain(marks).collect()
    }

    /// The problems covering byte `col` of `line`: where the worst starts, and every
    /// message, worst first.
    pub fn at(&self, key: &str, line: usize, col: usize) -> Option<(usize, Vec<(Sev, String)>)> {
        let covers = |d: &Diag| d.start <= col && col < d.end.max(d.start + 1);
        let mut hits: Vec<&Diag> = self.files.get(key).into_iter().chain(self.saved.get(key)).flat_map(|c| c.diags.iter().zip(&c.placed))
            .filter(|(d, l)| **l == Some(line) && covers(d))
            .map(|(d, _)| d)
            .chain(self.marks.get(key).into_iter().flatten().filter(|d| d.line == line && covers(d)))
            .collect();
        hits.sort_by_key(|d| std::cmp::Reverse(d.sev));
        let first = hits.first()?;
        Some((first.start, hits.iter().map(|d| (d.sev, d.msg.clone())).collect()))
    }
}

fn tool(key: &str, b: &Buffer) -> Option<Tool> {
    let path = key.strip_prefix("file:")?;
    if b.readonly.is_some() {
        return None;
    }
    match nav::lang_for(path) {
        Lang::C => Some(Tool::C),
        Lang::Rust => Some(Tool::Rust),
        Lang::Shell if !path.ends_with(".zsh") => Some(Tool::Shell),
        Lang::Other if Path::new(path).extension().is_none() => b.lines.first().and_then(|l| shebang(l)).map(|_| Tool::Shell),
        _ => None,
    }
}

/// The shell a `#!` line names, when shellcheck knows it.
fn shebang(l: &str) -> Option<&str> {
    let mut words = l.strip_prefix("#!")?.split_whitespace();
    let mut prog = words.next()?.rsplit('/').next()?;
    if prog == "env" {
        prog = words.find(|w| !w.starts_with('-'))?;
    }
    ["sh", "bash", "dash", "ksh"].contains(&prog).then_some(prog)
}

/// Collect a finished run, then start the next one a shown editor needs. True: redraw.
pub fn tick(app: &mut App) -> bool {
    if !app.lints.on {
        return false;
    }
    let mut changed = false;
    if let Some(late) = &app.lints.late {
        match late.rx.try_recv() {
            Ok(found) => {
                let late = app.lints.late.take().unwrap();
                app.lints.files.insert(late.key, Checked { ver: late.ver, diags: found.diags, placed: Vec::new(), placed_ver: u64::MAX });
                changed = true;
            }
            Err(TryRecvError::Disconnected) => app.lints.late = None,
            Err(TryRecvError::Empty) => {}
        }
    }
    if let Some(job) = &app.lints.job {
        match job.rx.try_recv() {
            Ok(found) => {
                let job = app.lints.job.take().unwrap();
                if let Some(h) = found.hint.as_ref().filter(|h| app.lints.told.insert(h.to_string())) {
                    app.msg = h.clone();
                }
                app.lints.files.insert(job.key, Checked { ver: job.ver, diags: found.diags, placed: Vec::new(), placed_ver: u64::MAX });
                changed = true;
            }
            Err(TryRecvError::Disconnected) => app.lints.job = None,
            Err(TryRecvError::Empty) if job.since.elapsed() > GIVE_UP => {
                app.lints.late = app.lints.job.take();
                let note = "Still checking for problems after 30 s (a slow disk?): they show when it ends";
                if app.lints.told.insert(note.to_string()) {
                    app.msg = note.to_string();
                }
            }
            Err(TryRecvError::Empty) => {}
        }
    }
    let docs = &app.docs;
    app.lints.files.retain(|k, _| docs.contains_key(k));
    app.lints.seen.retain(|k, _| docs.contains_key(k));
    // Problems follow their lines through edits elsewhere.
    app.lints.saved.retain(|k, _| docs.contains_key(k));
    for (key, c) in app.lints.files.iter_mut().chain(app.lints.saved.iter_mut()) {
        let Some(Doc::Text(b)) = app.docs.get(key) else { continue };
        if c.placed_ver != b.version() {
            c.placed = c.diags.iter().map(|d| place(d, &b.lines)).collect();
            c.placed_ver = b.version();
        }
    }
    app.lints.due = None;
    let shown: Vec<String> = app.groups.iter().filter_map(|g| g.tabs.get(g.active)).map(|t| t.key.clone()).collect();
    let now = Instant::now();
    for key in shown {
        let Some(Doc::Text(b)) = app.docs.get(&key) else { continue };
        let Some(tool) = tool(&key, b) else { continue };
        let ver = b.version();
        let running = |j: &Option<Job>| j.as_ref().is_some_and(|j| j.key == key && j.ver == ver);
        if app.lints.files.get(&key).is_some_and(|c| c.ver == ver) || running(&app.lints.job) || running(&app.lints.late) {
            continue;
        }
        // First sight (opened): at once. Then the typing pause, or for C/C++ the save.
        let seen = app.lints.seen.entry(key.clone()).or_insert((ver, now - SHELL_PAUSE));
        if seen.0 != ver {
            *seen = (ver, now);
        }
        let ready = seen.1 + match tool {
            Tool::Shell => SHELL_PAUSE,
            Tool::C => C_PAUSE,
            Tool::Rust => RUST_PAUSE,
        };
        if ready > now {
            app.lints.due = Some(app.lints.due.map_or(ready, |d| d.min(ready)));
            continue;
        }
        if app.lints.job.is_some() {
            continue;
        }
        let rel = &key["file:".len()..];
        let path = app.root.join(rel);
        let top = app.git_root.clone().unwrap_or_else(|| app.root.clone());
        let text = b.lines.join("\n") + "\n";
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(run(tool, &path, &top, &text));
        });
        app.lints.job = Some(Job { key, ver, since: now, rx });
    }
    changed |= cargo_tick(app);
    changed
}

/// The Rust workspace `file` is in: the outermost Cargo.toml with `[workspace]` above it,
/// else its nearest Cargo.toml's folder.
pub fn cargo_root(file: &Path) -> Option<PathBuf> {
    let krate = file.ancestors().skip(1).find(|d| d.join("Cargo.toml").is_file())?;
    let ws = krate.ancestors().filter(|d| std::fs::read_to_string(d.join("Cargo.toml")).is_ok_and(|t| t.contains("[workspace]"))).last();
    Some(ws.unwrap_or(krate).to_path_buf())
}

fn cargo_ok_file() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("termcode-rust-check-{}.json", std::process::id()));
    }
    crate::data_dir(true).join("rust-check.json")
}

impl Lints {
    /// Whether `cargo check` may run in workspace `root` (None: not asked yet).
    pub fn cargo_allowed(&mut self, root: &Path) -> Option<bool> {
        let ok = self.cargo_ok.get_or_insert_with(|| std::fs::read(cargo_ok_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
        ok.get(&root.display().to_string()).copied()
    }

    /// The user's answer for `root`, kept; a yes checks it now.
    pub fn set_cargo_allowed(&mut self, root: &Path, yes: bool) {
        self.cargo_allowed(root);
        let ok = self.cargo_ok.get_or_insert_with(HashMap::new);
        ok.insert(root.display().to_string(), yes);
        let _ = serde_json::to_vec_pretty(ok).map(|j| std::fs::write(cargo_ok_file(), j));
        if yes && !self.cargo_due.contains(&root.to_path_buf()) {
            self.cargo_due.push(root.to_path_buf());
        }
    }
}

/// Free memory, MB: the share of RAM macOS counts as free (`memory_pressure`'s figure).
#[cfg(target_os = "macos")]
fn mem_available_mb() -> Option<u64> {
    fn read<T: Default>(name: &std::ffi::CStr) -> Option<T> {
        let mut v = T::default();
        let mut len = std::mem::size_of::<T>();
        // SAFETY: v is a T-sized buffer and len says so.
        let r = unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut v).cast(), &mut len, std::ptr::null_mut(), 0) };
        (r == 0 && len == std::mem::size_of::<T>()).then_some(v)
    }
    let percent: u32 = read(c"kern.memorystatus_level")?;
    let total: u64 = read(c"hw.memsize")?;
    Some(total / 100 * u64::from(percent) / (1024 * 1024))
}

/// Free memory, MB (MemAvailable).
#[cfg(not(target_os = "macos"))]
fn mem_available_mb() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    info.lines().find_map(|l| l.strip_prefix("MemAvailable:")).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()).map(|kb| kb / 1024)
}

/// Rust files saved (or first opened): their workspace checked with `cargo check` in the
/// background, one run at a time; its problems put on the files they are in. Asked once
/// per workspace (it runs the project's build scripts and macros); never in an untrusted
/// repository, with error checking off, or with less than CARGO_MIN_MB free.
fn cargo_tick(app: &mut App) -> bool {
    let mut changed = false;
    if let Some(run) = &app.lints.cargo {
        match run.rx.try_recv() {
            Ok(res) => {
                let run = app.lints.cargo.take().unwrap();
                changed = true;
                match res {
                    Ok(found) => {
                        let root = std::fs::canonicalize(&app.root).unwrap_or_else(|_| app.root.clone());
                        // This workspace's files: their old results go, the new ones come.
                        app.lints.saved.retain(|k, _| !app.root.join(&k["file:".len()..]).starts_with(&run.root));
                        let mut by: HashMap<String, Vec<Diag>> = HashMap::new();
                        for (path, d) in found {
                            let Ok(rel) = std::fs::canonicalize(&path).unwrap_or(path).strip_prefix(&root).map(|r| r.display().to_string()) else { continue };
                            by.entry(format!("file:{rel}")).or_default().push(d);
                        }
                        for (key, diags) in by {
                            let ver = match app.docs.get(&key) {
                                Some(Doc::Text(b)) => b.version(),
                                _ => continue,
                            };
                            app.lints.saved.insert(key, Checked { ver, diags, placed: Vec::new(), placed_ver: u64::MAX });
                        }
                    }
                    Err(e) => {
                        if app.lints.told.insert(e.clone()) {
                            app.msg = e;
                        }
                    }
                }
            }
            Err(TryRecvError::Disconnected) => app.lints.cargo = None,
            Err(TryRecvError::Empty) if run.since.elapsed() > CARGO_GIVE_UP => app.lints.cargo = None,
            Err(TryRecvError::Empty) => {}
        }
    }
    // Saves (and first sights) since: their workspaces due.
    let rust: Vec<(String, u64)> = app.docs.iter().filter(|(k, _)| k.ends_with(".rs") && k.starts_with("file:")).filter_map(|(k, d)| match d {
        Doc::Text(b) if !b.dirty() && b.readonly.is_none() => Some((k.clone(), b.version())),
        _ => None,
    }).collect();
    app.lints.cargo_seen.retain(|k, _| app.docs.contains_key(k));
    for (key, ver) in rust {
        let prev = app.lints.cargo_seen.insert(key.clone(), ver);
        if prev == Some(ver) {
            continue;
        }
        let Some(root) = cargo_root(&app.root.join(&key["file:".len()..])) else { continue };
        if (prev.is_some() || !app.lints.cargo_done.contains(&root)) && !app.lints.cargo_due.contains(&root) {
            app.lints.cargo_due.push(root);
        }
    }
    if app.lints.cargo.is_some() || app.lints.cargo_due.is_empty() || !app.settings.error_checking || app.untrusted.is_some() {
        return changed;
    }
    let root = app.lints.cargo_due[0].clone();
    match app.lints.cargo_allowed(&root) {
        Some(false) => {
            app.lints.cargo_due.remove(0);
            return changed;
        }
        None => {
            // Asked once, when no other window is up.
            if app.prompt.is_none() {
                let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
                app.prompt = Some(crate::panel::Prompt {
                    text: vec![format!("Check {name} with cargo on save?"), "It builds the project: its build scripts and macros run.".into()],
                    buttons: vec![("Check on save".into(), crate::panel::PAct::RustCheck(root.clone(), true)), ("Syntax only".into(), crate::panel::PAct::RustCheck(root, false))],
                    sel: 1,
                });
                changed = true;
            }
            return changed;
        }
        Some(true) => {}
    }
    if mem_available_mb().is_some_and(|mb| mb < CARGO_MIN_MB) {
        let note = format!("Rust check skipped: under {CARGO_MIN_MB} MB of memory free");
        if app.lints.told.insert(note.clone()) {
            app.msg = note;
        }
        return changed;
    }
    app.lints.cargo_due.remove(0);
    app.lints.cargo_done.insert(root.clone());
    let (tx, rx) = mpsc::channel();
    let at = root.clone();
    std::thread::spawn(move || {
        let _ = tx.send(cargo_check(&at));
    });
    app.lints.cargo = Some(CargoRun { root, since: Instant::now(), rx });
    changed
}

/// `cargo check` in `root` (two jobs at most): each compiler message's main place, as a
/// problem on that file's line.
fn cargo_check(root: &Path) -> Result<Vec<(PathBuf, Diag)>, String> {
    let out = Command::new("cargo")
        .args(["check", "--message-format=json", "--quiet", "-j2"])
        .current_dir(root)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("Rust problems need cargo: {e}"))?;
    Ok(parse_cargo(root, &String::from_utf8_lossy(&out.stdout)))
}

/// `cargo check --message-format=json`'s compiler messages: (file, problem) each.
fn parse_cargo(root: &Path, out: &str) -> Vec<(PathBuf, Diag)> {
    let mut found = Vec::new();
    let mut texts: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["reason"] != "compiler-message" {
            continue;
        }
        let m = &v["message"];
        let sev = match m["level"].as_str() {
            Some("error") => Sev::Error,
            Some("warning") => Sev::Warning,
            _ => continue,
        };
        let Some(span) = m["spans"].as_array().into_iter().flatten().find(|s| s["is_primary"] == true) else { continue };
        let Some(file) = span["file_name"].as_str() else { continue };
        let path = root.join(file);
        let lines = texts.entry(path.clone()).or_insert_with(|| std::fs::read_to_string(&path).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default());
        let line = span["line_start"].as_u64().unwrap_or(1).saturating_sub(1) as usize;
        let text = lines.get(line).cloned().unwrap_or_default();
        let byte = |col: u64| text.char_indices().nth(col.saturating_sub(1) as usize).map_or(text.len(), |(i, _)| i);
        let start = byte(span["column_start"].as_u64().unwrap_or(1));
        let end = if span["line_end"] == span["line_start"] { byte(span["column_end"].as_u64().unwrap_or(1)) } else { text.len() };
        let mut msg = m["message"].as_str().unwrap_or_default().to_string();
        if let Some(label) = span["label"].as_str().filter(|l| !l.is_empty()) {
            msg = format!("{msg}: {label}");
        }
        found.push((path, Diag { line, start, end: end.max(start + 1), sev, msg, text }));
    }
    found
}

/// Where `d`'s line is now: its own index if unchanged, else the nearest same line.
fn place(d: &Diag, lines: &[String]) -> Option<usize> {
    if lines.get(d.line) == Some(&d.text) {
        return Some(d.line);
    }
    (1..=DRIFT).find_map(|k| {
        [d.line.checked_sub(k), Some(d.line + k)].into_iter().flatten().find(|&l| lines.get(l) == Some(&d.text))
    })
}

fn run(tool: Tool, path: &Path, top: &Path, text: &str) -> Found {
    let dir = path.parent().unwrap_or(top).to_path_buf();
    let lines: Vec<&str> = text.lines().collect();
    match tool {
        Tool::Shell => {
            let mut args = vec!["-f".to_string(), "json1".into()];
            // From stdin shellcheck cannot see the name: give it the dialect the extension says.
            if let Some(ext) = path.extension().and_then(|e| e.to_str()).filter(|e| ["bash", "ksh", "dash"].contains(e)) {
                args.extend(["-s".into(), ext.into()]);
            }
            args.push("-".into());
            match exec("shellcheck", &args, &dir, text) {
                Ok((out, _)) => Found { diags: parse_shellcheck(&out, &lines), hint: None },
                Err(e) => Found { diags: Vec::new(), hint: Some(format!("Shell problems need shellcheck: {e}")) },
            }
        }
        Tool::C => check_c(path, &dir, top, text, &lines),
        Tool::Rust => rust_syntax(text, &lines),
    }
}

/// A Rust file's syntax errors (syn: the first it finds), as rustc would place them.
fn rust_syntax(text: &str, lines: &[&str]) -> Found {
    let Err(err) = syn::parse_file(text) else { return Found::default() };
    let byte = |line: &str, col: usize| line.char_indices().nth(col).map_or(line.len(), |(i, _)| i);
    let diags = err
        .into_iter()
        .map(|e| {
            let (a, z) = (e.span().start(), e.span().end());
            let line = a.line.saturating_sub(1).min(lines.len().saturating_sub(1));
            let l = lines.get(line).copied().unwrap_or("");
            let start = byte(l, a.column);
            let end = if z.line == a.line { byte(l, z.column) } else { l.len() };
            Diag { line, start, end: end.max(start + 1), sev: Sev::Error, msg: e.to_string(), text: l.to_string() }
        })
        .collect();
    Found { diags, hint: None }
}

fn check_c(path: &Path, dir: &Path, top: &Path, text: &str, lines: &[&str]) -> Found {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let header = matches!(ext, "h" | "H" | "hh" | "hpp" | "hxx" | "inl");
    let quote = ["-iquote".to_string(), dir.to_string_lossy().into_owned()];
    let db = compile_db(path, top);
    let (cc, args, cwd) = match &db {
        Some(e) => (e.cc.clone(), e.args.clone(), e.dir.clone()),
        None => {
            let inc = |p: &Path| ["-I".to_string(), p.to_string_lossy().into_owned()];
            let args = [inc(dir), inc(&top.join("include")), inc(top)].concat();
            ("clang".to_string(), args, dir.to_path_buf())
        }
    };
    let cpp = matches!(ext, "C" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "inl") || db.as_ref().is_some_and(|e| e.cpp);
    // gcc takes no headers on stdin; it checks one as a source file instead.
    let clang = Path::new(&cc).file_name().is_some_and(|n| n.to_string_lossy().contains("clang"));
    let lang = match (cpp, header && clang) {
        (false, false) => "c",
        (false, true) => "c-header",
        (true, false) => "c++",
        (true, true) => "c++-header",
    };
    let (cc, mut args) = stand_in(cc, args);
    args.extend(quote);
    args.extend(["-fsyntax-only", "-x", lang, "-"].map(String::from));
    // clang standing in for gcc names the gcc-only flags it refuses: drop them, again.
    for _ in 0..3 {
        let err = match exec(&cc, &args, &cwd, text) {
            Ok((_, err)) => err,
            Err(e) => return Found { diags: Vec::new(), hint: Some(format!("C/C++ problems need {cc}: {e}")) },
        };
        let refused: Vec<String> = err.lines().filter_map(|l| l.split_once("error: unknown argument: '")).map(|(_, a)| a.trim_end_matches('\'').to_string()).collect();
        if refused.is_empty() {
            return parse_compiler(&err, lines, db.is_some(), &cc);
        }
        args.retain(|a| !refused.contains(a));
    }
    Found { diags: Vec::new(), hint: Some(format!("C/C++: {cc} refuses this file's flags")) }
}

/// The compiler to run: `cc` itself when installed, else clang aimed at its target (a
/// cross gcc's name starts with the triple), without gcc's ABI and plugin flags and with
/// clang's own `stdarg.h` and friends (the kernel's `-nostdinc` hides them).
fn stand_in(cc: String, args: Vec<String>) -> (String, Vec<String>) {
    if let Some(name) = known_compiler(&cc) {
        return (name.to_string(), args);
    }
    let name = cc.rsplit('/').next().unwrap_or(&cc);
    let triple = ["-gcc", "-cc", "-g++", "-c++", "-clang"].iter().find_map(|s| name.strip_suffix(s)).filter(|t| t.contains('-'));
    let mut out: Vec<String> = triple.map(|t| format!("--target={t}")).into_iter().collect();
    out.extend(args.into_iter().filter(|a| !a.starts_with("-mabi=") && !a.starts_with("-fplugin")));
    out.push("-Wno-unknown-warning-option".into());
    if out.iter().any(|a| a == "-nostdinc") {
        static RES: OnceLock<Option<String>> = OnceLock::new();
        let res = RES.get_or_init(|| {
            let o = Command::new("clang").arg("-print-resource-dir").output().ok()?;
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
        });
        if let Some(r) = res {
            out.extend(["-isystem".to_string(), format!("{r}/include")]);
        }
    }
    ("clang".into(), out)
}

/// The compiler a database names, when termcode may run it: a compiler's name (gcc, g++,
/// cc, c++, clang, clang++, with a target prefix or a version suffix) found on PATH by
/// that name. Never the path the database gives: that can be a program of the project's.
fn known_compiler(cc: &str) -> Option<&str> {
    let name = cc.rsplit('/').next()?;
    let core = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-');
    let core = core.rsplit('-').next()?;
    matches!(core, "gcc" | "g++" | "cc" | "c++" | "clang" | "clang++").then_some(name).filter(|n| on_path(n))
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

/// Runs `prog args` in `dir` with `input` on stdin, at a low priority: (stdout, stderr).
fn exec(prog: &str, args: &[String], dir: &Path, input: &str) -> Result<(String, String), String> {
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: nice(2) and setrlimit(2) are async-signal-safe. The limits keep a checker
    // fed a bottomless input (`-include /dev/zero`) from taking the machine with it.
    unsafe {
        cmd.pre_exec(|| {
            libc::nice(10);
            let cpu = libc::rlimit { rlim_cur: 30, rlim_max: 30 };
            let mem = libc::rlimit { rlim_cur: 4 << 30, rlim_max: 4 << 30 };
            libc::setrlimit(libc::RLIMIT_CPU, &cpu);
            libc::setrlimit(libc::RLIMIT_AS, &mem);
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "not installed".into() } else { e.to_string() })?;
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_string();
    // A writer thread: a big file never deadlocks against a full stderr pipe.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    Ok((String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}

fn diag(lines: &[&str], line: usize, start: usize, end: usize, sev: Sev, msg: String) -> Option<Diag> {
    let text = *lines.get(line)?;
    let fix = |b: usize| {
        let mut b = b.min(text.len());
        while !text.is_char_boundary(b) {
            b -= 1;
        }
        b
    };
    let (start, end) = (fix(start), fix(end));
    Some(Diag { line, start, end: end.max(start), sev, msg, text: text.to_string() })
}

/// Byte offset of 1-based character column `col` (shellcheck counts a tab as one).
fn char_byte(line: &str, col: usize) -> usize {
    line.char_indices().nth(col.saturating_sub(1)).map_or(line.len(), |(i, _)| i)
}

fn parse_shellcheck(json: &str, lines: &[&str]) -> Vec<Diag> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    let num = |c: &serde_json::Value, k: &str| c[k].as_u64().unwrap_or(1) as usize;
    v["comments"].as_array().into_iter().flatten().filter_map(|c| {
        let line = num(c, "line").checked_sub(1)?;
        let text = lines.get(line)?;
        let start = char_byte(text, num(c, "column"));
        let end = if num(c, "endLine") - 1 == line { char_byte(text, num(c, "endColumn")) } else { text.len() };
        let sev = match c["level"].as_str() {
            Some("error") => Sev::Error,
            Some("warning") => Sev::Warning,
            _ => Sev::Info,
        };
        let msg = format!("{} shellcheck(SC{})", c["message"].as_str().unwrap_or(""), num(c, "code"));
        diag(lines, line, start, end, sev, msg)
    }).collect()
}

/// `<stdin>:LINE:COL: error|warning|note: message` from gcc or clang; notes join the
/// problem before them, problems in other files are left out. A header not found ends
/// the check (both compilers stop there): it becomes the hint instead of a problem.
fn parse_compiler(err: &str, lines: &[&str], db: bool, cc: &str) -> Found {
    let mut found = Found::default();
    let mut driver = None;
    for l in err.lines() {
        let Some(rest) = l.strip_prefix("<stdin>:") else {
            // A header that includes one not found stops the check just the same.
            if let Some((_, m)) = l.split_once(": fatal error: ").filter(|(_, m)| m.contains("file not found") || m.ends_with("No such file or directory")) {
                found.hint = Some(missing(m, db));
                continue;
            }
            // The driver's own complaint (`clang: error: unknown argument …`), not a file's.
            let prog = l.split_once(": error: ").or_else(|| l.split_once(": fatal error: ")).map(|(p, _)| p);
            if driver.is_none() && prog.is_some_and(|p| !p.contains(':')) {
                driver = Some(l.trim().to_string());
            }
            continue;
        };
        let mut it = rest.splitn(3, ':');
        let (Some(Ok(line)), Some(Ok(col)), Some(body)) = (it.next().map(str::parse::<usize>), it.next().map(str::parse::<usize>), it.next()) else { continue };
        let body = body.trim_start();
        let (sev, msg) = if let Some(m) = body.strip_prefix("fatal error: ").or_else(|| body.strip_prefix("error: ")) {
            (Some(Sev::Error), m)
        } else if let Some(m) = body.strip_prefix("warning: ") {
            (Some(Sev::Warning), m)
        } else if let Some(m) = body.strip_prefix("note: ") {
            (None, m)
        } else {
            continue;
        };
        let Some(sev) = sev else {
            if let Some(d) = found.diags.last_mut() {
                d.msg.push_str(&format!("\nnote: {msg}"));
            }
            continue;
        };
        if msg.contains("file not found") || msg.ends_with("No such file or directory") {
            found.hint = Some(missing(msg, db));
            continue;
        }
        if msg.contains("#pragma once in main file") {
            continue;
        }
        let Some(text) = line.checked_sub(1).and_then(|i| lines.get(i)) else { continue };
        let start = col.saturating_sub(1).min(text.len());
        let word = text[start..].bytes().take_while(|c| c.is_ascii_alphanumeric() || *c == b'_').count();
        let end = start + if word > 0 { word } else { text[start..].chars().next().map_or(0, char::len_utf8) };
        let tag = Path::new(cc).file_name().map_or(cc.into(), |n| n.to_string_lossy());
        if let Some(d) = diag(lines, line - 1, start, end, sev, format!("{msg} {tag}")) {
            found.diags.push(d);
        }
    }
    if found.diags.is_empty() && found.hint.is_none() {
        found.hint = driver.map(|d| format!("C/C++: {d}"));
    }
    found
}

fn missing(msg: &str, db: bool) -> String {
    let what = msg.trim_end_matches(" file not found").split(": No such").next().unwrap_or(msg).trim_matches(['\'', '"']);
    if db {
        format!("C/C++: {what} not found, the rest of the file is unchecked")
    } else {
        format!("C/C++: {what} not found; build the project or add a compile_commands.json")
    }
}

/// What `compile_commands.json` says about a file: its compiler, flags and folder.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    cc: String,
    args: Vec<String>,
    dir: PathBuf,
    cpp: bool,
}

/// The nearest `compile_commands.json` (or `build/compile_commands.json`) from the file's
/// folder up to `top`, and its entry for the file, else for a file in the same folder
/// (a header has none of its own).
fn compile_db(path: &Path, top: &Path) -> Option<Entry> {
    let Some(db) = path.ancestors().skip(1).take_while(|d| d.starts_with(top)).find_map(|d| {
        [d.join("compile_commands.json"), d.join("build/compile_commands.json")].into_iter().find(|p| p.is_file())
    }) else {
        return kbuild(path).or_else(|| kbuild(&built_copy(path)?));
    };
    let mtime = std::fs::metadata(&db).and_then(|m| m.modified()).ok()?;
    type Cache = Mutex<HashMap<(PathBuf, PathBuf), (SystemTime, Option<Entry>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (db.clone(), path.to_path_buf());
    if let Some((t, e)) = cache.lock().unwrap().get(&key) {
        if *t == mtime {
            return e.clone();
        }
    }
    // ponytail: parses the whole database per file opened (a kernel's is ~30 MB, well
    // under a second); index it once if that shows up.
    let text = std::fs::read_to_string(&db).ok()?;
    let entry = find_entry(&text, path);
    cache.lock().unwrap().insert(key, (mtime, entry.clone()));
    entry
}

fn find_entry(json: &str, path: &Path) -> Option<Entry> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let want = clean(path);
    let mut same_dir = None;
    for e in v.as_array()? {
        let dir = PathBuf::from(e["directory"].as_str().unwrap_or("/"));
        let Some(file) = e["file"].as_str() else { continue };
        let file = clean(&dir.join(file));
        let exact = file == want;
        if !exact && (same_dir.is_some() || file.parent() != want.parent()) {
            continue;
        }
        let args: Vec<String> = match (e["arguments"].as_array(), e["command"].as_str()) {
            (Some(a), _) => a.iter().filter_map(|s| s.as_str().map(String::from)).collect(),
            (None, Some(c)) => split_command(c),
            _ => continue,
        };
        let Some((cc, args)) = compile_args(&args, &file, &dir) else { continue };
        let cpp = file.extension().is_some_and(|x| ["C", "cc", "cpp", "cxx"].contains(&x.to_string_lossy().as_ref()));
        let entry = Entry { cc, args, dir, cpp };
        if exact {
            return Some(entry);
        }
        same_dir = Some(entry);
    }
    same_dir
}

/// A built kernel's flags for the file: Kbuild leaves `dir/.name.o.cmd` (`cmd_<object> :=
/// <command>`, run from the tree's top) beside each object. A header, or a file not built
/// in this configuration, borrows the nearest object's (the include paths are the tree's).
fn kbuild(path: &Path) -> Option<Entry> {
    let own = path.parent()?.join(format!(".{}.o.cmd", path.file_stem()?.to_string_lossy()));
    let compile = |cmd: &Path| -> Option<(PathBuf, String, String)> {
        let first = std::fs::read_to_string(cmd).ok()?.lines().next()?.to_string();
        let (obj, command) = first.strip_prefix("savedcmd_").or_else(|| first.strip_prefix("cmd_"))?.split_once(" := ")?;
        (command.contains(" -c ")).then(|| (cmd.parent().unwrap().to_path_buf(), obj.to_string(), command.to_string()))
    };
    let near = || path.ancestors().skip(1).take(8).find_map(|dir| {
        let mut cmds: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| { let n = n.to_string_lossy(); n.starts_with('.') && n.ends_with(".o.cmd") && !n.ends_with(".mod.o.cmd") }))
            .collect();
        cmds.sort();
        cmds.iter().find_map(|c| compile(c))
    });
    let (dir, obj, command) = compile(&own).or_else(near)?;
    let depth = Path::new(&obj).parent()?.components().count();
    let top = dir.ancestors().nth(depth)?.to_path_buf();
    let (cc, args) = compile_args(&split_command(&command), path, &top)?;
    let cpp = matches!(path.extension().and_then(|e| e.to_str()), Some("cc" | "cpp" | "cxx"));
    Some(Entry { cc, args, dir: top, cpp })
}

/// The same file in a built copy of its kernel tree, when the tree itself is not built
/// (a firmware build's `fetched/linux_5_10` inside the source it copies): the first
/// folder one or two levels in with a `.config` and `include/generated`.
fn built_copy(path: &Path) -> Option<PathBuf> {
    let kernel = |d: &Path| d.join("Kbuild").is_file() && d.join("arch").is_dir() && d.join("include/linux").is_dir();
    let top = path.ancestors().skip(1).find(|d| kernel(d))?;
    // Found once (not found: looked for again, it may be built since).
    static FOUND: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    let found = FOUND.get_or_init(Default::default);
    let known = found.lock().unwrap_or_else(|e| e.into_inner()).get(top).cloned();
    let copy = match known {
        Some(c) => c,
        None => {
            let dirs = |d: &Path| -> Vec<PathBuf> {
                let mut v: Vec<PathBuf> = std::fs::read_dir(d).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
                v.sort();
                v
            };
            let built = |d: &PathBuf| d.join(".config").is_file() && d.join("include/generated").is_dir();
            let c = dirs(top).into_iter().flat_map(|d| std::iter::once(d.clone()).chain(dirs(&d))).find(built)?;
            found.lock().unwrap_or_else(|e| e.into_inner()).insert(top.to_path_buf(), c.clone());
            c
        }
    };
    Some(copy.join(path.strip_prefix(top).ok()?))
}

/// `.` and `..` resolved without touching the disk.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// A compile command's words, POSIX shell quoting.
fn split_command(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut any) = (Vec::new(), String::new(), false);
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                any = true;
                cur.extend(it.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                any = true;
                while let Some(c) = it.next() {
                    match c {
                        '"' => break,
                        '\\' => match it.next() {
                            Some(n @ ('"' | '\\' | '$' | '`')) => cur.push(n),
                            Some(n) => cur.extend(['\\', n]),
                            None => {}
                        },
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                any = true;
                cur.extend(it.next());
            }
            c if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            // The compile ends here (Kbuild chains objtool and friends after it).
            ';' => break,
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The compiler and the flags that matter for a syntax check: no output, no dependency
/// files (a kernel's `-Wp,-MMD,…` would write one), no source file (the buffer comes on
/// stdin; a header borrowing a neighbour's command drops the neighbour too).
///
/// The database comes with the project, so only flags that shape how the code parses are
/// kept: include paths, defines, the language standard, the target, -f/-m/-W/-O. Anything
/// that loads or runs a program (-fplugin, -B, -wrapper, -specs, -Xclang, -mllvm, @file)
/// or writes a file (-o, dependency files, -aux-info, dumps, -save-temps) is left out, and
/// so is every input file.
fn compile_args(args: &[String], _file: &Path, _dir: &Path) -> Option<(String, Vec<String>)> {
    let mut it = args.iter();
    let mut cc = it.next()?;
    if ["ccache", "sccache", "distcc"].contains(&cc.rsplit('/').next().unwrap_or(cc)) {
        cc = it.next()?;
    }
    const SEPARATE: [&str; 11] = ["-I", "-D", "-U", "-include", "-imacros", "-isystem", "-iquote", "-idirafter", "-isysroot", "--sysroot", "-target"];
    let mut out = Vec::new();
    while let Some(a) = it.next() {
        if SEPARATE.contains(&a.as_str()) {
            if let Some(v) = it.next() {
                out.extend([a.clone(), v.clone()]);
            }
        } else if parses(a) {
            out.push(a.clone());
        }
        // Anything else is dropped; a dropped flag's own value goes with it, as an input.
    }
    Some((cc.clone(), out))
}

/// A flag that only changes how the code is read (see `compile_args`).
fn parses(a: &str) -> bool {
    const JOINED: [&str; 12] = ["-I", "-D", "-U", "-isystem", "-iquote", "-idirafter", "-std=", "--std=", "--target=", "--sysroot=", "-isysroot", "-O"];
    const EXACT: [&str; 8] = ["-nostdinc", "-nostdinc++", "-ansi", "-pedantic", "-pedantic-errors", "-w", "-undef", "-pthread"];
    // -f options that load plugins or write files (profiles, dumps, traces, stack usage).
    const BAD_F: [&str; 11] =
        ["plugin", "pass-plugin", "dump", "profile", "stack-usage", "callgraph-info", "opt-info", "save-optimization-record", "crash-diagnostics", "time-trace", "coverage"];
    if let Some(f) = a.strip_prefix("-f") {
        let f = f.strip_prefix("no-").unwrap_or(f);
        return !f.is_empty() && !BAD_F.iter().any(|b| f.starts_with(b));
    }
    if let Some(w) = a.strip_prefix("-W") {
        return !w.is_empty() && !["l,", "a,", "p,"].iter().any(|p| w.starts_with(p));
    }
    if let Some(m) = a.strip_prefix("-m") {
        return !m.is_empty() && m != "llvm";
    }
    EXACT.contains(&a) || JOINED.iter().any(|j| a.starts_with(j) && a.len() > j.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_memory_is_read_and_under_the_total() {
        let mb = mem_available_mb().expect("free memory");
        assert!(mb > 0 && mb < 16 * 1024 * 1024, "{mb} MB");
    }

    #[test]
    fn cargo_checks_messages_land_on_their_files_lines() {
        let dir = std::env::temp_dir().join(format!("termcode-cargo-parse-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("app/src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = [\"app\"]\n").unwrap();
        std::fs::write(dir.join("app/Cargo.toml"), "[package]\nname = \"app\"\n").unwrap();
        std::fs::write(dir.join("app/src/main.rs"), "fn main() {\n    let n: u8 = \"x\";\n}\n").unwrap();
        assert_eq!(cargo_root(&dir.join("app/src/main.rs")), Some(dir.clone()), "the workspace above the crate");
        let out = concat!(
            r#"{"reason":"compiler-artifact","package_id":"x"}"#, "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","spans":[{"file_name":"app/src/main.rs","line_start":2,"line_end":2,"column_start":17,"column_end":20,"is_primary":true,"label":"expected `u8`, found `&str`"}]}}"#, "\n",
            r#"{"reason":"compiler-message","message":{"level":"failure-note","message":"aborting","spans":[]}}"#, "\n",
        );
        let found = parse_cargo(&dir, out);
        assert_eq!(found.len(), 1);
        let (path, d) = &found[0];
        assert_eq!(path, &dir.join("app/src/main.rs"));
        assert_eq!((d.line, &d.text[d.start..d.end], d.sev), (1, "\"x\"", Sev::Error));
        assert_eq!(d.msg, "mismatched types: expected `u8`, found `&str`");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rust_syntax_errors_show_while_typing_where_rustc_would_put_them() {
        let text = "fn main() {\n    let x = ;\n}\n";
        let lines: Vec<&str> = text.lines().collect();
        let found = rust_syntax(text, &lines);
        assert_eq!(found.diags.len(), 1, "{found:?}");
        let d = &found.diags[0];
        assert_eq!((d.line, &lines[d.line][d.start..d.end], d.sev), (1, ";", Sev::Error), "{d:?}");
        assert!(!d.msg.is_empty());
        assert!(rust_syntax("fn ok() -> u8 { 1 }\n", &["fn ok() -> u8 { 1 }"]).diags.is_empty());
    }

    #[test]
    fn shellcheck_columns_are_characters_and_a_tab_counts_once() {
        let lines = ["#!/bin/bash", "\techo $foo", "\t\tls `pwd` é"];
        let json = r#"{"comments":[
            {"line":2,"endLine":2,"column":7,"endColumn":11,"level":"warning","code":2154,"message":"foo is referenced but not assigned."},
            {"line":2,"endLine":2,"column":7,"endColumn":11,"level":"info","code":2086,"message":"Double quote."},
            {"line":3,"endLine":3,"column":6,"endColumn":11,"level":"style","code":2006,"message":"Use $(...)."},
            {"line":3,"endLine":4,"column":12,"endColumn":2,"level":"error","code":1,"message":"x"}]}"#;
        let d = parse_shellcheck(json, &lines);
        assert_eq!(d.len(), 4);
        assert_eq!((&lines[1][d[0].start..d[0].end], d[0].sev), ("$foo", Sev::Warning));
        assert_eq!(d[0].msg, "foo is referenced but not assigned. shellcheck(SC2154)");
        assert_eq!((&lines[2][d[2].start..d[2].end], d[2].sev), ("`pwd`", Sev::Info));
        assert_eq!((&lines[2][d[3].start..d[3].end], d[3].sev), ("é", Sev::Error), "to the line's end, by characters");
    }

    #[test]
    fn compiler_output_becomes_problems_and_a_missing_header_a_hint() {
        let lines = ["int f(void){", "\tint x = y;", "\treturn ", "}"];
        let gcc = "<stdin>: In function 'f':\n<stdin>:2:10: error: 'y' undeclared (first use in this function)\n\
                   <stdin>:2:10: note: each undeclared identifier is reported only once\n<stdin>:4:1: error: expected expression before '}' token\n\
                   <stdin>:1:1: warning: #pragma once in main file\n./x.h:3:1: error: in a header\n";
        let f = parse_compiler(gcc, &lines, true, "/usr/bin/gcc");
        assert_eq!(f.diags.len(), 2);
        assert_eq!(&lines[1][f.diags[0].start..f.diags[0].end], "y");
        assert_eq!(f.diags[0].msg, "'y' undeclared (first use in this function) gcc\nnote: each undeclared identifier is reported only once");
        assert_eq!((&lines[3][f.diags[1].start..f.diags[1].end], f.diags[1].sev), ("}", Sev::Error));
        assert_eq!(f.hint, None);
        let clang = "<stdin>:1:10: fatal error: 'linux/module.h' file not found\n    1 | #include <linux/module.h>\n1 error generated.\n";
        let f = parse_compiler(clang, &["#include <linux/module.h>"], false, "clang");
        assert!(f.diags.is_empty());
        assert_eq!(f.hint.as_deref(), Some("C/C++: linux/module.h not found; build the project or add a compile_commands.json"));
        let f = parse_compiler("<stdin>:1:10: fatal error: foo.h: No such file or directory\n", &["#include \"foo.h\""], true, "gcc");
        assert_eq!(f.hint.as_deref(), Some("C/C++: foo.h not found, the rest of the file is unchecked"));
        let f = parse_compiler("clang: error: unknown argument: '-mrecord-mcount'\n", &["x"], true, "clang");
        assert_eq!(f.hint.as_deref(), Some("C/C++: clang: error: unknown argument: '-mrecord-mcount'"));
    }

    #[test]
    fn compile_commands_give_the_flags_without_outputs_or_dependency_files() {
        let cmd = r#"ccache gcc -Wp,-MMD,drivers/net/.a.o.d -nostdinc -I./include -DKBUILD_MODNAME='"a"' -D "X=a b" -c -o drivers/net/a.o drivers/net/a.c"#;
        let args = split_command(cmd);
        assert_eq!(args[5], "-DKBUILD_MODNAME=\"a\"");
        assert_eq!(args[7], "X=a b");
        let (cc, out) = compile_args(&args, Path::new("/k/drivers/net/a.c"), Path::new("/k")).unwrap();
        assert_eq!((cc.as_str(), out), ("gcc", ["-nostdinc", "-I./include", "-DKBUILD_MODNAME=\"a\"", "-D", "X=a b"].map(String::from).to_vec()));
        let json = r#"[{"directory":"/k","command":"gcc -O2 -c -o b.o drivers/b.c","file":"drivers/b.c"},
                       {"directory":"/k","arguments":["clang","-DA","-c","drivers/net/./a.c"],"file":"/k/drivers/net/a.c"}]"#;
        let e = find_entry(json, Path::new("/k/drivers/net/a.c")).unwrap();
        assert_eq!((e.cc.as_str(), e.args.clone(), e.dir.clone()), ("clang", vec!["-DA".to_string()], PathBuf::from("/k")));
        let h = find_entry(json, Path::new("/k/drivers/b.h")).unwrap();
        assert_eq!((h.cc.as_str(), h.args), ("gcc", vec!["-O2".to_string()]), "a header borrows its folder's flags");
        assert_eq!(find_entry(json, Path::new("/k/fs/c.c")), None);
    }

    fn screen(app: &mut App, t: &mut ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()).collect()
    }

    /// Runs the checks until the active editor's text is checked.
    fn settle(app: &mut App) -> String {
        let key = app.groups[app.ag].tabs[app.groups[app.ag].active].key.clone();
        let end = Instant::now() + Duration::from_secs(20);
        while !app.lints.files.contains_key(&key) || app.lints.job.is_some() {
            assert!(Instant::now() < end, "no result for {key}");
            tick(app);
            std::thread::sleep(Duration::from_millis(5));
        }
        tick(app);
        key
    }

    #[test]
    fn problems_are_underlined_counted_and_explained_on_hover() {
        use ratatui::crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        let have = |p: &str| Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success());
        if !have("shellcheck") || !have("clang") {
            eprintln!("skipped: shellcheck or clang not installed");
            return;
        }
        let root = std::env::temp_dir().join(format!("termcode-lint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("s.sh"), "#!/bin/bash\necho $foo\n").unwrap();
        std::fs::write(root.join("c.c"), "int f(void) {\n\treturn y;\n}\n").unwrap();
        let mut app = App::new(root.clone(), None);
        app.lints.on = true;
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 24)).unwrap();
        for (file, word, sev, count, msg) in [
            ("s.sh", "$foo", Sev::Warning, (0, 1), "foo is referenced but not assigned."),
            ("c.c", "y;", Sev::Error, (1, 0), "use of undeclared identifier"),
        ] {
            app.open_file(0, file, false);
            screen(&mut app, &mut t);
            let key = settle(&mut app);
            let shown = app.lints.shown(&key);
            let n = |s| shown.iter().filter(|p| p.3 == s).count();
            assert_eq!((n(Sev::Error), n(Sev::Warning)), count, "{file}");
            let rows = screen(&mut app, &mut t);
            let y = rows.iter().position(|r| r.contains(word)).unwrap() as u16;
            let x = rows[y as usize].split(word).next().unwrap().chars().count() as u16;
            let cell = &t.backend().buffer()[(x, y)];
            assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED), "{file}: {cell:?}");
            assert_eq!((cell.fg, cell.underline_color), (crate::theme::P.problem[sev as usize], crate::theme::P.problem[sev as usize]));
            assert!(!rows.last().unwrap().contains(crate::theme::icons().problem[1]), "the status bar stays clean: {}", rows.last().unwrap());
            // Rest the pointer on it: the card explains.
            crate::navui::on_mouse(&mut app, MouseEvent { kind: MouseEventKind::Moved, column: x, row: y, modifiers: KeyModifiers::NONE });
            app.nav.hover.due = Some(Instant::now());
            crate::hover::poll(&mut app);
            let text = screen(&mut app, &mut t).join("\n");
            assert!(text.contains(msg), "{file}: no card with {msg:?}:\n{text}");
            crate::hover::dismiss(&mut app);
        }
        // C/C++ waits for the save: an unsaved edit starts no compiler.
        let (b, tab) = app.ed().unwrap();
        b.insert(&mut tab.view, "// x\n");
        tick(&mut app);
        assert!(app.lints.job.is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_compile_database_supplies_include_paths_and_writes_nothing() {
        if !Command::new("gcc").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("skipped: gcc not installed");
            return;
        }
        let root = std::env::temp_dir().join(format!("termcode-lintdb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inc")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("inc/defs.h"), "#define ANSWER 42\n").unwrap();
        let db = format!(r#"[{{"directory":"{}","command":"gcc -Wp,-MMD,src/.a.o.d -MD -Iinc -c -o src/a.o src/a.c","file":"src/a.c"}}]"#, root.display());
        std::fs::write(root.join("compile_commands.json"), db).unwrap();
        let text = "#include <defs.h>\nint f(void) { return ANSWER + nope; }\n";
        let f = run(Tool::C, &root.join("src/a.c"), &root, text);
        assert_eq!(f.hint, None, "defs.h found through the database's -Iinc");
        assert_eq!(f.diags.len(), 1, "{f:?}");
        assert_eq!(&text.lines().nth(1).unwrap()[f.diags[0].start..f.diags[0].end], "nope");
        let left: Vec<_> = std::fs::read_dir(root.join("src")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(left.is_empty(), "no object or dependency file written: {left:?}");
        // Without the database the header is the hint.
        std::fs::remove_file(root.join("compile_commands.json")).unwrap();
        let f = run(Tool::C, &root.join("src/b.c"), &root, text);
        assert!(f.hint.is_some_and(|h| h.contains("defs.h")) && f.diags.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_built_kernel_gives_its_flags_and_a_missing_cross_gcc_becomes_clang() {
        let top = std::env::temp_dir().join(format!("termcode-kbuild-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(top.join("drivers/net")).unwrap();
        std::fs::write(top.join("drivers/net/.a.o.cmd"), "cmd_drivers/net/a.o := aarch64-x-linux-gnu-gcc -Wp,-MMD,drivers/net/.a.o.d -nostdinc \
            -I./include -mabi=lp64 -D KBUILD_MODNAME='\"a\"' -c -o drivers/net/a.o drivers/net/a.c; ./tools/objtool/objtool check drivers/net/a.o\n\
            source_drivers/net/a.o := drivers/net/a.c\n").unwrap();
        std::fs::write(top.join("drivers/net/.a.mod.o.cmd"), "cmd_drivers/net/a.mod.o := gcc -DMOD -c -o x.o x.c\n").unwrap();
        let want = ["-nostdinc", "-I./include", "-mabi=lp64", "-D", "KBUILD_MODNAME=\"a\""].map(String::from).to_vec();
        let e = kbuild(&top.join("drivers/net/a.c")).unwrap();
        assert_eq!((e.cc.as_str(), &e.args, &e.dir), ("aarch64-x-linux-gnu-gcc", &want, &top), "objtool after ';' left out");
        let h = kbuild(&top.join("drivers/net/a.h")).unwrap();
        assert_eq!(h.args, want, "a header borrows a.o's flags, never a.mod.o's, and drops a.c");
        std::fs::create_dir_all(top.join("drivers/net/dsa/x")).unwrap();
        assert_eq!(kbuild(&top.join("drivers/net/dsa/x/c.h")).map(|e| e.args), Some(want.clone()), "a driver not built borrows its parent folder's");
        assert_eq!(kbuild(&top.join("fs/b.c")), None);
        // A source tree not built itself, its built copy inside it: the copy's flags.
        let src = top.join("src");
        for d in ["arch/arm64/include/asm", "include/linux", "drivers/net/dsa/x", "fetched/linux/include/generated"] {
            std::fs::create_dir_all(src.join(d)).unwrap();
        }
        std::fs::write(src.join("Kbuild"), "").unwrap();
        std::fs::write(src.join("fetched/linux/.config"), "").unwrap();
        assert_eq!(compile_db(&src.join("drivers/net/dsa/x/c.c"), &src), None, "no copy built yet");
        std::fs::create_dir_all(src.join("fetched/linux/drivers/net")).unwrap();
        std::fs::copy(top.join("drivers/net/.a.o.cmd"), src.join("fetched/linux/drivers/net/.a.o.cmd")).unwrap();
        let c = compile_db(&src.join("drivers/net/dsa/x/c.c"), &src).expect("the built copy's flags, once built");
        assert_eq!((c.args, c.dir), (want.clone(), src.join("fetched/linux")));
        std::fs::remove_dir_all(&top).unwrap();
        // The cross gcc is not installed: clang for its target, without its ABI flag.
        if on_path("clang") {
            let (cc, args) = stand_in(e.cc, e.args);
            assert_eq!(cc, "clang");
            assert_eq!(args[..5], ["--target=aarch64-x-linux-gnu", "-nostdinc", "-I./include", "-D", "KBUILD_MODNAME=\"a\""]);
            assert!(args.contains(&"-Wno-unknown-warning-option".into()) && args.iter().any(|a| a.ends_with("/include") && a.starts_with('/')), "{args:?}");
            assert!(!args.contains(&"-mabi=lp64".into()));
        }
        if on_path("gcc") {
            let (cc, args) = stand_in("/usr/bin/gcc".into(), vec!["-DX".into()]);
            assert_eq!((cc.as_str(), args), ("gcc", vec!["-DX".to_string()]), "an installed compiler runs by its name");
            assert_eq!(known_compiler("/project/tools/gcc"), Some("gcc"), "PATH's gcc, never the project's file");
        }
        assert_eq!(known_compiler("sh"), None, "not a compiler");
        assert_eq!(known_compiler("./evil-cc-wrapper"), None);
        assert_eq!(stand_in("sh".into(), vec![]).0, "clang", "anything else: clang checks instead");
        // A header not found inside another header ends the check just the same.
        let f = parse_compiler("include/linux/linkage.h:8:10: fatal error: 'asm/linkage.h' file not found\n", &["x"], false, "clang");
        assert_eq!(f.hint.as_deref(), Some("C/C++: asm/linkage.h not found; build the project or add a compile_commands.json"));
    }

    #[test]
    fn a_compile_database_never_runs_its_own_programs_or_writes_files() {
        let root = std::env::temp_dir().join(format!("termcode-linttrust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("RAN");
        let evil = root.join("cc-evil");
        std::fs::write(&evil, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        let _ = Command::new("chmod").arg("+x").arg(&evil).status();
        // The database's compiler is a script of the project's own.
        let db = serde_json::json!([{ "directory": root, "arguments": [evil, "-Iinc", "-c", "a.c"], "file": "a.c" }]);
        std::fs::write(root.join("compile_commands.json"), db.to_string()).unwrap();
        let _ = run(Tool::C, &root.join("a.c"), &root, "int x;\n");
        assert!(!marker.exists(), "the project's compiler script ran");
        // Flags that run or load programs, or write files, never reach the compiler.
        let args: Vec<String> = ["gcc", "-fplugin=./p.so", "-B./bin", "-specs=s", "-wrapper", "sh,-c,x", "@resp", "-aux-info", "out",
            "-Xclang", "-load", "-mllvm", "-x", "-fdump-tree-all", "-ftime-trace", "-Wl,-z", "-Wp,-MD,d", "-save-temps",
            "-Iinc", "-I", "inc2", "-DX=1", "-D", "Y", "-include", "k.h", "-std=gnu11", "-O2", "-fno-common", "-mlittle-endian",
            "-Wall", "-nostdinc", "-isystem", "sys", "--target=aarch64-linux-gnu", "-c", "-o", "a.o", "a.c"].map(String::from).to_vec();
        let (_, kept) = compile_args(&args, &root.join("a.c"), &root).unwrap();
        let want = ["-Iinc", "-I", "inc2", "-DX=1", "-D", "Y", "-include", "k.h", "-std=gnu11", "-O2", "-fno-common", "-mlittle-endian",
            "-Wall", "-nostdinc", "-isystem", "sys", "--target=aarch64-linux-gnu"];
        assert_eq!(kept, want);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_problem_follows_its_line_and_hides_once_that_line_changes() {
        let lines = ["a", "bad line", "c"];
        let d = diag(&lines, 1, 0, 3, Sev::Error, "x".into()).unwrap();
        let now = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(place(&d, &now(&lines)), Some(1));
        assert_eq!(place(&d, &now(&["new", "a", "bad line", "c"])), Some(2), "a line added above");
        assert_eq!(place(&d, &now(&["a", "bad line fixed", "c"])), None, "its own line edited");
        assert_eq!(shebang("#!/usr/bin/env bash"), Some("bash"));
        assert_eq!((shebang("#!/bin/sh -e"), shebang("#!/usr/bin/python3")), (Some("sh"), None));
    }

    #[test]
    fn a_check_past_its_time_frees_the_slot_shows_its_late_answer_and_is_not_run_again() {
        let root = std::env::temp_dir().join(format!("termcode-lint-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("s.sh"), "#!/bin/sh\necho $1\n").unwrap();
        let mut app = App::new(root.clone(), None);
        app.lints.on = true;
        app.open_file(0, "s.sh", false);
        let key = "file:s.sh".to_string();
        let Some(Doc::Text(b)) = app.docs.get(&key) else { panic!("opened") };
        let ver = b.version();
        // A run (a cold disk's) still going past GIVE_UP.
        let (tx, rx) = mpsc::channel();
        app.lints.job = Some(Job { key: key.clone(), ver, since: Instant::now() - GIVE_UP - Duration::from_secs(1), rx });
        tick(&mut app);
        assert!(app.lints.job.is_none() && app.lints.late.is_some(), "the slot freed, the run kept");
        assert!(app.msg.contains("Still checking"), "{}", app.msg);
        tick(&mut app);
        assert!(app.lints.job.is_none(), "the same text is not checked again meanwhile");
        let d = Diag { line: 1, start: 5, end: 7, sev: Sev::Warning, msg: "late".into(), text: "echo $1".into() };
        tx.send(Found { diags: vec![d], hint: None }).unwrap();
        assert!(tick(&mut app), "a redraw");
        assert_eq!(app.lints.shown(&key).len(), 1, "its late answer shown");
        assert!(app.lints.late.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
