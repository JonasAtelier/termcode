//! App state and input for the VS Code-like panel: activity bar, sidebar
//! (Explorer / Search / Source Control / History), editor groups with preview
//! tabs, find widget, prompts. Drawing lives in ui.rs.

use crate::diff;
use crate::editor::{self, Buffer, Pos, View};
use crate::git::{self, ScmFile, Section};
use crate::parse::{self, Commit, Hit, StatusEntry};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{cursor, execute, terminal};
use ratatui::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const PAGE: usize = 200;
pub const MAX_HITS: usize = 5_000;
pub const MAX_GROUPS: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    Explorer,
    Search,
    Scm,
    History,
}
/// Activity bar order (VS Code's).
pub const SIDES: [Side; 4] = [Side::Explorer, Side::Search, Side::Scm, Side::History];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Focus {
    Side,
    Editor,
    SearchInput,
    CommitInput,
    Find,
}

pub struct Node {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub dir: bool,
    pub open: bool,
}

pub struct DiffDoc {
    pub title: String,
    pub rows: Vec<diff::Row>,
    pub note: Option<String>,
    pub file: ScmFile,
}

pub enum Doc {
    Text(Buffer),
    Diff(DiffDoc),
}

pub struct Tab {
    pub key: String,
    pub preview: bool,
    pub view: View,
}

#[derive(Default)]
pub struct Group {
    pub tabs: Vec<Tab>,
    pub active: usize,
}

pub struct Find {
    pub query: String,
    pub replace: Option<String>,
    /// 0 = find field, 1 = replace field.
    pub field: usize,
    pub matches: Vec<(Pos, Pos)>,
    pub cur: Option<usize>,
    /// Match Case / Match Whole Word toggles.
    pub case: bool,
    pub word: bool,
    stamp: (String, u64, String, bool, bool),
}

/// Prompt actions. Tab actions name the document, not a (group, index) that
/// may have moved while the prompt was open.
#[derive(Clone, Debug)]
pub enum PAct {
    Cancel,
    Discard(Vec<ScmFile>),
    SaveClose(String),
    DropClose(String),
    SaveAllQuit,
    QuitNoSave,
    RestoreBackup(String),
    DiscardBackup(String),
}

/// Set by SIGHUP/SIGTERM (herdr closing the pane sends HUP, then TERM, then KILL).
static SIGNALED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    SIGNALED.store(true, Ordering::SeqCst);
}

pub struct Prompt {
    pub text: Vec<String>,
    pub buttons: Vec<(String, PAct)>,
    pub sel: usize,
}

pub struct Quick {
    pub query: String,
    pub items: Vec<String>,
    pub sel: usize,
    pub off: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    Left,
    Right,
    Center,
}

pub enum Drag {
    None,
    Divider,
    File { rel: String, from: Position, active: bool },
    Select(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScmAct {
    OpenFile(usize),
    Stage(usize),
    Unstage(usize),
    Discard(usize),
    StageAll,
    UnstageAll,
    DiscardAll,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FindBtn {
    Prev,
    Next,
    Close,
    ToggleReplace,
    ReplaceOne,
    ReplaceAll,
    Case,
    Word,
    /// Search view toggles and the sidebar refresh action share this hit list.
    SearchCase,
    SearchWord,
    SearchRegex,
    Refresh,
}

/// Source Control list rows (indices into `scm_files`).
pub enum SRow {
    Head(Section, usize),
    File(usize),
}

/// Search result rows.
pub enum HRow {
    File(String, usize),
    Hit(usize),
}

/// Screen areas from the last draw, for mouse hit-testing.
#[derive(Default)]
pub struct Hits {
    pub close: Rect,
    pub activity: Vec<(Rect, Side)>,
    pub divider: Rect,
    pub side_list: Rect,
    pub side_input: Rect,
    pub commit_btn: Rect,
    pub scm_btns: Vec<(Rect, ScmAct)>,
    pub groups: Vec<GroupHit>,
    pub editor_area: Rect,
    pub find_btns: Vec<(Rect, FindBtn)>,
    pub find_inputs: Vec<(Rect, usize)>,
    pub prompt_btns: Vec<(Rect, usize)>,
    /// "Split Editor Right" button per group.
    pub split_btns: Vec<(Rect, usize)>,
    pub quick: Rect,
    pub quick_list: Rect,
}

#[derive(Default, Clone)]
pub struct GroupHit {
    pub area: Rect,
    pub tabbar: Rect,
    /// (title area, close-button cell) per tab.
    pub tabs: Vec<(Rect, Rect)>,
    /// Text area right of the gutter.
    pub text: Rect,
}

enum Load {
    Diff(DiffDoc),
    Show(String),
    Cancelled,
}

pub struct App {
    pub root: PathBuf,
    pub repo: String,
    pub branch: String,
    pub side: Side,
    pub side_visible: bool,
    pub side_w: u16,
    layout_file: Option<PathBuf>,
    pub focus: Focus,
    pub status: Vec<StatusEntry>,
    pub marks: HashMap<String, char>,
    pub dirty_dirs: HashMap<String, char>,
    /// Staged files first, then Changes.
    pub scm_files: Vec<ScmFile>,
    pub commit_msg: String,
    pub tree: Vec<Node>,
    pub commits: Vec<Commit>,
    pub history_path: Option<String>,
    history_done: bool,
    pub query: String,
    /// Search toggles (Match Case, Whole Word, Regex); all off like VS Code.
    pub s_opts: (bool, bool, bool),
    pub hits: Vec<Hit>,
    pub search: Option<Child>,
    gen: u64,
    tx: mpsc::SyncSender<(u64, Option<Hit>)>,
    rx: mpsc::Receiver<(u64, Option<Hit>)>,
    pub sel: [usize; 4],
    pub off: [usize; 4],
    pub docs: HashMap<String, Doc>,
    pub groups: Vec<Group>,
    /// Active (focused) editor group.
    pub ag: usize,
    pub find: Option<Find>,
    pub prompt: Option<Prompt>,
    pub quick: Option<Quick>,
    files: Vec<String>,
    pub drag: Drag,
    pub drop: Option<(usize, Zone)>,
    last_click: Option<(Instant, Position)>,
    pub hover: Option<Position>,
    pending_k: bool,
    clip: String,
    load_tx: mpsc::Sender<(String, u64, Load)>,
    load_rx: mpsc::Receiver<(String, u64, Load)>,
    loading: HashMap<String, u64>,
    load_gen: u64,
    /// The last interactive diff load, cancelled when the next one replaces it.
    diff_cancel: Option<(String, Arc<AtomicBool>)>,
    /// Hot-exit backups: directory, the version backed up per document, next write.
    pub backup_dir: Option<PathBuf>,
    backed: HashMap<String, u64>,
    backup_due: Option<Instant>,
    backup_first: Option<Instant>,
    /// herdr socket path: backups of other sessions on this worktree are not ours.
    backup_scope: String,
    pub msg: String,
    pub quit: bool,
    pub ui: Hits,
}

pub fn run() -> Result<(), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = crate::repo_root(&cwd)
        .ok_or_else(|| format!("{} is not inside a git repository", cwd.display()))?;
    let mut app = App::new(root, Some(layout_path()));
    // herdr closes a pane with HUP, then TERM, then KILL 500 ms later: catch the
    // first two so unsaved edits are flushed to the backup directory in time.
    for sig in [libc::SIGHUP, libc::SIGTERM] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(sig, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    setup().map_err(|e| e.to_string())?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Terminal::new(CrosstermBackend::new(io::stdout())).and_then(|mut t| app.event_loop(&mut t))
    }));
    // Every exit path (quit, signal, error, panic) leaves unsaved edits on disk.
    app.sync_backups();
    restore();
    app.kill_search();
    match res {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(r) => {
            if app.quit {
                close_window();
            }
            r.map_err(|e| e.to_string())
        }
    }
}

fn setup() -> io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        terminal::EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    // Lets Ctrl+Shift chords through where the whole chain supports it; legacy otherwise.
    if terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    Ok(())
}

fn restore() {
    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        terminal::LeaveAlternateScreen,
        cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

/// Detach this window's herdr client; the attach wrapper then closes the
/// workspace and the terminal window. Never touches other clients or the server.
fn close_window() {
    let Ok(lock) = std::env::var("HERDR_CODE_LOCK") else { return };
    let Some(l) = crate::read_lock(Path::new(&lock)) else { return };
    if l["workspace_id"].as_str() != std::env::var("HERDR_WORKSPACE_ID").ok().as_deref() {
        return;
    }
    let (Some(client), Some(attach)) = (l["client_pid"].as_u64(), l["attach_pid"].as_u64()) else {
        return;
    };
    // Only a live child of this window's attach wrapper (guards against stale data / pid reuse).
    let ppid = fs::read_to_string(format!("/proc/{client}/stat")).ok().and_then(|s| parse::stat_ppid(&s));
    if ppid == Some(attach) {
        // SAFETY: plain kill(2) on a pid just verified to be the wrapper's client.
        unsafe {
            libc::kill(client as libc::pid_t, libc::SIGHUP);
        }
    }
}

fn layout_path() -> PathBuf {
    std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state/herdr-code"))
        .join("layout.json")
}

/// Put text on the system clipboard: wl-copy, then xclip, then OSC 52 (which
/// herdr forwards to the outer terminal; not every terminal honours it).
fn clipboard_set(text: &str) -> &'static str {
    for argv in [&["wl-copy"][..], &["xclip", "-selection", "clipboard"][..]] {
        let Ok(mut c) = Command::new(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut i) = c.stdin.take() {
            let _ = i.write_all(text.as_bytes());
        }
        if c.wait().is_ok_and(|s| s.success()) {
            return argv[0];
        }
    }
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", parse::base64(text.as_bytes()));
    let _ = out.flush();
    "OSC 52"
}

/// System clipboard text via wl-paste or xclip, each given 500 ms.
fn clipboard_get() -> Option<String> {
    for argv in [&["wl-paste", "-n"][..], &["xclip", "-o", "-selection", "clipboard"][..]] {
        let Ok(mut c) = Command::new(argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match c.try_wait() {
                Ok(Some(st)) => {
                    let mut s = String::new();
                    if st.success() && c.stdout.take().is_some_and(|mut o| o.read_to_string(&mut s).is_ok()) {
                        return Some(s);
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = c.kill();
                    let _ = c.wait();
                    break;
                }
            }
        }
    }
    None
}

fn filter(files: &[String], q: &str) -> Vec<String> {
    let mut v: Vec<(usize, &String)> = files.iter().filter_map(|f| parse::match_score(f, q).map(|s| (s, f))).collect();
    v.sort_by_key(|(s, _)| *s);
    v.into_iter().take(500).map(|(_, f)| f.clone()).collect()
}

pub fn base(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

fn rank(c: char) -> u8 {
    match c {
        '!' => 3,
        'U' => 1,
        _ => 2,
    }
}

impl App {
    pub fn new(root: PathBuf, layout_file: Option<PathBuf>) -> App {
        let (tx, rx) = mpsc::sync_channel(MAX_HITS + 1);
        let (load_tx, load_rx) = mpsc::channel();
        let repo = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let saved: serde_json::Value = layout_file
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut a = App {
            root,
            repo,
            branch: String::new(),
            side: Side::Explorer,
            side_visible: saved["sidebar_visible"].as_bool().unwrap_or(true),
            side_w: saved["sidebar_width"].as_u64().map_or(30, |w| w.clamp(12, 200) as u16),
            layout_file,
            focus: Focus::Side,
            status: Vec::new(),
            marks: HashMap::new(),
            dirty_dirs: HashMap::new(),
            scm_files: Vec::new(),
            commit_msg: String::new(),
            tree: Vec::new(),
            commits: Vec::new(),
            history_path: None,
            history_done: false,
            query: String::new(),
            s_opts: (false, false, false),
            hits: Vec::new(),
            search: None,
            gen: 0,
            tx,
            rx,
            sel: [0; 4],
            off: [0; 4],
            docs: HashMap::new(),
            groups: vec![Group::default()],
            ag: 0,
            find: None,
            prompt: None,
            quick: None,
            files: Vec::new(),
            drag: Drag::None,
            drop: None,
            last_click: None,
            hover: None,
            pending_k: false,
            clip: String::new(),
            load_tx,
            load_rx,
            loading: HashMap::new(),
            load_gen: 0,
            diff_cancel: None,
            backup_dir: None,
            backed: HashMap::new(),
            backup_due: None,
            backup_first: None,
            backup_scope: std::env::var("HERDR_SOCKET_PATH").unwrap_or_default(),
            msg: String::new(),
            quit: false,
            ui: Hits::default(),
        };
        a.backup_dir = a.layout_file.as_ref().and_then(|p| p.parent()).map(|d| d.join("backup"));
        a.refresh();
        a.tree = a.read_dir("", 0);
        let pending = a.pending_backups();
        if pending > 0 {
            a.msg = format!("{pending} file(s) have unsaved changes from a closed session; open one to restore");
        }
        a
    }

    /// Backups left by an earlier session for files in this worktree.
    fn pending_backups(&self) -> usize {
        let Some(dir) = &self.backup_dir else { return 0 };
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| serde_json::from_slice::<serde_json::Value>(&fs::read(e.path()).ok()?).ok())
            .filter(|v| v["scope"].as_str() == Some(self.backup_scope.as_str()))
            .filter(|v| v["path"].as_str().is_some_and(|p| Path::new(p).starts_with(&self.root)))
            .count()
    }

    /// Write backups for dirty buffers whose current version is not on disk yet;
    /// delete backups of buffers that became clean or were closed.
    pub fn sync_backups(&mut self) {
        self.backup_due = None;
        self.backup_first = None;
        let Some(dir) = self.backup_dir.clone() else { return };
        let mut seen = Vec::new();
        for (key, doc) in &self.docs {
            let Doc::Text(b) = doc else { continue };
            if b.path.is_none() || b.readonly.is_some() {
                continue;
            }
            if b.dirty() {
                seen.push(key.clone());
                if self.backed.get(key) != Some(&b.version()) && editor::write_backup(&dir, &self.backup_scope, b).is_ok() {
                    self.backed.insert(key.clone(), b.version());
                }
            }
        }
        let stale: Vec<String> = self.backed.keys().filter(|k| !seen.contains(k)).cloned().collect();
        for key in stale {
            self.backed.remove(&key);
            if let Some(rel) = key.strip_prefix("file:") {
                editor::remove_backup(&dir, &self.backup_scope, &self.root.join(rel));
            }
        }
    }

    /// After an edit: back up about 1 s after the last change, and no later than
    /// 5 s after the first unsaved one even while typing continues.
    fn schedule_backup(&mut self) {
        let pending = self.docs.iter().any(|(k, d)| matches!(d, Doc::Text(b) if b.dirty() && self.backed.get(k) != Some(&b.version())));
        let clean_backed = self.backed.keys().any(|k| !matches!(self.docs.get(k), Some(Doc::Text(b)) if b.dirty()));
        if pending {
            let now = Instant::now();
            let first = *self.backup_first.get_or_insert(now);
            self.backup_due = Some((now + Duration::from_secs(1)).min(first + Duration::from_secs(5)));
        } else if clean_backed {
            self.sync_backups();
        }
    }

    /// Back up, then save, then drop the backup. A clean buffer is not written
    /// at all; a failed write leaves the buffer dirty with its backup kept, so
    /// closing still prompts.
    fn save_doc(&mut self, key: &str) -> Result<(), String> {
        let Some(Doc::Text(b)) = self.docs.get(key) else { return Ok(()) };
        if !b.dirty() {
            return Ok(());
        }
        if let Some(dir) = &self.backup_dir {
            editor::write_backup(dir, &self.backup_scope, b).map_err(|e| format!("backup before save failed: {e}"))?;
            self.backed.insert(key.to_string(), b.version());
        }
        let r = match self.docs.get_mut(key) {
            Some(Doc::Text(b)) => b.save(),
            _ => Ok(()),
        };
        self.sync_backups();
        r
    }

    /// A file just opened has a backup from an earlier session: offer it.
    fn offer_restore(&mut self, key: &str) {
        let (Some(dir), Some(rel)) = (&self.backup_dir, key.strip_prefix("file:")) else { return };
        let path = self.root.join(rel);
        let Some(rec) = editor::read_backup(dir, &self.backup_scope, &path) else { return };
        let mut text = vec![
            format!("{} has unsaved changes from a closed session.", rel),
            "Restore them into the editor, or discard the backup?".into(),
        ];
        if Buffer::backup_is_stale(&rec, &path) {
            text.push("The file changed on disk after the backup was taken.".into());
        }
        self.prompt = Some(Prompt {
            text,
            buttons: vec![
                ("Restore".into(), PAct::RestoreBackup(key.to_string())),
                ("Discard Backup".into(), PAct::DiscardBackup(key.to_string())),
            ],
            sel: 0,
        });
    }

    fn event_loop<B: Backend>(&mut self, t: &mut Terminal<B>) -> io::Result<()> {
        let mut dirty = true;
        while !self.quit {
            if SIGNALED.load(Ordering::SeqCst) {
                return Ok(()); // run() flushes the backups
            }
            if self.backup_due.is_some_and(|t| Instant::now() >= t) {
                self.sync_backups();
            }
            if dirty {
                t.draw(|f| crate::ui::draw(self, f))?;
            }
            dirty = self.drain();
            // Wake often only while rg/git output streams in or a backup is pending,
            // so a HUP is noticed within 100 ms whenever there is something to save.
            let busy = self.search.is_some() || !self.loading.is_empty() || self.backup_due.is_some();
            let wait = if busy { Duration::from_millis(100) } else { Duration::from_secs(5) };
            let ev = match event::poll(wait).and_then(|ready| if ready { event::read().map(Some) } else { Ok(None) }) {
                Ok(ev) => ev,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            let Some(ev) = ev else { continue };
            dirty |= match ev {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    self.on_key(k);
                    self.schedule_backup();
                    true
                }
                Event::Mouse(m) => self.on_mouse(m),
                Event::Paste(s) => {
                    self.paste_text(&s);
                    self.schedule_backup();
                    true
                }
                Event::FocusGained => {
                    self.refresh();
                    true
                }
                Event::Resize(..) => true,
                _ => false,
            };
        }
        Ok(())
    }

    // ---- git / filesystem ------------------------------------------------------

    fn git(&self, args: &[&str]) -> Option<Vec<u8>> {
        let o = git::cmd(&self.root).args(args).stderr(Stdio::null()).output().ok()?;
        o.status.success().then_some(o.stdout)
    }

    fn git_line(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.git(args).unwrap_or_default()).trim().to_string()
    }

    /// Re-read git status, decorations, and the disk state of open files.
    pub fn refresh(&mut self) {
        let b = self.git_line(&["branch", "--show-current"]);
        self.branch = if !b.is_empty() {
            b
        } else {
            match self.git_line(&["rev-parse", "--short", "HEAD"]) {
                h if h.is_empty() => "(no commits)".into(),
                h => format!("detached {h}"),
            }
        };
        let raw = self.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]).unwrap_or_default();
        self.status = parse::parse_porcelain_z(&raw);
        let (staged, changes) = git::split(&self.status);
        self.scm_files = staged.into_iter().chain(changes).collect();
        self.marks.clear();
        self.dirty_dirs.clear();
        for e in &self.status {
            let Some(c) = git::decoration(e) else { continue };
            self.marks.insert(e.path.clone(), c);
            let mut p = e.path.as_str();
            while let Some((dir, _)) = p.rsplit_once('/') {
                let d = self.dirty_dirs.entry(dir.to_string()).or_insert(c);
                if rank(c) > rank(*d) {
                    *d = c;
                }
                p = dir;
            }
        }
        let mut notes = Vec::new();
        for (key, doc) in self.docs.iter_mut() {
            if let Doc::Text(b) = doc {
                match b.check_disk() {
                    Some(true) => notes.push(format!("reloaded {}", base(key))),
                    Some(false) => notes.push(format!("{} changed on disk (unsaved edits kept)", base(key))),
                    None => {}
                }
            }
        }
        if !notes.is_empty() {
            self.msg = notes.join("; ");
        }
        // Visible diff editors follow the new state.
        let visible: Vec<(String, ScmFile)> = self
            .groups
            .iter()
            .filter_map(|g| g.tabs.get(g.active))
            .filter_map(|t| match self.docs.get(&t.key) {
                Some(Doc::Diff(d)) => Some((t.key.clone(), d.file.clone())),
                _ => None,
            })
            .collect();
        for (key, f) in visible {
            self.load_diff(key, f, None);
        }
    }

    fn read_dir(&self, rel: &str, depth: usize) -> Vec<Node> {
        let mut v: Vec<Node> = fs::read_dir(self.root.join(rel))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                Some(Node { path, name, depth, dir: e.file_type().ok()?.is_dir(), open: false })
            })
            .collect();
        v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }

    fn toggle(&mut self, i: usize) {
        let (dir, open, depth, path) = {
            let n = &self.tree[i];
            (n.dir, n.open, n.depth, n.path.clone())
        };
        if !dir {
            return;
        }
        if open {
            let end = (i + 1..self.tree.len()).find(|&j| self.tree[j].depth <= depth).unwrap_or(self.tree.len());
            self.tree.drain(i + 1..end);
        } else {
            let kids = self.read_dir(&path, depth + 1);
            self.tree.splice(i + 1..i + 1, kids);
        }
        self.tree[i].open = !open;
    }

    fn reload_all(&mut self) {
        self.refresh();
        let open: Vec<String> = self.tree.iter().filter(|n| n.open).map(|n| n.path.clone()).collect();
        self.tree = self.read_dir("", 0);
        let mut i = 0;
        while i < self.tree.len() {
            if open.contains(&self.tree[i].path) {
                self.toggle(i);
            }
            i += 1;
        }
        self.files.clear();
        if self.side == Side::History {
            self.load_history(true);
        }
    }

    fn load_history(&mut self, reset: bool) {
        if reset {
            self.commits.clear();
            self.history_done = false;
            self.sel[3] = 0;
            self.off[3] = 0;
        }
        if self.history_done {
            return;
        }
        let n = format!("-n{PAGE}");
        let skip = format!("--skip={}", self.commits.len());
        let mut args = vec!["log", "--format=%H%x1f%h%x1f%an%x1f%ar%x1f%s", &n, &skip];
        if let Some(p) = &self.history_path {
            args.extend(["--follow", "--", p]);
        }
        let out = self.git(&args).unwrap_or_default();
        let page: Vec<Commit> = String::from_utf8_lossy(&out).lines().filter_map(parse::parse_log_line).collect();
        self.history_done = page.len() < PAGE;
        self.commits.extend(page);
    }

    // ---- search ------------------------------------------------------------------

    pub fn start_search(&mut self) {
        self.kill_search();
        self.hits.clear();
        self.sel[1] = 0;
        self.off[1] = 0;
        if self.query.is_empty() {
            return;
        }
        let (case, word, regex) = self.s_opts;
        let mut flags = vec![if case { "--case-sensitive" } else { "--ignore-case" }];
        if word {
            flags.push("--word-regexp");
        }
        if !regex {
            flags.push("--fixed-strings");
        }
        let child = Command::new("rg")
            .args(["--null", "--line-number", "--column", "--no-heading", "--color", "never", "--max-columns", "300"])
            .args(flags)
            .arg("--")
            .arg(&self.query)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.msg = format!("rg: {e}");
                return;
            }
        };
        let Some(out) = child.stdout.take() else { return };
        let (tx, gen) = (self.tx.clone(), self.gen);
        std::thread::spawn(move || {
            let mut sent = 0;
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                let Some(h) = parse::parse_rg_line(&String::from_utf8_lossy(&line)) else { continue };
                sent += 1;
                // Past the cap stop reading: closing the pipe ends rg (EPIPE), and the
                // UI kills it when the (MAX_HITS + 1)th hit arrives.
                if tx.send((gen, Some(h))).is_err() || sent > MAX_HITS {
                    return;
                }
            }
            let _ = tx.send((gen, None));
        });
        self.search = Some(child);
        self.msg = format!("searching {:?}…", self.query);
    }

    pub fn kill_search(&mut self) {
        self.gen += 1;
        if let Some(mut c) = self.search.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Pull streamed search hits and finished background loads.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok((gen, hit)) = self.rx.try_recv() {
            if gen != self.gen {
                continue;
            }
            changed = true;
            match hit {
                Some(_) if self.hits.len() >= MAX_HITS => {
                    self.kill_search();
                    self.msg = format!("{MAX_HITS}+ hits, search stopped");
                }
                Some(h) => self.hits.push(h),
                None => {
                    // rg closed stdout, so it is exiting: wait (not kill) to keep its exit code.
                    self.gen += 1;
                    let code = self.search.take().and_then(|mut c| c.wait().ok()).and_then(|s| s.code());
                    let n = self.hits.len();
                    self.msg = match code {
                        Some(2) if n == 0 => "rg error (invalid pattern?)".into(),
                        Some(2) => format!("{n} hits for {:?} (some files unreadable)", self.query),
                        _ => format!("{n} hits for {:?}", self.query),
                    };
                }
            }
        }
        while let Ok((key, gen, load)) = self.load_rx.try_recv() {
            if self.loading.get(&key) != Some(&gen) {
                continue;
            }
            self.loading.remove(&key);
            // A tab closed meanwhile does not resurrect its document.
            if !self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key)) {
                continue;
            }
            let doc = match load {
                Load::Diff(d) => Doc::Diff(d),
                Load::Show(text) => Doc::Text(Buffer::from_text(&text, "Diff", "commit view")),
                Load::Cancelled => continue,
            };
            self.docs.insert(key, doc);
            changed = true;
        }
        changed
    }

    fn load(&mut self, key: String, job: impl FnOnce(PathBuf) -> Load + Send + 'static) {
        self.load_gen += 1;
        let gen = self.load_gen;
        self.loading.insert(key.clone(), gen);
        let (tx, root) = (self.load_tx.clone(), self.root.clone());
        std::thread::spawn(move || {
            let r = job(root);
            let _ = tx.send((key, gen, r));
        });
    }

    fn load_diff(&mut self, key: String, f: ScmFile, cancel: Option<Arc<AtomicBool>>) {
        self.load(key, move |root| {
            let cancelled = || cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst));
            if cancelled() {
                return Load::Cancelled;
            }
            let (old, new) = git::diff_sides(&root, &f);
            if cancelled() {
                return Load::Cancelled;
            }
            let binary = |b: &[u8]| b[..b.len().min(8192)].contains(&0);
            let side = match f.section {
                Section::Staged => "Index",
                Section::Changes if f.letter == 'U' => "Untracked",
                Section::Changes => "Working Tree",
            };
            let title = format!("{} ({side})", base(&f.path));
            let (rows, note) = if binary(&old) || binary(&new) {
                (Vec::new(), Some("binary file: no text diff".to_string()))
            } else {
                (diff::align(&String::from_utf8_lossy(&old), &String::from_utf8_lossy(&new)), None)
            };
            Load::Diff(DiffDoc { title, rows, note, file: f })
        });
    }

    // ---- documents, tabs, groups --------------------------------------------------

    fn file_doc(&mut self, rel: &str) -> String {
        let key = format!("file:{rel}");
        if !self.docs.contains_key(&key) {
            self.docs.insert(key.clone(), Doc::Text(Buffer::open(&self.root.join(rel))));
        }
        key
    }

    /// Show `key` in group `g`: reuse its tab, else replace the group's preview
    /// tab (preview), else add a tab after the active one.
    fn show_tab(&mut self, g: usize, key: String, preview: bool) {
        let grp = &mut self.groups[g];
        if let Some(i) = grp.tabs.iter().position(|t| t.key == key) {
            grp.active = i;
            if !preview {
                grp.tabs[i].preview = false;
            }
        } else if let Some(i) = grp.tabs.iter().position(|t| t.preview).filter(|_| preview) {
            let old = std::mem::replace(&mut grp.tabs[i], Tab { key, preview: true, view: View::default() });
            grp.active = i;
            self.gc(&old.key);
        } else {
            let at = if grp.tabs.is_empty() { 0 } else { grp.active + 1 };
            grp.tabs.insert(at, Tab { key, preview, view: View::default() });
            grp.active = at;
        }
        self.ag = g;
    }

    /// Drop a document nobody shows any more (a dirty one is kept for the quit prompt).
    fn gc(&mut self, key: &str) {
        let used = self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key));
        let dirty = matches!(self.docs.get(key), Some(Doc::Text(b)) if b.dirty());
        if !used && !dirty {
            self.docs.remove(key);
            self.loading.remove(key);
        }
    }

    pub fn open_file(&mut self, g: usize, rel: &str, preview: bool) {
        let fresh = !self.docs.contains_key(&format!("file:{rel}"));
        let key = self.file_doc(rel);
        self.show_tab(g, key.clone(), preview);
        if fresh {
            self.offer_restore(&key);
        }
        self.find_refresh();
    }

    fn open_file_at(&mut self, rel: &str, line: usize, col: usize) {
        self.open_file(self.ag, rel, true);
        let q = self.query.clone();
        if let Some((b, tab)) = self.ed() {
            let p = b.clamp(Pos::new(line.saturating_sub(1), col.saturating_sub(1)));
            let end = Pos::new(p.line, p.col + q.len());
            let hit = b.lines[p.line].get(p.col..end.col).is_some_and(|s| s.eq_ignore_ascii_case(&q));
            tab.view.anchor = hit.then_some(p);
            tab.view.cursor = if hit { end } else { p };
            tab.view.scroll = p.line.saturating_sub(5);
        }
        self.reveal();
    }

    pub fn open_diff(&mut self, i: usize, preview: bool) {
        let Some(f) = self.scm_files.get(i).cloned() else { return };
        let sec = if f.section == Section::Staged { "staged" } else { "changes" };
        let key = format!("diff:{sec}:{}", f.path);
        self.show_tab(self.ag, key.clone(), preview);
        // Clicking through rows replaces the preview: stop the diff nobody shows now.
        if let Some((old, flag)) = self.diff_cancel.take() {
            if old != key && !self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == old)) {
                flag.store(true, Ordering::SeqCst);
                self.loading.remove(&old);
            }
        }
        if !self.docs.contains_key(&key) && !self.loading.contains_key(&key) {
            let flag = Arc::new(AtomicBool::new(false));
            self.diff_cancel = Some((key.clone(), flag.clone()));
            self.load_diff(key, f, Some(flag));
        }
    }

    fn open_show(&mut self, hash: String) {
        let key = format!("show:{hash}");
        self.show_tab(self.ag, key.clone(), true);
        if !self.docs.contains_key(&key) {
            self.load(key, move |root| {
                let out = git::capped(&root, &["show", "--no-color", "--no-ext-diff", "--stat", "--patch", &hash]);
                Load::Show(String::from_utf8_lossy(&out).into_owned())
            });
        }
    }

    pub fn tab_title(&self, key: &str) -> String {
        match self.docs.get(key) {
            Some(Doc::Diff(d)) => d.title.clone(),
            _ => match key.split_once(':') {
                Some(("show", h)) => format!("commit {}", &h[..h.len().min(7)]),
                Some(("diff", rest)) => base(rest).to_string(),
                Some((_, rel)) => base(rel).to_string(),
                None => key.to_string(),
            },
        }
    }

    /// Active text editor of the active group.
    pub fn ed(&mut self) -> Option<(&mut Buffer, &mut Tab)> {
        let grp = self.groups.get_mut(self.ag)?;
        let tab = grp.tabs.get_mut(grp.active)?;
        match self.docs.get_mut(&tab.key)? {
            Doc::Text(b) => Some((b, tab)),
            Doc::Diff(_) => None,
        }
    }

    fn active_tab(&mut self) -> Option<&mut Tab> {
        let grp = self.groups.get_mut(self.ag)?;
        grp.tabs.get_mut(grp.active)
    }

    /// Keep the active editor's cursor on screen.
    pub fn reveal(&mut self) {
        let (h, w) = self
            .ui
            .groups
            .get(self.ag)
            .map_or((20, 80), |g| (g.text.height.max(1) as usize, g.text.width.max(1) as usize));
        let Some((b, tab)) = self.ed() else { return };
        let v = &mut tab.view;
        let c = b.clamp(v.cursor);
        if c.line < v.scroll {
            v.scroll = c.line;
        } else if c.line >= v.scroll + h {
            v.scroll = c.line + 1 - h;
        }
        let x = editor::display_col(&b.lines[c.line], c.col);
        if x < v.hscroll {
            v.hscroll = x;
        } else if x >= v.hscroll + w {
            v.hscroll = x + 1 - w;
        }
    }

    fn pin_active(&mut self) {
        if let Some(t) = self.active_tab() {
            t.preview = false;
        }
    }

    fn cycle_tab(&mut self, d: isize) {
        if let Some(grp) = self.groups.get_mut(self.ag) {
            if !grp.tabs.is_empty() {
                grp.active = (grp.active as isize + d).rem_euclid(grp.tabs.len() as isize) as usize;
            }
        }
        self.find_refresh();
    }

    /// Close tab `i` of group `g`; asks first when it is the last view of a dirty file.
    pub fn close_tab(&mut self, g: usize, i: usize, force: bool) {
        let Some(key) = self.groups.get(g).and_then(|grp| grp.tabs.get(i)).map(|t| t.key.clone()) else { return };
        let refs = self.groups.iter().map(|grp| grp.tabs.iter().filter(|t| t.key == key).count()).sum::<usize>();
        let dirty = matches!(self.docs.get(&key), Some(Doc::Text(b)) if b.dirty());
        if !force && refs == 1 && dirty {
            self.prompt = Some(Prompt {
                text: vec![
                    format!("Do you want to save the changes you made to {}?", base(&key)),
                    "Your changes will be lost if you don't save them.".into(),
                ],
                buttons: vec![
                    ("Save".into(), PAct::SaveClose(key.clone())),
                    ("Don't Save".into(), PAct::DropClose(key.clone())),
                    ("Cancel".into(), PAct::Cancel),
                ],
                sel: 0,
            });
            return;
        }
        let grp = &mut self.groups[g];
        grp.tabs.remove(i);
        if i < grp.active || grp.active >= grp.tabs.len() {
            grp.active = grp.active.saturating_sub(1);
        }
        if grp.tabs.is_empty() && self.groups.len() > 1 {
            self.groups.remove(g);
            if self.ag >= g && self.ag > 0 {
                self.ag -= 1;
            }
        }
        if refs == 1 {
            // Closing the last view (after Save or an explicit Don't Save) also
            // retires its hot-exit backup.
            self.docs.remove(&key);
            self.loading.remove(&key);
            self.sync_backups();
        }
        self.find_refresh();
    }

    /// The (group, index) currently showing `key`, preferring the active group.
    fn tab_of(&self, key: &str) -> Option<(usize, usize)> {
        let find = |g: usize| self.groups.get(g)?.tabs.iter().position(|t| t.key == key).map(|i| (g, i));
        find(self.ag).or_else(|| (0..self.groups.len()).find_map(find))
    }

    fn save_active(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        tab.preview = false;
        let key = tab.key.clone();
        if !matches!(self.docs.get(&key), Some(Doc::Text(_))) {
            return;
        }
        self.msg = match self.save_doc(&key) {
            Ok(()) => format!("saved {}", base(&key)),
            Err(e) => e,
        };
    }

    pub fn request_quit(&mut self) {
        let n = self.docs.values().filter(|d| matches!(d, Doc::Text(b) if b.dirty())).count();
        if n == 0 {
            self.quit = true;
            return;
        }
        self.prompt = Some(Prompt {
            text: vec![
                format!("Do you want to save the changes to {n} file(s)?"),
                "Your changes will be lost if you don't save them.".into(),
            ],
            buttons: vec![
                ("Save All".into(), PAct::SaveAllQuit),
                ("Don't Save".into(), PAct::QuitNoSave),
                ("Cancel".into(), PAct::Cancel),
            ],
            sel: 0,
        });
    }

    fn run_prompt(&mut self, act: PAct) {
        self.prompt = None;
        match act {
            PAct::Cancel => {}
            PAct::Discard(files) => {
                self.msg = match git::discard(&self.root, &files) {
                    Ok(_) => format!("discarded {} file(s)", files.len()),
                    Err(e) => e,
                };
                self.reload_all();
            }
            PAct::SaveClose(key) => match self.save_doc(&key) {
                Ok(()) => {
                    if let Some((g, i)) = self.tab_of(&key) {
                        self.close_tab(g, i, true);
                    }
                }
                Err(e) => self.msg = e,
            },
            PAct::DropClose(key) => {
                if let Some((g, i)) = self.tab_of(&key) {
                    self.close_tab(g, i, true);
                }
            }
            PAct::SaveAllQuit => {
                let dirty: Vec<String> = self
                    .docs
                    .iter()
                    .filter(|(_, d)| matches!(d, Doc::Text(b) if b.dirty()))
                    .map(|(k, _)| k.clone())
                    .collect();
                let errs: Vec<String> = dirty
                    .iter()
                    .filter_map(|k| self.save_doc(k).err().map(|e| format!("{}: {e}", base(k))))
                    .collect();
                if errs.is_empty() {
                    self.quit = true;
                } else {
                    self.msg = errs.join("; ");
                }
            }
            PAct::QuitNoSave => {
                // An explicit Don't Save: drop the edits and their backups.
                self.docs.retain(|_, d| !matches!(d, Doc::Text(b) if b.dirty()));
                self.sync_backups();
                self.quit = true;
            }
            PAct::RestoreBackup(key) => {
                let rec = key
                    .strip_prefix("file:")
                    .zip(self.backup_dir.as_ref())
                    .and_then(|(rel, dir)| editor::read_backup(dir, &self.backup_scope, &self.root.join(rel)));
                let restored = match (rec, self.tab_of(&key)) {
                    (Some(rec), Some((g, i))) => {
                        let tab = &mut self.groups[g].tabs[i];
                        tab.preview = false;
                        match self.docs.get_mut(&key) {
                            Some(Doc::Text(b)) => b.restore_backup(&mut tab.view, &rec),
                            _ => false,
                        }
                    }
                    _ => false,
                };
                self.msg = if restored {
                    format!("restored unsaved changes to {}", base(&key))
                } else {
                    format!("could not restore {} (backup kept)", base(&key))
                };
                self.sync_backups();
            }
            PAct::DiscardBackup(key) => {
                if let (Some(dir), Some(rel)) = (&self.backup_dir, key.strip_prefix("file:")) {
                    editor::remove_backup(dir, &self.backup_scope, &self.root.join(rel));
                }
            }
        }
    }

    // ---- find widget ----------------------------------------------------------------

    fn open_find(&mut self, replace: bool) {
        let seed = self.ed().and_then(|(b, t)| b.selected_text(&t.view)).filter(|s| !s.contains('\n'));
        let f = self.find.get_or_insert(Find {
            query: String::new(),
            replace: None,
            field: 0,
            matches: Vec::new(),
            cur: None,
            case: false,
            word: false,
            stamp: (String::new(), u64::MAX, String::new(), false, false),
        });
        if let Some(s) = seed {
            f.query = s;
        }
        if replace && f.replace.is_none() {
            f.replace = Some(String::new());
        }
        f.field = usize::from(replace);
        self.focus = Focus::Find;
        self.find_refresh();
    }

    /// Recompute matches when the query, the document or its content changed.
    pub fn find_refresh(&mut self) {
        let Some(f) = self.find.as_mut() else { return };
        let Some(tab) = self.groups.get(self.ag).and_then(|grp| grp.tabs.get(grp.active)) else {
            f.matches.clear();
            return;
        };
        let Some(Doc::Text(b)) = self.docs.get(&tab.key) else {
            f.matches.clear();
            return;
        };
        let stamp = (tab.key.clone(), b.version(), f.query.clone(), f.case, f.word);
        if stamp != f.stamp {
            f.matches = b.find_all(&f.query, f.case, f.word);
            f.stamp = stamp;
        }
        let c = tab.view.cursor;
        f.cur = f.matches.iter().position(|(a, e)| tab.view.anchor == Some(*a) && c == *e);
    }

    pub fn find_step(&mut self, dir: isize) {
        self.find_refresh();
        let Some(f) = self.find.as_ref() else { return };
        if f.matches.is_empty() {
            self.msg = "No results".into();
            return;
        }
        let matches = f.matches.clone();
        let cur = f.cur;
        let Some((_, tab)) = self.ed() else { return };
        let c = tab.view.cursor;
        let n = matches.len();
        let i = match (cur, dir > 0) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => matches.iter().position(|(a, _)| *a >= c).unwrap_or(0),
            (None, false) => matches.iter().rposition(|(_, e)| *e <= c).unwrap_or(n - 1),
        };
        let (a, e) = matches[i];
        tab.view.anchor = Some(a);
        tab.view.cursor = e;
        self.reveal();
        self.find_refresh();
    }

    fn replace_one(&mut self) {
        self.find_refresh();
        let Some(f) = self.find.as_ref() else { return };
        let with = f.replace.clone().unwrap_or_default();
        if f.cur.is_some() {
            if let Some((b, tab)) = self.ed() {
                b.insert(&mut tab.view, &with);
                tab.preview = false;
            }
        }
        self.find_step(1);
    }

    fn replace_all(&mut self) {
        self.find_refresh();
        let Some(f) = self.find.as_ref() else { return };
        let (matches, with) = (f.matches.clone(), f.replace.clone().unwrap_or_default());
        if let Some((b, tab)) = self.ed() {
            let n = b.replace_all(&mut tab.view, &matches, &with);
            if n > 0 {
                tab.preview = false;
            }
            self.msg = format!("replaced {n} occurrence(s)");
        }
        self.find_refresh();
    }

    pub fn find_button(&mut self, b: FindBtn) {
        match b {
            FindBtn::Prev => self.find_step(-1),
            FindBtn::Next => self.find_step(1),
            FindBtn::Close => {
                self.find = None;
                self.focus = Focus::Editor;
            }
            FindBtn::ToggleReplace => {
                if let Some(f) = self.find.as_mut() {
                    f.replace = if f.replace.is_some() { None } else { Some(String::new()) };
                    f.field = 0;
                }
            }
            FindBtn::ReplaceOne => self.replace_one(),
            FindBtn::ReplaceAll => self.replace_all(),
            FindBtn::Case | FindBtn::Word => {
                if let Some(f) = self.find.as_mut() {
                    if b == FindBtn::Case {
                        f.case = !f.case;
                    } else {
                        f.word = !f.word;
                    }
                }
                self.find_refresh();
            }
            FindBtn::SearchCase | FindBtn::SearchWord | FindBtn::SearchRegex => {
                let t = match b {
                    FindBtn::SearchCase => &mut self.s_opts.0,
                    FindBtn::SearchWord => &mut self.s_opts.1,
                    _ => &mut self.s_opts.2,
                };
                *t = !*t;
                self.start_search();
            }
            FindBtn::Refresh => {
                self.reload_all();
                if self.side == Side::Search {
                    self.start_search();
                }
            }
        }
    }

    // ---- source control ----------------------------------------------------------------

    pub fn scm_rows(&self) -> Vec<SRow> {
        let staged = self.scm_files.iter().filter(|f| f.section == Section::Staged).count();
        let mut rows = Vec::new();
        if staged > 0 {
            rows.push(SRow::Head(Section::Staged, staged));
            rows.extend((0..staged).map(SRow::File));
        }
        rows.push(SRow::Head(Section::Changes, self.scm_files.len() - staged));
        rows.extend((staged..self.scm_files.len()).map(SRow::File));
        rows
    }

    fn section_files(&self, s: Section) -> Vec<ScmFile> {
        self.scm_files.iter().filter(|f| f.section == s).cloned().collect()
    }

    fn confirm_discard(&mut self, files: Vec<ScmFile>) {
        if files.is_empty() {
            return;
        }
        let untracked = files.iter().filter(|f| f.letter == 'U').count();
        let first = if files.len() == 1 {
            format!("Are you sure you want to discard changes in {}?", files[0].path)
        } else {
            format!("Are you sure you want to discard ALL changes in {} files?", files.len())
        };
        let second = if untracked > 0 {
            format!("{untracked} untracked file(s) will be DELETED. This is IRREVERSIBLE!")
        } else {
            "This is IRREVERSIBLE!".to_string()
        };
        self.prompt = Some(Prompt {
            text: vec![first, second],
            buttons: vec![("Discard".into(), PAct::Discard(files)), ("Cancel".into(), PAct::Cancel)],
            sel: 1,
        });
    }

    pub fn scm_action(&mut self, a: ScmAct) {
        let one = |i: usize| self.scm_files.get(i).cloned().into_iter().collect::<Vec<_>>();
        let res = match a {
            ScmAct::OpenFile(i) => {
                if let Some(f) = self.scm_files.get(i).cloned() {
                    self.open_file(self.ag, &f.path, false);
                    self.focus = Focus::Editor;
                }
                return;
            }
            ScmAct::Discard(i) => return self.confirm_discard(one(i)),
            ScmAct::DiscardAll => return self.confirm_discard(self.section_files(Section::Changes)),
            ScmAct::Stage(i) => git::stage(&self.root, &one(i)),
            ScmAct::Unstage(i) => git::unstage(&self.root, &one(i)),
            ScmAct::StageAll => git::stage(&self.root, &self.section_files(Section::Changes)),
            ScmAct::UnstageAll => git::unstage(&self.root, &self.section_files(Section::Staged)),
        };
        if let Err(e) = res {
            self.msg = e;
        }
        self.refresh();
    }

    pub fn commit(&mut self) {
        let msg = self.commit_msg.trim().to_string();
        if !self.scm_files.iter().any(|f| f.section == Section::Staged) {
            self.msg = "No staged changes".into();
        } else if msg.is_empty() {
            self.msg = "Enter a commit message first".into();
            self.focus = Focus::CommitInput;
        } else {
            match git::commit(&self.root, &msg) {
                Ok(_) => {
                    self.msg = format!("committed: {}", msg.lines().next().unwrap_or(""));
                    self.commit_msg.clear();
                }
                Err(e) => self.msg = format!("commit failed: {e}"),
            }
            self.refresh();
        }
    }

    // ---- sidebar ------------------------------------------------------------------------

    pub fn search_rows(&self) -> Vec<HRow> {
        let mut rows = Vec::new();
        for (i, h) in self.hits.iter().enumerate() {
            if i == 0 || self.hits[i - 1].path != h.path {
                let n = self.hits[i..].iter().take_while(|x| x.path == h.path).count();
                rows.push(HRow::File(h.path.clone(), n));
            }
            rows.push(HRow::Hit(i));
        }
        rows
    }

    pub fn side_count(&self) -> usize {
        match self.side {
            Side::Explorer => self.tree.len(),
            Side::Search => self.search_rows().len(),
            Side::Scm => self.scm_rows().len(),
            Side::History => self.commits.len(),
        }
    }

    pub fn show_side(&mut self, s: Side, input: bool) {
        self.side = s;
        self.side_visible = true;
        self.focus = match s {
            Side::Search if input => Focus::SearchInput,
            // Like VS Code, opening Source Control puts the caret in the message box.
            Side::Scm => Focus::CommitInput,
            _ => Focus::Side,
        };
        self.refresh();
        if s == Side::History {
            self.load_history(true);
        }
        self.save_layout();
    }

    fn save_layout(&self) {
        let Some(p) = &self.layout_file else { return };
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        let v = serde_json::json!({ "sidebar_width": self.side_w, "sidebar_visible": self.side_visible });
        let _ = fs::write(p, v.to_string());
    }

    /// Single click opens a preview; `pin` (double click, Enter) keeps it open.
    fn activate_side(&mut self, i: usize, pin: bool) {
        match self.side {
            Side::Explorer => match self.tree.get(i) {
                Some(n) if n.dir => {
                    if !pin {
                        self.toggle(i)
                    }
                }
                Some(n) => {
                    let p = n.path.clone();
                    self.open_file(self.ag, &p, !pin);
                    if pin {
                        self.focus = Focus::Editor;
                    }
                }
                None => {}
            },
            Side::Search => match self.search_rows().get(i) {
                Some(HRow::Hit(h)) => {
                    let h = self.hits[*h].clone();
                    self.open_file_at(&h.path, h.line, h.col);
                    if pin {
                        self.pin_active();
                    }
                }
                Some(HRow::File(p, _)) => {
                    let p = p.clone();
                    self.open_file(self.ag, &p, !pin);
                }
                None => {}
            },
            Side::Scm => {
                if let Some(SRow::File(f)) = self.scm_rows().get(i) {
                    let f = *f;
                    self.open_diff(f, !pin);
                }
            }
            Side::History => {
                if let Some(c) = self.commits.get(i) {
                    let h = c.hash.clone();
                    self.open_show(h);
                    if pin {
                        self.pin_active();
                    }
                }
            }
        }
    }

    /// "Split Editor Right": the active file again in a new group to the right.
    fn split_right(&mut self, g: usize) {
        if self.groups.len() >= MAX_GROUPS {
            return;
        }
        let Some(t) = self.groups[g].tabs.get(self.groups[g].active) else { return };
        let tab = Tab { key: t.key.clone(), preview: false, view: t.view.clone() };
        self.groups.insert(g + 1, Group { tabs: vec![tab], active: 0 });
        self.ag = g + 1;
        self.focus = Focus::Editor;
    }

    fn move_side(&mut self, d: isize) {
        let n = self.side_count();
        if n == 0 {
            return;
        }
        let s = self.side as usize;
        self.sel[s] = (self.sel[s] as isize + d).clamp(0, n as isize - 1) as usize;
        let h = self.ui.side_list.height.max(1) as usize;
        if self.sel[s] < self.off[s] {
            self.off[s] = self.sel[s];
        } else if self.sel[s] >= self.off[s] + h {
            self.off[s] = self.sel[s] + 1 - h;
        }
        if self.side == Side::History && self.sel[s] + 20 >= n {
            self.load_history(false);
        }
    }

    // ---- quick open ----------------------------------------------------------------------

    fn open_quick(&mut self) {
        if self.files.is_empty() {
            let out = self.git(&["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
            self.files = out
                .split(|&b| b == 0)
                .filter(|f| !f.is_empty())
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            self.files.dedup();
        }
        let items = filter(&self.files, "");
        self.quick = Some(Quick { query: String::new(), items, sel: 0, off: 0 });
    }

    fn quick_key(&mut self, k: KeyEvent, modded: bool) {
        let Some(q) = self.quick.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.quick = None,
            KeyCode::Enter => {
                let p = q.items.get(q.sel).cloned();
                self.quick = None;
                if let Some(p) = p {
                    self.open_file(self.ag, &p, false);
                    self.focus = Focus::Editor;
                }
            }
            KeyCode::Up => q.sel = q.sel.saturating_sub(1),
            KeyCode::Down => q.sel = (q.sel + 1).min(q.items.len().saturating_sub(1)),
            KeyCode::Backspace | KeyCode::Char(_) => {
                match k.code {
                    KeyCode::Char(c) if !modded => q.query.push(c),
                    KeyCode::Backspace => {
                        q.query.pop();
                    }
                    _ => return,
                }
                q.items = filter(&self.files, &q.query);
                q.sel = 0;
            }
            _ => {}
        }
    }

    // ---- keyboard --------------------------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let is = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if self.quick.is_some() {
            return self.quick_key(k, ctrl || alt);
        }
        if std::mem::take(&mut self.pending_k) && k.code == KeyCode::Enter {
            return self.pin_active(); // Ctrl+K Enter: Keep Open
        }
        if ctrl && is('q') {
            return self.request_quit();
        }
        if ((ctrl && !shift) || alt) && is('p') {
            return self.open_quick();
        }
        // Ctrl+Shift+F only arrives distinctly with kitty keyboard support; legacy
        // terminals send it as Ctrl+F, which is in-file find (VS Code's Ctrl+F).
        if ctrl && shift && is('f') {
            return self.show_side(Side::Search, true);
        }
        if ctrl && is('e') {
            return self.show_side(Side::Explorer, false);
        }
        if ctrl && is('g') {
            return self.show_side(Side::Scm, false);
        }
        if ctrl && is('f') {
            return self.open_find(false);
        }
        if ctrl && is('h') {
            return self.open_find(true);
        }
        if ctrl && is('s') {
            return self.save_active();
        }
        if ctrl && is('w') {
            let g = self.ag;
            let i = self.groups.get(g).map_or(0, |grp| grp.active);
            return self.close_tab(g, i, false);
        }
        if ctrl && is('k') {
            self.pending_k = true;
            return;
        }
        match k.code {
            KeyCode::F(1) => return self.show_side(Side::Explorer, false),
            KeyCode::F(2) => return self.show_side(Side::Scm, false),
            KeyCode::F(4) => return self.show_side(Side::Search, true),
            KeyCode::F(3) => return self.find_step(if shift { -1 } else { 1 }),
            KeyCode::Char(c @ '1'..='4') if alt => {
                let s = [Side::Explorer, Side::Scm, Side::History, Side::Search][c as usize - '1' as usize];
                return self.show_side(s, s == Side::Search);
            }
            KeyCode::Char(c @ '1'..='3') if ctrl => {
                let g = c as usize - '1' as usize;
                if g < self.groups.len() {
                    self.ag = g;
                    self.focus = Focus::Editor;
                    self.find_refresh();
                }
                return;
            }
            KeyCode::PageDown | KeyCode::Tab if ctrl => return self.cycle_tab(1),
            KeyCode::PageUp | KeyCode::BackTab if ctrl => return self.cycle_tab(-1),
            _ => {}
        }
        match self.focus {
            Focus::Find => self.find_key(k, ctrl || alt, shift),
            Focus::SearchInput | Focus::CommitInput => self.input_key(k, ctrl, alt),
            Focus::Side => self.side_key(k),
            Focus::Editor => self.editor_key(k, ctrl, alt, shift),
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = self.prompt.as_mut() else { return };
        let n = p.buttons.len();
        match k.code {
            KeyCode::Left | KeyCode::BackTab => p.sel = (p.sel + n - 1) % n,
            KeyCode::Right | KeyCode::Tab => p.sel = (p.sel + 1) % n,
            KeyCode::Enter => {
                let act = p.buttons[p.sel].1.clone();
                self.run_prompt(act);
            }
            // A restore offer must be answered: editing on would overwrite the old backup.
            KeyCode::Esc if !p.buttons.iter().any(|b| matches!(b.1, PAct::RestoreBackup(_))) => self.prompt = None,
            _ => {}
        }
    }

    fn find_key(&mut self, k: KeyEvent, modded: bool, shift: bool) {
        let Some(f) = self.find.as_mut() else { return };
        let field = if f.field == 1 { f.replace.as_mut() } else { Some(&mut f.query) };
        match k.code {
            KeyCode::Esc => {
                self.find = None;
                self.focus = Focus::Editor;
            }
            KeyCode::Enter if f.field == 1 => self.replace_one(),
            KeyCode::Enter | KeyCode::Down => self.find_step(if shift { -1 } else { 1 }),
            KeyCode::Up => self.find_step(-1),
            KeyCode::Tab | KeyCode::BackTab => {
                if f.replace.is_some() {
                    f.field = 1 - f.field;
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = field {
                    s.pop();
                }
                self.find_refresh();
            }
            KeyCode::Char(c) if !modded => {
                if let Some(s) = field {
                    s.push(c);
                }
                self.find_refresh();
            }
            _ => {}
        }
    }

    fn input_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool) {
        let commit = self.focus == Focus::CommitInput;
        let s = if commit { &mut self.commit_msg } else { &mut self.query };
        match k.code {
            // Ctrl+Enter is VS Code's commit key; legacy terminals send it as Enter, so Alt+Enter too.
            KeyCode::Enter if commit && (ctrl || alt) => self.commit(),
            KeyCode::Enter if commit => s.push('\n'),
            KeyCode::Enter => {
                self.start_search();
                self.focus = Focus::Side;
            }
            KeyCode::Esc if !commit && self.search.is_some() => {
                self.kill_search();
                self.msg = "search cancelled".into();
            }
            KeyCode::Esc => self.focus = if commit { Focus::Editor } else { Focus::Side },
            KeyCode::Down if !commit => self.focus = Focus::Side,
            KeyCode::Tab if commit => self.focus = Focus::Side,
            KeyCode::Backspace => {
                s.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt => s.push(c),
            _ => {}
        }
    }

    fn side_key(&mut self, k: KeyEvent) {
        let page = self.ui.side_list.height.max(1) as isize;
        let s = self.side as usize;
        match k.code {
            KeyCode::Up => self.move_side(-1),
            KeyCode::Down => self.move_side(1),
            KeyCode::PageUp => self.move_side(-page),
            KeyCode::PageDown => self.move_side(page),
            KeyCode::Home => self.move_side(-1_000_000),
            KeyCode::End => self.move_side(1_000_000),
            KeyCode::Enter => self.activate_side(self.sel[s], true),
            KeyCode::Char(' ') => self.activate_side(self.sel[s], false),
            KeyCode::Left | KeyCode::Right if self.side == Side::Explorer => {
                let i = self.sel[s];
                if self.tree.get(i).is_some_and(|n| n.dir && n.open == (k.code == KeyCode::Left)) {
                    self.toggle(i);
                }
            }
            KeyCode::Char('/') if self.side == Side::Search => self.focus = Focus::SearchInput,
            KeyCode::Char('h') if self.side == Side::Explorer => {
                if let Some(n) = self.tree.get(self.sel[s]).filter(|n| !n.dir) {
                    self.history_path = Some(n.path.clone());
                    self.show_side(Side::History, false);
                }
            }
            KeyCode::Char('r') => self.reload_all(),
            KeyCode::Esc if self.side == Side::History && self.history_path.is_some() => {
                self.history_path = None;
                self.load_history(true);
            }
            // Source Control list: + stage, - unstage, Delete discard (row or section header).
            KeyCode::Char('+' | '-') | KeyCode::Delete if self.side == Side::Scm => {
                let op = match k.code {
                    KeyCode::Char(c) => c,
                    _ => 'd',
                };
                let changes = |i: usize| self.scm_files[i].section == Section::Changes;
                let act = match self.scm_rows().get(self.sel[s]) {
                    Some(SRow::File(i)) if op == '+' && changes(*i) => ScmAct::Stage(*i),
                    Some(SRow::File(i)) if op == '-' && !changes(*i) => ScmAct::Unstage(*i),
                    Some(SRow::File(i)) if op == 'd' && changes(*i) => ScmAct::Discard(*i),
                    Some(SRow::Head(Section::Changes, _)) if op == '+' => ScmAct::StageAll,
                    Some(SRow::Head(Section::Staged, _)) if op == '-' => ScmAct::UnstageAll,
                    Some(SRow::Head(Section::Changes, _)) if op == 'd' => ScmAct::DiscardAll,
                    _ => return,
                };
                self.scm_action(act);
            }
            KeyCode::BackTab if self.side == Side::Scm => self.focus = Focus::CommitInput,
            KeyCode::Esc | KeyCode::Tab => self.focus = Focus::Editor,
            _ => {}
        }
    }

    fn editor_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) {
        if ctrl && k.code == KeyCode::Char('c') {
            return self.copy_cut(false);
        }
        if ctrl && k.code == KeyCode::Char('x') {
            return self.copy_cut(true);
        }
        if ctrl && k.code == KeyCode::Char('v') {
            let text = clipboard_get().unwrap_or_else(|| self.clip.clone());
            return self.paste_text(&text);
        }
        if k.code == KeyCode::Esc && self.find.is_some() {
            self.find = None;
            return;
        }
        let page = self.ui.groups.get(self.ag).map_or(20, |g| g.text.height.max(1) as isize);
        if self.ed().is_none() {
            // Diff views: scroll only.
            if let Some(t) = self.active_tab() {
                let d = match k.code {
                    KeyCode::Up => -1,
                    KeyCode::Down => 1,
                    KeyCode::PageUp => -page,
                    KeyCode::PageDown => page,
                    _ => 0,
                };
                t.view.scroll = (t.view.scroll as isize + d).max(0) as usize;
            }
            return;
        }
        let mut readonly_hit = None;
        {
            let Some((b, tab)) = self.ed() else { return };
            let v = &mut tab.view;
            let before = b.version();
            match k.code {
                KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                    'a' => b.select_all(v),
                    'z' if shift => {
                        b.redo(v);
                    }
                    'z' => {
                        b.undo(v);
                    }
                    'y' => {
                        b.redo(v);
                    }
                    _ => return,
                },
                KeyCode::Char(_) if alt => return,
                KeyCode::Char(c) => b.insert(v, &c.to_string()),
                KeyCode::Enter => b.newline(v),
                KeyCode::Tab => b.indent(v),
                KeyCode::BackTab => b.outdent(v),
                KeyCode::Backspace => b.backspace(v),
                KeyCode::Delete => b.delete(v),
                KeyCode::Left => b.left(v, shift, ctrl),
                KeyCode::Right => b.right(v, shift, ctrl),
                KeyCode::Up if ctrl => v.scroll = v.scroll.saturating_sub(1),
                KeyCode::Down if ctrl => v.scroll = (v.scroll + 1).min(b.lines.len() - 1),
                KeyCode::Up => b.vertical(v, -1, shift),
                KeyCode::Down => b.vertical(v, 1, shift),
                KeyCode::PageUp => {
                    b.vertical(v, -page, shift);
                    v.scroll = v.scroll.saturating_sub(page as usize);
                }
                KeyCode::PageDown => {
                    b.vertical(v, page, shift);
                    v.scroll = (v.scroll + page as usize).min(b.lines.len() - 1);
                }
                KeyCode::Home if ctrl => b.move_to(v, Pos::new(0, 0), shift),
                KeyCode::End if ctrl => {
                    let last = b.lines.len() - 1;
                    let p = Pos::new(last, b.lines[last].len());
                    b.move_to(v, p, shift);
                }
                KeyCode::Home => b.home(v, shift),
                KeyCode::End => b.end_of_line(v, shift),
                KeyCode::Esc => v.anchor = None,
                _ => return,
            }
            let typing = !ctrl && matches!(k.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete | KeyCode::Tab);
            if b.version() != before {
                tab.preview = false; // editing keeps the tab open
            } else if typing {
                readonly_hit = b.readonly.clone();
            }
        }
        if let Some(ro) = readonly_hit {
            self.msg = format!("read-only: {ro}");
        }
        self.reveal();
        self.find_refresh();
    }

    fn copy_cut(&mut self, cut: bool) {
        let Some((b, tab)) = self.ed() else { return };
        let v = &mut tab.view;
        let text = match b.selected_text(v) {
            Some(t) => t,
            None => {
                // No selection: VS Code copies/cuts the whole line.
                let c = b.clamp(v.cursor);
                if cut {
                    let (a, e) = if c.line + 1 < b.lines.len() {
                        (Pos::new(c.line, 0), Pos::new(c.line + 1, 0))
                    } else if c.line > 0 {
                        (Pos::new(c.line - 1, b.lines[c.line - 1].len()), Pos::new(c.line, b.lines[c.line].len()))
                    } else {
                        (Pos::new(0, 0), Pos::new(0, b.lines[0].len()))
                    };
                    v.anchor = Some(a);
                    v.cursor = e;
                }
                format!("{}\n", b.lines[c.line])
            }
        };
        if cut && b.readonly.is_none() {
            b.backspace(v);
            tab.preview = false;
        }
        let how = clipboard_set(&text);
        self.clip = text;
        self.msg = format!("{} via {how}", if cut { "cut" } else { "copied" });
        self.reveal();
        self.find_refresh();
    }

    /// Bracketed paste or Ctrl+V: into the focused input or the editor.
    pub fn paste_text(&mut self, text: &str) {
        if self.prompt.is_some() || self.quick.is_some() {
            return;
        }
        match self.focus {
            Focus::SearchInput => self.query.push_str(&text.replace(['\r', '\n'], " ")),
            Focus::CommitInput => self.commit_msg.push_str(&text.replace("\r\n", "\n")),
            Focus::Find => {
                if let Some(f) = self.find.as_mut() {
                    let s = if f.field == 1 { f.replace.get_or_insert_with(String::new) } else { &mut f.query };
                    s.push_str(text.lines().next().unwrap_or(""));
                }
                self.find_refresh();
            }
            // A paste while a list has focus is dropped, never redirected into a file.
            Focus::Side => {}
            Focus::Editor => {
                if let Some((b, tab)) = self.ed() {
                    b.insert(&mut tab.view, text);
                    if b.readonly.is_none() {
                        tab.preview = false;
                    }
                }
                self.reveal();
                self.find_refresh();
            }
        }
    }

    // ---- mouse ---------------------------------------------------------------------------

    pub fn on_mouse(&mut self, m: MouseEvent) -> bool {
        let p = Position::new(m.column, m.row);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.last_click.is_some_and(|(t, q)| t.elapsed() < DOUBLE_CLICK && q == p);
                self.last_click = Some((Instant::now(), p));
                self.click(p, double, shift);
            }
            // With a prompt open, a drag that started before it must not drop anything.
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) if self.prompt.is_some() => {
                self.drag = Drag::None;
                self.drop = None;
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(p),
            MouseEventKind::Up(MouseButton::Left) => self.release(),
            MouseEventKind::ScrollUp => self.wheel(p, -3),
            MouseEventKind::ScrollDown => self.wheel(p, 3),
            MouseEventKind::Moved => {
                let changed = self.hover != Some(p);
                self.hover = Some(p);
                return changed && (self.ui.side_list.contains(p) || self.ui.groups.iter().any(|g| g.tabbar.contains(p)));
            }
            _ => return false,
        }
        true
    }

    fn click(&mut self, p: Position, double: bool, shift: bool) {
        if let Some(pr) = self.prompt.as_ref() {
            if let Some(&(_, i)) = self.ui.prompt_btns.iter().find(|(r, _)| r.contains(p)) {
                let act = pr.buttons[i].1.clone();
                self.run_prompt(act);
            }
            return;
        }
        if let Some(q) = self.quick.as_mut() {
            if self.ui.quick_list.contains(p) {
                let i = q.off + (p.y - self.ui.quick_list.y) as usize;
                if let Some(f) = q.items.get(i).cloned() {
                    self.quick = None;
                    self.open_file(self.ag, &f, false);
                    self.focus = Focus::Editor;
                }
            } else if !self.ui.quick.contains(p) {
                self.quick = None;
            }
            return;
        }
        if self.ui.close.contains(p) {
            return self.request_quit();
        }
        if let Some(&(_, s)) = self.ui.activity.iter().find(|(r, _)| r.contains(p)) {
            if s == self.side && self.side_visible {
                self.side_visible = false; // clicking the active view hides the sidebar
                self.focus = Focus::Editor;
                self.save_layout();
            } else {
                self.show_side(s, s == Side::Search);
            }
            return;
        }
        if self.ui.divider.contains(p) {
            self.drag = Drag::Divider;
            return;
        }
        if let Some(&(_, b)) = self.ui.find_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.find_button(b);
        }
        if let Some(&(_, field)) = self.ui.find_inputs.iter().find(|(r, _)| r.contains(p)) {
            if let Some(f) = self.find.as_mut() {
                f.field = field;
            }
            self.focus = Focus::Find;
            return;
        }
        if self.ui.side_input.contains(p) {
            self.focus = if self.side == Side::Scm { Focus::CommitInput } else { Focus::SearchInput };
            return;
        }
        if self.ui.commit_btn.contains(p) {
            return self.commit();
        }
        if let Some(&(_, a)) = self.ui.scm_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.scm_action(a);
        }
        if let Some(&(_, g)) = self.ui.split_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.split_right(g);
        }
        if self.ui.side_list.contains(p) {
            let s = self.side as usize;
            let i = self.off[s] + (p.y - self.ui.side_list.y) as usize;
            if i < self.side_count() {
                self.sel[s] = i;
                self.focus = Focus::Side;
                if self.side == Side::Explorer {
                    if let Some(n) = self.tree.get(i).filter(|n| !n.dir) {
                        self.drag = Drag::File { rel: n.path.clone(), from: p, active: false };
                    }
                }
                self.activate_side(i, double);
            }
            return;
        }
        for g in 0..self.ui.groups.len() {
            let gh = self.ui.groups[g].clone();
            if let Some(i) = gh.tabs.iter().position(|(_, x)| x.contains(p)) {
                return self.close_tab(g, i, false);
            }
            if let Some(i) = gh.tabs.iter().position(|(r, _)| r.contains(p)) {
                self.groups[g].active = i;
                self.ag = g;
                self.focus = Focus::Editor;
                if double {
                    self.groups[g].tabs[i].preview = false;
                }
                return self.find_refresh();
            }
            if gh.area.contains(p) {
                self.ag = g;
                self.focus = Focus::Editor;
                if !gh.tabbar.contains(p) {
                    let pos = self.pos_at(g, p);
                    if let (Some(pos), Some((b, tab))) = (pos, self.ed()) {
                        if double {
                            b.select_word(&mut tab.view, pos);
                        } else {
                            b.move_to(&mut tab.view, pos, shift);
                            tab.view.want_x = None;
                        }
                        self.drag = Drag::Select(g);
                    }
                }
                return self.find_refresh();
            }
        }
    }

    /// Buffer position under screen cell `p` in group `g`'s text area.
    fn pos_at(&self, g: usize, p: Position) -> Option<Pos> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let tab = grp.tabs.get(grp.active)?;
        let Some(Doc::Text(b)) = self.docs.get(&tab.key) else { return None };
        let line = (tab.view.scroll + p.y.saturating_sub(gh.text.y) as usize).min(b.lines.len() - 1);
        let x = tab.view.hscroll + p.x.saturating_sub(gh.text.x) as usize;
        Some(Pos::new(line, editor::byte_at_display(&b.lines[line], x)))
    }

    /// Where a dragged file would land: the group under the cursor and which part.
    fn drop_target(&self, p: Position) -> Option<(usize, Zone)> {
        let can_split = self.groups.len() < MAX_GROUPS;
        self.ui.groups.iter().enumerate().find(|(_, g)| g.area.contains(p)).map(|(i, g)| {
            let rx = (p.x - g.area.x) as u32 * 10 / g.area.width.max(1) as u32;
            let zone = if g.tabbar.contains(p) || !can_split {
                Zone::Center
            } else if rx < 3 {
                Zone::Left
            } else if rx >= 7 {
                Zone::Right
            } else {
                Zone::Center
            };
            (i, zone)
        })
    }

    fn drag_to(&mut self, p: Position) {
        match &mut self.drag {
            Drag::None => {}
            Drag::Divider => {
                let start = self.ui.divider.x.saturating_sub(self.side_w);
                let max = self.ui.editor_area.right().saturating_sub(start + 20).max(12);
                self.side_w = p.x.saturating_sub(start).clamp(12, max);
            }
            Drag::File { from, active, .. } => {
                if !*active && (from.x.abs_diff(p.x) + from.y.abs_diff(p.y)) >= 2 {
                    *active = true;
                }
                if *active {
                    self.drop = self.drop_target(p);
                }
            }
            Drag::Select(g) => {
                let g = *g;
                let Some(gh) = self.ui.groups.get(g).cloned() else { return };
                // Auto-scroll past the top/bottom edge.
                let scroll: isize = if p.y < gh.text.y {
                    -1
                } else if p.y >= gh.text.bottom() {
                    1
                } else {
                    0
                };
                if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
                    t.view.scroll = (t.view.scroll as isize + scroll).max(0) as usize;
                }
                let q = Position::new(p.x.max(gh.text.x), p.y.clamp(gh.text.y, gh.text.bottom().saturating_sub(1)));
                let pos = self.pos_at(g, q);
                if let (Some(pos), true) = (pos, g == self.ag) {
                    if let Some((b, tab)) = self.ed() {
                        b.move_to(&mut tab.view, pos, true);
                    }
                }
            }
        }
    }

    fn release(&mut self) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Divider => self.save_layout(),
            Drag::File { rel, active: true, .. } => {
                if let Some((g, zone)) = self.drop.take() {
                    let target = match zone {
                        Zone::Center => g,
                        Zone::Left | Zone::Right => {
                            let at = if zone == Zone::Left { g } else { g + 1 };
                            self.groups.insert(at, Group::default());
                            at
                        }
                    };
                    self.open_file(target, &rel, false);
                    self.focus = Focus::Editor;
                }
            }
            _ => {}
        }
        self.drop = None;
    }

    fn wheel(&mut self, p: Position, d: isize) {
        if let Some(q) = self.quick.as_mut() {
            q.sel = (q.sel as isize + d).clamp(0, (q.items.len() as isize - 1).max(0)) as usize;
            return;
        }
        if self.ui.side_list.contains(p) {
            let s = self.side as usize;
            let n = self.side_count();
            let h = self.ui.side_list.height as usize;
            self.off[s] = (self.off[s] as isize + d).clamp(0, n.saturating_sub(h) as isize) as usize;
            if self.side == Side::History && self.off[s] + h + 20 >= n {
                self.load_history(false);
            }
            return;
        }
        let Some(g) = self.ui.groups.iter().position(|g| g.area.contains(p)) else { return };
        let max = {
            let grp = &self.groups[g];
            match grp.tabs.get(grp.active).and_then(|t| self.docs.get(&t.key)) {
                Some(Doc::Text(b)) => b.lines.len(),
                Some(Doc::Diff(dd)) => dd.rows.len(),
                None => 0,
            }
        };
        if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
            t.view.scroll = (t.view.scroll as isize + d).clamp(0, max.saturating_sub(1) as isize) as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    pub fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height)
            .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(app: &mut App, code: KeyCode, m: KeyModifiers) {
        app.on_key(KeyEvent::new(code, m));
    }

    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            key(app, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
    }

    fn wait(app: &mut App) {
        for _ in 0..300 {
            app.drain();
            if app.search.is_none() && app.loading.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A throwaway repo with one commit; local config keeps hooks and signing out.
    fn repo(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("herdr-code-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        let g = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&d).args(args).output().unwrap().status.success();
            assert!(ok, "{args:?}");
        };
        g(&["init", "-q"]);
        g(&["config", "user.name", "t"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "commit.gpgsign", "false"]);
        g(&["config", "core.hooksPath", "/dev/null"]);
        fs::write(d.join("src/main.rs"), "fn main() {\n    println!(\"needle\");\n}\n").unwrap();
        fs::write(d.join("README.md"), "hello\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", "init"]);
        d.canonicalize().unwrap()
    }

    #[test]
    fn edit_save_find_split_and_scm_flow() {
        let root = repo("flow");
        let mut app = App::new(root.clone(), None);
        let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
        let s = screen(&mut app, &mut t);
        assert!(s.contains("EXPLORER") && s.contains("README.md") && s.contains("Close"), "{s}");

        // A single click opens a preview tab; the next one replaces it.
        app.open_file(0, "README.md", true);
        assert!(app.groups[0].tabs[0].preview);
        app.open_file(0, "src/main.rs", true);
        assert_eq!(app.groups[0].tabs.len(), 1);
        assert_eq!(app.groups[0].tabs[0].key, "file:src/main.rs");

        // Typing pins the tab and marks it dirty; Ctrl+S saves.
        app.focus = Focus::Editor;
        typ(&mut app, "// hi");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.groups[0].tabs[0].preview);
        let s = screen(&mut app, &mut t);
        assert!(s.contains(&format!("main.rs {}", crate::theme::icons().dirty)) && s.contains("Ln 2, Col 1"), "{s}");
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(fs::read_to_string(root.join("src/main.rs")).unwrap().starts_with("// hi\nfn main()"));

        // In-file find with Ctrl+F; Enter moves to the match.
        key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        typ(&mut app, "needle");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("1 of 1"), "{s}");
        assert_eq!(app.find.as_ref().unwrap().cur, Some(0));
        // Replace all is one undo step.
        key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
        typ(&mut app, "pin");
        app.find_button(FindBtn::ReplaceAll);
        assert!(app.ed().unwrap().0.lines[2].contains("\"pin\""));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(app.ed().unwrap().0.lines[2].contains("\"needle\""));
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);

        // Drag README.md from the explorer onto the right edge: a new group.
        screen(&mut app, &mut t);
        let row = app.tree.iter().position(|n| n.path == "README.md").unwrap();
        let y = app.ui.side_list.y + (row - app.off[0]) as u16;
        let (ea, lx) = (app.ui.editor_area, app.ui.side_list.x);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), lx + 2, y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), ea.right() - 3, ea.y + 5);
        assert_eq!(app.drop.map(|d| d.1), Some(Zone::Right));
        screen(&mut app, &mut t);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), ea.right() - 3, ea.y + 5);
        assert_eq!(app.groups.len(), 2);
        assert_eq!(app.groups[1].tabs[0].key, "file:README.md");
        assert_eq!(app.ag, 1);

        // Source Control: the saved edit is under Changes; diff, stage, commit.
        app.refresh();
        app.show_side(Side::Scm, false);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Changes") && s.contains("main.rs") && s.contains("Commit"), "{s}");
        let i = app.scm_files.iter().position(|f| f.path == "src/main.rs").unwrap();
        app.open_diff(i, true);
        wait(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("main.rs (Working Tree)") && s.contains("// hi"), "{s}");
        app.commit();
        assert_eq!(app.msg, "No staged changes");
        app.scm_action(ScmAct::Stage(i));
        assert!(app.scm_files.iter().any(|f| f.section == Section::Staged && f.path == "src/main.rs"));
        app.commit_msg = "edit main".into();
        app.commit();
        assert!(app.msg.starts_with("committed"), "{}", app.msg);
        assert!(app.scm_files.is_empty());

        // Discard always asks; Cancel (the default) keeps the file, Discard deletes it.
        fs::write(root.join("junk.txt"), "x").unwrap();
        app.refresh();
        let j = app.scm_files.iter().position(|f| f.path == "junk.txt").unwrap();
        app.scm_action(ScmAct::Discard(j));
        assert!(app.prompt.as_ref().unwrap().text.join(" ").contains("IRREVERSIBLE"));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(root.join("junk.txt").exists());
        app.scm_action(ScmAct::Discard(j));
        app.prompt.as_mut().unwrap().sel = 0;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!root.join("junk.txt").exists());

        // Closing a dirty tab prompts (only for its last view); quitting with unsaved edits prompts.
        // The explorer click before the drag also left README.md as a preview in group 0.
        let i0 = app.groups[0].tabs.iter().position(|t| t.key == "file:README.md").unwrap();
        app.close_tab(0, i0, false);
        assert!(app.docs.contains_key("file:README.md"), "still shown in group 1");
        app.ag = 1;
        app.groups[1].active = app.groups[1].tabs.iter().position(|t| t.key == "file:README.md").unwrap();
        app.focus = Focus::Editor;
        typ(&mut app, "x");
        key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(app.prompt.is_some());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(!app.quit && app.prompt.is_some());
        app.prompt.as_mut().unwrap().sel = 1; // Don't Save
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.quit);
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "hello\n");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hot_exit_backup_is_offered_on_reopen() {
        let root = repo("hot");
        let state = root.join(".state");
        let mut app = App::new(root.clone(), Some(state.join("layout.json")));
        app.open_file(0, "README.md", false);
        app.focus = Focus::Editor;
        typ(&mut app, "unsaved ");
        app.sync_backups(); // what HUP/TERM/panic and the 1 s timer do
        let bak = editor::backup_file(&state.join("backup"), &app.backup_scope, &root.join("README.md"));
        assert!(bak.exists());
        drop(app); // the panel dies without saving

        let mut app = App::new(root.clone(), Some(state.join("layout.json")));
        assert!(app.msg.contains("1 file(s) have unsaved changes"), "{}", app.msg);
        app.open_file(0, "README.md", true);
        let p = app.prompt.as_ref().expect("restore offer");
        assert_eq!(p.buttons[0].0, "Restore");
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.prompt.is_some(), "the offer must be answered");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let (b, tab) = app.ed().unwrap();
        assert_eq!(b.text(), "unsaved hello\n");
        assert!(b.dirty() && !tab.preview);
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "hello\n");
        // Saving retires the backup.
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(!bak.exists());
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "unsaved hello\n");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn failed_in_place_save_stays_dirty_with_backup_and_clean_save_is_a_no_op() {
        let root = repo("inplace");
        let state = root.join(".state");
        fs::hard_link(root.join("README.md"), root.join("README.link")).unwrap();
        let mut app = App::new(root.clone(), Some(state.join("layout.json")));
        app.open_file(0, "README.md", false);
        app.focus = Focus::Editor;
        // Ctrl+S on a clean buffer writes nothing.
        let before = fs::metadata(root.join("README.md")).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(fs::metadata(root.join("README.md")).unwrap().modified().unwrap(), before);
        // A hard link saves in place; make that write fail after the truncate.
        typ(&mut app, "x");
        editor::FAIL_IN_PLACE.with(|c| c.set(true));
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(app.msg.contains("save failed"), "{}", app.msg);
        assert!(app.ed().unwrap().0.dirty());
        let bak = editor::backup_file(&state.join("backup"), &app.backup_scope, &root.join("README.md"));
        let rec: serde_json::Value = serde_json::from_slice(&fs::read(&bak).unwrap()).unwrap();
        assert_eq!(rec["text"], "xhello");
        key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(app.prompt.is_some(), "closing still asks");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sidebar_divider_stays_in_one_column() {
        let root = repo("div");
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "src/main.rs", false);
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        screen(&mut app, &mut t);
        let x = app.ui.divider.x;
        let b = t.backend().buffer();
        for y in 1..19 {
            assert_eq!(b[(x, y)].symbol(), "│", "row {y}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn search_streams_and_stops_at_cap() {
        let tmp = std::env::temp_dir().join(format!("herdr-code-cap-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("many.txt"), "e\n".repeat(MAX_HITS * 3)).unwrap();
        let mut app = App::new(tmp.clone(), None);
        app.query = "e".into();
        app.start_search();
        wait(&mut app);
        assert!(app.search.is_none(), "rg still running");
        assert_eq!(app.hits.len(), MAX_HITS);
        assert!(app.msg.contains("search stopped"), "{}", app.msg);
        fs::remove_dir_all(&tmp).unwrap();
    }
}
