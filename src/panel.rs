//! App state and input for the VS Code-like panel: activity bar, sidebar
//! (Explorer with Timeline / Search / Source Control with Graph), editor groups with preview
//! tabs, find widget, prompts. Drawing lives in ui.rs.

use crate::decor::{self, Hunk, Mark, TrackHit};
use crate::minimap::Mini;
use crate::diff;
use crate::editor::{self, Buffer, Pos, View};
use crate::git::{self, ScmFile, Section};
use crate::nav::{reap, take_slot, MAX_LIVE_RG};
use crate::parse::{self, Hit, StatusEntry};
use crate::settings::{self, Settings, ThemeData};
use crate::theme;
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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

/// A file's (mtime, length), or None when it does not exist.
type Stamp = Option<(SystemTime, u64)>;
/// Quick-diff recompute delay after an edit.
const QDIFF_DEBOUNCE: Duration = Duration::from_millis(200);
pub const MAX_HITS: usize = 5_000;
pub const MAX_GROUPS: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// /proc/<pid>/comm of a running panel: its backups are its own.
const PANEL_COMM: &str = "herdr-code";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Side {
    Explorer,
    Search,
    Scm,
}
/// Activity bar order (VS Code's).
pub const SIDES: [Side; 3] = [Side::Explorer, Side::Search, Side::Scm];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Focus {
    Side,
    Editor,
    SearchInput,
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
    /// Keys the minimap's buffer of this diff in `App::diff_minis`.
    pub id: DiffId,
}

/// One DiffDoc, unique per construction (`Default` draws the next number): the
/// minimap's buffer is not Send, so it lives in the panel, not in the doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffId(u64);

impl Default for DiffId {
    fn default() -> DiffId {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        DiffId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
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
    /// A Source Control action asked for confirmation.
    Git(crate::scm::GitAct),
    SaveAllQuit,
    QuitNoSave,
    /// A document and the backup file chosen for it; the backup file to drop.
    RestoreBackup(String, PathBuf),
    DiscardBackup(PathBuf),
}

/// Wheel and motion frames come at most this often (herdr itself renders every 16 ms).
const FRAME: Duration = Duration::from_millis(16);
/// FocusGained waits this long, so focus flicker runs one refresh.
const FOCUS_DEBOUNCE: Duration = Duration::from_millis(150);
/// The running `git status` shows a spinner after this long.
pub const STATUS_SLOW: Duration = Duration::from_millis(300);
const STATUS_KEY: &str = "status:";

/// See `App::hover_key`.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct HoverKey {
    row: Option<usize>,
    sticky: Option<usize>,
    git_row: Option<u16>,
    tab: Option<(usize, usize)>,
    scrollbar: Option<usize>,
    minimap: Option<usize>,
    peek: Option<usize>,
    /// A commit-dialog splitter (sash.hoverBorder).
    sash: Option<usize>,
    /// An activity bar item (its icon lights up).
    activity: Option<usize>,
}

/// Events the loop batches before one frame: wheel ticks and plain pointer motion.
fn coalescable(ev: &Event) -> bool {
    matches!(ev, Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight | MouseEventKind::Moved))
}

/// (mtime, length) of each file, None for a missing one: what a confirmed discard re-checks.
fn file_stamps(g: &Path, files: &[ScmFile]) -> Vec<Option<(SystemTime, u64)>> {
    files.iter().map(|f| fs::symlink_metadata(g.join(&f.path)).ok().and_then(|m| Some((m.modified().ok()?, m.len())))).collect()
}

/// The branch label and `git status -z` of the repo at `g` (runs git; off the UI thread).
fn read_status(g: &Path) -> (String, Vec<u8>) {
    let run = |args: &[&str]| git::cmd(g).args(args).stderr(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| o.stdout);
    let line = |args: &[&str]| String::from_utf8_lossy(&run(args).unwrap_or_default()).trim().to_string();
    let b = line(&["branch", "--show-current"]);
    let branch = if !b.is_empty() {
        b
    } else {
        match line(&["rev-parse", "--short", "HEAD"]) {
            h if h.is_empty() => "(no commits)".into(),
            h => format!("detached {h}"),
        }
    };
    (branch, run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]).unwrap_or_default())
}

// The perf batch-2 behaviours, switchable only in tests (to measure before and after).
#[cfg(test)]
thread_local! {
    pub(crate) static BATCH2: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}
#[cfg(test)]
fn on() -> bool {
    BATCH2.with(|b| b.get())
}
#[cfg(not(test))]
fn on() -> bool {
    true
}
fn coalesce() -> bool {
    on()
}
fn hover_keyed() -> bool {
    on()
}
fn async_status() -> bool {
    on()
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
    /// File paths, or command labels in the command palette.
    pub items: Vec<String>,
    pub sel: usize,
    pub off: usize,
    /// F1 command palette rather than Quick Open.
    pub commands: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cmd {
    OpenSettings,
    ToggleRainbow,
    ToggleRuler,
    ToggleGuides,
    ToggleSidebar,
    ToggleMinimap,
    ToggleRulers,
    NextChange,
    PrevChange,
    QuickOpen,
    Save,
    ReloadSettings,
    Nav(crate::navui::Act),
    Git(crate::gitcmds::GitCmd),
}

/// The F1 command palette: label, command, key hint.
pub const COMMANDS: &[(&str, Cmd, &str)] = &[
    ("Preferences: Open Settings (JSON)", Cmd::OpenSettings, ""),
    ("View: Toggle Indent Rainbow", Cmd::ToggleRainbow, ""),
    ("View: Toggle Minimap Ruler", Cmd::ToggleRuler, ""),
    ("View: Toggle Indent Guides", Cmd::ToggleGuides, ""),
    ("View: Toggle Primary Side Bar Visibility", Cmd::ToggleSidebar, ""),
    ("View: Toggle Minimap", Cmd::ToggleMinimap, ""),
    ("View: Toggle Rulers", Cmd::ToggleRulers, ""),
    ("Go to Next Change", Cmd::NextChange, "Alt+F5 / F7"),
    ("Go to Previous Change", Cmd::PrevChange, "Shift+Alt+F5 / Shift+F7"),
    ("Go to File...", Cmd::QuickOpen, "Ctrl+P"),
    ("File: Save", Cmd::Save, "Ctrl+S"),
    ("Developer: Reload Settings", Cmd::ReloadSettings, ""),
    ("Go to Definition", Cmd::Nav(crate::navui::Act::Definition), "F12"),
    ("Peek Definition", Cmd::Nav(crate::navui::Act::Peek), "Alt+F12"),
    ("Find All References", Cmd::Nav(crate::navui::Act::References), "Shift+F12"),
    ("Go Back", Cmd::Nav(crate::navui::Act::Back), "Alt+←"),
    ("Go Forward", Cmd::Nav(crate::navui::Act::Forward), "Alt+→"),
];

/// Every palette command: the app's, then the Git ones.
pub fn all_commands() -> impl Iterator<Item = (&'static str, Cmd)> {
    COMMANDS.iter().map(|c| (c.0, c.1)).chain(crate::gitcmds::GIT_COMMANDS.iter().map(|(l, g)| (*l, Cmd::Git(*g))))
}

/// Commands matching `q`, best tier first, palette order within a tier.
pub fn filter_commands(q: &str) -> Vec<String> {
    let mut v: Vec<(usize, usize, &str)> =
        all_commands().enumerate().filter_map(|(i, c)| parse::match_score(c.0, q).map(|s| (s / 100_000, i, c.0))).collect();
    v.sort();
    v.into_iter().map(|(_, _, l)| l.to_string()).collect()
}

/// Something with a scrollbar.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScrollTarget {
    Editor(usize),
    Side,
    Quick,
    /// Source Control lists: 0 = GRAPH, 1 = Explorer TIMELINE.
    Git(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct ScrollHit {
    pub track: Rect,
    pub target: ScrollTarget,
    /// Slider (first row, rows) within the track.
    pub slider: (u16, u16),
    pub max_off: usize,
    pub page: usize,
}

/// Quick diff of one open file against its index version.
#[derive(Default)]
pub struct QDiff {
    /// None: the file is not in the index (untracked, outside the repo).
    pub base: Option<Vec<String>>,
    pub marks: Vec<Option<Mark>>,
    pub hunks: Vec<Hunk>,
    /// The index blob is valid UTF-8 (else Revert would write replacement characters).
    pub base_utf8: bool,
    /// Buffer version the marks are for.
    ver: Option<u64>,
}

/// The quick-diff peek (VS Code's dirty-diff widget): one hunk of one editor,
/// drawn as a zone below the hunk.
#[derive(Clone, Debug, PartialEq)]
pub struct Peek {
    pub group: usize,
    pub key: String,
    pub hunk: usize,
    /// First body row shown (the body scrolls when the hunk is taller than the zone).
    pub scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PeekBtn {
    Stage,
    Revert,
    Prev,
    Next,
    Close,
}

/// A drawn minimap, for clicks and slider drags.
#[derive(Clone, Copy, Debug)]
pub struct MiniHit {
    pub area: Rect,
    pub group: usize,
    pub mini: Mini,
    /// The editor's scroll when it was drawn.
    pub scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    Left,
    Right,
    Center,
    /// A dragged tab over a tab bar: inserted before tab n (n = the count: at the end).
    Tab(usize),
}

pub enum Drag {
    None,
    Divider,
    File { rel: String, from: Position, active: bool },
    /// Tab `i` of group `g` (VS Code's tab drag and drop); active once moved 2 cells.
    Tab { g: usize, i: usize, from: Position, active: bool },
    Select(usize),
    /// Scrollbar slider grabbed `grab` rows below its top.
    Scroll(ScrollHit, u16),
    /// Minimap slider grabbed `grab` rows below its top.
    Minimap(MiniHit, u16),
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
    /// "Stage All Merge Changes": mark every conflict resolved.
    StageMerge,
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
    /// "Merge Changes": the conflicts, above Staged Changes.
    MergeHead(usize),
    File(usize),
    /// Blank row between sections (comfortable density).
    Gap,
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
    pub scrollbars: Vec<ScrollHit>,
    /// The sidebar, and how its list was laid out: items that fit, blank rows between items.
    pub sidebar: Rect,
    pub side_rows: usize,
    pub side_gap: u16,
    /// Sticky scroll: the list items pinned over slots 0, 1, …, and how many may be.
    pub sticky: Vec<usize>,
    pub sticky_cap: usize,
    pub minimaps: Vec<MiniHit>,
    pub peek_btns: Vec<(Rect, PeekBtn)>,
    /// The peek zone, and its body rows (for the wheel).
    pub peek_zone: Rect,
    pub peek_body: Rect,
    /// Body rows that do not fit (the peek's scroll range).
    pub peek_max: usize,
}

impl Hits {
    /// The sidebar list item drawn at screen row `y` (None on a gap row).
    pub fn side_item(&self, y: u16, off: usize) -> Option<usize> {
        let rel = y.checked_sub(self.side_list.y)?;
        let step = 1 + self.side_gap;
        (rel % step == 0 && rel < self.side_list.height).then(|| off + (rel / step) as usize)
    }

    /// The pinned (sticky) item drawn at screen row `y`: its slot's item row and gap row.
    pub fn sticky_at(&self, y: u16) -> Option<usize> {
        let rel = y.checked_sub(self.side_list.y)?;
        self.sticky.get((rel / (1 + self.side_gap)) as usize).copied()
    }
}

#[derive(Default, Clone)]
pub struct GroupHit {
    pub area: Rect,
    pub tabbar: Rect,
    /// (title area, close-button cell) per tab.
    pub tabs: Vec<(Rect, Rect)>,
    /// Text area right of the gutter.
    pub text: Rect,
    /// The quick-diff marker column.
    pub marks_x: Option<u16>,
    /// The peek zone's rows (first row, height): text rows below it are shifted down.
    pub zone: Option<(u16, u16)>,
}

enum Load {
    Diff(DiffDoc),
    /// `git status` off the UI thread: the branch label and the porcelain bytes.
    Status(String, Vec<u8>),
    /// Quick-diff base (the index blob) for a `file:` document.
    Base(Option<Vec<u8>>),
    Cancelled,
}

pub struct App {
    /// The workspace root: Explorer, Search, Quick Open and document paths.
    pub root: PathBuf,
    /// The git worktree holding the root (VS Code's parent-repo detection); None = no git.
    pub git_root: Option<PathBuf>,
    /// The root relative to git_root, "" or "sub/dir/": git paths under the root start with it.
    prefix: String,
    /// Launcher requests carry this (see `on_paste`); None = take none.
    pub token: Option<String>,
    pub repo: String,
    pub branch: String,
    pub side: Side,
    pub side_visible: bool,
    /// Dragged sidebar width; None = `herdr-code.spacing.sidebarWidth`.
    pub side_w: Option<u16>,
    /// `side_w` came from a layout.json before version 2: a content width, not an outer one.
    pub side_w_legacy: bool,
    pub(crate) layout_file: Option<PathBuf>,
    pub focus: Focus,
    pub status: Vec<StatusEntry>,
    pub marks: HashMap<String, char>,
    pub dirty_dirs: HashMap<String, char>,
    /// Staged files first, then Changes.
    pub scm_files: Vec<ScmFile>,
    pub tree: Vec<Node>,
    pub query: String,
    /// Search toggles (Match Case, Whole Word, Regex); all off like VS Code.
    pub s_opts: (bool, bool, bool),
    pub hits: Vec<Hit>,
    pub search: Option<Child>,
    /// Tells the running search's reader thread to stop.
    search_stop: Arc<AtomicBool>,
    /// rg processes not yet reaped (running or abandoned), Search and nav lookups alike.
    pub(crate) live_rg: Arc<AtomicUsize>,
    /// A search waits until fewer than MAX_LIVE_RG rg processes are left.
    pub queued_search: Option<(String, (bool, bool, bool))>,
    gen: u64,
    tx: mpsc::SyncSender<(u64, Option<Hit>)>,
    rx: mpsc::Receiver<(u64, Option<Hit>)>,
    pub sel: [usize; 3],
    pub off: [usize; 3],
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
    /// A key or click that ended a wheel/motion batch: handled after that batch's frame.
    pending_ev: Option<Event>,
    /// When the last frame was drawn (wheel/motion frames are at most one per FRAME).
    last_draw: Option<Instant>,
    /// FocusGained, debounced: the refresh it asked for.
    refresh_due: Option<Instant>,
    /// A refresh came while `git status` ran: run it again when that one lands.
    status_again: bool,
    /// The running `git status` started then (a spinner after STATUS_SLOW).
    pub status_since: Option<Instant>,
    clip: String,
    load_tx: mpsc::Sender<(String, u64, Load)>,
    load_rx: mpsc::Receiver<(String, u64, Load)>,
    pub(crate) loading: HashMap<String, u64>,
    load_gen: u64,
    /// The last interactive diff load, cancelled when the next one replaces it.
    diff_cancel: Option<(String, Arc<AtomicBool>)>,
    /// Hot-exit backups: directory, the version backed up per document, next write.
    pub backup_dir: Option<PathBuf>,
    backed: HashMap<String, u64>,
    backup_due: Option<Instant>,
    backup_first: Option<Instant>,
    /// herdr socket path ("local" outside herdr) and this panel (pane id or
    /// `local`, plus pid): the backups this panel writes and removes.
    backup_scope: String,
    backup_instance: String,
    pub msg: String,
    pub quit: bool,
    pub ui: Hits,
    pub settings: Settings,
    /// settings.json; None in tests unless set.
    pub settings_file: Option<PathBuf>,
    /// mtimes of settings.json (and VS Code's, when followed) at the last load.
    settings_stamp: Option<(Stamp, Stamp)>,
    /// The last load failed: shown in the status bar until a good save.
    pub settings_err: Option<String>,
    /// The theme last installed, so an unrelated settings change does not recolour.
    installed: ThemeData,
    /// Command-palette toggles, reset from settings.json on each load.
    pub rainbow_on: bool,
    pub guides_on: bool,
    pub ruler_on: bool,
    pub minimap_on: bool,
    pub rulers_on: bool,
    pub peek: Option<Peek>,
    pub qdiff: HashMap<String, QDiff>,
    qdiff_due: Option<Instant>,
    /// Each Explorer row's parent folder row (usize::MAX at the top), for sticky
    /// scroll; empty until needed and after the tree changes.
    tree_parents: std::cell::RefCell<Vec<usize>>,
    /// Per diff tab: the modified side as a buffer (one line per row) and each
    /// row's mark, for its minimap; rebuilt when the doc is replaced.
    pub diff_minis: HashMap<String, (DiffId, Buffer, Vec<Option<Mark>>)>,
    /// Go to Definition / Peek / References state.
    pub nav: crate::navui::Nav,
    /// Source Control view, Timeline, hover card and commit dialog state.
    pub gitui: crate::scm::GitUi,
    /// The saved session of this root (`session`).
    pub session: crate::session::State,
}

/// The TUI on this process's terminal for `root`, first opening `req`
/// (`open:<path>:<line>:<col>`, see `request`). `tab`: the plugin's tab pane,
/// which also takes requests from `herdr-code --tab FILE`; in place it does not.
/// Every exit restores the terminal, so in place the shell gets it back.
pub fn run(root: PathBuf, req: Option<String>, tab: bool) -> Result<(), String> {
    let state = crate::plugin_dir(true);
    let mut app = App::new(root, Some(state.join("layout.json")));
    app.settings_file = Some(settings::path());
    app.check_settings();
    // In place and in a tab alike: the same state dir, so one session per root.
    app.session.file = Some(crate::session::path(&state, &app.root));
    crate::session::restore(&mut app);
    // `herdr-code FILE:L` opens on top of the restored tabs.
    if let Some(req) = req {
        app.request(&req);
    }
    // herdr closes a pane with HUP, then TERM, then KILL 500 ms later: catch the
    // first two so unsaved edits are flushed to the backup directory in time.
    for sig in [libc::SIGHUP, libc::SIGTERM] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(sig, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    setup().map_err(|e| e.to_string())?;
    // Written once the terminal is raw, so a request never lands in cooked mode.
    let token_file = match (std::env::var("HERDR_SOCKET_PATH"), std::env::var("HERDR_PANE_ID")) {
        (Ok(socket), Ok(pane)) if tab => Some(crate::token_path(&socket, &pane)),
        _ => None,
    };
    app.token = token_file.as_deref().and_then(|p| write_token(p).ok());
    // Only the event loop's own panic leaves the terminal (then the default hook prints it
    // on the shell's screen). A job thread's panic is logged; its Reply guard or closed
    // channel shows the failure, and the UI goes on: restoring would close the gate.
    let main = std::thread::current().id();
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if panic_restores(main) {
            restore();
            hook(info);
        } else {
            let name = std::thread::current().name().unwrap_or("job").to_string();
            log_panic(&crate::plugin_dir(true), &format!("thread '{name}' {info}"));
        }
    }));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // One write per frame (ratatui flushes the backend after each draw): herdr then
        // gets whole frames, not ~14 line-buffered pieces.
        let out = io::BufWriter::with_capacity(1 << 16, Gate { w: io::stdout(), off: &RESTORED });
        Terminal::new(CrosstermBackend::new(out)).and_then(|mut t| app.event_loop(&mut t))
    }));
    // The terminal first: saving backups and the session never holds the shell back.
    restore();
    finish(&mut app);
    // Git writes still queued or running finish before the process ends (closing their
    // pipes would SIGPIPE git mid-write). After HUP/TERM or a panic only briefly.
    if app.gitui.writes() > 0 {
        let hurry = SIGNALED.load(Ordering::SeqCst) || res.is_err();
        eprintln!("herdr-code: waiting for {} git operation(s) to finish…", app.gitui.writes());
        crate::scm::finish_writes(&mut app, hurry.then_some(Duration::from_secs(2)));
    }
    app.stop_search();
    if let Some(p) = token_file.filter(|_| app.token.is_some()) {
        let _ = fs::remove_file(p);
    }
    // Quitting just ends this process: herdr then closes the tab. Nothing here
    // signals or closes anything else.
    match res {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(r) => r.map_err(|e| e.to_string()),
    }
}

/// Every exit path (quit, HUP/TERM, an error, a panic) leaves the unsaved edits
/// and the session on disk.
fn finish(app: &mut App) {
    app.sync_backups();
    crate::session::save_guarded(app);
}

pub(crate) fn setup() -> io::Result<()> {
    setup_on(&mut io::stdout(), terminal::enable_raw_mode, terminal::disable_raw_mode)?;
    // Lets Ctrl+Shift chords through where the whole chain supports it; legacy otherwise.
    if terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let _ = execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    Ok(())
}

/// Raw mode (`raw`), then the screen modes on `w`. If writing them fails, undo
/// it all (`restore_seq`, then `cooked`) before the error goes up: a shell must
/// never be left raw. This is the only early return between setup and the loop.
fn setup_on<W: Write>(w: &mut W, raw: fn() -> io::Result<()>, cooked: fn() -> io::Result<()>) -> io::Result<()> {
    raw()?;
    let r = execute!(w, terminal::EnterAlternateScreen, EnableMouseCapture, EnableFocusChange, EnableBracketedPaste);
    if r.is_err() {
        let _ = restore_seq(w);
        let _ = cooked();
    }
    r
}

/// Undo every mode `setup` turned on (raw mode aside), so a shell gets back
/// the terminal it had.
fn restore_seq(w: &mut impl Write) -> io::Result<()> {
    execute!(
        w,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        terminal::LeaveAlternateScreen,
        cursor::Show
    )
}

/// Whether a panic on this thread tears the terminal down: only on the event loop's.
fn panic_restores(main: std::thread::ThreadId) -> bool {
    std::thread::current().id() == main
}

/// A job thread's panic, appended to `panic.log` in `dir` (best effort: never in the UI).
fn log_panic(dir: &Path, msg: &str) {
    let _ = fs::create_dir_all(dir);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("panic.log")) {
        let _ = writeln!(f, "{msg}");
    }
}

/// Set once `restore` ran: the terminal's buffered output is dropped from then on.
static RESTORED: AtomicBool = AtomicBool::new(false);

/// The terminal's stdout, closed by `restore`: a frame a panic cut short, still in the
/// `BufWriter` when it is dropped after the panic hook, never lands on the shell's screen.
struct Gate<W> {
    w: W,
    off: &'static AtomicBool,
}

impl<W: Write> Write for Gate<W> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if self.off.load(Ordering::SeqCst) {
            return Ok(b.len());
        }
        self.w.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.off.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.w.flush()
    }
}

pub(crate) fn restore() {
    RESTORED.store(true, Ordering::SeqCst);
    let _ = restore_seq(&mut io::stdout());
    let _ = terminal::disable_raw_mode();
}

/// A fresh random token in `path` (0600, written whole by a rename).
fn write_token(path: &Path) -> io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    let token: String = b.iter().map(|x| format!("{x:02x}")).collect();
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?.write_all(token.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(token)
}

/// Put text on the system clipboard: wl-copy, then xclip, then OSC 52 (which
/// herdr forwards to the outer terminal; not every terminal honours it).
pub(crate) fn clipboard_set(text: &str) -> &'static str {
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
                    reap(c, None);
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

impl App {
    pub fn new(root: PathBuf, layout_file: Option<PathBuf>) -> App {
        let (tx, rx) = mpsc::sync_channel(MAX_HITS + 1);
        let (load_tx, load_rx) = mpsc::channel();
        let repo = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let git_root = crate::repo_root(&root);
        let prefix = git_root
            .as_ref()
            .and_then(|g| root.strip_prefix(g).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|p| !p.is_empty())
            .map_or(String::new(), |p| p + "/");
        let saved: serde_json::Value = layout_file
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut a = App {
            root,
            git_root,
            prefix,
            token: None,
            repo,
            branch: String::new(),
            side: Side::Explorer,
            side_visible: saved["sidebar_visible"].as_bool().unwrap_or(true),
            // Only a dragged width is kept (older files saved the default too).
            side_w: saved["sidebar_width"]
                .as_u64()
                .filter(|_| saved["sidebar_dragged"] == true)
                .map(|w| w.clamp(12, 200) as u16),
            // Before layout_version 2 the width was the content's: a card adds its frame.
            side_w_legacy: saved["layout_version"].as_u64().unwrap_or(1) < 2,
            layout_file,
            focus: Focus::Side,
            status: Vec::new(),
            marks: HashMap::new(),
            dirty_dirs: HashMap::new(),
            scm_files: Vec::new(),
            tree: Vec::new(),
            query: String::new(),
            s_opts: (false, false, false),
            hits: Vec::new(),
            search: None,
            search_stop: Arc::new(AtomicBool::new(false)),
            live_rg: Arc::new(AtomicUsize::new(0)),
            queued_search: None,
            gen: 0,
            tx,
            rx,
            sel: [0; 3],
            off: [0; 3],
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
            pending_ev: None,
            last_draw: None,
            refresh_due: None,
            status_again: false,
            status_since: None,
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
            // Outside herdr (in place in a plain terminal) the session is "local".
            backup_scope: std::env::var("HERDR_SOCKET_PATH").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "local".into()),
            // The pid too: panels run one after another in one pane never share a file.
            backup_instance: format!(
                "{}-{}",
                std::env::var("HERDR_PANE_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "local".into()),
                std::process::id()
            ),
            msg: String::new(),
            quit: false,
            ui: Hits::default(),
            settings: Settings::default(),
            settings_file: None,
            settings_stamp: None,
            settings_err: None,
            installed: Settings::default().theme,
            rainbow_on: true,
            guides_on: true,
            ruler_on: true,
            minimap_on: true,
            rulers_on: true,
            peek: None,
            qdiff: HashMap::new(),
            qdiff_due: None,
            tree_parents: Default::default(),
            diff_minis: HashMap::new(),
            nav: Default::default(),
            gitui: crate::scm::GitUi::new(&saved),
            session: Default::default(),
        };
        a.backup_dir = a.layout_file.as_ref().and_then(|p| p.parent()).map(|d| d.join("backup"));
        // The first frame shows the real status: read it here, once, synchronously.
        a.refresh_files();
        if let Some(g) = a.git_root.clone() {
            // The index stat refresh is the writer's first job (slow on a big repo).
            let gw = g.clone();
            a.gitui.write(move || {
                crate::gitops::refresh_index(&gw);
                crate::scm::Msg::Wrote(Ok(String::new()), false)
            });
            let (branch, raw) = read_status(&g);
            a.apply_status(branch, raw);
        }
        a.tree = a.read_dir("", 0);
        let pending = a.pending_backups();
        if pending > 0 {
            a.msg = format!("{pending} file(s) have unsaved changes from a closed session; open one to restore");
        }
        a
    }

    /// Reload settings.json when it (or VS Code's, when followed) changed on disk.
    pub fn check_settings(&mut self) {
        let Some(path) = self.settings_file.clone() else { return };
        let mtime = |p: &Path| fs::metadata(p).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
        let stamp = (mtime(&path), self.settings.follow_vscode.then(|| mtime(&settings::vscode_path())).flatten());
        if self.settings_stamp == Some(stamp) {
            return;
        }
        self.settings_stamp = Some(stamp);
        match settings::load(&path) {
            Ok(s) => {
                if self.settings_err.take().is_some() {
                    self.msg = "settings.json applied".into();
                }
                self.apply_settings(s);
            }
            Err(e) => {
                self.msg = format!("settings.json not applied: {e}");
                self.settings_err = Some(e);
            }
        }
    }

    pub fn apply_settings(&mut self, s: Settings) {
        theme::set_ascii(s.ascii_icons);
        editor::set_tab(s.tab_size);
        if s.theme != self.installed {
            let colors = s.theme.json["colors"].as_object().cloned().unwrap_or_default();
            theme::install(theme::build_pal(&colors), theme::build_syntax(&s.theme.json, &s.theme.extra_rules));
            self.installed = s.theme.clone();
        }
        self.rainbow_on = s.rainbow.enabled;
        self.guides_on = s.guides;
        self.minimap_on = s.minimap.enabled;
        self.rulers_on = true;
        if !s.warnings.is_empty() {
            self.msg = format!("settings.json: {}", s.warnings.join("; "));
        }
        self.settings = s;
    }

    /// "Preferences: Open Settings (JSON)": the file, created from a commented template.
    fn open_settings(&mut self) {
        let Some(path) = self.settings_file.clone() else {
            self.msg = "no settings file in this session".into();
            return;
        };
        if !path.exists() {
            let made = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| {
                fs::OpenOptions::new().write(true).create_new(true).open(&path)?.write_all(settings::TEMPLATE.as_bytes())
            });
            if let Err(e) = made {
                self.msg = format!("{}: {e}", path.display());
                return;
            }
        }
        self.open_file(self.ag, &path.to_string_lossy(), false);
        self.focus = Focus::Editor;
    }

    fn run_command(&mut self, c: Cmd) {
        let on_off = |b: bool| if b { "on" } else { "off" };
        match c {
            Cmd::OpenSettings => self.open_settings(),
            Cmd::ToggleRainbow => {
                self.rainbow_on = !self.rainbow_on;
                self.msg = format!("indent rainbow {}", on_off(self.rainbow_on));
            }
            Cmd::ToggleRuler => {
                self.ruler_on = !self.ruler_on;
                self.msg = format!("minimap ruler {}", on_off(self.ruler_on));
            }
            Cmd::ToggleGuides => {
                self.guides_on = !self.guides_on;
                self.msg = format!("indent guides {}", on_off(self.guides_on));
            }
            Cmd::ToggleSidebar => {
                self.side_visible = !self.side_visible;
                if !self.side_visible && self.focus == Focus::Side {
                    self.focus = Focus::Editor;
                }
                self.save_layout();
            }
            Cmd::ToggleMinimap => {
                self.minimap_on = !self.minimap_on;
                self.msg = format!("minimap {}", on_off(self.minimap_on));
            }
            Cmd::ToggleRulers => {
                self.rulers_on = !self.rulers_on;
                self.msg = format!("rulers {}", on_off(self.rulers_on));
            }
            Cmd::NextChange => self.peek_step(1),
            Cmd::PrevChange => self.peek_step(-1),
            Cmd::QuickOpen => self.open_quick(),
            Cmd::Save => self.save_active(),
            Cmd::Nav(a) => crate::navui::command(self, a),
            Cmd::Git(g) => crate::gitcmds::run_cmd(self, g),
            Cmd::ReloadSettings => {
                self.settings_stamp = None;
                self.check_settings();
                if self.settings_err.is_none() {
                    self.msg = "settings reloaded".into();
                }
            }
        }
    }

    // ---- quick diff ------------------------------------------------------------

    /// Fetch the index version of every open file (after open, stage, focus...).
    fn load_bases(&mut self) {
        let keys: Vec<String> = self
            .docs
            .iter()
            .filter(|(k, d)| k.starts_with("file:") && matches!(d, Doc::Text(b) if b.readonly.is_none()))
            .map(|(k, _)| k.clone())
            .collect();
        self.qdiff.retain(|k, _| keys.contains(k));
        for key in keys {
            // No repo, or outside it: no base, no gutter marks, no git call.
            let Some(rel) = self.repo_path(&key["file:".len()..]) else { continue };
            self.load(format!("base:{key}"), move |root| Load::Base(git::blob(&root, &format!(":0:{rel}"))));
        }
    }

    /// Recompute the gutter marks of buffers edited since their last diff.
    fn update_qdiff(&mut self) {
        self.qdiff_due = None;
        for (key, qd) in self.qdiff.iter_mut() {
            let Some(Doc::Text(b)) = self.docs.get(key) else { continue };
            if qd.ver == Some(b.version()) && qd.marks.len() == b.lines.len() {
                continue;
            }
            qd.ver = Some(b.version());
            (qd.marks, qd.hunks) = match &qd.base {
                Some(base) => decor::quick_diff(base, &b.lines),
                None => (Vec::new(), Vec::new()),
            };
        }
        // The peek follows its hunks: the same index if it still exists, else gone.
        if let Some(pk) = &mut self.peek {
            match self.qdiff.get(&pk.key).map_or(0, |q| q.hunks.len()) {
                0 => self.peek = None,
                n => pk.hunk = pk.hunk.min(n - 1),
            }
        }
    }

    // ---- quick-diff peek ----------------------------------------------------------

    /// The active editor's document key and cursor line.
    fn active_text(&self) -> Option<(usize, String, usize)> {
        let grp = self.groups.get(self.ag)?;
        let tab = grp.tabs.get(grp.active)?;
        matches!(self.docs.get(&tab.key), Some(Doc::Text(_))).then(|| (self.ag, tab.key.clone(), tab.view.cursor.line))
    }

    fn hunks(&self, key: &str) -> &[Hunk] {
        self.qdiff.get(key).map_or(&[][..], |q| &q.hunks)
    }

    /// Open the peek on hunk `i` of group `g`'s active document and bring it into view.
    fn show_peek(&mut self, g: usize, key: String, i: usize) {
        let Some(h) = self.hunks(&key).get(i).cloned() else { return };
        self.peek = Some(Peek { group: g, key, hunk: i, scroll: 0 });
        self.ag = g;
        self.focus = Focus::Editor;
        if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
            let rows = self.ui.groups.get(g).map_or(20, |gh| gh.text.height as usize);
            // As VS Code: the cursor on the hunk's last line, and the zone below it on
            // screen even for a hunk taller than the view (context above when it fits).
            if h.mark_line() < t.view.scroll || h.peek_before() + rows / 3 > t.view.scroll + rows {
                t.view.scroll = h.mark_line().saturating_sub(3).max((h.peek_before() + rows / 3).saturating_sub(rows));
            }
            t.view.cursor = Pos::new(if h.cur.is_empty() { h.mark_line() } else { h.cur.end - 1 }, 0);
            t.view.anchor = None;
        }
    }

    /// A click on the gutter marker of `line`.
    fn peek_at_line(&mut self, g: usize, line: usize) {
        let Some(key) = self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map(|t| t.key.clone()) else { return };
        if let Some(i) = self.hunks(&key).iter().position(|h| h.has_line(line)) {
            self.show_peek(g, key, i);
        }
    }

    /// Alt+F5 / Shift+Alt+F5 (and the palette): the next / previous change from
    /// the peek or the cursor, wrapping around, shown in the peek.
    pub fn peek_step(&mut self, d: isize) {
        let Some((g, key, line)) = self.active_text() else { return };
        let n = self.hunks(&key).len();
        if n == 0 {
            self.msg = "No changes".into();
            return;
        }
        let i = match self.peek.as_ref().filter(|p| p.key == key && p.group == g) {
            Some(p) => (p.hunk as isize + d).rem_euclid(n as isize) as usize,
            None if d > 0 => self.hunks(&key).iter().position(|h| h.mark_line() > line).unwrap_or(0),
            None => self.hunks(&key).iter().rposition(|h| h.mark_line() < line).unwrap_or(n - 1),
        };
        self.show_peek(g, key, i);
    }

    /// Revert: the hunk's lines become the index version again, in the buffer
    /// (one undo step; not a git operation).
    /// The peek's hunk still describes the buffer (the diff runs 200 ms after an
    /// edit): else re-diff now and ask again, rather than act on moved lines.
    fn peek_fresh(&mut self) -> bool {
        let Some(pk) = self.peek.as_ref() else { return false };
        let ver = match self.docs.get(&pk.key) {
            Some(Doc::Text(b)) => b.version(),
            _ => return false,
        };
        if self.qdiff.get(&pk.key).is_some_and(|q| q.ver == Some(ver)) {
            return true;
        }
        self.update_qdiff();
        self.msg = "Changes moved, try again".into();
        false
    }

    fn peek_revert(&mut self) {
        if !self.peek_fresh() {
            return;
        }
        let Some(pk) = self.peek.clone() else { return };
        let Some(q) = self.qdiff.get(&pk.key) else { return };
        if !q.base_utf8 {
            self.msg = "the index version is not UTF-8: Revert is disabled".into();
            return;
        }
        let (Some(base), Some(h)) = (q.base.as_ref(), q.hunks.get(pk.hunk).cloned()) else { return };
        let with = base[h.base.clone()].to_vec();
        let Some(tab) = self.groups.get_mut(pk.group).and_then(|grp| grp.tabs.iter_mut().find(|t| t.key == pk.key)) else { return };
        let ok = match self.docs.get_mut(&pk.key) {
            Some(Doc::Text(b)) => b.replace_lines(&mut tab.view, h.cur.start, h.cur.end, &with),
            _ => false,
        };
        if ok {
            tab.preview = false;
            self.msg = "reverted the change".into();
            self.update_qdiff();
            self.schedule_backup();
        } else {
            self.msg = "read-only: cannot revert".into();
        }
    }

    /// The peek's Stage. git stages the file on disk, so an unsaved buffer is
    /// saved first, after asking (Cancel by default).
    fn stage_hunk(&mut self) -> Result<(), String> {
        let pk = self.peek.clone().ok_or("no change selected")?;
        if matches!(self.docs.get(&pk.key), Some(Doc::Text(b)) if b.dirty()) {
            self.prompt = Some(Prompt {
                text: vec!["Save the file and stage this change?".into(), "Staging works on the file as saved.".into()],
                buttons: vec![("Save & Stage".into(), PAct::Git(crate::scm::GitAct::SaveStage(pk.key))), ("Cancel".into(), PAct::Cancel)],
                sel: 1,
            });
            return Ok(());
        }
        self.stage_peek_hunk()
    }

    /// Stage the peek's hunk: the git hunk (index vs the file as git stages it)
    /// over the same buffer lines, through `gitcmds::stage_hunk`, which refreshes
    /// and refetches the gutter base.
    pub(crate) fn stage_peek_hunk(&mut self) -> Result<(), String> {
        let pk = self.peek.clone().ok_or("no change selected")?;
        let g = self.git_root.clone().ok_or("no git repository")?;
        let rel = pk.key.strip_prefix("file:").and_then(|r| self.repo_path(r)).ok_or("not a file in the repository")?;
        let cur = self.qdiff.get(&pk.key).and_then(|q| q.hunks.get(pk.hunk)).map(|h| h.cur.clone()).ok_or("Changes moved, try again")?;
        // The change as the peek shows it: the git hunk must carry exactly these lines.
        let want: Vec<String> = match self.docs.get(&pk.key) {
            Some(Doc::Text(b)) => b.lines.get(cur.clone()).unwrap_or_default().to_vec(),
            _ => return Err("not a text file".into()),
        };
        // All of it on the index writer: the sides, the matching git hunk, the apply.
        self.gitui.write(move || {
            let staged = crate::gitops::stage_sides(&g, &rel).and_then(|(index, work)| {
                let h = crate::gitops::hunks(&index, &work)
                    .into_iter()
                    .find(|h| {
                        let n = h.new_start..h.new_start + h.new_len;
                        let lines = String::from_utf8_lossy(&work).split_inclusive('\n').skip(n.start).take(n.len()).map(|l| l.trim_end_matches(['\n', '\r']).to_string()).collect::<Vec<_>>();
                        let same = lines.iter().map(|l| l.trim_end_matches('\r')).eq(want.iter().map(|l| l.trim_end_matches('\r')));
                        let at = if n.is_empty() || cur.is_empty() { n.is_empty() && cur.is_empty() && n.start == cur.start } else { n.start < cur.end && cur.start < n.end };
                        at && same
                    })
                    .ok_or("Changes moved, try again")?;
                crate::gitops::stage_hunk(&g, &rel, &h)
            });
            crate::gitops::refresh_index(&g);
            crate::scm::Msg::Wrote(staged.map(|_| "staged the change".into()), false)
        });
        self.peek = None;
        self.msg = "staging the change…".into();
        Ok(())
    }

    fn peek_button(&mut self, b: PeekBtn) {
        match b {
            PeekBtn::Stage => {
                if !self.peek_fresh() {
                    return;
                }
                if let Err(e) = self.stage_hunk() {
                    self.msg = e;
                }
            }
            PeekBtn::Revert => self.peek_revert(),
            PeekBtn::Prev => self.peek_step(-1),
            PeekBtn::Next => self.peek_step(1),
            PeekBtn::Close => self.peek = None,
        }
    }

    fn schedule_qdiff(&mut self) {
        let stale = self.qdiff.iter().any(|(k, q)| matches!(self.docs.get(k), Some(Doc::Text(b)) if q.ver != Some(b.version())));
        if stale && self.qdiff_due.is_none() {
            self.qdiff_due = Some(Instant::now() + QDIFF_DEBOUNCE);
        }
    }

    /// Files under the root with a backup no running panel owns.
    fn pending_backups(&self) -> usize {
        let Some(dir) = &self.backup_dir else { return 0 };
        let files: std::collections::HashSet<String> = editor::orphan_backups(dir, PANEL_COMM)
            .into_iter()
            .filter_map(|(_, v)| v["path"].as_str().map(String::from))
            .filter(|p| Path::new(p).starts_with(&self.root))
            .collect();
        files.len()
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
                if self.backed.get(key) != Some(&b.version()) && editor::write_backup(&dir, &self.backup_scope, &self.backup_instance, b).is_ok() {
                    self.backed.insert(key.clone(), b.version());
                }
            }
        }
        let stale: Vec<String> = self.backed.keys().filter(|k| !seen.contains(k)).cloned().collect();
        for key in stale {
            self.backed.remove(&key);
            if let Some(rel) = key.strip_prefix("file:") {
                editor::remove_backup(&dir, &self.backup_scope, &self.backup_instance, &self.root.join(rel));
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
    pub(crate) fn save_doc(&mut self, key: &str) -> Result<(), String> {
        let Some(Doc::Text(b)) = self.docs.get(key) else { return Ok(()) };
        if !b.dirty() {
            return Ok(());
        }
        if let Some(dir) = &self.backup_dir {
            editor::write_backup(dir, &self.backup_scope, &self.backup_instance, b).map_err(|e| format!("backup before save failed: {e}"))?;
            self.backed.insert(key.to_string(), b.version());
        }
        let r = match self.docs.get_mut(key) {
            Some(Doc::Text(b)) => b.save(),
            _ => Ok(()),
        };
        self.sync_backups();
        r
    }

    /// A file just opened has backups no running panel owns (any session or
    /// panel): offer the newest; the others wait for the next open.
    pub(crate) fn offer_restore(&mut self, key: &str) {
        let (Some(dir), Some(rel)) = (&self.backup_dir, key.strip_prefix("file:")) else { return };
        let path = self.root.join(rel);
        let mine = path.to_string_lossy();
        let found: Vec<(PathBuf, serde_json::Value)> =
            editor::orphan_backups(dir, PANEL_COMM).into_iter().filter(|(_, v)| v["path"].as_str() == Some(&mine)).collect();
        let Some((file, rec)) = found.first().cloned() else { return };
        let mut text = vec![
            format!("{} has unsaved changes from a closed session.", rel),
            "Restore them into the editor, or discard the backup?".into(),
        ];
        if found.len() > 1 {
            text.push(format!("{} backups found: this is the newest.", found.len()));
        }
        if Buffer::backup_is_stale(&rec, &path) {
            text.push("The file changed on disk after the backup was taken.".into());
        }
        self.prompt = Some(Prompt {
            text,
            buttons: vec![
                ("Restore".into(), PAct::RestoreBackup(key.to_string(), file.clone())),
                ("Discard Backup".into(), PAct::DiscardBackup(file)),
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
            let next = |wait| event::poll(wait).and_then(|ready| if ready { event::read().map(Some) } else { Ok(None) });
            self.step(t, &mut dirty, next)?;
        }
        Ok(())
    }

    /// One turn of the event loop: timers, background results, then the frame (so what
    /// `drain` brought is on screen before the wait), then at most one input event from
    /// `next`, given how long it may wait. `dirty` carries a redraw the event asked for.
    /// Returns that wait.
    pub(crate) fn step<B: Backend>(
        &mut self,
        t: &mut Terminal<B>,
        dirty: &mut bool,
        mut next: impl FnMut(Duration) -> io::Result<Option<Event>>,
    ) -> io::Result<Duration> {
        if self.backup_due.is_some_and(|t| Instant::now() >= t) {
            self.sync_backups();
        }
        if self.qdiff_due.is_some_and(|t| Instant::now() >= t) {
            self.update_qdiff();
            *dirty = true;
        }
        if self.refresh_due.is_some_and(|t| Instant::now() >= t) {
            self.refresh_due = None;
            self.refresh();
            *dirty = true;
        }
        // Source Control timers: the resting-pointer card, the blame delay, the op spinner.
        *dirty |= crate::scm::tick(self);
        *dirty |= crate::session::tick(self);
        *dirty |= self.drain();
        if std::mem::take(dirty) {
            t.draw(|f| crate::ui::draw(self, f))?;
            self.last_draw = Some(Instant::now());
        }
        // Wake often only while rg/git output streams in or a backup is pending,
        // so a HUP is noticed within 100 ms whenever there is something to save.
        let busy = self.search.is_some()
            || self.queued_search.is_some()
            || !self.loading.is_empty()
            || self.backup_due.is_some()
            || self.nav.busy()
            || self.gitui.busy();
        let mut wait = if busy { Duration::from_millis(100) } else { Duration::from_secs(5) };
        for t in [self.qdiff_due, self.gitui.due(), self.refresh_due, self.session.due].into_iter().flatten() {
            wait = wait.min(t.saturating_duration_since(Instant::now()));
        }
        // A far jump's first highlighting catches up one budgeted slice per frame
        // (editor::FrameBudget): draw again soon, input still comes first.
        if self.hl_pending() {
            *dirty = true;
            wait = wait.min(Duration::from_millis(2));
        }
        // A key or click held back by the last batch runs now, after that batch's frame.
        let ev = match self.pending_ev.take() {
            Some(ev) => Some(ev),
            None => match next(wait) {
                Ok(ev) => ev,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(wait),
                Err(e) => return Err(e),
            },
        };
        let Some(ev) = ev else { return Ok(wait) };
        let batch = coalescable(&ev);
        *dirty |= self.handle_event(ev);
        if batch && coalesce() {
            // Wheel and plain motion come in bursts: take every one queued, and those that
            // arrive until a frame is due (FRAME after the last), then draw once. A key or
            // click ends the batch and waits for that frame: clicks hit-test the drawn
            // layout, and keys are never held for the cap.
            let start = Instant::now();
            let due = self.last_draw.map_or(start, |d| (d + FRAME).max(start));
            loop {
                let now = Instant::now();
                // A stream that never pauses still gets a frame every FRAME of handling.
                if now >= due.max(start) + FRAME {
                    break;
                }
                match next(due.saturating_duration_since(now)) {
                    Ok(Some(e)) if coalescable(&e) => *dirty |= self.handle_event(e),
                    Ok(Some(e)) => {
                        self.pending_ev = Some(e);
                        break;
                    }
                    Ok(None) => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(if self.pending_ev.is_some() { Duration::ZERO } else { wait })
    }

    fn handle_event(&mut self, ev: Event) -> bool {
        let redraw = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                crate::navui::on_key(self, k);
                self.schedule_backup();
                crate::session::touch(self);
                true
            }
            Event::Mouse(m) => {
                if m.kind != MouseEventKind::Moved {
                    crate::session::touch(self); // clicks, drags, wheel: tabs, cursors, scroll
                }
                crate::navui::on_mouse(self, m)
            }
            Event::Paste(s) => {
                self.on_paste(&s);
                self.schedule_backup();
                crate::session::touch(self);
                true
            }
            // Debounced: focus flickers between windows run one refresh.
            Event::FocusGained => {
                if !async_status() {
                    self.refresh();
                    return true;
                }
                self.refresh_due.get_or_insert(Instant::now() + FOCUS_DEBOUNCE);
                false
            }
            Event::Resize(..) => true,
            _ => false,
        };
        // Any edit (keys, paste, a Replace All click) re-diffs the gutter shortly after.
        self.schedule_qdiff();
        redraw
    }

    // ---- git / filesystem ------------------------------------------------------

    /// A repo path (git status, SCM rows) as a document path: relative under the root, else absolute.
    pub fn doc_path(&self, repo_rel: &str) -> String {
        match (repo_rel.strip_prefix(&self.prefix), &self.git_root) {
            (Some(r), _) => r.to_string(),
            (None, Some(g)) => g.join(repo_rel).to_string_lossy().into_owned(),
            (None, None) => repo_rel.to_string(),
        }
    }

    /// A document path (relative to the root, or absolute) as a repo path; None outside the repo.
    pub fn repo_path(&self, rel: &str) -> Option<String> {
        let g = self.git_root.as_ref()?;
        Some(self.root.join(rel).strip_prefix(g).ok()?.to_string_lossy().into_owned())
    }

    /// Re-read git status (in the background: the old status shows until the new one
    /// lands), decorations, and the disk state of open files.
    pub fn refresh(&mut self) {
        self.refresh_files();
        self.refresh_status();
    }

    /// `git status` as a job. One at a time: a refresh meanwhile reruns it after.
    fn refresh_status(&mut self) {
        let Some(g) = self.git_root.clone() else { return };
        if !async_status() {
            let (branch, raw) = read_status(&g);
            return self.apply_status(branch, raw);
        }
        if self.loading.contains_key(STATUS_KEY) {
            self.status_again = true;
            return;
        }
        self.status_since = Some(Instant::now());
        self.load(STATUS_KEY.into(), move |_| {
            let (branch, raw) = read_status(&g);
            Load::Status(branch, raw)
        });
    }

    /// The status `git status` reported: SCM rows, Explorer marks, then what follows them.
    fn apply_status(&mut self, branch: String, raw: Vec<u8>) {
        self.status_since = None;
        self.branch = branch;
        self.status = parse::parse_porcelain_z(&raw);
        let (staged, changes) = git::split(&self.status);
        self.scm_files = staged.into_iter().chain(changes).collect();
        // The Explorer shows the root: its files' marks, relative to it.
        self.marks = self
            .status
            .iter()
            .filter_map(|e| Some((e.path.strip_prefix(&self.prefix)?.to_string(), git::decoration(e)?)))
            .collect();
        self.dirty_dirs = git::folder_marks(self.marks.iter().map(|(p, c)| (p.as_str(), *c)));
        crate::scm::on_refresh(self);
    }

    /// The disk state of open files, the gutter bases, settings, visible diffs.
    fn refresh_files(&mut self) {
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
        // Staging changes the index: the gutter compares against the new one.
        self.load_bases();
        self.check_settings();
        // Visible diff editors follow the new state.
        let visible: Vec<(String, ScmFile)> = self
            .groups
            .iter()
            .filter_map(|g| g.tabs.get(g.active))
            .filter_map(|t| match self.docs.get(&t.key) {
                // A commit's diff never changes.
                Some(Doc::Diff(d)) if !t.key.starts_with("cdiff:") => Some((t.key.clone(), d.file.clone())),
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

    pub(crate) fn toggle(&mut self, i: usize) {
        let (dir, open, depth, path) = {
            let n = &self.tree[i];
            (n.dir, n.open, n.depth, n.path.clone())
        };
        if !dir {
            return;
        }
        self.tree_parents.get_mut().clear();
        if open {
            let end = (i + 1..self.tree.len()).find(|&j| self.tree[j].depth <= depth).unwrap_or(self.tree.len());
            self.tree.drain(i + 1..end);
        } else {
            let kids = self.read_dir(&path, depth + 1);
            self.tree.splice(i + 1..i + 1, kids);
        }
        self.tree[i].open = !open;
    }

    pub(crate) fn reload_all(&mut self) {
        self.refresh();
        let open: Vec<String> = self.tree.iter().filter(|n| n.open).map(|n| n.path.clone()).collect();
        self.tree = self.read_dir("", 0);
        self.tree_parents.get_mut().clear();
        let mut i = 0;
        while i < self.tree.len() {
            if open.contains(&self.tree[i].path) {
                self.toggle(i);
            }
            i += 1;
        }
        self.files.clear();
        if self.side == Side::Scm {
            crate::scm::load_graph(self, true);
        }
    }

    // ---- search ------------------------------------------------------------------

    /// Search for the query and toggles as they are now (Enter, a toggle, Refresh).
    pub fn start_search(&mut self) {
        let req = (self.query.clone(), self.s_opts);
        self.run_search(req);
    }

    fn run_search(&mut self, (query, (case, word, regex)): (String, (bool, bool, bool))) {
        self.stop_search();
        self.hits.clear();
        self.sel[1] = 0;
        self.off[1] = 0;
        self.queued_search = None;
        if query.is_empty() {
            return;
        }
        // Without kill, an abandoned rg runs until it ends: cap them, and run only
        // the latest request, as submitted, once one has ended. The slot is ours from here.
        if !take_slot(&self.live_rg) {
            self.queued_search = Some((query, (case, word, regex)));
            self.msg = "waiting for the previous search…".into();
            return;
        }
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
            .arg(&query)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.live_rg.fetch_sub(1, Ordering::SeqCst);
                self.msg = format!("rg: {e}");
                return;
            }
        };
        let Some(out) = child.stdout.take() else {
            reap(child, Some(self.live_rg.clone()));
            return;
        };
        let (tx, gen) = (self.tx.clone(), self.gen);
        let stop = Arc::new(AtomicBool::new(false));
        self.search_stop = stop.clone();
        std::thread::spawn(move || {
            let mut sent = 0;
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                // Cancelled or past the cap: stop reading. Closing the pipe ends rg
                // at its next write (EPIPE); no signal is sent.
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let Some(h) = parse::parse_rg_line(&String::from_utf8_lossy(&line)) else { continue };
                sent += 1;
                if tx.send((gen, Some(h))).is_err() || sent > MAX_HITS {
                    return;
                }
            }
            let _ = tx.send((gen, None));
        });
        self.search = Some(child);
        self.msg = format!("searching {query:?}…");
    }

    /// Abandon the running search: its reader stops, rg exits on the closed
    /// pipe (or when done) and is reaped in the background.
    pub fn stop_search(&mut self) {
        self.gen += 1;
        self.search_stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.search.take() {
            reap(c, Some(self.live_rg.clone()));
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
                    self.stop_search();
                    self.msg = format!("{MAX_HITS}+ hits, search stopped");
                }
                Some(h) => self.hits.push(h),
                None => {
                    // rg closed stdout, so it is exiting: wait (not kill) to keep its exit code.
                    self.gen += 1;
                    let code = self.search.take().and_then(|mut c| {
                        let st = c.wait().ok();
                        self.live_rg.fetch_sub(1, Ordering::SeqCst);
                        st
                    });
                    let code = code.and_then(|s| s.code());
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
            if let Load::Status(branch, raw) = load {
                let since = self.status_since;
                self.apply_status(branch, raw);
                if std::mem::take(&mut self.status_again) {
                    // What landed is already older than the refresh asked for meanwhile:
                    // the spinner keeps its start, so it stays on until the rerun lands.
                    self.refresh_status();
                    self.status_since = since.or(self.status_since);
                }
                changed = true;
                continue;
            }
            if let Load::Base(blob) = load {
                let doc = key.trim_start_matches("base:").to_string();
                // Decoded exactly as the buffer is, so Revert writes the index's own lines.
                let utf8 = blob.as_ref().is_none_or(|b| std::str::from_utf8(b).is_ok());
                let base: Option<Vec<String>> = blob.map(|b| editor::split_lines(&String::from_utf8_lossy(&b)).0);
                // Focus and view switches refetch the index: an unchanged one changes nothing.
                if self.docs.contains_key(&doc) && self.qdiff.get(&doc).is_none_or(|q| q.base != base) {
                    self.qdiff.insert(doc, QDiff { base, base_utf8: utf8, ..Default::default() });
                    self.qdiff_due.get_or_insert(Instant::now() + QDIFF_DEBOUNCE);
                }
                continue;
            }
            // A tab closed meanwhile does not resurrect its document.
            if !self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key)) {
                continue;
            }
            let doc = match load {
                Load::Diff(d) => Doc::Diff(d),
                Load::Base(_) | Load::Status(..) | Load::Cancelled => continue,
            };
            self.docs.insert(key, doc);
            changed = true;
        }
        if self.queued_search.is_some() && self.live_rg.load(Ordering::SeqCst) < MAX_LIVE_RG {
            if let Some(req) = self.queued_search.take() {
                self.run_search(req);
            }
            changed = true;
        }
        changed | crate::navui::poll(self) | crate::scm::drain(self)
    }

    fn load(&mut self, key: String, job: impl FnOnce(PathBuf) -> Load + Send + 'static) {
        self.load_gen += 1;
        let gen = self.load_gen;
        self.loading.insert(key.clone(), gen);
        // Every job is git's: it runs in the repo.
        let (tx, root) = (self.load_tx.clone(), self.git_root.clone().unwrap_or_else(|| self.root.clone()));
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
            Load::Diff(DiffDoc { title, rows, note, file: f, id: Default::default() })
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
    pub(crate) fn show_tab(&mut self, g: usize, key: String, preview: bool) {
        if let Some(i) = self.groups[g].tabs.iter().position(|t| t.key == key) {
            let grp = &mut self.groups[g];
            grp.active = i;
            if !preview {
                grp.tabs[i].preview = false;
            }
            self.ag = g;
            return;
        }
        // A file opened again comes back where it was (workbench.editor.restoreViewState).
        let view = crate::session::recall(self, &key);
        let grp = &mut self.groups[g];
        if let Some(i) = grp.tabs.iter().position(|t| t.preview).filter(|_| preview) {
            let old = std::mem::replace(&mut grp.tabs[i], Tab { key, preview: true, view });
            grp.active = i;
            crate::session::remember(self, &old.key, &old.view);
            self.gc(&old.key);
        } else {
            let at = if grp.tabs.is_empty() { 0 } else { grp.active + 1 };
            grp.tabs.insert(at, Tab { key, preview, view });
            grp.active = at;
        }
        self.ag = g;
    }

    /// A restored tab's document, first drawn (`session::materialize`): a file (with
    /// its hot-exit backup offered), a working-tree or index diff, or a commit diff.
    pub(crate) fn open_restored(&mut self, key: &str) {
        if let Some(rel) = key.strip_prefix("file:") {
            self.file_doc(rel);
            // One prompt at a time: with another one open, this offer follows it.
            crate::session::offer(self, key);
            self.load_bases();
        } else if let Some((sec, path)) = key.strip_prefix("diff:").and_then(|r| r.split_once(':')) {
            let f = self.scm_files.iter().find(|f| f.path == path && (f.section == Section::Staged) == (sec == "staged")).cloned();
            if let Some(f) = f {
                self.load_diff(key.to_string(), f, None);
            }
        } else if let Some((hash, path)) = key.strip_prefix("cdiff:").and_then(|r| r.split_once(':')) {
            crate::scm::load_commit_diff(self, key, hash, path);
        }
    }

    /// Drop a document nobody shows any more (a dirty one is kept for the quit prompt).
    fn gc(&mut self, key: &str) {
        let used = self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key));
        let dirty = matches!(self.docs.get(key), Some(Doc::Text(b)) if b.dirty());
        if !used && !dirty {
            self.drop_doc(key);
        }
    }

    /// Forget document `key` and everything kept per document, in one place: the
    /// buffer or diff, its loads (and its index base's), the quick diff and its peek,
    /// a diff's minimap buffer, its conflict markers. Backups are bookkept by
    /// `sync_backups`, which also deletes the file of one no longer needed.
    fn drop_doc(&mut self, key: &str) {
        self.docs.remove(key);
        self.loading.remove(key);
        self.loading.remove(&format!("base:{key}"));
        self.qdiff.remove(key);
        self.diff_minis.remove(key);
        self.gitui.cmds.conflicts.remove(key);
        crate::session::forget(self, key);
        if self.peek.as_ref().is_some_and(|p| p.key == key) {
            self.peek = None;
        }
        if self.diff_cancel.as_ref().is_some_and(|(k, _)| k == key) {
            if let Some((_, stop)) = self.diff_cancel.take() {
                stop.store(true, Ordering::SeqCst);
            }
        }
    }

    pub fn open_file(&mut self, g: usize, rel: &str, preview: bool) {
        let fresh = !self.docs.contains_key(&format!("file:{rel}"));
        let key = self.file_doc(rel);
        self.show_tab(g, key.clone(), preview);
        if fresh {
            self.offer_restore(&key);
            self.load_bases();
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

    pub fn tab_title(&self, key: &str) -> String {
        match self.docs.get(key) {
            Some(Doc::Diff(d)) => d.title.clone(),
            _ => match key.split_once(':') {
                Some(("diff" | "cdiff", rest)) => base(rest.split_once(':').map_or(rest, |(_, p)| p)).to_string(),
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
        // Rows the peek zone takes above the cursor are not text rows.
        let zone = self.ui.groups.get(self.ag).and_then(|g| g.zone).map_or(0, |(_, zh)| zh as usize);
        let before = self.peek.as_ref().filter(|p| p.group == self.ag).and_then(|p| self.qdiff.get(&p.key)?.hunks.get(p.hunk)).map(Hunk::peek_before);
        let Some((b, tab)) = self.ed() else { return };
        let v = &mut tab.view;
        let c = b.clamp(v.cursor);
        let h = if before.is_some_and(|l| c.line >= l) { h.saturating_sub(zone).max(1) } else { h };
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
        let tab = grp.tabs.remove(i);
        if i < grp.active || grp.active >= grp.tabs.len() {
            grp.active = grp.active.saturating_sub(1);
        }
        if grp.tabs.is_empty() && self.groups.len() > 1 {
            self.groups.remove(g);
            if self.ag >= g && self.ag > 0 {
                self.ag -= 1;
            }
        }
        crate::session::remember(self, &tab.key, &tab.view);
        if refs == 1 {
            // Closing the last view (after Save or an explicit Don't Save) also
            // retires its hot-exit backup.
            self.drop_doc(&key);
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
        // Saving settings.json in this editor applies it at once.
        self.check_settings();
    }

    pub fn request_quit(&mut self) {
        // Queued or running git writes (a commit, a stage...) would be lost: ask, Wait first.
        let w = self.gitui.writes();
        if w > 0 && !self.gitui.quit_anyway {
            self.prompt = Some(Prompt {
                text: vec![
                    format!("Git is still writing ({w} operation{}).", if w == 1 { "" } else { "s" }),
                    "Wait quits when it is done; Quit Anyway lets it finish after the window closes.".into(),
                ],
                buttons: vec![("Wait".into(), PAct::Git(crate::scm::GitAct::QuitWait)), ("Quit Anyway".into(), PAct::Git(crate::scm::GitAct::QuitAnyway))],
                sel: 0,
            });
            return;
        }
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
                let Some(g) = self.git_root.clone() else { return };
                // The index writer runs it, maybe after a slow op queued ahead: it acts only
                // on the files as they were when the user confirmed.
                let seen = file_stamps(&g, &files);
                self.gitui.write(move || {
                    if file_stamps(&g, &files) != seen {
                        return crate::scm::Msg::Wrote(Err("File changed since you confirmed; discard cancelled".into()), true);
                    }
                    let r = git::discard(&g, &files).map(|_| format!("discarded {} file(s)", files.len()));
                    crate::gitops::refresh_index(&g);
                    crate::scm::Msg::Wrote(r, true)
                });
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
            PAct::Git(a) => crate::scm::run_act(self, a),
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
                let dirty: Vec<String> = self.docs.iter().filter(|(_, d)| matches!(d, Doc::Text(b) if b.dirty())).map(|(k, _)| k.clone()).collect();
                for k in dirty {
                    self.drop_doc(&k);
                }
                self.sync_backups();
                self.quit = true;
            }
            PAct::RestoreBackup(key, file) => {
                let rec = key.strip_prefix("file:").and_then(|rel| editor::read_backup(&file, &self.root.join(rel)));
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
                // Our own backup now holds the restored text: retire the chosen one,
                // unless it is that very file or ours could not be written.
                let ours = key.strip_prefix("file:").zip(self.backup_dir.as_ref()).map(|(rel, dir)| {
                    editor::backup_file(dir, &self.backup_scope, &self.backup_instance, &self.root.join(rel))
                });
                if restored && self.backed.contains_key(&key) && ours.as_ref() != Some(&file) {
                    let _ = fs::remove_file(&file);
                }
            }
            PAct::DiscardBackup(file) => {
                let _ = fs::remove_file(&file);
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

    /// VS Code's groups: Merge Changes (conflicts), Staged Changes, Changes.
    pub fn scm_rows(&self) -> Vec<SRow> {
        let idx = |keep: &dyn Fn(&ScmFile) -> bool| (0..self.scm_files.len()).filter(|&i| keep(&self.scm_files[i])).collect::<Vec<_>>();
        let merge = idx(&|f| f.section == Section::Changes && f.letter == '!');
        let staged = idx(&|f| f.section == Section::Staged);
        let changes = idx(&|f| f.section == Section::Changes && f.letter != '!');
        let gap = self.settings.spacing.section_gap;
        let mut rows = Vec::new();
        for (head, files) in [(SRow::MergeHead(merge.len()), merge), (SRow::Head(Section::Staged, staged.len()), staged)] {
            if !files.is_empty() {
                rows.push(head);
                rows.extend(files.into_iter().map(SRow::File));
                rows.extend((0..gap).map(|_| SRow::Gap));
            }
        }
        rows.push(SRow::Head(Section::Changes, changes.len()));
        rows.extend(changes.into_iter().map(SRow::File));
        rows
    }

    /// A group's files; Changes leaves out the conflicts (Merge Changes has them).
    fn section_files(&self, s: Section) -> Vec<ScmFile> {
        self.scm_files.iter().filter(|f| f.section == s && (s == Section::Staged || f.letter != '!')).cloned().collect()
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
        let Some(g) = self.git_root.clone() else { return };
        let one = |i: usize| self.scm_files.get(i).cloned().into_iter().collect::<Vec<_>>();
        let res = match a {
            ScmAct::OpenFile(i) => {
                if let Some(f) = self.scm_files.get(i).cloned() {
                    self.open_file(self.ag, &self.doc_path(&f.path), false);
                    self.focus = Focus::Editor;
                }
                return;
            }
            ScmAct::Discard(i) => return self.confirm_discard(one(i)),
            ScmAct::DiscardAll => return self.confirm_discard(self.section_files(Section::Changes)),
            ScmAct::Stage(i) => (true, one(i)),
            ScmAct::Unstage(i) => (false, one(i)),
            ScmAct::StageAll => (true, self.section_files(Section::Changes)),
            ScmAct::UnstageAll => (false, self.section_files(Section::Staged)),
            ScmAct::StageMerge => (true, self.scm_files.iter().filter(|f| f.letter == '!').cloned().collect()),
        };
        // On the index writer, in order; status follows each through drain.
        let (stage, files) = res;
        self.gitui.write(move || {
            let r = if stage { git::stage(&g, &files) } else { git::unstage(&g, &files) };
            crate::gitops::refresh_index(&g);
            crate::scm::Msg::Wrote(r.map(|_| String::new()), false)
        });
    }

    /// "Add commit": the git-gui style commit dialog.
    pub fn commit(&mut self) {
        crate::commitdlg::open(self);
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
        }
    }

    pub fn show_side(&mut self, s: Side, input: bool) {
        self.side = s;
        self.side_visible = true;
        self.focus = match s {
            Side::Search if input => Focus::SearchInput,
            _ => Focus::Side,
        };
        self.refresh();
        self.save_layout();
    }

    pub(crate) fn save_layout(&self) {
        let Some(p) = &self.layout_file else { return };
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        let mut v = serde_json::json!({ "sidebar_visible": self.side_visible, "scm": self.gitui.layout() });
        v["layout_version"] = 2.into();
        if let Some(w) = self.side_w {
            // Written as the sidebar's outer width, as it is drawn.
            let w = w + if self.side_w_legacy { crate::blocks::frame(self.settings.blocks) } else { 0 };
            v["sidebar_width"] = w.into();
            v["sidebar_dragged"] = true.into();
        }
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

    /// The depth-`k` folder holding Explorer row `i`, by parent links rebuilt once
    /// per tree change (O(rows)); then O(depth) a call, however far the folder is.
    fn tree_ancestor(&self, i: usize, k: usize) -> Option<usize> {
        if self.tree.get(i)?.depth <= k {
            return None;
        }
        let mut up = self.tree_parents.borrow_mut();
        if up.len() != self.tree.len() {
            up.clear();
            let mut open: Vec<usize> = Vec::new();
            for (j, n) in self.tree.iter().enumerate() {
                while open.last().is_some_and(|&o| self.tree[o].depth >= n.depth) {
                    open.pop();
                }
                up.push(open.last().copied().unwrap_or(usize::MAX));
                open.push(j);
            }
        }
        let mut a = i;
        // Links point backwards, so this ends even on a stale list.
        while self.tree[a].depth > k {
            a = up.get(a).copied().filter(|&p| p < a)?;
        }
        (self.tree[a].depth == k).then_some(a)
    }

    /// Sticky scroll's pinned rows for the sidebar scrolled to `off`: Explorer folders
    /// over their contents, Search file groups over their hits, the CHANGES sections
    /// (Merge, Staged, Changes headers) over their files. `search` / `scm`: that
    /// view's rows, as the frame built them for drawing.
    pub fn pinned(&self, off: usize, cap: usize, search: &[HRow], scm: &[SRow]) -> Vec<usize> {
        match self.side {
            Side::Explorer => decor::sticky(|i, k| self.tree_ancestor(i, k), off, cap),
            Side::Search => decor::sticky(|i, k| decor::walk_back(|j| search.get(j).map(|r| matches!(r, HRow::Hit(_)) as usize), i, k), off, cap),
            Side::Scm => decor::sticky(|i, k| decor::walk_back(|j| scm.get(j).map(|r| matches!(r, SRow::File(_)) as usize), i, k), off, cap),
        }
    }

    /// `pinned` for key handling, which has no rows built.
    fn pinned_now(&self, off: usize) -> Vec<usize> {
        let (search, scm) = match self.side {
            Side::Explorer => (Vec::new(), Vec::new()),
            Side::Search => (self.search_rows(), Vec::new()),
            Side::Scm => (Vec::new(), self.scm_rows()),
        };
        self.pinned(off, self.ui.sticky_cap, &search, &scm)
    }

    /// The scroll offset that shows item `i` right under its pinned ancestors.
    fn under_pinned(&self, i: usize) -> usize {
        let depth = match self.side {
            Side::Explorer => self.tree.get(i).map_or(0, |n| n.depth),
            Side::Search => matches!(self.search_rows().get(i), Some(HRow::Hit(_))) as usize,
            Side::Scm => matches!(self.scm_rows().get(i), Some(SRow::File(_))) as usize,
        };
        i - depth.min(self.ui.sticky_cap).min(i)
    }

    /// A click on a sticky row: that folder (header) scrolls to the top and is selected.
    fn reveal_pinned(&mut self, i: usize) {
        let s = self.side as usize;
        self.off[s] = self.under_pinned(i);
        self.sel[s] = i;
        self.focus = Focus::Side;
    }

    fn move_side(&mut self, d: isize) {
        let n = self.side_count();
        if n == 0 {
            return;
        }
        let s = self.side as usize;
        self.sel[s] = (self.sel[s] as isize + d).clamp(0, n as isize - 1) as usize;
        if self.side == Side::Scm {
            let rows = self.scm_rows();
            let step = if d < 0 { -1 } else { 1 };
            for _ in 0..rows.len() {
                if !matches!(rows.get(self.sel[s]), Some(SRow::Gap)) {
                    break;
                }
                self.sel[s] = (self.sel[s] as isize + step).clamp(0, n as isize - 1) as usize;
            }
        }
        let h = self.ui.side_rows.max(1);
        if self.sel[s] < self.off[s] {
            self.off[s] = self.sel[s];
        } else if self.sel[s] >= self.off[s] + h {
            self.off[s] = self.sel[s] + 1 - h;
        }
        // A row under the sticky rows is hidden: show it just below its pinned folders.
        if self.sel[s] < self.off[s] + self.pinned_now(self.off[s]).len() {
            self.off[s] = self.under_pinned(self.sel[s]);
        }
    }

    // ---- quick open ----------------------------------------------------------------------

    fn open_quick(&mut self) {
        if self.files.is_empty() {
            // From the root, ls-files lists only what is under it, relative to it.
            let out = match self.git_root {
                Some(_) => git::cmd(&self.root).args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).stderr(Stdio::null()).output(),
                None => Command::new("rg").args(["--files", "--null"]).current_dir(&self.root).stdin(Stdio::null()).stderr(Stdio::null()).output(),
            };
            let out = out.ok().filter(|o| o.status.success()).map(|o| o.stdout).unwrap_or_default();
            self.files = out
                .split(|&b| b == 0)
                .filter(|f| !f.is_empty())
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            self.files.dedup();
        }
        let items = filter(&self.files, "");
        self.quick = Some(Quick { query: String::new(), items, sel: 0, off: 0, commands: false });
    }

    fn open_palette(&mut self) {
        self.quick = Some(Quick { query: String::new(), items: filter_commands(""), sel: 0, off: 0, commands: true });
    }

    /// Enter or click on a Quick Open / palette row.
    fn quick_pick(&mut self, i: usize) {
        let Some(q) = self.quick.take() else { return };
        let Some(item) = q.items.get(i).cloned() else { return };
        if q.commands {
            if let Some(c) = all_commands().find(|c| c.0 == item) {
                self.run_command(c.1);
            }
        } else {
            self.open_file(self.ag, &item, false);
            self.focus = Focus::Editor;
        }
    }

    fn quick_key(&mut self, k: KeyEvent, modded: bool) {
        let Some(q) = self.quick.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.quick = None,
            KeyCode::Enter => {
                let i = q.sel;
                self.quick_pick(i);
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
                q.items = if q.commands { filter_commands(&q.query) } else { filter(&self.files, &q.query) };
                q.sel = 0;
            }
            _ => {}
        }
    }

    // ---- keyboard --------------------------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) {
        // Esc drops a file or tab drag where it is, before anything else sees the key.
        if k.code == KeyCode::Esc && matches!(self.drag, Drag::File { active: true, .. } | Drag::Tab { active: true, .. }) {
            self.drag = Drag::None;
            self.drop = None;
            return;
        }
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
        if self.gitui.dlg.is_some() {
            return crate::commitdlg::key(self, k);
        }
        if self.gitui.cmds.picker.is_some() {
            return crate::gitcmds::picker_key(self, k);
        }
        if k.code == KeyCode::Esc && (self.gitui.card.take().is_some() || self.gitui.cmds.toast.take().is_some()) {
            return;
        }
        if std::mem::take(&mut self.pending_k) && k.code == KeyCode::Enter {
            return self.pin_active(); // Ctrl+K Enter: Keep Open
        }
        if ctrl && is('q') {
            return self.request_quit();
        }
        if ctrl && shift && is('p') {
            return self.open_palette();
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
            KeyCode::F(1) => return self.open_palette(),
            KeyCode::F(2) => return self.show_side(Side::Scm, false),
            KeyCode::F(4) => return self.show_side(Side::Search, true),
            KeyCode::F(3) => return self.find_step(if shift { -1 } else { 1 }),
            // VS Code's Go to Next/Previous Change (Alt+F5); F7 where Alt+F5 does not arrive.
            // In a diff tab F7 walks the diff's changes instead.
            KeyCode::F(5) if alt => return self.peek_step(if shift { -1 } else { 1 }),
            KeyCode::F(7) if self.ed().is_none() => return crate::scm::jump_change(self, if shift { -1 } else { 1 }),
            KeyCode::F(7) => return self.peek_step(if shift { -1 } else { 1 }),
            KeyCode::Char(c @ ('1' | '2' | '4')) if alt => {
                let s = match c {
                    '1' => Side::Explorer,
                    '2' => Side::Scm,
                    _ => Side::Search,
                };
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
            Focus::SearchInput => self.input_key(k, ctrl, alt),
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
            KeyCode::Esc if !p.buttons.iter().any(|b| matches!(b.1, PAct::RestoreBackup(..))) => self.prompt = None,
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
        match k.code {
            KeyCode::Enter => {
                self.start_search();
                self.focus = Focus::Side;
            }
            KeyCode::Esc if self.search.is_some() || self.queued_search.is_some() => {
                self.queued_search = None;
                self.stop_search();
                self.msg = "search cancelled".into();
            }
            KeyCode::Esc | KeyCode::Down => self.focus = Focus::Side,
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char(c) if !ctrl && !alt => self.query.push(c),
            _ => {}
        }
    }

    fn side_key(&mut self, k: KeyEvent) {
        if self.side == Side::Scm && crate::scm::key(self, k) {
            return;
        }
        let page = self.ui.side_rows.max(1) as isize;
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
            // h: the file's history, in the TIMELINE under the tree.
            KeyCode::Char('h') if self.side == Side::Explorer => {
                if let Some(n) = self.tree.get(self.sel[s]).filter(|n| !n.dir) {
                    let p = n.path.clone();
                    self.open_file(self.ag, &p, true);
                    self.gitui.timeline.open = true;
                }
            }
            KeyCode::Char('r') => self.reload_all(),
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
        if k.code == KeyCode::Esc && self.peek.is_some() {
            self.peek = None;
            return;
        }
        if k.code == KeyCode::Esc && self.find.is_some() {
            self.find = None;
            return;
        }
        let page = self.ui.groups.get(self.ag).map_or(20, |g| g.text.height.max(1) as isize);
        if self.ed().is_none() {
            // Diff views scroll, both sides at once (F7 is routed in on_key).
            let rows = match self.active_tab().map(|t| t.key.clone()).and_then(|k| self.docs.get(&k)) {
                Some(Doc::Diff(d)) => d.rows.len(),
                _ => 0,
            };
            if let Some(t) = self.active_tab() {
                let v = &mut t.view;
                let last = rows.saturating_sub(1) as isize;
                let to = |d: isize| (v.scroll as isize + d).clamp(0, last) as usize;
                match k.code {
                    KeyCode::Up => v.scroll = to(-1),
                    KeyCode::Down => v.scroll = to(1),
                    KeyCode::PageUp => v.scroll = to(-page),
                    KeyCode::PageDown => v.scroll = to(page),
                    KeyCode::Home if ctrl => v.scroll = 0,
                    // The last row at the bottom.
                    KeyCode::End if ctrl => v.scroll = rows.saturating_sub(page as usize),
                    KeyCode::Home => v.hscroll = 0,
                    // Clamped where drawn: as far as the longest visible line needs.
                    KeyCode::End => v.hscroll = 1 << 30,
                    _ => {}
                }
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

    /// A bracketed paste: a launcher request when it starts with
    /// `herdr-code:<our token>:` (never inserted anywhere), else ordinary text.
    pub fn on_paste(&mut self, s: &str) {
        let req = self.token.as_deref().and_then(|t| s.strip_prefix("herdr-code:")?.strip_prefix(t)?.strip_prefix(':'));
        match req {
            Some(body) => self.request(body),
            None => self.paste_text(s),
        }
    }

    /// A launcher request, `open:<abs path>:<line>:<col>`: open that file pinned,
    /// cursor at line:col (from 1, col in characters). Anything else is refused.
    pub fn request(&mut self, body: &str) {
        match self.parse_request(body) {
            Ok((rel, line, col)) => {
                self.quick = None;
                self.open_file(self.ag, &rel, false);
                if let Some((b, tab)) = self.ed() {
                    let l = line.saturating_sub(1).min(b.lines.len() - 1);
                    let c = b.lines[l].char_indices().nth(col.saturating_sub(1)).map_or(b.lines[l].len(), |(i, _)| i);
                    tab.view.cursor = Pos::new(l, c);
                    tab.view.anchor = None;
                    tab.view.scroll = l.saturating_sub(5);
                }
                self.reveal();
                self.focus = Focus::Editor;
                self.msg = format!("opened {rel}:{line}");
            }
            Err(e) => self.msg = format!("request refused: {e}"),
        }
    }

    /// (path relative to the root, line, col) of a valid `open:` request.
    fn parse_request(&self, body: &str) -> Result<(String, usize, usize), String> {
        let bad = || format!("{body:?}");
        let rest = body.strip_prefix("open:").ok_or_else(bad)?;
        let (rest, col) = rest.rsplit_once(':').ok_or_else(bad)?;
        let (path, line) = rest.rsplit_once(':').ok_or_else(bad)?;
        let (line, col) = line.parse::<usize>().ok().zip(col.parse::<usize>().ok()).ok_or_else(bad)?;
        if !Path::new(path).is_absolute() {
            return Err(bad());
        }
        let p = fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
        let rel = p.strip_prefix(&self.root).map_err(|_| format!("{path} is outside {}", self.root.display()))?;
        if !fs::metadata(&p).is_ok_and(|m| m.is_file()) {
            return Err(format!("{path} is not a regular file"));
        }
        Ok((rel.to_string_lossy().into_owned(), line, col))
    }

    /// Bracketed paste or Ctrl+V: into the focused input or the editor.
    pub fn paste_text(&mut self, text: &str) {
        if self.prompt.is_some() || self.quick.is_some() {
            return;
        }
        if self.gitui.dlg.is_some() {
            return crate::commitdlg::paste(self, text);
        }
        if self.gitui.cmds.picker.is_some() {
            return crate::gitcmds::picker_paste(self, text);
        }
        match self.focus {
            Focus::SearchInput => self.query.push_str(&text.replace(['\r', '\n'], " ")),
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
            // Shift+wheel and the horizontal wheel scroll a diff sideways.
            MouseEventKind::ScrollUp if shift && self.hwheel(p, -6) => {}
            MouseEventKind::ScrollDown if shift && self.hwheel(p, 6) => {}
            // No diff there: nothing moved, no redraw.
            MouseEventKind::ScrollLeft => return self.hwheel(p, -6),
            MouseEventKind::ScrollRight => return self.hwheel(p, 6),
            MouseEventKind::ScrollUp => return self.wheel(p, -3),
            MouseEventKind::ScrollDown => return self.wheel(p, 3),
            // Redraw only when what the frame shows for the pointer changes.
            MouseEventKind::Moved => {
                let old = self.hover.replace(p);
                let redraw = if hover_keyed() {
                    old.map(|q| self.hover_key(q)) != Some(self.hover_key(p))
                } else {
                    let watched = |q: Position| self.hover_key(q) != HoverKey::default();
                    old != Some(p) && (watched(p) || old.is_some_and(watched))
                };
                return crate::scm::moved(self, p) || redraw;
            }
            _ => return false,
        }
        true
    }

    /// Everything a frame draws differently under the pointer: the sidebar row (and a
    /// sticky one), a GRAPH or TIMELINE row, a tab, a scrollbar, a minimap, a peek
    /// button, a commit-dialog splitter.
    fn hover_key(&self, p: Position) -> HoverKey {
        let s = self.side as usize;
        let side = self.ui.side_list.contains(p);
        let g = &self.gitui.hits;
        HoverKey {
            row: side.then(|| self.ui.side_item(p.y, self.off[s])).flatten(),
            sticky: side.then(|| self.ui.sticky_at(p.y)).flatten(),
            git_row: (g.graph.contains(p) || g.timeline.contains(p)).then_some(p.y),
            tab: self.ui.groups.iter().enumerate().find_map(|(gi, gh)| gh.tabs.iter().position(|(t, x)| t.contains(p) || x.contains(p)).map(|i| (gi, i))),
            scrollbar: self.ui.scrollbars.iter().position(|sb| sb.track.contains(p)),
            minimap: self.ui.minimaps.iter().position(|m| m.area.contains(p)),
            peek: self.ui.peek_btns.iter().position(|(r, _)| r.contains(p)).or_else(|| self.ui.peek_zone.contains(p).then_some(usize::MAX)),
            sash: self.gitui.dlg.as_ref().and_then(|d| d.hits.sashes.iter().position(|r| r.contains(p))),
            activity: self.ui.activity.iter().position(|(r, _)| r.contains(p)),
        }
    }

    fn click(&mut self, p: Position, double: bool, shift: bool) {
        // Every press starts afresh: a release the terminal never reported (outside
        // the pane, a lost event) must not leave a sash, tab or file drag behind.
        self.drag = Drag::None;
        self.drop = None;
        crate::scm::drop_drags(self);
        if let Some(pr) = self.prompt.as_ref() {
            if let Some(&(_, i)) = self.ui.prompt_btns.iter().find(|(r, _)| r.contains(p)) {
                let act = pr.buttons[i].1.clone();
                self.run_prompt(act);
            }
            return;
        }
        if let Some(q) = self.quick.as_ref() {
            if let Some(sb) = self.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Quick && s.track.contains(p)).copied() {
                return self.scrollbar_click(sb, p);
            }
            if self.ui.quick_list.contains(p) {
                let i = q.off + (p.y - self.ui.quick_list.y) as usize;
                self.quick_pick(i);
            } else if !self.ui.quick.contains(p) {
                self.quick = None;
            }
            return;
        }
        if crate::scm::click(self, p, double) {
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
        if let Some(sb) = self.ui.scrollbars.iter().find(|s| s.track.contains(p)).copied() {
            return self.scrollbar_click(sb, p);
        }
        if let Some(&(_, b)) = self.ui.peek_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.peek_button(b);
        }
        if self.ui.peek_zone.contains(p) {
            self.focus = Focus::Editor;
            return;
        }
        if let Some(m) = self.ui.minimaps.iter().find(|m| m.area.contains(p)).copied() {
            return self.minimap_click(m, p);
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
            self.focus = Focus::SearchInput;
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
        if let Some(i) = self.ui.sticky_at(p.y).filter(|_| self.ui.side_list.contains(p)) {
            return self.reveal_pinned(i);
        }
        if self.ui.side_list.contains(p) {
            let s = self.side as usize;
            // A blank row between items selects nothing.
            let Some(i) = self.ui.side_item(p.y, self.off[s]) else { return };
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
                self.drag = Drag::Tab { g, i, from: p, active: false };
                return self.find_refresh();
            }
            if gh.area.contains(p) {
                self.ag = g;
                self.focus = Focus::Editor;
                // A quick-diff marker opens the peek on its change (VS Code).
                if gh.marks_x == Some(p.x) {
                    if let Some(line) = self.line_at(g, p.y) {
                        let marked = self.groups[g].tabs.get(self.groups[g].active).and_then(|t| self.qdiff.get(&t.key)).and_then(|q| q.marks.get(line).copied().flatten());
                        if marked.is_some() {
                            return self.peek_at_line(g, line);
                        }
                    }
                }
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

    /// A click on the minimap: on the slider it starts a drag, elsewhere it
    /// scrolls so the clicked line is centred (VS Code).
    fn minimap_click(&mut self, m: MiniHit, p: Position) {
        let row = p.y - m.area.y;
        let (top, len) = m.mini.slider(m.scroll);
        self.ag = m.group;
        self.focus = Focus::Editor;
        if (top..top + len).contains(&row) {
            self.drag = Drag::Minimap(m, row - top);
        } else {
            self.set_scroll(ScrollTarget::Editor(m.group), m.mini.click_scroll(m.scroll, row));
            // Keep dragging from here, as VS Code does after a jump.
            let (top, _) = m.mini.slider(self.scroll_of(ScrollTarget::Editor(m.group)));
            self.drag = Drag::Minimap(m, row.saturating_sub(top));
        }
    }

    fn scrollbar_click(&mut self, sb: ScrollHit, p: Position) {
        let cur = self.scroll_of(sb.target);
        match decor::track_hit(p.y - sb.track.y, sb.slider) {
            TrackHit::PageUp => self.set_scroll(sb.target, cur.saturating_sub(sb.page)),
            TrackHit::PageDown => self.set_scroll(sb.target, (cur + sb.page).min(sb.max_off)),
            TrackHit::Slider(grab) => self.drag = Drag::Scroll(sb, grab),
        }
    }

    fn scroll_of(&self, t: ScrollTarget) -> usize {
        match t {
            ScrollTarget::Editor(g) => self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map_or(0, |t| t.view.scroll),
            ScrollTarget::Side => self.off[self.side as usize],
            ScrollTarget::Quick => self.quick.as_ref().map_or(0, |q| q.off),
            ScrollTarget::Git(w) => crate::scm::scroll_of(self, w),
        }
    }

    /// Scroll a list or editor to `off` (clamped where it is drawn).
    fn set_scroll(&mut self, t: ScrollTarget, off: usize) {
        match t {
            ScrollTarget::Editor(g) => {
                let max = {
                    let grp = self.groups.get(g);
                    match grp.and_then(|grp| grp.tabs.get(grp.active)).and_then(|t| self.docs.get(&t.key)) {
                        Some(Doc::Text(b)) => b.lines.len(),
                        Some(Doc::Diff(d)) => d.rows.len(),
                        None => 0,
                    }
                };
                if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
                    t.view.scroll = off.min(max.saturating_sub(1));
                }
            }
            ScrollTarget::Side => {
                let n = self.side_count();
                let h = self.ui.side_rows;
                self.off[self.side as usize] = off.min(n.saturating_sub(h));
            }
            ScrollTarget::Git(w) => crate::scm::set_scroll(self, w, off),
            ScrollTarget::Quick => {
                let h = self.ui.quick_list.height.max(1) as usize;
                if let Some(q) = self.quick.as_mut() {
                    q.off = off.min(q.items.len().saturating_sub(h));
                    q.sel = q.sel.clamp(q.off, q.off + h - 1).min(q.items.len().saturating_sub(1));
                }
            }
        }
    }

    /// Buffer position under screen cell `p` in group `g`'s text area.
    fn pos_at(&self, g: usize, p: Position) -> Option<Pos> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let tab = grp.tabs.get(grp.active)?;
        let Some(Doc::Text(b)) = self.docs.get(&tab.key) else { return None };
        let line = self.line_at(g, p.y)?;
        let x = tab.view.hscroll + p.x.saturating_sub(gh.text.x) as usize;
        Some(Pos::new(line, editor::byte_at_display(&b.lines[line], x)))
    }

    /// The buffer line drawn on screen row `y` of group `g`: rows below the peek
    /// zone are shifted down by its height; a row inside it is no line.
    pub(crate) fn line_at(&self, g: usize, y: u16) -> Option<usize> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let tab = grp.tabs.get(grp.active)?;
        let Some(Doc::Text(b)) = self.docs.get(&tab.key) else { return None };
        let mut row = y.saturating_sub(gh.text.y);
        if let Some((zy, zh)) = gh.zone {
            if y >= zy + zh {
                row -= zh;
            } else if y >= zy {
                return None;
            }
        }
        Some((tab.view.scroll + row as usize).min(b.lines.len() - 1))
    }

    /// The screen row buffer line `line` of group `g` is drawn on, if any: `line_at` backwards.
    pub(crate) fn line_y(&self, g: usize, line: usize) -> Option<u16> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let row = u16::try_from(line.checked_sub(grp.tabs.get(grp.active)?.view.scroll)?).ok()?;
        let mut y = gh.text.y.checked_add(row)?;
        if let Some((_, zh)) = gh.zone.filter(|&(zy, _)| y >= zy) {
            y = y.checked_add(zh)?;
        }
        (y < gh.text.bottom()).then_some(y)
    }

    /// Where a dragged file would land: the group under the cursor and which part.
    /// `bar`: a tab bar takes the drop at a slot between its tabs (a dragged tab).
    fn drop_target(&self, p: Position, bar: bool) -> Option<(usize, Zone)> {
        let can_split = self.groups.len() < MAX_GROUPS;
        self.ui.groups.iter().enumerate().find(|(_, g)| g.area.contains(p)).map(|(i, g)| {
            let rx = (p.x - g.area.x) as u32 * 10 / g.area.width.max(1) as u32;
            let zone = if bar && g.tabbar.contains(p) {
                // Before the first drawn tab whose middle is right of the pointer.
                Zone::Tab(g.tabs.iter().take_while(|(r, x)| p.x >= (r.x + x.right()) / 2).count())
            } else if g.tabbar.contains(p) || !can_split {
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
        if crate::scm::drag(self, p) {
            return;
        }
        match &mut self.drag {
            Drag::None => {}
            Drag::Divider => {
                let start = self.ui.sidebar.x;
                let max = self.ui.editor_area.right().saturating_sub(start + 20).max(12);
                self.side_w = Some(p.x.saturating_sub(start).clamp(12, max));
                self.side_w_legacy = false;
            }
            Drag::File { from, active, .. } => {
                if !*active && (from.x.abs_diff(p.x) + from.y.abs_diff(p.y)) >= 2 {
                    *active = true;
                }
                if *active {
                    self.drop = self.drop_target(p, false);
                }
            }
            Drag::Tab { g, i, from, active } => {
                if !*active && (from.x.abs_diff(p.x) + from.y.abs_diff(p.y)) >= 2 {
                    *active = true;
                }
                if *active {
                    let (g, i) = (*g, *i);
                    self.drop = self.drop_target(p, true).filter(|&(t, z)| !self.tab_drop_is_noop(g, i, t, z));
                }
            }
            Drag::Minimap(m, grab) => {
                let (m, grab) = (*m, *grab);
                let off = m.mini.drag_scroll(p.y as i32 - m.area.y as i32 - grab as i32);
                self.set_scroll(ScrollTarget::Editor(m.group), off);
            }
            Drag::Scroll(sb, grab) => {
                let (sb, grab) = (*sb, *grab);
                let off = decor::drag_offset(p.y as i32 - sb.track.y as i32, grab, sb.track.height, sb.slider.1, sb.max_off);
                self.set_scroll(sb.target, off);
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
        if crate::scm::release(self) {
            return;
        }
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Divider => self.save_layout(),
            Drag::File { rel, active: true, .. } => {
                if let Some((g, zone)) = self.drop.take() {
                    let target = match zone {
                        Zone::Center | Zone::Tab(_) => g,
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
            Drag::Tab { g, i, active: true, .. } => {
                if let Some((t, zone)) = self.drop.take() {
                    self.move_tab(g, i, t, zone);
                }
            }
            _ => {}
        }
        self.drop = None;
    }

    /// Dropping tab `i` of group `g` on (`t`, `zone`) changes nothing: its own
    /// group's content, or a split off a group it is the only tab of.
    fn tab_drop_is_noop(&self, g: usize, i: usize, t: usize, zone: Zone) -> bool {
        let alone = self.groups.get(g).is_some_and(|grp| grp.tabs.len() == 1);
        t == g && match zone {
            Zone::Center => true,
            Zone::Tab(k) => k == i || k == i + 1,
            Zone::Left | Zone::Right => alone,
        }
    }

    /// VS Code's tab drop: tab `i` of group `g` moves (pinned, with its view) into
    /// group `t` at the tab-bar slot, after its active tab, or into a new group on
    /// the `zone` side. The document is not touched: a dirty buffer keeps its
    /// version, undo and backup. A group left empty closes.
    fn move_tab(&mut self, g: usize, i: usize, t: usize, zone: Zone) {
        if g >= self.groups.len() || t >= self.groups.len() || i >= self.groups[g].tabs.len() || self.tab_drop_is_noop(g, i, t, zone) {
            return;
        }
        let mut tab = self.groups[g].tabs.remove(i);
        tab.preview = false;
        let src = &mut self.groups[g];
        if i < src.active || src.active >= src.tabs.len() {
            src.active = src.active.saturating_sub(1);
        }
        // (target group, slot in it, the source group after an insert before it)
        let (mut dst, at, src) = match zone {
            Zone::Left | Zone::Right => {
                let at = if zone == Zone::Left { t } else { t + 1 };
                self.groups.insert(at, Group::default());
                (at, 0, if at <= g { g + 1 } else { g })
            }
            // A slot right of the tab's old place moved left by one when it left.
            Zone::Tab(k) => (t, if t == g && k > i { k - 1 } else { k }, g),
            Zone::Center => (t, self.groups[t].active + 1, g),
        };
        let grp = &mut self.groups[dst];
        // Already open there (a split shows a file twice): that tab takes the focus.
        grp.active = match grp.tabs.iter().position(|x| x.key == tab.key) {
            Some(e) => e,
            None => {
                let at = at.min(grp.tabs.len());
                grp.tabs.insert(at, tab);
                at
            }
        };
        if self.groups[src].tabs.is_empty() && self.groups.len() > 1 {
            self.groups.remove(src);
            if dst > src {
                dst -= 1;
            }
        }
        self.peek = None; // its group and lines are another editor's now
        self.ag = dst;
        self.focus = Focus::Editor;
        self.find_refresh();
    }

    /// Sideways scroll of the diff tab under `p` (both sides; clamped where drawn).
    /// False when no diff is there, so the wheel scrolls as usual.
    fn hwheel(&mut self, p: Position, d: isize) -> bool {
        let Some(g) = self.ui.groups.iter().position(|g| g.area.contains(p)) else { return false };
        let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) else { return false };
        if !matches!(self.docs.get(&t.key), Some(Doc::Diff(_))) {
            return false;
        }
        t.view.hscroll = (t.view.hscroll as isize + d).max(0) as usize;
        true
    }

    /// Scroll what is under `p`; false when it could not move (no redraw needed).
    fn wheel(&mut self, p: Position, d: isize) -> bool {
        if let Some(q) = self.quick.as_mut() {
            let old = q.sel;
            q.sel = (q.sel as isize + d).clamp(0, (q.items.len() as isize - 1).max(0)) as usize;
            return q.sel != old;
        }
        if crate::scm::wheel(self, p, d) {
            return true;
        }
        if self.ui.peek_zone.contains(p) {
            let max = self.ui.peek_max;
            let Some(pk) = self.peek.as_mut() else { return false };
            let old = pk.scroll;
            pk.scroll = (pk.scroll.min(max) as isize + d).clamp(0, max as isize) as usize;
            return pk.scroll != old;
        }
        // Over an editor's scrollbar the wheel scrolls that editor.
        if let Some(sb) = self.ui.scrollbars.iter().find(|s| s.track.contains(p)).copied() {
            if let ScrollTarget::Editor(_) = sb.target {
                let cur = self.scroll_of(sb.target);
                self.set_scroll(sb.target, (cur as isize + d).max(0) as usize);
                return self.scroll_of(sb.target) != cur;
            }
        }
        if self.ui.side_list.contains(p) {
            let s = self.side as usize;
            let n = self.side_count();
            let h = self.ui.side_rows;
            let old = self.off[s];
            self.off[s] = (self.off[s] as isize + d).clamp(0, n.saturating_sub(h) as isize) as usize;
            return self.off[s] != old;
        }
        let Some(g) = self.ui.groups.iter().position(|g| g.area.contains(p)) else { return false };
        let max = {
            let grp = &self.groups[g];
            match grp.tabs.get(grp.active).and_then(|t| self.docs.get(&t.key)) {
                Some(Doc::Text(b)) => b.lines.len(),
                Some(Doc::Diff(dd)) => dd.rows.len(),
                None => 0,
            }
        };
        let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) else { return false };
        let old = t.view.scroll;
        t.view.scroll = (t.view.scroll as isize + d).clamp(0, max.saturating_sub(1) as isize) as usize;
        t.view.scroll != old
    }

    /// A drawn editor's (or the definition peek's) first highlighting is still catching
    /// up. Only what was drawn last frame counts: a hidden tab cannot keep the loop awake.
    pub(crate) fn hl_pending(&self) -> bool {
        let shown = |g: usize| self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).and_then(|t| self.docs.get(&t.key));
        // A terminal too small to draw (under 30x5) records no groups: then nothing counts,
        // a peek kept from a bigger frame included.
        (0..self.ui.groups.len()).any(|g| self.ui.groups[g].text.height > 0 && matches!(shown(g), Some(Doc::Text(b)) if b.hl_pending()))
            || (!self.ui.groups.is_empty() && crate::navui::hl_pending(self))
            // A drawn diff minimap highlights its own buffer (ui::diff_mini).
            || self.ui.minimaps.iter().any(|m| {
                let key = self.groups.get(m.group).and_then(|grp| grp.tabs.get(grp.active)).map(|t| &t.key);
                key.and_then(|k| self.diff_minis.get(k)).is_some_and(|e| e.1.hl_pending())
            })
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
            if app.search.is_none() && app.queued_search.is_none() && app.loading.is_empty() && !app.gitui.busy() {
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
        let gap = " ".repeat(app.settings.spacing.icon_gap as usize);
        assert!(s.contains(&format!("main.rs{gap}{}", crate::theme::icons().dirty)) && s.contains("Ln 2, Col 1"), "{s}");
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
        wait(&mut app);
        app.show_side(Side::Scm, false);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Changes") && s.contains("main.rs") && s.contains("Add commit"), "{s}");
        let i = app.scm_files.iter().position(|f| f.path == "src/main.rs").unwrap();
        app.open_diff(i, true);
        wait(&mut app);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("main.rs (Working Tree)") && s.contains("// hi"), "{s}");
        app.scm_action(ScmAct::Stage(i));
        wait(&mut app);
        assert!(app.scm_files.iter().any(|f| f.section == Section::Staged && f.path == "src/main.rs"));
        app.commit(); // "Add commit": the commit dialog, caret in its message
        typ(&mut app, "edit main");
        key(&mut app, KeyCode::Enter, KeyModifiers::ALT);
        wait(&mut app);
        assert!(app.gitui.dlg.is_none() && app.msg.starts_with("Committed"), "{}", app.msg);
        assert!(app.scm_files.is_empty());

        // Discard always asks; Cancel (the default) keeps the file, Discard deletes it.
        fs::write(root.join("junk.txt"), "x").unwrap();
        app.refresh();
        wait(&mut app);
        let j = app.scm_files.iter().position(|f| f.path == "junk.txt").unwrap();
        app.scm_action(ScmAct::Discard(j));
        assert!(app.prompt.as_ref().unwrap().text.join(" ").contains("IRREVERSIBLE"));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(root.join("junk.txt").exists());
        app.scm_action(ScmAct::Discard(j));
        app.prompt.as_mut().unwrap().sel = 0;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait(&mut app);
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
        let bak = editor::backup_file(&state.join("backup"), &app.backup_scope, &app.backup_instance, &root.join("README.md"));
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
        let bak = editor::backup_file(&state.join("backup"), &app.backup_scope, &app.backup_instance, &root.join("README.md"));
        let rec: serde_json::Value = serde_json::from_slice(&fs::read(&bak).unwrap()).unwrap();
        assert_eq!(rec["text"], "xhello");
        key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(app.prompt.is_some(), "closing still asks");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lines_join_where_a_section_line_meets_the_vertical_ones() {
        use crate::blocks::BlockStyle;
        let root = repo("join");
        let mut app = App::new(root.clone(), None);
        app.settings.blocks = BlockStyle::Lines;
        app.open_file(0, "src/main.rs", false); // the Explorer shows TIMELINE under the tree
        let mut t = Terminal::new(TestBackend::new(110, 22)).unwrap();
        screen(&mut app, &mut t);
        let b = t.backend().buffer();
        let (l, r) = (app.ui.sidebar.x - 1, app.ui.divider.x);
        let y = (2..21).find(|&y| b[(l + 1, y)].symbol() == "─").expect("a line between the Explorer's sections");
        assert_eq!((b[(l, y)].symbol(), b[(r, y)].symbol()), ("├", "┤"));
        assert_eq!((b[(l, y - 1)].symbol(), b[(r, y + 1)].symbol()), ("│", "│"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_focused_card_is_a_little_lighter_never_blue() {
        let root = repo("focuscard");
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "src/main.rs", false);
        app.focus = Focus::Editor;
        let mut t = Terminal::new(TestBackend::new(110, 22)).unwrap();
        screen(&mut app, &mut t);
        let b = t.backend().buffer();
        let g = app.ui.groups[0].area;
        let (ed, side) = (&b[(g.x - 1, g.y - 1)], &b[(app.ui.sidebar.x, app.ui.side_list.y - 2)]);
        assert_eq!((ed.symbol(), ed.fg), ("╭", crate::blocks::border(true)), "the focused editor's card");
        assert_eq!((side.symbol(), side.fg), ("╭", crate::blocks::border(false)), "the Explorer card, not focused");
        // vs-dark: #444444, focused a little toward the foreground (~#5A5A5A), not focusBorder.
        assert_eq!(crate::blocks::border(false), Color::Rgb(0x44, 0x44, 0x44));
        let Color::Rgb(r, g, b) = crate::blocks::border(true) else { panic!() };
        assert!([r, g, b].iter().all(|c| c.abs_diff(0x5A) <= 3), "{r:02X}{g:02X}{b:02X}");
        assert_ne!(crate::blocks::border(true), crate::theme::P.focus_border);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn activity_items_are_tall_with_the_active_one_marked_and_hover_lit() {
        let root = repo("activity");
        for compact in [false, true] {
            let mut app = App::new(root.clone(), None);
            app.settings.compact = compact;
            let mut t = Terminal::new(TestBackend::new(110, 22)).unwrap();
            screen(&mut app, &mut t);
            let ih = if compact { 2 } else { 3 };
            let (items, b) = (app.ui.activity.clone(), t.backend().buffer().clone());
            assert!(items.iter().all(|(r, _)| r.height == ih), "{compact}: one {ih}-row cell per item");
            assert!(items.windows(2).all(|w| w[1].0.y == w[0].0.bottom()), "stacked without gaps");
            // The icon in its item's middle row, a blank kept after it (its 2-cell slot).
            let (scm, _) = items[2];
            let iy = scm.y + (ih - 1) / 2;
            let ix = (scm.x..scm.right()).find(|&x| b[(x, iy)].symbol() == crate::theme::icons().scm).expect("the SCM icon");
            assert_eq!(b[(ix + 1, iy)].symbol(), " ");
            // The active item (Explorer): the indicator down its rows over the active background.
            let (ex, side) = items[0];
            assert_eq!(side, Side::Explorer);
            for y in ex.y..ex.bottom() {
                assert_eq!((b[(ex.x, y)].symbol(), b[(ex.x, y)].fg), ("▎", crate::theme::P.activity_border), "row {y}");
                assert_eq!(b[(ex.x + 1, y)].bg, crate::blocks::active_bg(&serde_json::Map::new()));
            }
            assert_ne!(b[(scm.x + 1, scm.y)].bg, b[(ex.x + 1, ex.y)].bg, "only the active item is lifted");
            // Hover: inactive foreground -> foreground, and the motion asks for a redraw.
            assert_eq!(b[(ix, iy)].fg, crate::theme::P.activity_inactive);
            assert!(app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: ix, row: iy, modifiers: KeyModifiers::NONE }));
            screen(&mut app, &mut t);
            assert_eq!(t.backend().buffer()[(ix, iy)].fg, crate::theme::P.activity_fg, "hovered");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn widths_are_the_content_and_old_dragged_widths_keep_theirs() {
        use crate::blocks::BlockStyle;
        let root = repo("widths");
        // Defaults: cards add their 2-cell frame; none keeps the old 36 and 5.
        for (st, side, act) in [(BlockStyle::Cards, 38, 7), (BlockStyle::Lines, 36, 5), (BlockStyle::None, 36, 5)] {
            let mut app = App::new(root.clone(), None);
            app.settings.blocks = st;
            let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
            screen(&mut app, &mut t);
            assert_eq!((app.ui.divider.x - app.ui.sidebar.x, app.ui.activity[0].0.width + crate::blocks::frame(st)), (side, act), "{st:?}");
        }
        // A width dragged before layout_version 2 was the content's: a card adds its frame,
        // and saving writes the outer width with the version.
        let lay = root.join("layout.json");
        fs::write(&lay, r#"{"sidebar_visible": true, "sidebar_width": 30, "sidebar_dragged": true}"#).unwrap();
        let mut app = App::new(root.clone(), Some(lay.clone()));
        let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
        screen(&mut app, &mut t);
        assert_eq!(app.ui.divider.x - app.ui.sidebar.x, 32, "30 columns of content, in a card");
        app.save_layout();
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&lay).unwrap()).unwrap();
        assert_eq!((v["sidebar_width"].as_u64(), v["layout_version"].as_u64()), (Some(32), Some(2)));
        let mut again = App::new(root.clone(), Some(lay));
        screen(&mut again, &mut t);
        assert_eq!(again.ui.divider.x - again.ui.sidebar.x, 32, "a version 2 width is the outer one");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sidebar_divider_stays_in_one_column() {
        use crate::blocks::BlockStyle;
        let root = repo("div");
        for st in [BlockStyle::None, BlockStyle::Lines, BlockStyle::Cards] {
            let mut app = App::new(root.clone(), None);
            app.settings.blocks = st;
            app.open_file(0, "src/main.rs", false);
            let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
            screen(&mut app, &mut t);
            let x = app.ui.divider.x;
            let b = t.backend().buffer();
            for y in 1..19 {
                if st == BlockStyle::Cards {
                    // The divider is the gutter: blank on every row; beside it only the section
                    // cards' right edges (the title row and the gutter between cards are blank).
                    assert_eq!(b[(x, y)].symbol(), " ", "{st:?} row {y}: the gutter");
                    let edge = b[(x - 1, y)].symbol();
                    assert!(["│", "╮", "╯", " "].contains(&edge), "{st:?} row {y}: {edge:?} in the card edge");
                } else {
                    // One straight line, joined where a section line meets it.
                    let s = b[(x, y)].symbol();
                    assert!(["│", "┤", "┼", "├"].contains(&s), "{st:?} row {y}: {s:?}");
                }
            }
            if st == BlockStyle::Cards {
                assert!((1..19).filter(|&y| b[(x - 1, y)].symbol() == "│").count() > 10, "the sidebar card's edge");
            }
        }
        fs::remove_dir_all(&root).unwrap();
    }

    const SAMPLE_C: &str = "#include <stdio.h>\n#include \"mxl.h\"\n\n#define MAX_PORTS 8\n\nstruct mxl_priv {\n\tint port;\n};\n\n/* Set the port */\nstatic int mxl_set(struct mxl_priv *priv, int idx)\n{\n\tint ret = 0;\n\n\tif (idx >= MAX_PORTS)\n\t\treturn -EINVAL;\n\tpriv->port = idx;\n\tpr_info(\"port %d\\n\", idx);\n\treturn ret;\n}\n";

    /// Screen cell of the `k`-th char of the first `needle` on screen.
    fn cell_of(t: &Terminal<TestBackend>, needle: &str, k: usize) -> (u16, u16) {
        let b = t.backend().buffer();
        for y in 0..b.area.height {
            let syms: Vec<&str> = (0..b.area.width).map(|x| b[(x, y)].symbol()).collect();
            let line: String = syms.concat();
            if let Some(i) = line.find(needle) {
                let x = line[..i].chars().count() + k;
                return (x as u16, y);
            }
        }
        panic!("{needle:?} not on screen");
    }

    fn fg(t: &Terminal<TestBackend>, needle: &str, k: usize) -> Color {
        let (x, y) = cell_of(t, needle, k);
        t.backend().buffer()[(x, y)].fg
    }

    fn rgb(h: u32) -> Color {
        Color::Rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
    }

    /// Every cell has an explicit truecolor fg and bg: nothing falls back to the
    /// terminal's palette (Indexed, named ANSI colours such as White, or Reset).
    fn assert_all_rgb(t: &Terminal<TestBackend>, what: &str) {
        let b = t.backend().buffer();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                let c = &b[(x, y)];
                assert!(
                    matches!(c.fg, Color::Rgb(..)) && matches!(c.bg, Color::Rgb(..)),
                    "{what}: cell ({x},{y}) {:?} fg {:?} bg {:?}",
                    c.symbol(),
                    c.fg,
                    c.bg
                );
            }
        }
    }

    #[test]
    fn c_colours_match_vscode_cpptools_and_no_cell_uses_terminal_colours() {
        let root = repo("colors");
        fs::write(root.join("mxl.c"), SAMPLE_C).unwrap();
        fs::write(root.join("README.md"), "changed\n").unwrap(); // a change: SCM badge and counts
        let mut app = App::new(root.clone(), None);
        app.refresh();
        wait(&mut app);
        app.open_file(0, "mxl.c", false);
        let mut t = Terminal::new(TestBackend::new(150, 40)).unwrap();
        screen(&mut app, &mut t);
        let golden = [
            ("#include <stdio.h>", 0, 0x9A9A9A, "#include: keyword.control.directive"),
            ("<stdio.h>", 0, 0xE8C9BB, "< : punctuation.definition.string"),
            ("<stdio.h>", 1, 0xCE9178, "stdio.h: string"),
            ("#define MAX_PORTS", 0, 0x9A9A9A, "#define"),
            ("#define MAX_PORTS", 8, 0xBEB7FF, "MAX_PORTS in #define: macro"),
            ("MAX_PORTS 8", 10, 0xB5CEA8, "8: number"),
            ("struct mxl_priv {", 0, 0x569CD6, "struct: storage.type"),
            ("struct mxl_priv {", 7, 0x4EC9B0, "mxl_priv: entity.name.type"),
            ("int port;", 4, 0xDADADA, "struct member: property"),
            ("/* Set the port */", 0, 0x57A64A, "comment"),
            ("static int mxl_set", 0, 0x569CD6, "static: storage.modifier"),
            ("static int mxl_set", 11, 0xDCDCAA, "mxl_set: function definition"),
            ("struct mxl_priv *priv", 7, 0x4EC9B0, "type in a parameter list"),
            ("*priv, int idx", 1, 0x9A9A9A, "priv in the parameter list: variable.parameter"),
            ("int ret = 0;", 4, 0x9CDCFE, "local ret: variable"),
            ("int ret = 0;", 10, 0xB5CEA8, "0: number"),
            ("int ret = 0;", 11, 0xB4B4B4, "; : punctuation"),
            ("if (idx >= MAX_PORTS)", 0, 0xD8A0DF, "if: control keyword"),
            ("if (idx >= MAX_PORTS)", 4, 0x9A9A9A, "idx in the body: parameter"),
            ("if (idx >= MAX_PORTS)", 11, 0xBEB7FF, "MAX_PORTS use: macro"),
            ("return -EINVAL;", 8, 0xBEB7FF, "EINVAL: macro"),
            ("priv->port = idx;", 0, 0x9A9A9A, "priv: parameter"),
            ("priv->port = idx;", 6, 0xDADADA, "port: variable.other.property"),
            ("pr_info(\"port", 0, 0xDCDCAA, "pr_info( : function"),
            ("pr_info(\"port", 8, 0xE8C9BB, "quote punctuation"),
            ("pr_info(\"port", 9, 0xCE9178, "string"),
            ("return ret;", 7, 0x9CDCFE, "ret: variable"),
        ];
        let bad: Vec<String> = golden
            .iter()
            .filter(|(n, k, want, _)| fg(&t, n, *k) != rgb(*want))
            .map(|(n, k, want, why)| format!("{why}: {n:?}[{k}] is {:?}, want #{want:06X}", fg(&t, n, *k)))
            .collect();
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        // Comfortable gutter: [1][number][gap][marker][gap] code, gap = editorPadding.
        let (x, y) = cell_of(&t, "#define MAX_PORTS", 0);
        let nx = x - 2 * app.settings.spacing.editor_padding - 2;
        assert_eq!(t.backend().buffer()[(nx, y)].symbol(), "4");
        assert_eq!(t.backend().buffer()[(nx, y)].fg, rgb(0x2B91AF));
        assert_all_rgb(&t, "explorer + C editor");
        // Every other view and popup, too.
        app.show_side(Side::Scm, false);
        screen(&mut app, &mut t);
        assert_eq!(fg(&t, "Changes", 0), rgb(0xCCCCCC));
        assert_all_rgb(&t, "source control");
        app.query = "port".into();
        app.start_search();
        wait(&mut app);
        app.show_side(Side::Search, false);
        screen(&mut app, &mut t);
        assert_all_rgb(&t, "search");
        app.open_find(true);
        screen(&mut app, &mut t);
        assert_all_rgb(&t, "find widget");
        app.find = None;
        app.open_quick();
        screen(&mut app, &mut t);
        assert_all_rgb(&t, "quick open");
        app.quick = None;
        key(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        screen(&mut app, &mut t);
        assert_all_rgb(&t, "command palette");
        app.quick = None;
        app.request_quit();
        app.focus = Focus::Editor;
        typ(&mut app, "x");
        app.request_quit();
        screen(&mut app, &mut t);
        assert!(app.prompt.is_some());
        assert_all_rgb(&t, "prompt");
        // The compact layout draws its popups without a frame: same rule.
        app.settings.compact = true;
        screen(&mut app, &mut t);
        assert_all_rgb(&t, "compact prompt");
        app.prompt = None;
        app.open_find(false);
        app.open_quick();
        screen(&mut app, &mut t);
        assert_all_rgb(&t, "compact quick open over the find widget");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settings_hot_reload_keeps_the_last_good_and_f1_opens_them() {
        let root = repo("settings");
        let cfg = root.join(".cfg/settings.json");
        let mut app = App::new(root.clone(), None);
        app.settings_file = Some(cfg.clone());
        app.check_settings();
        assert!(!app.settings.compact && app.settings_err.is_none(), "no file: defaults");
        key(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        typ(&mut app, "open settings");
        assert_eq!(app.quick.as_ref().unwrap().items[0], "Preferences: Open Settings (JSON)");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.quick.is_none() && cfg.exists(), "created from the template");
        assert!(app.ed().unwrap().0.text().starts_with("// herdr-code settings"));
        assert!(app.settings_err.is_none(), "the all-comments template is valid");
        let set = |app: &mut App, text: &str| {
            let (b, tab) = app.ed().unwrap();
            b.select_all(&mut tab.view);
            b.insert(&mut tab.view, text);
            key(app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        };
        set(&mut app, "{ \"workbench.density\": \"compact\", // ok\n}\n");
        assert!(app.settings.compact, "saved in our editor: applied at once");
        set(&mut app, "{ \"workbench.density\": \"comfortable\" oops }\n");
        assert!(app.settings_err.is_some(), "{}", app.msg);
        assert!(app.settings.compact, "a bad file keeps the last good settings");
        let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
        assert!(screen(&mut app, &mut t).contains("settings.json:"), "error in the status bar");
        set(&mut app, "{ \"workbench.density\": \"comfortable\" }\n");
        assert!(!app.settings.compact && app.settings_err.is_none());
        // Palette toggles.
        key(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        typ(&mut app, "rainbow");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.rainbow_on);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn gutter_follows_the_index_and_scrollbars_page_and_drag() {
        let root = repo("qdiff");
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        fs::write(root.join("long.txt"), &text).unwrap();
        let g = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).output().unwrap().status.success());
        g(&["add", "long.txt"]);
        g(&["commit", "-q", "-m", "long"]);
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "long.txt", false);
        wait(&mut app);
        assert!(app.qdiff_due.is_some(), "the new base is diffed after the debounce, not on arrival");
        app.focus = Focus::Editor;
        typ(&mut app, "x");
        app.update_qdiff();
        let marks = &app.qdiff["file:long.txt"].marks;
        assert_eq!(marks[0], Some(Mark::Modified));
        assert!(marks[1..].iter().all(Option::is_none));
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        screen(&mut app, &mut t);
        let (x, y) = cell_of(&t, "xline 0", 0);
        let mark = &t.backend().buffer()[(x - app.settings.spacing.editor_padding - 1, y)];
        assert_eq!((mark.symbol(), mark.fg), (crate::theme::icons().gutter_bar, crate::theme::P.gutter_modified));
        // Save and stage: the index now matches, so the marker goes away.
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.refresh();
        wait(&mut app);
        let i = app.scm_files.iter().position(|f| f.path == "long.txt").unwrap();
        app.scm_action(ScmAct::Stage(i));
        wait(&mut app);
        app.update_qdiff();
        assert!(app.qdiff["file:long.txt"].marks.iter().all(Option::is_none), "staged: no markers");
        // Another refresh fetches the same index: no new diff is scheduled.
        app.refresh();
        wait(&mut app);
        assert!(app.qdiff_due.is_none());
        // Editor scrollbar: a click under the slider pages down, dragging it scrolls.
        screen(&mut app, &mut t);
        let sb = *app.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Editor(0)).unwrap();
        let mm = app.ui.minimaps[0].area;
        assert_eq!((mm.x, sb.track.x), (app.ui.groups[0].text.right(), mm.right() + 1), "text, minimap, a blank column, the scrollbar");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x, sb.track.bottom() - 1);
        assert_eq!(app.groups[0].tabs[0].view.scroll, sb.page);
        screen(&mut app, &mut t);
        let sb = *app.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Editor(0)).unwrap();
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x, sb.track.y + sb.slider.0);
        assert!(matches!(app.drag, Drag::Scroll(..)));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sb.track.x, sb.track.bottom() + 5);
        assert_eq!(app.groups[0].tabs[0].view.scroll, 99, "dragged to the end");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sb.track.x, sb.track.bottom());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hit_testing_matches_the_drawing_for_every_spacing() {
        use crate::settings::Spacing;
        let custom = Spacing::parse(
            Spacing::COMFORTABLE,
            &serde_json::json!({ "listItemGap": 1, "sidebarPadding": 3, "sidebarWidth": 40, "sectionGap": 2,
                                  "editorPadding": 4, "tabPadding": 3, "activityBarWidth": 7, "statusBarGap": 5,
                                  "iconGap": 3, "treeIndent": 4, "titleBarPadding": 3, "buttonPadding": 3 }),
        );
        use crate::blocks::BlockStyle;
        let styles = [BlockStyle::Cards, BlockStyle::Lines, BlockStyle::None];
        let runs = [("hitf", false, Spacing::COMFORTABLE), ("hitc", true, Spacing::COMPACT), ("hitx", false, custom)];
        for (tag, compact, sp, st) in runs.into_iter().flat_map(|r| styles.map(move |st| (r.0, r.1, r.2, st))) {
            let tag = format!("{tag}{}", st as u8); // short: the repo's name is on screen
            let tag = tag.as_str();
            let root = repo(tag);
            let mut app = App::new(root.clone(), None);
            app.settings.compact = compact;
            app.settings.spacing = sp;
            app.settings.blocks = st;
            let gap = crate::blocks::icon_gap(sp.icon_gap);
            let fr = crate::blocks::frame(st);
            let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
            screen(&mut app, &mut t);
            assert_eq!(app.ui.divider.x - app.ui.sidebar.x, sp.sidebar_width + fr, "{tag}: default width (and frame) until dragged");
            // A gutter (cards) or a line (lines) between the activity bar and the sidebar.
            assert_eq!(app.ui.sidebar.x, sp.activity_width + fr + u16::from(st != BlockStyle::None), "{tag}");
            // The activity icon is drawn centred inside its hit rect: the middle row, its
            // 2-cell slot centred right of the indicator column.
            let (ar, _) = app.ui.activity[2];
            let (ix, iy) = (ar.x + 1 + ar.width.saturating_sub(3) / 2, ar.y + (ar.height - 1) / 2);
            assert_eq!(t.backend().buffer()[(ix, iy)].symbol(), crate::theme::icons().scm, "{tag}");
            // Explorer header: the root's name; the tree starts on the next row.
            let (_, hy) = cell_of(&t, &app.repo.to_uppercase(), 0);
            assert_eq!(app.ui.side_list.y, hy + 1, "{tag}: tree rows start below it");
            let (x, y) = cell_of(&t, "README.md", 0);
            // [icon][gap blanks][name]: the icon is one cell, then only blanks (wide icons:
            // at least the rest of their 2-cell slot and one more).
            let buf = t.backend().buffer();
            assert_ne!(buf[(x - gap - 1, y)].symbol(), " ", "{tag}: the file icon");
            for k in 1..=gap {
                assert_eq!(buf[(x - k, y)].symbol(), " ", "{tag}: gap after the icon");
            }
            if sp.list_gap > 0 {
                // The blank row above README.md (below src/) hits nothing.
                let (sel, n) = (app.sel[0], app.tree.len());
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y - 1);
                assert!(app.groups[0].tabs.is_empty() && app.sel[0] == sel && app.tree.len() == n, "{tag}");
            }
            // Click the README.md row where it is drawn.
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
            assert_eq!(app.groups[0].tabs[0].key, "file:README.md", "{tag}");
            screen(&mut app, &mut t);
            // A click on the drawn text puts the caret on that character.
            let (x, y) = cell_of(&t, "hello", 2);
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
            assert_eq!(app.groups[0].tabs[0].view.cursor, Pos::new(0, 2), "{tag}");
            screen(&mut app, &mut t);
            // Its tab: [tabPadding][icon][iconGap][title][iconGap][close], close where drawn.
            let (tr, xr) = app.ui.groups[0].tabs[0];
            let buf = t.backend().buffer();
            for k in 1..=sp.icon_gap {
                assert_eq!(buf[(tr.x + sp.tab_padding + k, tr.y)].symbol(), " ", "{tag}: gap after the tab icon");
                assert_eq!(buf[(xr.x - k, xr.y)].symbol(), " ", "{tag}: gap before the close button");
            }
            assert_eq!(buf[(xr.x, xr.y)].symbol(), crate::theme::icons().close, "{tag}");
            // The title bar's Close button sits titleBarPadding from the edge.
            assert_eq!(app.ui.close.right() + sp.title_padding, 120, "{tag}");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), xr.x, xr.y);
            assert!(app.groups[0].tabs.is_empty(), "{tag}");
            // treeIndent: src/main.rs (depth 1) is drawn treeIndent cells right of README.md.
            let (sx, sy) = cell_of(&t, "src", 0);
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sx, sy);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sx, sy);
            screen(&mut app, &mut t);
            let (rx, _) = cell_of(&t, "README.md", 0);
            let (mx, my) = cell_of(&t, "main.rs", 0);
            assert_eq!(mx - rx, sp.tree_indent, "{tag}");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), mx, my);
            assert_eq!(app.groups[0].tabs[0].key, "file:src/main.rs", "{tag}");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    /// Popups, dialogs and the find widget on terminals from 30x5 to 40x18, in both
    /// densities and with every spacing key at its maximum: nothing may draw
    /// outside the buffer (that panics). `cargo test tiny` runs it.
    #[test]
    fn a_far_jump_highlights_in_slices_and_settles_to_the_unbudgeted_frame() {
        let root = repo("farjump");
        let text: String = (0..3000)
            .map(|i| match i {
                2500 => "/* a comment opened far above".to_string(),
                2600 => "*/".to_string(),
                _ => format!("int v{i} = {i};"),
            } + "\n")
            .collect();
        fs::write(root.join("src/far.c"), text).unwrap();
        let jump = |app: &mut App| {
            app.open_file(0, "src/far.c", false);
            app.focus = Focus::Editor;
            if let Some((_, t)) = app.ed() {
                (t.view.cursor, t.view.scroll) = (Pos::new(2560, 0), 2540);
            }
        };
        // The reference: the same view drawn with no budget, as before.
        crate::editor::set_frame_budget(None);
        let mut e = App::new(root.clone(), None);
        jump(&mut e);
        let mut te = Terminal::new(TestBackend::new(120, 30)).unwrap();
        te.draw(|f| crate::ui::draw(&mut e, f)).unwrap();
        assert!(!e.hl_pending());
        // Budgeted, through the loop's own step and no input: it keeps drawing with short
        // waits while the highlighting catches up, then stops asking.
        crate::editor::set_frame_budget(Some(Duration::from_millis(1)));
        let mut a = App::new(root.clone(), None);
        jump(&mut a);
        let mut ta = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let (mut dirty, mut steps) = (true, 0);
        loop {
            let wait = a.step(&mut ta, &mut dirty, |_| Ok(None)).unwrap();
            steps += 1;
            if !a.hl_pending() {
                break;
            }
            assert!(dirty && wait <= Duration::from_millis(2), "step {steps}: pending, yet dirty {dirty} wait {wait:?}");
            assert!(steps < 20_000, "never settles");
        }
        crate::editor::set_frame_budget(Some(crate::editor::FRAME_BUDGET));
        assert!(steps > 2, "one frame did it all: not budgeted");
        assert!(!a.hl_pending() && !dirty, "settled: no more frames asked for");
        // The editor group (text, gutter, minimap) is what the budget draws; the rest of the
        // frame shows async git state (status bar, Timeline) that only `a` has drained.
        let g = a.ui.groups[0].area;
        assert_eq!(g, e.ui.groups[0].area);
        let (ba, be) = (ta.backend().buffer(), te.backend().buffer());
        for (x, y) in (g.top()..g.bottom()).flat_map(|y| (g.left()..g.right()).map(move |x| (x, y))) {
            assert!(ba[(x, y)] == be[(x, y)], "the settled editor differs from the unbudgeted one at {x},{y}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_diff_minimap_still_highlighting_keeps_the_loop_drawing() {
        let d = repo("dmini");
        let mut app = App::new(d.clone(), None);
        open_big_diff(&mut app);
        let mut t = Terminal::new(TestBackend::new(200, 40)).unwrap();
        crate::editor::set_frame_budget(Some(Duration::ZERO)); // one line per frame
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        assert!(!app.ui.minimaps.is_empty() && app.hl_pending(), "the minimap's buffer is catching up");
        let (mut dirty, mut steps) = (false, 0);
        loop {
            let wait = app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
            if !app.hl_pending() {
                break;
            }
            assert!(dirty && wait <= Duration::from_millis(2), "step {steps}: pending, yet dirty {dirty} wait {wait:?}");
            steps += 1;
            assert!(steps < 1000, "never settles");
        }
        crate::editor::set_frame_budget(Some(crate::editor::FRAME_BUDGET));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_wheel_that_cannot_scroll_asks_for_no_redraw() {
        let root = repo("wheel");
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "src/main.rs", false); // 3 lines
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let tx = app.ui.groups[0].text;
        let wheel = |app: &mut App, kind, x, y| app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
        assert!(!wheel(&mut app, MouseEventKind::ScrollUp, tx.x + 2, tx.y), "editor already at the top");
        assert!(wheel(&mut app, MouseEventKind::ScrollDown, tx.x + 2, tx.y));
        assert!(!wheel(&mut app, MouseEventKind::ScrollDown, tx.x + 2, tx.y), "editor at its last line");
        let sl = app.ui.side_list;
        assert!(!wheel(&mut app, MouseEventKind::ScrollDown, sl.x + 1, sl.y), "a short list cannot scroll");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_the_event_loops_panic_restores_the_terminal() {
        let main = std::thread::current().id();
        assert!(panic_restores(main));
        assert!(!std::thread::spawn(move || panic_restores(main)).join().unwrap(), "a job thread leaves the terminal and the gate alone");
        let d = std::env::temp_dir().join(format!("herdr-code-panic-{}", std::process::id()));
        log_panic(&d, "thread 'git' panicked at x.rs:1:1:\nboom");
        log_panic(&d, "thread 'nav' panicked at y.rs:2:2:\nagain");
        let log = fs::read_to_string(d.join("panic.log")).unwrap();
        assert!(log.contains("boom") && log.contains("again"), "{log}");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn output_after_restore_is_dropped() {
        static OFF: AtomicBool = AtomicBool::new(false);
        let mut out = io::BufWriter::new(Gate { w: Vec::new(), off: &OFF });
        out.write_all(b"frame").unwrap();
        out.flush().unwrap();
        out.write_all(b" half a frame").unwrap(); // a panic cut this frame short
        OFF.store(true, Ordering::SeqCst); // restore() ran
        out.flush().unwrap(); // the buffer being dropped
        assert_eq!(out.get_ref().w, b"frame");
    }

    #[test]
    fn tiny_terminals_never_panic() {
        use crate::settings::Spacing;
        let root = repo("tiny");
        fs::write(root.join("README.md"), "changed\n").unwrap();
        let max = Spacing::parse(
            Spacing::COMFORTABLE,
            &serde_json::json!({ "sidebarWidth": 200, "sidebarPadding": 4, "sectionGap": 3, "editorPadding": 8,
                "tabPadding": 4, "statusBarGap": 8, "activityBarWidth": 9, "listItemGap": 1, "popupPadding": [8, 4],
                "iconGap": 4, "treeIndent": 8, "titleBarPadding": 6, "buttonPadding": 4, "findWidgetPadding": 3,
                "statusBarPadding": 6 }),
        );
        use crate::blocks::BlockStyle;
        let looks = [BlockStyle::Cards, BlockStyle::Lines, BlockStyle::None];
        let runs = [(false, Spacing::COMFORTABLE), (true, Spacing::COMPACT), (false, max), (true, max)];
        for (compact, sp, st) in runs.into_iter().flat_map(|r| looks.map(move |l| (r.0, r.1, l))) {
            let mut app = App::new(root.clone(), None);
            app.settings.compact = compact;
            app.settings.spacing = sp;
            app.settings.blocks = st;
            app.settings.rulers = vec![(2, None), (90, None)];
            app.refresh();
            wait(&mut app);
            app.open_file(0, "src/main.rs", false);
            wait(&mut app);
            app.focus = Focus::Editor;
            typ(&mut app, "x"); // dirty: the quit dialog has something to ask
            app.update_qdiff();
            app.peek_step(1); // the peek zone open on that change, the minimap on
            for w in 30..=40 {
                for h in 5..=18 {
                    app.settings.minimap.render_characters = h % 2 == 0; // braille and half blocks
                    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                    let mut draw = |app: &mut App| {
                        t.draw(|f| crate::ui::draw(app, f)).unwrap();
                    };
                    for side in [Side::Explorer, Side::Scm, Side::Search] {
                        app.side = side;
                        draw(&mut app);
                    }
                    app.open_quick();
                    draw(&mut app);
                    app.quick = None;
                    app.open_palette();
                    draw(&mut app);
                    app.quick = None;
                    app.open_find(true);
                    draw(&mut app);
                    app.find = None;
                    app.request_quit();
                    draw(&mut app);
                    let i = app.scm_files.iter().position(|f| f.path == "README.md").unwrap();
                    app.prompt = None;
                    app.scm_action(ScmAct::Discard(i));
                    draw(&mut app);
                    app.prompt = Some(Prompt {
                        text: vec!["README.md has unsaved changes from a closed session.".into(), "Restore them?".into()],
                        buttons: vec![("Restore".into(), PAct::RestoreBackup("file:README.md".into(), PathBuf::new())), ("Discard Backup".into(), PAct::Cancel)],
                        sel: 0,
                    });
                    draw(&mut app);
                    app.prompt = None;
                    assert!(app.peek.is_some(), "{w}x{h}");
                    app.hover = Some(Position::new(w - 3, 3)); // over the minimap: slider shown
                    draw(&mut app);
                    app.hover = None;
                    let def = crate::nav::Location { path: "src/main.rs".into(), line: 2, col: 5, preview: String::new(), kind: crate::nav::Kind::Function, score: 0 };
                    crate::navui::open_peek(&mut app, "println", "2 definitions".into(), vec![def.clone(), def]);
                    draw(&mut app);
                    app.nav.peek = None;
                    // The commit dialog with each splitter dragged to both corners.
                    app.commit();
                    draw(&mut app);
                    for k in 0..3 {
                        for (x, y) in [(0, 0), (w - 1, h - 1)] {
                            let Some(r) = app.gitui.dlg.as_ref().map(|d| d.hits.sashes[k]).filter(|r| !r.is_empty()) else { continue };
                            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x, r.y);
                            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, y);
                            draw(&mut app);
                            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
                        }
                    }
                    app.gitui.dlg = None;
                }
            }
        }
        // Tab drags to the corners, edges and middle: splits, moves and group closes.
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "src/main.rs", false);
        app.open_file(0, "README.md", false);
        wait(&mut app);
        for w in 30..=40 {
            for h in 5..=18 {
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
                for (x, y) in [(0, 0), (w - 1, h - 1), (w - 1, 0), (0, h - 1), (w / 2, h / 2), (w - 3, h / 2)] {
                    let Some(r) = app.ui.groups.iter().rev().find_map(|g| g.tabs.first()).map(|t| t.0) else { continue };
                    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x, r.y);
                    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, y);
                    t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
                    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
                    t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
                }
            }
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// Press on tab `i` of group `g`, drag through `via` (live hit rects), release on the last.
    fn drag_tab(app: &mut App, t: &mut Terminal<TestBackend>, g: usize, i: usize, via: &[(u16, u16)]) {
        screen(app, t);
        let r = app.ui.groups[g].tabs[i].0;
        mouse(app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        for &(x, y) in via {
            mouse(app, MouseEventKind::Drag(MouseButton::Left), x, y);
            screen(app, t);
        }
        let &(x, y) = via.last().unwrap();
        mouse(app, MouseEventKind::Up(MouseButton::Left), x, y);
        screen(app, t);
    }

    fn tab_names(app: &App) -> Vec<Vec<String>> {
        app.groups.iter().map(|g| g.tabs.iter().map(|t| base(t.key.trim_start_matches("file:")).to_string()).collect()).collect()
    }

    #[test]
    fn tabs_drag_to_reorder_split_move_and_close_their_group() {
        let root = repo("tabdnd");
        fs::write(root.join("b.txt"), "b\n").unwrap();
        fs::write(root.join("c.txt"), "c\n").unwrap();
        let mut app = App::new(root.clone(), None);
        for f in ["README.md", "b.txt", "c.txt"] {
            app.open_file(0, f, false);
        }
        wait(&mut app);
        let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
        screen(&mut app, &mut t);
        let names = |v: &[&[&str]]| v.iter().map(|g| g.iter().map(|s| s.to_string()).collect()).collect::<Vec<Vec<String>>>();
        // A click, or a press that moves under 2 cells, is a plain tab click.
        let r = app.ui.groups[0].tabs[0].0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), r.x + 2, r.y);
        assert!(app.drop.is_none(), "no drop target under the threshold");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 2, r.y);
        assert_eq!((tab_names(&app), app.groups[0].active), (names(&[&["README.md", "b.txt", "c.txt"]]), 0));
        // Reorder: c.txt onto README.md's left half; the slot marker shows while dragging.
        let r0 = app.ui.groups[0].tabs[0].0;
        screen(&mut app, &mut t);
        let r2 = app.ui.groups[0].tabs[2].0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r2.x + 1, r2.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), r0.x + 1, r0.y);
        assert_eq!(app.drop, Some((0, Zone::Tab(0))));
        screen(&mut app, &mut t);
        let m = &t.backend().buffer()[(r0.x, r0.y)];
        assert_eq!((m.symbol(), m.fg), ("▌", crate::theme::P.tab_dnd_border), "insertion marker before the first tab");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r0.x + 1, r0.y);
        assert_eq!(tab_names(&app), names(&[&["c.txt", "README.md", "b.txt"]]));
        assert_eq!(app.groups[0].active, 0, "the moved tab is active");
        // Split right: b.txt onto the right part of the group, with the half overlay.
        let a = app.ui.groups[0].area;
        let right = (a.right() - 3, a.y + a.height / 2);
        screen(&mut app, &mut t);
        let rb = app.ui.groups[0].tabs[2].0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), rb.x + 1, rb.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), right.0, right.1);
        screen(&mut app, &mut t);
        assert_eq!(app.drop, Some((0, Zone::Right)));
        assert_eq!(t.backend().buffer()[(a.right() - 2, a.y + 3)].bg, crate::theme::P.drop_bg, "the right half lights up");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), right.0, right.1);
        assert_eq!(tab_names(&app), names(&[&["c.txt", "README.md"], &["b.txt"]]));
        assert_eq!(app.ag, 1);
        // Into the other group at a slot: README.md before b.txt.
        screen(&mut app, &mut t);
        let rb = app.ui.groups[1].tabs[0].0;
        drag_tab(&mut app, &mut t, 0, 1, &[(rb.x + 1, rb.y)]);
        assert_eq!(tab_names(&app), names(&[&["c.txt"], &["README.md", "b.txt"]]));
        // The last tab out closes its group: c.txt to the end of the other bar.
        let bar = app.ui.groups[1].tabbar;
        drag_tab(&mut app, &mut t, 0, 0, &[(bar.right() - 6, bar.y)]);
        assert_eq!(tab_names(&app), names(&[&["README.md", "b.txt", "c.txt"]]));
        assert_eq!((app.ag, app.groups[0].active), (0, 2));
        // Split left; then a group's only tab onto its own sides or content: nothing.
        let a = app.ui.groups[0].area;
        drag_tab(&mut app, &mut t, 0, 1, &[(a.x + 2, a.y + a.height / 2)]);
        assert_eq!(tab_names(&app), names(&[&["b.txt"], &["README.md", "c.txt"]]));
        let a = app.ui.groups[0].area;
        drag_tab(&mut app, &mut t, 0, 0, &[(a.x + 2, a.y + 5), (a.right() - 2, a.y + 5)]);
        assert!(app.drop.is_none());
        assert_eq!(tab_names(&app), names(&[&["b.txt"], &["README.md", "c.txt"]]));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_press_drops_drags_whose_release_never_came() {
        let root = repo("stale");
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "src/main.rs", false);
        app.open_file(0, "README.md", false);
        wait(&mut app);
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        screen(&mut app, &mut t);
        let a = app.ui.groups[0].area;
        let inside = (a.x + 5, 0); // the title bar: a press that starts nothing of its own
        // A tab drag, then a press with no release in between: the next drag moves nothing.
        let r = app.ui.groups[0].tabs[0].0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), a.right() - 2, a.y + 5);
        assert!(app.drop.is_some());
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inside.0, inside.1);
        assert!(app.drop.is_none());
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), a.right() - 2, a.y + 6);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), a.right() - 2, a.y + 6);
        assert_eq!(app.groups.len(), 1, "no split from the stale tab drag");
        // The same for an Explorer file drag.
        app.show_side(Side::Explorer, false);
        screen(&mut app, &mut t);
        let row = app.ui.side_list.y + (app.tree.iter().position(|n| n.path == "README.md").unwrap() * (1 + app.ui.side_gap as usize)) as u16;
        let sx = app.ui.side_list.x + 4;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sx, row);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), a.right() - 2, a.y + 5);
        assert!(matches!(app.drag, Drag::File { active: true, .. }));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inside.0, inside.1);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), a.right() - 2, a.y + 6);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), a.right() - 2, a.y + 6);
        assert_eq!(app.groups.len(), 1, "no split from the stale file drag");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_moved_tab_keeps_its_dirty_buffer_and_preview_tabs_get_pinned_and_esc_cancels() {
        let root = repo("tabdirty");
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "src/main.rs", false);
        app.open_file(0, "README.md", true); // a preview tab
        wait(&mut app);
        app.focus = Focus::Editor;
        typ(&mut app, "edited ");
        let (ver, text) = match app.docs.get("file:README.md") {
            Some(Doc::Text(b)) => (b.version(), b.text()),
            _ => unreachable!(),
        };
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        screen(&mut app, &mut t);
        // Esc during a drag: no drop, nothing moves, the release does nothing.
        let a = app.ui.groups[0].area;
        let r = app.ui.groups[0].tabs[1].0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), a.right() - 2, a.y + 5);
        assert!(app.drop.is_some());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.drop.is_none() && matches!(app.drag, Drag::None));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), a.right() - 2, a.y + 5);
        assert_eq!(app.groups.len(), 1);
        // Split right with the dirty preview tab: the same buffer, pinned, undo intact.
        drag_tab(&mut app, &mut t, 0, 1, &[(a.right() - 2, a.y + 5)]);
        assert_eq!(tab_names(&app), vec![vec!["main.rs".to_string()], vec!["README.md".to_string()]]);
        assert!(!app.groups[1].tabs[0].preview, "a moved preview tab is pinned");
        let Some(Doc::Text(b)) = app.docs.get("file:README.md") else { unreachable!() };
        assert_eq!((b.version(), b.text(), b.dirty()), (ver, text, true), "not reloaded");
        for _ in 0..10 {
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        }
        let Some(Doc::Text(b)) = app.docs.get("file:README.md") else { unreachable!() };
        assert_eq!((b.text(), b.dirty()), ("hello\n".to_string(), false), "undo history came along");
        fs::remove_dir_all(&root).unwrap();
    }

    /// herdr closes a pane with HUP: the loop ends, and the exit path writes the
    /// session next to the backups. (The handler is called as the signal would.)
    #[test]
    fn hup_ends_the_loop_and_saves_the_session() {
        let root = repo("hup");
        let state = std::env::temp_dir().join(format!("herdr-code-hup-state-{}", std::process::id()));
        let mut app = App::new(root.clone(), Some(state.join("layout.json")));
        app.session.file = Some(crate::session::path(&state, &root));
        app.open_file(0, "README.md", false);
        app.focus = Focus::Editor;
        typ(&mut app, "x"); // unsaved: a backup too
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        on_term(libc::SIGHUP);
        let r = app.event_loop(&mut t);
        SIGNALED.store(false, Ordering::SeqCst);
        r.unwrap();
        finish(&mut app);
        let saved = fs::read_to_string(app.session.file.as_ref().unwrap()).unwrap();
        assert!(saved.contains("\"file:README.md\""), "{saved}");
        assert!(fs::read_dir(state.join("backup")).unwrap().count() > 0, "and the hot-exit backup");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&state).unwrap();
    }

    /// A committed 100-line file with three changes in the buffer: line 5
    /// modified, a line added after 20, line 40 deleted.
    fn changed_file(tag: &str) -> (PathBuf, App) {
        let root = repo(tag);
        let base: Vec<String> = (1..=100).map(|i| format!("int line_{i} = {i};")).collect();
        fs::write(root.join("long.c"), base.join("\n") + "\n").unwrap();
        let g = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).output().unwrap().status.success());
        g(&["add", "long.c"]);
        g(&["commit", "-q", "-m", "long"]);
        let mut cur = base.clone();
        cur[4] = "int line_5 = 50;".into();
        cur.insert(20, "int added = 1;".into());
        cur.remove(40); // base line 40 (index 39) is now gone: index 40 after the insert
        fs::write(root.join("long.c"), cur.join("\n") + "\n").unwrap();
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "long.c", false);
        wait(&mut app);
        app.update_qdiff();
        assert_eq!(app.qdiff["file:long.c"].hunks.len(), 3);
        (root, app)
    }

    #[test]
    fn minimap_draws_git_lane_find_and_slider_and_scrolls() {
        let (root, mut app) = changed_file("mini");
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        screen(&mut app, &mut t);
        let m = app.ui.minimaps[0];
        let buf = t.backend().buffer();
        // Git lane: the modified line 5 (index 4) on its row, in minimapGutter.modifiedBackground.
        let row = m.mini.row_of(m.scroll, 4).unwrap();
        let lane = &buf[(m.area.x, m.area.y + row)];
        assert_eq!((lane.symbol(), lane.fg), ("▎", crate::theme::P.mini_modified));
        // Braille by default (VS Code's renderCharacters): 4 lines a cell, a dot per run of
        // columns with code ("int line_N = N;": the first two dot columns), nothing past it.
        let p = &crate::theme::P;
        let first = buf[(m.area.x + 1, m.area.y)].clone();
        assert_eq!(first.symbol(), "⣿", "lines 1-4, columns 0..10: all code");
        assert_eq!(buf[(m.area.x + 4, m.area.y)].symbol(), "⠀", "the fourth cell: past every line");
        // Muted: the cell's token colour 65% over the background, never the raw colour.
        let b = match app.docs.get("file:long.c") {
            Some(Doc::Text(b)) => b,
            _ => unreachable!(),
        };
        let mut raw: Vec<Color> = b.segs(0).unwrap().iter().map(|(_, st)| Color::Rgb(st.foreground.r, st.foreground.g, st.foreground.b)).collect();
        raw.push(p.editor_fg);
        let inked = |c: Color| raw.iter().any(|&r| crate::minimap::ink(r, p.mini_fg_opacity, p.mini_bg) == c);
        assert!(inked(first.fg), "{:?} from {raw:?}", first.fg);
        assert!(!raw.contains(&first.fg));
        assert_eq!(first.bg, p.mini_bg);
        // A find match colours its cell (lines 9-12 for line 9) with minimap.findMatchHighlight.
        app.open_find(false);
        typ(&mut app, "line_9 ");
        app.focus = Focus::Editor;
        screen(&mut app, &mut t);
        let m = app.ui.minimaps[0];
        let r9 = m.mini.row_of(m.scroll, 8).unwrap();
        assert_eq!(t.backend().buffer()[(m.area.right() - 1, m.area.y + r9)].bg, p.mini_find);
        // Mouseover shows the slider (VS Code's showSlider default), translucent: blended
        // over the background, the dots keep their colour.
        app.find = None;
        screen(&mut app, &mut t);
        let before = t.backend().buffer()[(m.area.x + 1, m.area.y)].clone();
        app.hover = Some(Position::new(m.area.x + 2, m.area.bottom() - 1));
        screen(&mut app, &mut t);
        let top = t.backend().buffer()[(m.area.x + 1, m.area.y)].clone();
        assert_eq!((top.symbol(), top.fg), (before.symbol(), before.fg), "dots visible under the slider");
        assert_eq!(top.bg, p.mini_slider_hover.over(p.mini_bg), "slider over the viewport rows");
        let sc = p.mini_slider_hover;
        assert_ne!(top.bg, Color::Rgb(sc.0, sc.1, sc.2), "not an opaque block");
        // renderCharacters false: half blocks, a line per half (lines 1 and 2), in the same
        // muted inks; the slider tints both halves.
        app.settings.minimap.render_characters = false;
        app.hover = None;
        screen(&mut app, &mut t);
        let m = app.ui.minimaps[0];
        let half = t.backend().buffer()[(m.area.x + 1, m.area.y)].clone();
        assert_eq!(half.symbol(), "▀");
        assert!(inked(half.fg) && inked(half.bg), "{half:?}");
        assert_eq!(t.backend().buffer()[(m.area.right() - 1, m.area.y)].fg, p.mini_bg, "past the text: background");
        app.hover = Some(Position::new(m.area.x + 2, m.area.bottom() - 1));
        screen(&mut app, &mut t);
        let lit = t.backend().buffer()[(m.area.x + 1, m.area.y)].clone();
        assert_eq!((lit.fg, lit.bg), (sc.over(half.fg), sc.over(half.bg)), "the slider over both halves");
        app.hover = None;
        app.settings.minimap = crate::minimap::Cfg::default();
        // Click below the slider: the clicked line is centred.
        let m = app.ui.minimaps[0];
        let y = m.area.bottom() - 1;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), m.area.x + 3, y);
        let want = m.mini.click_scroll(m.scroll, y - m.area.y);
        assert!(want > 0);
        assert_eq!(app.groups[0].tabs[0].view.scroll, want);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), m.area.x + 3, y);
        // Drag the slider back to the top.
        screen(&mut app, &mut t);
        let m = app.ui.minimaps[0];
        let (top, _) = m.mini.slider(m.scroll);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), m.area.x + 3, m.area.y + top);
        assert!(matches!(app.drag, Drag::Minimap(..)));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), m.area.x + 3, m.area.y);
        assert_eq!(app.groups[0].tabs[0].view.scroll, 0);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), m.area.x + 3, m.area.y);
        // renderCharacters (braille) on the left side: the gutter moves right of it.
        app.settings.minimap.render_characters = true;
        app.settings.minimap.left = true;
        screen(&mut app, &mut t);
        let m = app.ui.minimaps[0];
        assert_eq!(m.area.x, app.ui.groups[0].area.x);
        assert!(app.ui.groups[0].marks_x.unwrap() > m.area.right(), "gutter right of the minimap");
        let c = t.backend().buffer()[(m.area.x + 1, m.area.y)].symbol().chars().next().unwrap();
        assert!(('\u{2800}'..='\u{28ff}').contains(&c), "braille {c:?}");
        app.settings.minimap = crate::minimap::Cfg::default();
        // Toggle from the palette; too narrow an editor hides it.
        app.run_command(Cmd::ToggleMinimap);
        screen(&mut app, &mut t);
        assert!(app.ui.minimaps.is_empty());
        app.run_command(Cmd::ToggleMinimap);
        let mut narrow = Terminal::new(TestBackend::new(70, 20)).unwrap();
        screen(&mut app, &mut narrow);
        assert!(app.ui.minimaps.is_empty(), "text under 40 + 4 columns: hidden");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn peek_opens_from_the_gutter_shifts_rows_navigates_and_reverts() {
        let (root, mut app) = changed_file("peek");
        let mut t = Terminal::new(TestBackend::new(120, 34)).unwrap();
        screen(&mut app, &mut t);
        let gh = app.ui.groups[0].clone();
        // Click the blue marker of line 5 (row 4).
        let mx = gh.marks_x.unwrap();
        assert_eq!(t.backend().buffer()[(mx, gh.text.y + 4)].symbol(), crate::theme::icons().gutter_bar);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), mx, gh.text.y + 4);
        assert_eq!(app.peek.as_ref().map(|p| p.hunk), Some(0));
        let s = screen(&mut app, &mut t);
        assert!(s.contains("long.c") && s.contains("1 of 3 changes"), "{s}");
        // The zone sits right below the hunk; the lines below it are shifted down.
        let gh = app.ui.groups[0].clone();
        let (zy, zh) = gh.zone.unwrap();
        let scroll = app.groups[0].tabs[0].view.scroll;
        assert_eq!(zy, gh.text.y + (5 - scroll) as u16, "below line 5");
        assert!(s.contains("int line_5 = 5;") && s.contains("int line_5 = 50;"), "base and buffer lines");
        assert_eq!(app.line_at(0, zy), None, "a zone row is no line");
        assert_eq!(app.line_at(0, zy + zh), Some(5), "the row under the zone is line 6");
        let (x, y) = cell_of(&t, "int line_7 ", 4);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
        assert_eq!(app.groups[0].tabs[0].view.cursor, Pos::new(6, 4), "clicks below the zone land on the shifted line");
        // ↓ / ↑ move between changes and wrap around.
        app.peek_button(PeekBtn::Prev);
        assert_eq!(app.peek.as_ref().unwrap().hunk, 2, "wraps to the last");
        app.peek_button(PeekBtn::Next);
        assert_eq!(app.peek.as_ref().unwrap().hunk, 0, "and back to the first");
        key(&mut app, KeyCode::F(5), KeyModifiers::ALT);
        assert_eq!(app.peek.as_ref().unwrap().hunk, 1, "Alt+F5: next change");
        key(&mut app, KeyCode::F(7), KeyModifiers::SHIFT);
        assert_eq!(app.peek.as_ref().unwrap().hunk, 0, "Shift+F7: previous");
        // (Stage: peek_stage_stages_one_hunk_and_asks_to_save_first.)
        screen(&mut app, &mut t);
        // Revert (clicked where drawn) puts back the index line as one undo step.
        let revert = app.ui.peek_btns.iter().find(|(_, b)| *b == PeekBtn::Revert).unwrap().0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), revert.x + 1, revert.y);
        assert_eq!(app.ed().unwrap().0.lines[4], "int line_5 = 5;");
        assert_eq!(app.qdiff["file:long.c"].hunks.len(), 2, "that change is gone");
        assert_eq!(app.peek.as_ref().unwrap().hunk, 0, "the peek moves on to the next change");
        app.focus = Focus::Editor;
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.ed().unwrap().0.lines[4], "int line_5 = 50;", "one undo restores the edit");
        // A Revert faster than the 200 ms re-diff must not act on moved lines.
        app.show_peek(0, "file:long.c".into(), 0);
        {
            let (b, tab) = app.ed().unwrap();
            b.move_to(&mut tab.view, Pos::new(0, 0), false);
        }
        let original = app.ed().unwrap().0.text();
        typ(&mut app, "\n"); // pushes the hunk down one line; no update_qdiff yet
        let before = app.ed().unwrap().0.text();
        app.peek_button(PeekBtn::Revert);
        assert_eq!(app.ed().unwrap().0.text(), before, "stale hunk: nothing reverted");
        assert_eq!(app.msg, "Changes moved, try again");
        // Re-diffed now: change 1 of 4 is the new empty line, and Revert takes it out.
        assert_eq!(app.qdiff["file:long.c"].hunks.len(), 4);
        app.peek_button(PeekBtn::Revert);
        assert_eq!(app.ed().unwrap().0.text(), original);
        // Esc closes the peek.
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.peek.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn revert_uses_the_index_bytes_and_refuses_non_utf8() {
        let root = repo("revb");
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).output().unwrap().status.success());
        // CRLF file with a lone \r inside a line: Revert must give back exactly those lines.
        fs::write(root.join("crlf.txt"), "a\r\nb\rc\r\nd\r\n").unwrap();
        fs::write(root.join("bad.txt"), b"x\xff\ny\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "b"]);
        fs::write(root.join("crlf.txt"), "a\r\nCHANGED\r\nd\r\n").unwrap();
        fs::write(root.join("bad.txt"), "x\nY\n").unwrap(); // valid UTF-8 now
        let mut app = App::new(root.clone(), None);
        for f in ["crlf.txt", "bad.txt"] {
            app.open_file(0, f, false);
        }
        wait(&mut app);
        app.update_qdiff();
        app.show_peek(0, "file:crlf.txt".into(), 0);
        app.peek_button(PeekBtn::Revert);
        let b = match app.docs.get("file:crlf.txt") { Some(Doc::Text(b)) => b, _ => unreachable!() };
        assert_eq!(b.text(), "a\r\nb\rc\r\nd\r\n", "the index line with its lone \\r");
        app.show_peek(0, "file:bad.txt".into(), 0);
        app.peek_button(PeekBtn::Revert);
        assert!(app.msg.contains("not UTF-8"), "{}", app.msg);
        let b = match app.docs.get("file:bad.txt") { Some(Doc::Text(b)) => b, _ => unreachable!() };
        assert_eq!(b.text(), "x\nY\n", "nothing written");
        fs::remove_dir_all(&root).unwrap();
    }

    /// VS Code's tight gutter by default: `␠number␠marker␠code`, the same columns in
    /// the editor, its quick-diff peek and a diff; compact keeps its own.
    #[test]
    fn gutters_are_tight_and_line_up() {
        let (root, mut app) = changed_file("gutter");
        assert_eq!(app.settings.spacing.editor_padding, 1);
        let mut t = Terminal::new(TestBackend::new(120, 34)).unwrap();
        screen(&mut app, &mut t);
        let gh = app.ui.groups[0].clone();
        let mx = gh.marks_x.unwrap();
        let sym = |t: &Terminal<TestBackend>, x: u16, y: u16| t.backend().buffer()[(x, y)].symbol().to_string();
        assert_eq!(gh.text.y, gh.tabbar.y + 2, "comfortable keeps the blank row under the tab bar");
        // Editor: the last digit, one blank, the marker column, one blank, the code.
        let (x, y) = cell_of(&t, "int line_1 ", 0);
        assert_eq!((x, sym(&t, mx - 2, y), sym(&t, mx - 1, y), sym(&t, mx + 1, y)), (mx + 2, "1".into(), " ".into(), " ".into()));
        // The quick-diff peek: its numbers and signs in the editor's columns.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), mx, gh.text.y + 4);
        screen(&mut app, &mut t);
        let (x, y) = cell_of(&t, "int line_5 = 5;", 0);
        assert_eq!((x, sym(&t, mx, y), sym(&t, mx - 2, y), sym(&t, mx - 1, y)), (mx + 2, "-".into(), "5".into(), " ".into()), "removed row");
        // Line 5 itself is above the zone too: find the added row inside it.
        let (zy, zh) = app.ui.groups[0].zone.unwrap();
        let y = (zy..zy + zh).find(|&y| line_of(&t, y).contains("int line_5 = 50;")).unwrap();
        let x = line_of(&t, y).find("int line_5 = 50;").map(|i| line_of(&t, y)[..i].chars().count() as u16).unwrap();
        assert_eq!((x, sym(&t, mx, y), sym(&t, mx - 2, y)), (mx + 2, "+".into(), "5".into()), "added row");
        app.peek = None;
        // A diff: `number␠sign␠code` on both sides.
        open_big_diff(&mut app);
        for (compact, pre) in [(false, " "), (true, "")] {
            app.settings.compact = compact;
            app.settings.spacing = if compact { crate::settings::Spacing::COMPACT } else { crate::settings::Spacing::COMFORTABLE };
            screen(&mut app, &mut t);
            let (x, y) = cell_of(&t, "line 020 changed", 0);
            let row = line_of(&t, y);
            assert!(row.contains(&format!(" 21 +{pre}line 020 changed")) && row.contains(&format!(" 21 -{pre}line 020 ")), "compact {compact}: {row}");
            assert_eq!(sym(&t, x - 1 - pre.len() as u16, y), "+");
            let gh = &app.ui.groups[0];
            assert_eq!(gh.text.y, gh.tabbar.y + 1 + u16::from(!compact), "compact {compact}: the blank row");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn peek_beside_a_left_minimap_and_below_a_long_hunk() {
        let (root, mut app) = changed_file("peekl");
        app.settings.minimap.left = true;
        let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
        screen(&mut app, &mut t);
        app.peek_step(1);
        let s = screen(&mut app, &mut t);
        let m = app.ui.minimaps[0].area;
        let z = app.ui.peek_zone;
        assert!(z.x >= m.right() && z.intersection(m).is_empty(), "zone {z:?} clear of the left minimap {m:?}");
        assert!(s.contains(" of 3 changes"), "title visible: {s}");
        // The minimap still takes clicks on the zone's rows.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), m.x + 2, z.y + 1);
        assert!(matches!(app.drag, Drag::Minimap(..)) || app.groups[0].tabs[0].view.scroll > 0);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), m.x + 2, z.y + 1);
        // A 40-line hunk in a 20-row view: the zone and its buttons are still on screen.
        let base: Vec<String> = (1..=100).map(|i| format!("int line_{i} = {i};")).collect();
        let mut cur = base.clone();
        for k in 0..40 {
            cur.insert(50, format!("int new_{k};"));
        }
        fs::write(root.join("long.c"), cur.join("\n") + "\n").unwrap();
        app.settings.minimap.left = false;
        app.refresh();
        wait(&mut app);
        app.update_qdiff();
        let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
        screen(&mut app, &mut t);
        let i = app.qdiff["file:long.c"].hunks.iter().position(|h| h.cur.len() == 40).unwrap();
        app.show_peek(0, "file:long.c".into(), i);
        screen(&mut app, &mut t);
        assert!(app.ui.groups[0].zone.is_some(), "zone drawn below the long hunk");
        let close = app.ui.peek_btns.iter().find(|(_, b)| *b == PeekBtn::Close).expect("buttons").0;
        assert_eq!(app.groups[0].tabs[0].view.cursor.line, 89, "cursor on the hunk's last line");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), close.x, close.y);
        assert!(app.peek.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn peek_title_keeps_its_buttons_on_narrow_editors() {
        let (root, mut app) = changed_file("peekw");
        app.side_visible = false;
        let ic = crate::theme::icons();
        for w in [40u16, 50, 60, 72, 120] {
            let mut t = Terminal::new(TestBackend::new(w, 30)).unwrap();
            app.peek = None;
            screen(&mut app, &mut t);
            app.peek_step(1);
            let s = screen(&mut app, &mut t);
            let zone = app.ui.peek_zone;
            let gh = app.ui.groups[0].clone();
            assert_eq!(zone.right(), gh.text.right(), "{w}: the zone ends at the text, not under the minimap");
            for (b, glyph) in [(PeekBtn::Prev, ic.up), (PeekBtn::Next, ic.down), (PeekBtn::Close, ic.close)] {
                let r = app.ui.peek_btns.iter().find(|(_, x)| *x == b).map(|(r, _)| *r).unwrap_or_else(|| panic!("{w}: {b:?} missing"));
                assert!(zone.contains(r.as_position()), "{w}: {b:?} inside the zone");
                assert_eq!(t.backend().buffer()[(r.x, r.y)].symbol(), glyph, "{w}: {b:?} drawn where it is hit");
            }
            let title: String = s.lines().find(|l| l.contains(ic.close)).unwrap_or("").to_string();
            if gh.text.width >= 60 {
                assert!(s.contains(" of 3 changes"), "{w}: room for the count: {title}");
            }
            let close = app.ui.peek_btns.iter().find(|(_, x)| *x == PeekBtn::Close).unwrap().0;
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), close.x, close.y);
            assert!(app.peek.is_none(), "{w}: × closes");
        }
        assert_eq!(crate::ui::fit_left("drivers/net/sample.c", 9), "…sample.c");
        // Never Stage without Revert, at any width.
        for w in 30..=80u16 {
            let mut t = Terminal::new(TestBackend::new(w, 30)).unwrap();
            app.peek = None;
            screen(&mut app, &mut t);
            app.peek_step(1);
            screen(&mut app, &mut t);
            let has = |b: PeekBtn| app.ui.peek_btns.iter().any(|(_, x)| *x == b);
            assert!(!has(PeekBtn::Stage) || has(PeekBtn::Revert), "{w}: Stage without Revert");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rulers_sit_after_n_columns_behind_the_text() {
        let root = repo("rulers");
        // Ruler at 10: tabs expand to 4, CJK is 2 wide.
        let text = "\t\tab\n中文中文中\n中文中文中文\n0123456789X\n0123456789\nnext\n";
        fs::write(root.join("r.txt"), text).unwrap();
        let mut app = App::new(root.clone(), None);
        app.settings.rulers = vec![(10, None), (4, Some(crate::theme::Rgba(255, 0, 0, 255)))];
        app.settings.minimap.enabled = false;
        app.minimap_on = false;
        app.open_file(0, "r.txt", false);
        app.focus = Focus::Editor;
        let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
        screen(&mut app, &mut t);
        let tx = app.ui.groups[0].text.x;
        let ty = app.ui.groups[0].text.y;
        let g = crate::theme::icons().guide;
        let at = |t: &Terminal<TestBackend>, col: u16, row: u16| {
            let c = &t.backend().buffer()[(tx + col, ty + row)];
            (c.symbol().to_string(), c.fg)
        };
        let grey = crate::theme::P.ruler;
        assert_eq!(at(&t, 10, 0), (g.to_string(), grey), "after two tabs and 'ab' (10 columns)");
        assert_eq!(at(&t, 10, 1), (g.to_string(), grey), "after five CJK chars (10 columns)");
        assert_ne!(at(&t, 10, 2).0, g, "the 6th CJK char covers column 10");
        assert_eq!(at(&t, 10, 3).0, "X", "never over text");
        assert_eq!(at(&t, 10, 8), (g.to_string(), grey), "below the last line too");
        assert_eq!(at(&t, 4, 5), (g.to_string(), Color::Rgb(255, 0, 0)), "its own colour (row 5: 'next' is 4 wide)");
        assert_eq!(at(&t, 4, 3).0, "4", "under text on the other rows");
        // A selection's end-of-line cell on column 10 hides the ruler.
        {
            let (b, tab) = app.ed().unwrap();
            b.move_to(&mut tab.view, Pos::new(4, 0), false);
            b.move_to(&mut tab.view, Pos::new(5, 2), true);
        }
        screen(&mut app, &mut t);
        assert_ne!(at(&t, 10, 4).0, g, "selected: no ruler");
        // Horizontal scroll moves it with the text.
        app.groups[0].tabs[0].view.hscroll = 3;
        app.groups[0].tabs[0].view.anchor = None;
        screen(&mut app, &mut t);
        assert_eq!(at(&t, 7, 8).0, g);
        app.groups[0].tabs[0].view.hscroll = 0;
        // Toggle off for the session, and [] disables.
        app.run_command(Cmd::ToggleRulers);
        screen(&mut app, &mut t);
        assert_ne!(at(&t, 10, 8).0, g);
        app.run_command(Cmd::ToggleRulers);
        app.settings.rulers.clear();
        screen(&mut app, &mut t);
        assert_ne!(at(&t, 10, 8).0, g);
        // Both sides of the diff editor: commit, change a line, open the diff.
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).output().unwrap().status.success());
        git(&["add", "r.txt"]);
        git(&["commit", "-q", "-m", "r"]);
        fs::write(root.join("r.txt"), text.replace("next", "changed")).unwrap();
        app.settings.rulers = vec![(12, None)];
        app.refresh();
        wait(&mut app);
        let i = app.scm_files.iter().position(|f| f.path == "r.txt").unwrap();
        app.open_diff(i, false);
        wait(&mut app);
        let s = screen(&mut app, &mut t);
        let row: Vec<&str> = s.lines().filter(|l| l.matches("0123456789").count() == 2 && !l.contains('X')).collect();
        assert_eq!(row.len(), 1, "{s}");
        assert_eq!(row[0].matches(g).count(), 2, "a ruler on each side: {:?}", row[0]);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Records from the older key formats (no session; a session but no panel) are
    /// found by content and never renamed, so every start leaves them as they are.
    /// The newest is offered, and only the chosen one goes.
    #[test]
    fn old_backup_formats_are_found_in_place() {
        let root = repo("migrate");
        let state = root.join(".state");
        let bak = state.join("backup");
        fs::create_dir_all(&bak).unwrap();
        let readme = root.join("README.md");
        let rec = |text: &str, taken: u64| serde_json::json!({ "path": readme, "text": text, "crlf": false, "final_newline": true, "taken": taken });
        fs::write(bak.join("legacy.json"), rec("legacy edit", 1).to_string()).unwrap();
        let mut scoped = rec("scoped edit", 2);
        scoped["scope"] = "/old/socket".into();
        fs::write(bak.join("scoped.json"), scoped.to_string()).unwrap();
        fs::write(bak.join("other.json"), serde_json::json!({ "path": "/elsewhere/x.txt", "text": "y" }).to_string()).unwrap();
        let listing = || {
            let mut v: Vec<_> = fs::read_dir(&bak).unwrap().map(|e| e.unwrap().file_name()).collect();
            v.sort();
            v
        };
        let before = listing();
        for _ in 0..2 {
            let app = App::new(root.clone(), Some(state.join("layout.json")));
            assert!(app.msg.contains("1 file(s)"), "{}", app.msg);
        }
        assert_eq!(listing(), before, "a start renames nothing");
        let mut app = App::new(root.clone(), Some(state.join("layout.json")));
        app.open_file(0, "README.md", false);
        let p = app.prompt.as_ref().expect("restore offer");
        assert!(p.text.iter().any(|t| t == "2 backups found: this is the newest."), "{:?}", p.text);
        let discard = p.buttons[1].1.clone();
        app.run_prompt(discard);
        assert!(!bak.join("scoped.json").exists() && bak.join("legacy.json").exists(), "only the chosen one");
        // The next open offers the one left, alone; restoring retires it into ours.
        let mut app = App::new(root.clone(), Some(state.join("layout.json")));
        app.open_file(0, "README.md", false);
        let p = app.prompt.as_ref().expect("restore offer");
        assert!(!p.text.iter().any(|t| t.contains("backups found")), "{:?}", p.text);
        let restore = p.buttons[0].1.clone();
        app.run_prompt(restore);
        assert!(app.ed().unwrap().0.text().starts_with("legacy edit"));
        assert!(!bak.join("legacy.json").exists());
        assert!(editor::backup_file(&bak, &app.backup_scope, &app.backup_instance, &readme).exists());
        assert!(bak.join("other.json").exists(), "another worktree's backup is left alone");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn two_panels_editing_one_file_keep_two_backups() {
        let root = repo("twopanes");
        let state = root.join(".state");
        let (mut a, mut b) = (App::new(root.clone(), Some(state.join("layout.json"))), App::new(root.clone(), Some(state.join("layout.json"))));
        a.backup_instance = "w1:p1-101".into();
        b.backup_instance = "w1:p2-102".into();
        // Both open the clean file, then both edit it.
        a.open_file(0, "README.md", false);
        b.open_file(0, "README.md", false);
        for (app, t) in [(&mut a, "from a "), (&mut b, "from b ")] {
            app.focus = Focus::Editor;
            typ(app, t);
            app.sync_backups();
        }
        let file = |app: &App| editor::backup_file(&state.join("backup"), &app.backup_scope, &app.backup_instance, &root.join("README.md"));
        let (fa, fb) = (file(&a), file(&b));
        assert!(fa != fb && fa.exists() && fb.exists());
        // b saves: only b's backup goes; a's edits stay backed up.
        key(&mut b, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(!fb.exists() && fa.exists());
        let rec: serde_json::Value = serde_json::from_slice(&fs::read(&fa).unwrap()).unwrap();
        assert_eq!((rec["text"].as_str(), rec["instance"].as_str()), (Some("from a hello"), Some("w1:p1-101")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reaping_closes_pipes_so_a_child_blocked_on_a_full_pipe_exits() {
        // More than a pipe buffer (64 KiB) that nobody reads.
        let c = Command::new("sh").args(["-c", "head -c 300000 /dev/zero"]).stdout(Stdio::piped()).spawn().unwrap();
        let n = Arc::new(AtomicUsize::new(1));
        let h = reap(c, Some(n.clone()));
        for _ in 0..500 {
            if h.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(h.is_finished(), "the reaper still waits on a writer blocked on its pipe");
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn searches_queue_behind_abandoned_rg_processes() {
        let root = repo("rgq");
        let mut app = App::new(root.clone(), None);
        app.query = "needle".into();
        app.live_rg.store(MAX_LIVE_RG, Ordering::SeqCst); // two abandoned rg still running
        app.start_search();
        assert!(app.search.is_none() && app.queued_search.is_some());
        assert!(app.msg.contains("waiting for the previous search"), "{}", app.msg);
        // Esc cancels the queued request.
        app.focus = Focus::SearchInput;
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.queued_search.is_none() && app.msg == "search cancelled");
        // The latest submitted request runs, with its query as submitted: text typed
        // afterwards without Enter is not searched.
        app.query = "fn main".into();
        app.start_search();
        app.query = "needle".into(); // typed, not submitted
        app.drain();
        assert!(app.queued_search.is_some(), "still two live");
        app.live_rg.store(MAX_LIVE_RG - 1, Ordering::SeqCst); // one of them ended
        app.drain();
        assert!(app.queued_search.is_none() && app.search.is_some());
        wait(&mut app);
        assert!(!app.hits.is_empty() && app.hits.iter().all(|h| h.text.contains("fn main")), "{:?}", app.hits);
        assert_eq!(app.live_rg.load(Ordering::SeqCst), MAX_LIVE_RG - 1, "finished rg counted out");
        // A cancelled search is counted out once rg ends.
        app.live_rg.store(0, Ordering::SeqCst);
        app.start_search();
        app.stop_search();
        for _ in 0..300 {
            if app.live_rg.load(Ordering::SeqCst) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.live_rg.load(Ordering::SeqCst), 0);
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

    /// A write failing after raw mode is on still hands the shell a cooked terminal.
    #[test]
    fn a_failed_setup_restores_the_terminal() {
        static RAW: AtomicBool = AtomicBool::new(false);
        fn raw() -> io::Result<()> {
            RAW.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn cooked() -> io::Result<()> {
            RAW.store(false, Ordering::SeqCst);
            Ok(())
        }
        /// Fails its first write (the alt-screen switch), then takes the rest.
        struct FailOnce(Vec<u8>, bool);
        impl Write for FailOnce {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                if !self.1 {
                    self.1 = true;
                    return Err(io::Error::other("EIO"));
                }
                self.0.extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut w = FailOnce(Vec::new(), false);
        assert!(setup_on(&mut w, raw, cooked).is_err());
        assert!(!RAW.load(Ordering::SeqCst), "raw mode undone");
        let s = String::from_utf8(w.0).unwrap();
        assert!(s.contains("\x1b[?1049l") && s.contains("\x1b[?1000l") && s.contains("\x1b[?25h"), "{s:?}");
        let mut ok = Vec::new();
        setup_on(&mut ok, raw, cooked).unwrap();
        assert!(RAW.load(Ordering::SeqCst) && String::from_utf8(ok).unwrap().contains("\x1b[?1049h"));
    }

    /// Quitting in place hands the shell its terminal back: every mode `setup` set is undone.
    #[test]
    fn quit_restores_the_terminal() {
        let mut v = Vec::new();
        restore_seq(&mut v).unwrap();
        let s = String::from_utf8(v).unwrap();
        for (seq, what) in [
            ("\x1b[<1u", "keyboard enhancement popped"),
            ("\x1b[?2004l", "bracketed paste off"),
            ("\x1b[?1004l", "focus events off"),
            ("\x1b[?1000l", "mouse capture off"),
            ("\x1b[?1006l", "SGR mouse off"),
            ("\x1b[?1049l", "main screen back"),
            ("\x1b[?25h", "cursor shown"),
        ] {
            assert!(s.contains(seq), "{what}: {s:?}");
        }
    }

    fn text(app: &App, key: &str) -> String {
        match app.docs.get(key) {
            Some(Doc::Text(b)) => b.lines.join("\n"),
            _ => panic!("{key} is not open"),
        }
    }

    #[test]
    fn a_subfolder_root_maps_paths_to_its_repo() {
        let root = repo("ws");
        fs::write(root.join("README.md"), "changed\n").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(root.join("src"), None);
        assert_eq!((app.git_root.as_deref(), app.prefix.as_str(), app.repo.as_str()), (Some(root.as_path()), "src/", "src"));
        // Source Control lists the whole repo; the Explorer marks what is under the root.
        let scm: Vec<&str> = app.scm_files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(scm, ["README.md", "src/main.rs"]);
        assert_eq!(app.marks, HashMap::from([("main.rs".to_string(), 'M')]));
        let readme = root.join("README.md").to_string_lossy().into_owned();
        assert_eq!(app.doc_path("src/main.rs"), "main.rs");
        assert_eq!(app.doc_path("README.md"), readme, "outside the root: absolute");
        assert_eq!(app.repo_path("main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(app.repo_path(&readme).as_deref(), Some("README.md"));
        assert_eq!(app.repo_path("/elsewhere/x"), None);
        // SCM rows open as documents; their quick diff is against the repo's index.
        app.scm_action(ScmAct::OpenFile(1));
        app.scm_action(ScmAct::OpenFile(0));
        let keys: Vec<String> = app.groups[0].tabs.iter().map(|t| t.key.clone()).collect();
        assert_eq!(keys, ["file:main.rs".to_string(), format!("file:{readme}")]);
        wait(&mut app);
        app.update_qdiff();
        for k in &keys {
            assert!(app.qdiff[k].marks.iter().any(Option::is_some), "{k}: changed against the index");
        }
        // An Explorer file's history (the TIMELINE) follows its repo path.
        app.groups[0].active = 0;
        let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
        screen(&mut app, &mut t);
        wait(&mut app);
        assert_eq!(app.gitui.timeline.path.as_deref(), Some("main.rs"));
        let tl: Vec<&str> = app.gitui.timeline.items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(tl, ["src/main.rs"]);
        // Staging from a subfolder root stages the repo path.
        app.scm_action(ScmAct::Stage(1));
        wait(&mut app);
        assert!(app.scm_files.iter().any(|f| f.section == Section::Staged && f.path == "src/main.rs"), "{:?}", app.scm_files);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The peek's Stage stages that one hunk; an unsaved buffer asks to save first (Cancel by default).
    #[test]
    fn peek_stage_stages_one_hunk_and_asks_to_save_first() {
        let root = repo("peekstage");
        fs::write(root.join("README.md"), "1\n2\n3\n4\n5\n6\n7\n8\n").unwrap();
        let ok = Command::new("git").arg("-C").arg(&root).args(["commit", "-qam", "lines"]).status().unwrap().success();
        assert!(ok);
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "README.md", false);
        wait(&mut app);
        app.focus = Focus::Editor;
        for (line, text) in [(0, "one"), (7, "eight")] {
            let (b, t) = app.ed().unwrap();
            t.view.anchor = Some(Pos::new(line, 0));
            t.view.cursor = Pos::new(line, 1);
            b.insert(&mut t.view, text);
        }
        app.update_qdiff();
        assert_eq!(app.hunks("file:README.md").len(), 2);
        app.show_peek(0, "file:README.md".into(), 1);
        app.peek_button(PeekBtn::Stage);
        let p = app.prompt.as_ref().expect("save first?");
        assert!(p.text[0].contains("Save the file and stage") && p.buttons[p.sel].0 == "Cancel", "{:?}", p.text);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "1\n2\n3\n4\n5\n6\n7\n8\n", "Cancel: nothing saved");
        app.peek_button(PeekBtn::Stage);
        app.prompt.as_mut().unwrap().sel = 0;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait(&mut app);
        assert!(app.msg.contains("staged"), "{}", app.msg);
        let index = Command::new("git").arg("-C").arg(&root).args(["show", ":README.md"]).output().unwrap().stdout;
        assert_eq!(String::from_utf8(index).unwrap(), "1\n2\n3\n4\n5\n6\n7\neight\n", "only that hunk");
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "one\n2\n3\n4\n5\n6\n7\neight\n", "saved");
        wait(&mut app);
        app.update_qdiff();
        assert_eq!(app.hunks("file:README.md").len(), 1, "the base was refetched");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_without_git_has_no_scm_and_runs_no_git() {
        let d = std::env::temp_dir().join(format!("herdr-code-nogit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("sub/a.txt"), "alpha\nbeta\n").unwrap();
        let d = d.canonicalize().unwrap();
        let mut app = App::new(d.clone(), None);
        assert_eq!(app.git_root, None);
        let before = git::CALLS.with(|n| n.get());
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        assert!(screen(&mut app, &mut t).contains("sub"));
        app.open_file(0, "sub/a.txt", false);
        app.refresh();
        wait(&mut app);
        app.show_side(Side::Scm, false);
        assert_eq!(app.focus, Focus::Side, "no commit box to type into");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("The folder currently open") && s.contains("git repository."), "{s}");
        assert!(!s.contains("Commit") && !s.contains("Changes"), "{s}");
        app.open_quick();
        assert_eq!(app.quick.as_ref().unwrap().items, ["sub/a.txt"]);
        app.quick = None;
        screen(&mut app, &mut t);
        assert_eq!(git::CALLS.with(|n| n.get()), before, "no git command at all");
        assert!(app.loading.is_empty() && app.qdiff.is_empty() && app.branch.is_empty());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn launcher_requests_open_files_and_other_pastes_insert() {
        let root = repo("req");
        let mut app = App::new(root.clone(), None);
        app.token = Some("t0k".into());
        let main = root.join("src/main.rs");
        let req = |p: &Path, rest: &str| format!("herdr-code:t0k:open:{}{rest}", p.display());
        // A request opens the file pinned at line:col (col in characters); nothing is inserted.
        app.on_paste(&req(&main, ":2:5"));
        let tab = &app.groups[0].tabs[app.groups[0].active];
        assert_eq!((tab.key.as_str(), tab.preview, tab.view.cursor), ("file:src/main.rs", false, Pos::new(1, 4)));
        assert_eq!(app.focus, Focus::Editor);
        let clean = "fn main() {\n    println!(\"needle\");\n}";
        assert!(text(&app, "file:src/main.rs").starts_with(clean));
        // Refused, never inserted: outside the root, not a regular file, bad numbers, a relative path.
        let out = std::env::temp_dir().join(format!("herdr-code-req-out-{}", std::process::id()));
        fs::write(&out, "x").unwrap();
        let bad = [
            (req(&out, ":1:1"), "outside"),
            (req(&root.join("src"), ":1:1"), "not a regular file"),
            (req(&main, ":x:1"), "open:"),
            (req(&main, ":1"), "open:"),
            ("herdr-code:t0k:open:src/main.rs:1:1".into(), "open:"),
            ("herdr-code:t0k:shell:true".into(), "shell"),
        ];
        for (body, why) in bad {
            app.on_paste(&body);
            assert!(app.msg.starts_with("request refused") && app.msg.contains(why), "{body}: {}", app.msg);
        }
        assert_eq!(app.groups[0].tabs.len(), 1);
        assert!(text(&app, "file:src/main.rs").starts_with(clean));
        // Another token is no request, and any other paste is ordinary text.
        app.on_paste("herdr-code:bad:open:/x:1:1");
        app.on_paste("hi");
        assert!(text(&app, "file:src/main.rs").contains("    herdr-code:bad:open:/x:1:1hiprintln!"));
        fs::remove_file(&out).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    /// A non-git tree arch/{sh/{boards/b00..b59.c, kernel.c}, x86/x.c}, block/{blk,blk2}.c,
    /// zz.txt, with arch, sh and boards expanded.
    fn deep_tree(tag: &str) -> (PathBuf, App) {
        let d = std::env::temp_dir().join(format!("herdr-code-sticky-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for dir in ["arch/sh/boards", "arch/x86", "block"] {
            fs::create_dir_all(d.join(dir)).unwrap();
        }
        for i in 0..60 {
            fs::write(d.join(format!("arch/sh/boards/b{i:02}.c")), "x\n").unwrap();
        }
        for f in ["arch/sh/kernel.c", "arch/x86/x.c", "block/blk.c", "block/blk2.c", "zz.txt"] {
            fs::write(d.join(f), "x\n").unwrap();
        }
        let d = d.canonicalize().unwrap();
        let mut app = App::new(d.clone(), None);
        for p in ["arch", "arch/sh", "arch/sh/boards"] {
            let i = at(&app, p);
            app.toggle(i);
        }
        (d, app)
    }

    fn at(app: &App, path: &str) -> usize {
        app.tree.iter().position(|n| n.path == path).unwrap()
    }

    /// The sidebar list's cells on screen row `y`.
    fn side_line(t: &Terminal<TestBackend>, app: &App, y: u16) -> String {
        let (b, l) = (t.backend().buffer(), app.ui.side_list);
        (l.x..l.right()).map(|x| b[(x, y)].symbol()).collect()
    }

    #[test]
    fn explorer_sticky_scroll_pins_the_top_level_folder() {
        let (d, mut app) = deep_tree("one");
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let (arch, block, b10) = (at(&app, "arch"), at(&app, "block"), at(&app, "arch/sh/boards/b10.c"));
        app.toggle(block);
        app.off[0] = b10;
        screen(&mut app, &mut t);
        // The default, one row: arch (not boards) under the root header, then the tree.
        assert_eq!((app.settings.sticky_max, app.ui.sticky.clone()), (1, vec![arch]));
        let (x, y) = (app.ui.side_list.x, app.ui.side_list.y);
        let (_, hy) = cell_of(&t, &app.repo.to_uppercase(), 0);
        assert_eq!(y, hy + 1);
        assert!(side_line(&t, &app, y).contains("arch") && side_line(&t, &app, y + 1).contains("b11.c"));
        let c = &t.backend().buffer()[(x + 1, y)];
        assert!(c.modifier.contains(Modifier::UNDERLINED) && c.underline_color == crate::theme::P.sticky_shadow);
        // arch while the first visible row is in arch. When arch's last row (x86) is the
        // one under the sticky row, arch is pushed out: it would sit over block.
        app.off[0] = block - 2;
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [arch]);
        assert!(side_line(&t, &app, y + 1).contains("x86"));
        app.off[0] = block - 1;
        screen(&mut app, &mut t);
        assert!(app.ui.sticky.is_empty() && side_line(&t, &app, y).contains("x86") && side_line(&t, &app, y + 1).contains("block"));
        app.off[0] = block;
        screen(&mut app, &mut t);
        assert!(app.ui.sticky.is_empty() && side_line(&t, &app, y).contains("block"));
        app.off[0] = block + 1;
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [block], "replaced by the next top-level folder");
        // A click on it: block back on top.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 4, y);
        assert_eq!((app.off[0], app.sel[0]), (block, block));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn explorer_sticky_scroll_pins_the_ancestors() {
        let (d, mut app) = deep_tree("pin");
        app.settings.sticky_max = 7;
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        screen(&mut app, &mut t);
        assert!(app.ui.sticky.is_empty(), "at the top");
        let (arch, sh, boards, b10) = (at(&app, "arch"), at(&app, "arch/sh"), at(&app, "arch/sh/boards"), at(&app, "arch/sh/boards/b10.c"));
        app.off[0] = b10;
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [arch, sh, boards], "above 1: outermost first");
        let (x, y) = (app.ui.side_list.x, app.ui.side_list.y);
        let rows: Vec<String> = (0..4).map(|k| side_line(&t, &app, y + k)).collect();
        // Each pinned row is drawn as in the tree: its own indent, chevron and icon.
        let col = |r: &str, name: &str| r.find(name).map(|i| r[..i].chars().count()).unwrap_or_else(|| panic!("{name} in {r:?}"));
        let ind = app.settings.spacing.tree_indent as usize;
        assert_eq!(col(&rows[1], "sh"), col(&rows[0], "arch") + ind, "{rows:?}");
        assert_eq!(col(&rows[2], "boards"), col(&rows[1], "sh") + ind, "{rows:?}");
        assert!(rows[..3].iter().all(|r| r.contains(crate::theme::icons().chev_down)), "{rows:?}");
        assert!(rows[3].contains("b13.c"), "the tree goes on under them: {rows:?}");
        // The shadow: the last pinned row is underlined in sideBarStickyScroll.shadow.
        let buf = t.backend().buffer();
        let c = &buf[(x + 1, y + 2)];
        assert!(c.modifier.contains(Modifier::UNDERLINED) && c.underline_color == crate::theme::P.sticky_shadow);
        assert!(!buf[(x + 1, y + 3)].modifier.contains(Modifier::UNDERLINED));
        // The wheel over a sticky row scrolls the tree.
        mouse(&mut app, MouseEventKind::ScrollDown, x + 2, y);
        assert_eq!(app.off[0], b10 + 3);
        assert!(app.groups[0].tabs.is_empty());
        // Up from the first row under them: that row would hide, so it shows just below them.
        app.off[0] = b10;
        screen(&mut app, &mut t);
        (app.focus, app.sel[0]) = (Focus::Side, b10 + 3);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!((app.sel[0], app.off[0]), (b10 + 2, b10 - 1));
        screen(&mut app, &mut t);
        assert!(side_line(&t, &app, y + 3).contains("b12.c"));
        // A click on a sticky row reveals that folder right under its own parents.
        app.off[0] = b10;
        screen(&mut app, &mut t);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 4, y + 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 4, y + 1);
        assert_eq!((app.sel[0], app.off[0]), (sh, arch));
        assert!(app.tree[sh].open, "it reveals, it does not collapse");
        screen(&mut app, &mut t);
        assert!(app.ui.sticky.is_empty() && side_line(&t, &app, y + 1).contains("sh"));
        // A real row below the sticky ones is hit as itself.
        app.off[0] = b10;
        screen(&mut app, &mut t);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 4, y + 3);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 4, y + 3);
        assert_eq!(app.groups[0].tabs[0].key, "file:arch/sh/boards/b13.c");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn sticky_scroll_caps_densities_gaps_and_off() {
        use crate::settings::Spacing;
        let (d, mut app) = deep_tree("cap");
        let (arch, sh, boards, b10) = (at(&app, "arch"), at(&app, "arch/sh"), at(&app, "arch/sh/boards"), at(&app, "arch/sh/boards/b10.c"));
        // Clamped like VS Code's setting: 1..=10.
        for (v, want) in [(0, 1), (3, 3), (99, 10)] {
            let user = serde_json::json!({ "workbench.tree.stickyScrollMaxItemCount": v });
            let s = settings::from_maps(user.as_object().unwrap(), None, &[]).unwrap();
            assert_eq!(s.sticky_max, want);
        }
        app.off[0] = b10;
        app.settings.sticky_max = 2;
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [arch, sh], "stickyScrollMaxItemCount");
        app.settings.sticky_max = 3;
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [arch, sh, boards], "3: all three, outermost first");
        app.settings.sticky_max = 1;
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [arch], "1: the outermost, not boards");
        app.settings.sticky_max = 7;
        for h in [10, 12, 16] {
            let mut t = Terminal::new(TestBackend::new(100, h)).unwrap();
            screen(&mut app, &mut t);
            let lh = app.ui.side_list.height as usize;
            assert_eq!(app.ui.sticky.len(), 3.min(lh * 2 / 5), "never over 40% of {lh} rows");
        }
        // Both densities and listItemGap 1: slot k is its item row and its gap row.
        let gap = Spacing::parse(Spacing::COMFORTABLE, &serde_json::json!({ "listItemGap": 1 }));
        for (tag, compact, sp) in [("comfortable", false, Spacing::COMFORTABLE), ("compact", true, Spacing::COMPACT), ("gap", false, gap)] {
            (app.settings.compact, app.settings.spacing) = (compact, sp);
            app.off[0] = b10;
            let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
            screen(&mut app, &mut t);
            let (x, y, step) = (app.ui.side_list.x, app.ui.side_list.y, 1 + sp.list_gap);
            assert_eq!(app.ui.sticky, [arch, sh, boards], "{tag}");
            for (k, name) in ["arch", "sh", "boards"].into_iter().enumerate() {
                let row = y + k as u16 * step;
                assert!(side_line(&t, &app, row).contains(name), "{tag}: slot {k}");
                for r in row..row + step {
                    assert_eq!(app.ui.sticky_at(r), Some(app.ui.sticky[k]), "{tag}: row {r}");
                }
            }
            assert_eq!(app.ui.sticky_at(y + 3 * step), None, "{tag}: the tree below");
            assert_eq!(app.ui.side_item(y + 3 * step, app.off[0]), Some(b10 + 3), "{tag}");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 3, y + 3 * step - 1);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 3, y + 3 * step - 1);
            assert_eq!(app.sel[0], boards, "{tag}: boards' last row");
        }
        // workbench.tree.enableStickyScroll off: the top row is the tree's own.
        (app.settings.sticky, app.off[0]) = (false, b10);
        screen(&mut app, &mut t);
        assert!(app.ui.sticky.is_empty() && side_line(&t, &app, app.ui.side_list.y).contains("b10.c"));
        // A tiny terminal never panics, in any view.
        app.settings.sticky = true;
        for (w, h) in [(1, 1), (10, 3), (20, 5), (40, 8)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            for side in SIDES {
                app.side = side;
                app.off[0] = b10;
                screen(&mut app, &mut t);
            }
        }
        fs::remove_dir_all(&d).unwrap();
    }

    /// The git UI's CHANGES list: each section header (Merge Changes, Staged Changes,
    /// Changes) sticks over its files, and a click on it hits the header, not the
    /// file drawn under it.
    #[test]
    fn changes_section_headers_stick_and_take_the_click() {
        let root = repo("stickychg");
        let mut app = App::new(root.clone(), None);
        app.side = Side::Scm;
        let file = |section, path: String, letter| ScmFile { section, path, orig: None, letter };
        app.scm_files = (0..20)
            .map(|i| file(Section::Changes, format!("m{i:02}.c"), '!'))
            .chain((0..20).map(|i| file(Section::Staged, format!("s{i:02}.c"), 'M')))
            .chain((0..20).map(|i| file(Section::Changes, format!("c{i:02}.c"), 'M')))
            .collect();
        let rows = app.scm_rows();
        let at_row = |want: &dyn Fn(&SRow) -> bool| rows.iter().position(want).unwrap();
        let heads = [
            (at_row(&|r| matches!(r, SRow::MergeHead(_))), "Merge Changes"),
            (at_row(&|r| matches!(r, SRow::Head(Section::Staged, _))), "Staged Changes"),
            (at_row(&|r| matches!(r, SRow::Head(Section::Changes, _))), "Changes"),
        ];
        let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
        for (h, name) in heads {
            app.off[2] = h + 5;
            screen(&mut app, &mut t);
            assert_eq!(app.ui.sticky, [h], "{name}");
            let (x, y) = (app.ui.side_list.x, app.ui.side_list.y);
            assert!(side_line(&t, &app, y).contains(name), "{name}: {}", side_line(&t, &app, y));
            assert!(side_line(&t, &app, y + 1).contains("05.c"), "{name}: the files go on below (file 04 is under it)");
            let tabs = app.groups[0].tabs.len();
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 3, y);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 3, y);
            assert_eq!((app.off[2], app.sel[2]), (h, h), "{name}: the header back on top");
            assert_eq!(app.groups[0].tabs.len(), tabs, "{name}: no file under it opened");
            // The wheel over it scrolls the list.
            app.off[2] = h + 5;
            screen(&mut app, &mut t);
            mouse(&mut app, MouseEventKind::ScrollDown, x + 3, y);
            assert_eq!(app.off[2], (h + 8).min(rows.len() - app.ui.side_rows), "{name}: clamped at the end");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn search_groups_and_scm_sections_stick_too() {
        let root = repo("stickyscm");
        for i in 0..40 {
            fs::write(root.join(format!("n{i:02}.txt")), "x\n").unwrap();
        }
        fs::write(root.join("many.txt"), "needle\n".repeat(30)).unwrap();
        let mut app = App::new(root.clone(), None);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        app.show_side(Side::Scm, false);
        app.off[2] = 5;
        screen(&mut app, &mut t);
        let head = app.scm_rows().iter().position(|r| matches!(r, SRow::Head(Section::Changes, _))).unwrap();
        assert_eq!(app.ui.sticky, [head]);
        assert!(side_line(&t, &app, app.ui.side_list.y).contains("Changes"));
        app.show_side(Side::Search, false);
        app.query = "needle".into();
        app.start_search();
        wait(&mut app);
        let g = app.search_rows().iter().position(|r| matches!(r, HRow::File(p, _) if p == "many.txt")).unwrap();
        app.off[1] = g + 5;
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sticky, [g]);
        let (x, y) = (app.ui.side_list.x, app.ui.side_list.y);
        assert!(side_line(&t, &app, y).contains("many.txt"));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 3, y);
        assert_eq!((app.off[1], app.sel[1]), (g, g), "the group's header back on top");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The sticky rows at `off` must be the folders, outermost first, holding the
    /// first row visible below them (`off + pinned`), and no more could be pinned.
    fn assert_sticky_holds(depth: &dyn Fn(usize) -> Option<usize>, pinned: &[usize], off: usize, cap: usize, tag: &str) {
        let c = pinned.len();
        let first = off + c;
        for (k, &a) in pinned.iter().enumerate() {
            assert_eq!(depth(a), Some(k), "{tag} off {off}: pin {k} is at depth {k}");
            assert_eq!(decor::walk_back(depth, first, k), Some(a), "{tag} off {off}: pin {k} ({a}) holds the first visible row {first}");
        }
        // One more pin would have to hold the row it covers and the row below that.
        if c < cap {
            let more = decor::walk_back(depth, first, c);
            let holds_next = more.is_some_and(|a| decor::walk_back(depth, first + 1, c) == Some(a));
            assert!(!holds_next, "{tag} off {off}: {pinned:?} could pin {more:?} too");
        }
    }

    /// Scroll through a tree of several nested top-level folders one row at a time:
    /// at every offset the sticky row is the folder holding the first visible row.
    #[test]
    fn sticky_rows_hold_the_first_visible_row_at_every_offset() {
        let (d, mut app) = deep_tree("sweep");
        for p in ["arch/x86", "block"] {
            let i = at(&app, p);
            app.toggle(i);
        }
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        for cap in [1, 3] {
            app.settings.sticky_max = cap;
            for off in 0..app.tree.len() {
                app.off[0] = off;
                screen(&mut app, &mut t);
                let depth = |j: usize| app.tree.get(j).map(|n| n.depth);
                let tag = format!("cap {cap}, top {}", app.tree[off].path);
                assert_sticky_holds(&depth, &app.ui.sticky, off, cap, &tag);
                // What the screen says: the sticky row's name, then a row inside it.
                if let Some(&a) = app.ui.sticky.first() {
                    let y = app.ui.side_list.y;
                    assert!(side_line(&t, &app, y).contains(&app.tree[a].name), "{tag}");
                }
            }
        }
        fs::remove_dir_all(&d).unwrap();
    }

    /// The same sweep over Search file groups and the CHANGES sections.
    #[test]
    fn search_groups_and_changes_sections_hold_the_first_visible_row() {
        let root = repo("sweepscm");
        let mut app = App::new(root.clone(), None);
        app.hits = (0..40).map(|i| Hit { path: format!("f{}.c", i / [1, 7, 2, 13][i % 4].max(1)), line: i + 1, col: 1, text: "x".into() }).collect();
        app.hits.sort_by(|a, b| a.path.cmp(&b.path));
        app.side = Side::Search;
        let rows = app.search_rows();
        let depth = |j: usize| rows.get(j).map(|r| matches!(r, HRow::Hit(_)) as usize);
        for cap in [1, 2] {
            for off in 0..rows.len() {
                assert_sticky_holds(&depth, &app.pinned(off, cap, &rows, &[]), off, cap, "search");
            }
        }
        let file = |section, path: String, letter| ScmFile { section, path, orig: None, letter };
        app.scm_files = (0..3)
            .map(|i| file(Section::Changes, format!("m{i}"), '!'))
            .chain((0..1).map(|i| file(Section::Staged, format!("s{i}"), 'M')))
            .chain((0..4).map(|i| file(Section::Changes, format!("c{i}"), 'M')))
            .collect();
        app.side = Side::Scm;
        let rows = app.scm_rows();
        let depth = |j: usize| rows.get(j).map(|r| matches!(r, SRow::File(_)) as usize);
        for off in 0..rows.len() {
            assert_sticky_holds(&depth, &app.pinned(off, 1, &[], &rows), off, 1, "changes");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// The Explorer's parent links find the same folders as walking back, and follow
    /// the tree when it changes.
    #[test]
    fn explorer_parent_links_match_walking_back() {
        let (d, mut app) = deep_tree("links");
        let check = |app: &App, tag: &str| {
            for i in 0..app.tree.len() {
                for k in 0..4 {
                    let walked = decor::walk_back(|j| app.tree.get(j).map(|n| n.depth), i, k);
                    assert_eq!(app.tree_ancestor(i, k), walked, "{tag}: row {i}, depth {k}");
                }
            }
        };
        check(&app, "expanded");
        let sh = at(&app, "arch/sh");
        app.toggle(sh);
        check(&app, "sh collapsed");
        let x86 = at(&app, "arch/x86");
        app.toggle(x86);
        app.toggle(sh);
        check(&app, "x86 and sh expanded");
        app.reload_all();
        check(&app, "reloaded");
        fs::remove_dir_all(&d).unwrap();
    }

    /// Tiny terminals scrolled into a sticky state: drawing, the wheel and a click on
    /// the sticky row never panic, and it stays inside the list.
    #[test]
    fn tiny_terminals_scrolled_into_a_sticky_state() {
        let (d, mut app) = deep_tree("tiny");
        let (arch, b10) = (at(&app, "arch"), at(&app, "arch/sh/boards/b10.c"));
        app.settings.sticky_max = 3;
        let mut stuck = 0;
        for h in 1..=12 {
            for w in [1, 6, 12, 24, 40] {
                let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
                app.side = Side::Explorer;
                app.off[0] = b10;
                screen(&mut app, &mut t);
                let l = app.ui.side_list;
                if app.ui.sticky.is_empty() {
                    continue;
                }
                stuck += 1;
                assert_eq!(app.ui.sticky[0], arch, "{w}x{h}");
                assert!(app.ui.sticky.len() <= l.height as usize * 2 / 5 && l.bottom() <= h, "{w}x{h}: {l:?}");
                mouse(&mut app, MouseEventKind::ScrollDown, l.x, l.y);
                screen(&mut app, &mut t);
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), l.x, l.y);
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), l.x, l.y);
                screen(&mut app, &mut t);
            }
        }
        assert!(stuck > 0, "some tiny size is in a sticky state");
        fs::remove_dir_all(&d).unwrap();
    }

    /// A diff tab of a 300-line file changed in three places (line 20 replaced, five
    /// lines inserted after 150, three removed at 260) with one long line at 100.
    fn open_big_diff(app: &mut App) -> Vec<diff::Row> {
        let old: Vec<String> = (0..300).map(|i| if i == 100 { format!("line 100 {}", "x".repeat(300)) } else { format!("line {i:03}") }).collect();
        let mut new = old.clone();
        new[20] = "line 020 changed".into();
        for k in 0..5 {
            new.insert(151 + k, format!("inserted {k}"));
        }
        new.drain(265..268);
        let rows = diff::align(&(old.join("\n") + "\n"), &(new.join("\n") + "\n"));
        let file = ScmFile { section: Section::Changes, path: "big.c".into(), orig: None, letter: 'M' };
        let doc = DiffDoc { title: "big.c".into(), rows: rows.clone(), note: None, file, id: Default::default() };
        app.docs.insert("cdiff:abc:big.c".into(), Doc::Diff(doc));
        app.show_tab(0, "cdiff:abc:big.c".into(), false);
        app.focus = Focus::Editor;
        rows
    }

    fn line_of(t: &Terminal<TestBackend>, y: u16) -> String {
        let b = t.backend().buffer();
        (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
    }

    #[test]
    fn diff_tabs_scroll_like_the_editor() {
        let d = std::env::temp_dir().join(format!("herdr-code-dscroll-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.canonicalize().unwrap(), None);
        let rows = open_big_diff(&mut app);
        let n = rows.len();
        // Wide enough that the modified side keeps minEditorWidth beside its minimap.
        let mut t = Terminal::new(TestBackend::new(200, 40)).unwrap();
        screen(&mut app, &mut t);
        let view = |app: &App| app.groups[0].tabs[0].view.clone();
        // One scrollbar for both sides, at the right edge, the editor's geometry.
        let sb = *app.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Editor(0)).unwrap();
        let (th, text) = (sb.track.height, app.ui.groups[0].text);
        assert_eq!(sb.track.right(), text.right(), "the right edge");
        assert_eq!((sb.slider, sb.max_off, sb.page), (decor::slider(th, n + th as usize - 1, th as usize, 0, n - 1), n - 1, th as usize));
        // The overview ruler: removed in the left half, inserted in the right, both split.
        let row_of = |i: usize| sb.track.y + (i * th as usize / n).min(th as usize - 1) as u16;
        let buf = t.backend().buffer();
        let ic = crate::theme::icons();
        let first = |k: diff::Kind| rows.iter().position(|r| r.kind == k).unwrap();
        let (chg, ins, del) = (first(diff::Kind::Change), first(diff::Kind::Ins), first(diff::Kind::Del));
        let c = &buf[(sb.track.x, row_of(ins))];
        assert_eq!((c.symbol(), c.fg), (ic.lane_right, crate::theme::P.diff_ruler_ins.over(c.bg)), "inserted");
        let c = &buf[(sb.track.x, row_of(del))];
        assert_eq!((c.symbol(), c.fg), (ic.lane_left, crate::theme::P.diff_ruler_del.over(c.bg)), "removed");
        let c = &buf[(sb.track.x, row_of(chg))];
        assert!(c.symbol() == ic.lane_left && c.bg != crate::theme::P.editor_bg, "replaced: both halves");
        assert_eq!(buf[(sb.track.x, row_of(220))].symbol(), " ", "unchanged rows: no mark");
        // The minimap sits on the modified side, between its text and the scrollbar.
        let mm = app.ui.minimaps.iter().find(|m| m.group == 0).copied().expect("a minimap");
        assert!(mm.area.x > text.x + text.width / 2 && mm.area.right() < sb.track.x, "{:?} {text:?}", mm.area);
        assert!((mm.area.y..mm.area.bottom()).any(|y| buf[(mm.area.x, y)].symbol() != " "), "its git lane marks the change");
        // The wheel scrolls both sides together.
        let (cx, cy) = (text.x + 10, text.y + 5);
        mouse(&mut app, MouseEventKind::ScrollDown, cx, cy);
        screen(&mut app, &mut t);
        assert_eq!(view(&app).scroll, 3);
        assert_eq!(line_of(&t, text.y).matches("line 003").count(), 2, "{}", line_of(&t, text.y));
        // Shift+wheel and the horizontal wheel scroll both sides sideways, no further than needed.
        app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: cx, row: cy, modifiers: KeyModifiers::SHIFT });
        screen(&mut app, &mut t);
        assert_eq!(view(&app).hscroll, 0, "short lines: nothing to scroll");
        app.groups[0].tabs[0].view.scroll = 98;
        app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: cx, row: cy, modifiers: KeyModifiers::SHIFT });
        mouse(&mut app, MouseEventKind::ScrollRight, cx, cy);
        screen(&mut app, &mut t);
        assert_eq!((view(&app).scroll, view(&app).hscroll), (98, 12));
        let long = line_of(&t, text.y + 2);
        assert!(!long.contains("line 100") && long.matches("xxxxxxxx").count() >= 2, "both sides moved: {long}");
        mouse(&mut app, MouseEventKind::ScrollLeft, cx, cy);
        assert_eq!(view(&app).hscroll, 6);
        // Keys: Home / End sideways, Ctrl+Home / Ctrl+End, Up / Down, PgUp / PgDn.
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        screen(&mut app, &mut t);
        // End: the 309-column line's end in view on the narrower side, not past it.
        let (hs, side) = (view(&app).hscroll, text.width as usize / 2);
        assert!(hs < 309 && hs + side > 309, "End: hscroll {hs}, side {side}");
        assert!(line_of(&t, text.y + 2).contains("xxx"));
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(view(&app).hscroll, 0);
        key(&mut app, KeyCode::End, KeyModifiers::CONTROL);
        assert_eq!(view(&app).scroll, n - text.height as usize);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
        assert_eq!(view(&app).scroll, 0);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(view(&app).scroll, text.height as usize + 1);
        key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(view(&app).scroll, 0);
        // F7: the next change, revealed centred, marked as the cursor on the ruler.
        key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
        key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
        assert_eq!(view(&app).scroll, ins - text.height as usize / 2);
        screen(&mut app, &mut t);
        assert_eq!(t.backend().buffer()[(sb.track.x, row_of(ins))].symbol(), ic.lane_cursor);
        // A track click pages; the slider drags.
        app.groups[0].tabs[0].view.scroll = 0;
        screen(&mut app, &mut t);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x, sb.track.bottom() - 1);
        assert_eq!(view(&app).scroll, sb.page);
        screen(&mut app, &mut t);
        let sb = *app.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Editor(0)).unwrap();
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x, sb.track.y + sb.slider.0);
        assert!(matches!(app.drag, Drag::Scroll(..)));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sb.track.x, sb.track.bottom() + 5);
        assert_eq!(view(&app).scroll, n - 1, "dragged to the end");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sb.track.x, sb.track.bottom());
        // A minimap click scrolls too.
        app.groups[0].tabs[0].view.scroll = 0;
        screen(&mut app, &mut t);
        let mm = app.ui.minimaps[0];
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), mm.area.x + 2, mm.area.bottom() - 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), mm.area.x + 2, mm.area.bottom() - 1);
        assert!(view(&app).scroll > 0);
        fs::remove_dir_all(&d).unwrap();
    }

    /// Closing tabs drops every per-document cache with the document (hc-sup6: a
    /// closed diff tab used to keep its minimap buffer for good).
    #[test]
    fn closing_tabs_drops_every_per_doc_cache() {
        let root = repo("dropdoc");
        let mut app = App::new(root.clone(), None);
        let mut t = Terminal::new(TestBackend::new(200, 40)).unwrap();
        open_big_diff(&mut app);
        screen(&mut app, &mut t);
        assert_eq!(app.diff_minis.len(), 1, "the diff's minimap buffer");
        app.close_tab(0, app.groups[0].active, false);
        assert!(app.diff_minis.is_empty() && app.docs.is_empty(), "closed: its minimap buffer goes with it");
        // A text file and a diff, the diff in a second group too; then every tab closed.
        app.open_file(0, "README.md", false);
        screen(&mut app, &mut t);
        open_big_diff(&mut app);
        app.split_right(0);
        wait(&mut app);
        screen(&mut app, &mut t);
        app.groups[0].active = 0;
        screen(&mut app, &mut t);
        assert!(!app.qdiff.is_empty() && !app.diff_minis.is_empty() && !app.gitui.cmds.conflicts.is_empty(), "every cache in use");
        while let Some(g) = (0..app.groups.len()).find(|&g| !app.groups[g].tabs.is_empty()) {
            app.close_tab(g, 0, true);
        }
        assert!(app.docs.is_empty() && app.loading.is_empty() && app.qdiff.is_empty() && app.peek.is_none());
        assert!(app.diff_minis.is_empty() && app.gitui.cmds.conflicts.is_empty() && app.diff_cancel.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tiny_terminals_with_a_diff_tab_never_panic() {
        let d = std::env::temp_dir().join(format!("herdr-code-dtiny-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.canonicalize().unwrap(), None);
        open_big_diff(&mut app);
        for (w, h) in [(1, 1), (10, 3), (30, 5), (31, 6), (40, 8), (60, 10), (80, 12)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            app.groups[0].tabs[0].view.scroll = 98;
            screen(&mut app, &mut t);
            for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollRight, MouseEventKind::Down(MouseButton::Left)] {
                for (x, y) in [(0, 0), (w - 1, h - 1), (w / 2, h / 2)] {
                    mouse(&mut app, kind, x, y);
                    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
                    screen(&mut app, &mut t);
                }
            }
            app.focus = Focus::Editor;
            for k in [KeyCode::End, KeyCode::PageDown, KeyCode::F(7), KeyCode::Home] {
                key(&mut app, k, KeyModifiers::NONE);
                screen(&mut app, &mut t);
            }
        }
        fs::remove_dir_all(&d).unwrap();
    }

    /// The sweep on a real tree, read only:
    /// `HERDR_CODE_BIG_TREE=<kernel tree> cargo test -- --ignored --nocapture sticky_sweep`
    /// expands arch, arch/sh, arch/sh/boards and scrolls one row at a time to past arch.
    #[test]
    #[ignore]
    fn sticky_sweep_on_a_big_tree() {
        let Some(root) = std::env::var_os("HERDR_CODE_BIG_TREE").map(PathBuf::from) else { return };
        let mut app = App::new(root.canonicalize().unwrap(), None);
        for p in ["arch", "arch/sh", "arch/sh/boards"] {
            let i = at(&app, p);
            app.toggle(i);
        }
        let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let arch = at(&app, "arch");
        let end = (arch + 1..app.tree.len()).find(|&j| app.tree[j].depth == 0).unwrap();
        let mut bad = Vec::new();
        for cap in [1, 3] {
            app.settings.sticky_max = cap;
            for off in arch..end + 3 {
                app.off[0] = off;
                screen(&mut app, &mut t);
                let depth = |j: usize| app.tree.get(j).map(|n| n.depth);
                let pinned: Vec<&str> = app.ui.sticky.iter().map(|&a| app.tree[a].path.as_str()).collect();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assert_sticky_holds(&depth, &app.ui.sticky, off, cap, "")));
                if r.is_err() {
                    bad.push(format!("cap {cap}: top {} | pinned {pinned:?} | first visible {}", app.tree[off].path, app.tree.get(off + pinned.len()).map_or("-", |n| n.path.as_str())));
                }
            }
        }
        eprintln!("{} rows swept twice, {} wrong:\n{}", end + 3 - arch, bad.len(), bad.join("\n"));
        assert!(bad.is_empty());
    }

    /// Frame time over big lists, sticky scroll on and off. Read only:
    /// `HERDR_CODE_BIG_TREE=<kernel tree> cargo test --release -- --ignored --nocapture frame_time`
    /// expands its drivers/ in full; Search gets 5000 synthetic hits.
    #[test]
    #[ignore]
    fn frame_time_with_big_lists() {
        let Some(root) = std::env::var_os("HERDR_CODE_BIG_TREE").map(PathBuf::from) else { return };
        let mut app = App::new(root.canonicalize().unwrap(), None);
        let d = at(&app, "drivers");
        app.toggle(d);
        let mut j = d + 1;
        while j < app.tree.len() && app.tree[j].depth > app.tree[d].depth {
            if app.tree[j].dir && !app.tree[j].open {
                app.toggle(j);
            }
            j += 1;
        }
        let mut t = Terminal::new(TestBackend::new(200, 60)).unwrap();
        let mut frame = |app: &mut App, label: &str| {
            for on in [true, false] {
                app.settings.sticky = on;
                t.draw(|f| crate::ui::draw(app, f)).unwrap();
                let start = Instant::now();
                for _ in 0..40 {
                    t.draw(|f| crate::ui::draw(app, f)).unwrap();
                }
                eprintln!("{label}, sticky {}: {:?}/frame", if on { "on " } else { "off" }, start.elapsed() / 40);
            }
            // As a frame does it: the rows are built for drawing anyway.
            let (search, off) = (app.search_rows(), app.off[app.side as usize]);
            app.tree_parents.get_mut().clear(); // as after a tree change
            let start = Instant::now();
            std::hint::black_box(app.pinned(off, 1, &search, &[]));
            let first = start.elapsed();
            let start = Instant::now();
            for _ in 0..200 {
                std::hint::black_box(app.pinned(off, 1, &search, &[]));
            }
            eprintln!("{label}, the sticky rows alone: {:?} (first call after a tree change {first:?})", start.elapsed() / 200);
        };
        // Deep at the end of drivers/: the farthest walk back to the drivers row.
        app.off[0] = j.saturating_sub(5);
        let label = format!("Explorer, {} rows, off {}", app.tree.len(), app.off[0]);
        frame(&mut app, &label);
        app.hits = (0..5000).map(|i| Hit { path: "big.c".into(), line: i + 1, col: 1, text: format!("needle {i}") }).collect();
        (app.side, app.query, app.off[1]) = (Side::Search, "needle".into(), 4990);
        frame(&mut app, "Search, 1 file x 5000 hits, off 4990");
        app.hits = (0..5000).map(|i| Hit { path: format!("dir/f{:03}.c", i / 10), line: i + 1, col: 1, text: format!("needle {i}") }).collect();
        app.off[1] = 5400;
        frame(&mut app, "Search, 500 files x 10 hits, off 5400");
    }

    // ---- perf batch 2: coalescing, hover keys, the frame cap, async status --------

    fn mev(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE })
    }

    fn many_files(tag: &str, n: usize) -> PathBuf {
        let root = repo(tag);
        for i in 0..n {
            fs::write(root.join(format!("f{i:03}.txt")), "x\n").unwrap();
        }
        root
    }

    /// Wheel ticks batch before one frame; the click that ends the batch runs after that
    /// frame (hit-testing what was drawn), the key after it, and nothing is lost.
    #[test]
    fn coalescing_batches_wheel_and_keeps_clicks_and_keys_in_order() {
        let root = many_files("coalesce", 80);
        let mut app = App::new(root.clone(), None);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut dirty = true;
        app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
        let l = app.ui.side_list;
        let wheel = mev(MouseEventKind::ScrollDown, l.x + 2, l.y + 2);
        let click = mev(MouseEventKind::Down(MouseButton::Left), l.x + 2, l.y + 1);
        let key = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let mut q: std::collections::VecDeque<Event> = [wheel.clone(), wheel.clone(), wheel.clone(), click, key, wheel].into();
        app.step(&mut t, &mut dirty, |_| Ok(q.pop_front())).unwrap();
        assert_eq!(app.off[0], 9, "three ticks in one batch");
        assert!(app.pending_ev.is_some() && q.len() == 2, "the click waits for the frame; the rest stays queued");
        assert_eq!(app.sel[0], 0);
        let drawn = app.last_draw;
        let mut calls = 0;
        let wait = app
            .step(&mut t, &mut dirty, |_| {
                calls += 1;
                Ok(None)
            })
            .unwrap();
        assert!(app.last_draw > drawn && calls == 0, "drawn first, then the held click, without waiting");
        assert_eq!(app.sel[0], 10, "the click hit row 1 of the scrolled list");
        assert!(wait > Duration::ZERO);
        app.step(&mut t, &mut dirty, |_| Ok(q.pop_front())).unwrap();
        assert_eq!(app.sel[0], 11, "then the key");
        app.step(&mut t, &mut dirty, |_| Ok(q.pop_front())).unwrap();
        assert!(q.is_empty() && app.off[0] > 9, "and the last tick");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Plain motion asks for a frame only when what is drawn under the pointer changes.
    #[test]
    fn motion_redraws_only_when_the_hover_target_changes() {
        let root = many_files("hoverkey", 10);
        let mut app = App::new(root.clone(), None);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        app.open_file(0, "f001.txt", false);
        app.open_file(0, "f002.txt", false);
        screen(&mut app, &mut t);
        let (l, text, tab) = (app.ui.side_list, app.ui.groups[0].text, app.ui.groups[0].tabs[0].0);
        let mv = |app: &mut App, x: u16, y: u16| app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: x, row: y, modifiers: KeyModifiers::NONE });
        assert!(mv(&mut app, l.x + 1, l.y), "into a row");
        assert!(!mv(&mut app, l.x + 6, l.y), "along the same row");
        assert!(mv(&mut app, l.x + 1, l.y + 1), "the next row");
        assert!(mv(&mut app, text.x + 4, text.y + 2), "off the list");
        assert!(!mv(&mut app, text.x + 9, text.y + 3), "over plain text: nothing changes");
        assert!(mv(&mut app, tab.x + 1, tab.y), "onto a tab");
        assert!(!mv(&mut app, tab.x + 2, tab.y), "within it");
        let moves: usize = (0..200).map(|k| usize::from(mv(&mut app, l.x + (k % 20) as u16, l.y + (k / 50) as u16))).sum();
        assert_eq!(moves, 4, "200 moves over 4 rows (from the tab): one frame per row entered");
        BATCH2.with(|b| b.set(false));
        let before: usize = (0..200).map(|k| usize::from(mv(&mut app, l.x + (k % 20) as u16, l.y + (k / 50) as u16))).sum();
        BATCH2.with(|b| b.set(true));
        assert!(before > 150, "before: almost every move redrew ({before})");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The 16 ms cap holds wheel frames, never keys: a key ends the wait at once and runs
    /// on the next turn without waiting; a key on its own is handled when it arrives.
    #[test]
    fn the_frame_cap_never_delays_keys() {
        let root = many_files("cap", 40);
        let mut app = App::new(root.clone(), None);
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut dirty = true;
        app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
        let l = app.ui.side_list;
        let key = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        // Right after a frame: the tick may wait up to 16 ms for more, but the key ends it.
        let mut q: std::collections::VecDeque<Event> = [mev(MouseEventKind::ScrollDown, l.x + 2, l.y), key.clone()].into();
        let mut asked = Vec::new();
        let ret = app
            .step(&mut t, &mut dirty, |w| {
                asked.push(w);
                Ok(q.pop_front())
            })
            .unwrap();
        assert!(asked.len() == 2 && asked[1] <= FRAME, "the batch waited at most a frame for more: {asked:?}");
        assert_eq!(ret, Duration::ZERO, "the next turn runs at once");
        let sel = app.sel[0];
        app.step(&mut t, &mut dirty, |_| panic!("the held key must not wait for input")).unwrap();
        assert_eq!(app.sel[0], sel + 1);
        // A key alone, right after a frame: handled when it arrives, no batch, no cap.
        let mut calls = 0;
        let mut one = Some(key);
        app.step(&mut t, &mut dirty, |_| {
            calls += 1;
            Ok(one.take())
        })
        .unwrap();
        assert_eq!((calls, app.sel[0]), (1, sel + 2));
        fs::remove_dir_all(&root).unwrap();
    }

    /// git status runs as a job: the old status shows (never cleared) until the new one
    /// lands; it is right after a stage and a discard; focus flicker runs one refresh;
    /// and once it all settles the loop goes idle (no busy wake-ups).
    #[test]
    fn status_refreshes_in_the_background_and_the_loop_goes_idle() {
        let root = repo("asyncstatus");
        fs::write(root.join("README.md"), "changed\n").unwrap();
        fs::write(root.join("junk.txt"), "x\n").unwrap();
        let mut app = App::new(root.clone(), None);
        assert_eq!(app.scm_files.len(), 2, "startup reads it at once");
        let i = app.scm_files.iter().position(|f| f.path == "README.md").unwrap();
        let rows = app.scm_files.len();
        app.scm_action(ScmAct::Stage(i));
        assert!(app.gitui.write_since.is_some() && app.scm_files.len() == rows, "queued: the old rows stay");
        wait(&mut app);
        assert!(app.status_since.is_none() && app.scm_files.iter().any(|f| f.path == "README.md" && f.section == Section::Staged));
        let j = app.scm_files.iter().position(|f| f.path == "junk.txt").unwrap();
        app.scm_action(ScmAct::Discard(j));
        app.prompt.as_mut().unwrap().sel = 0;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait(&mut app);
        assert!(!app.scm_files.iter().any(|f| f.path == "junk.txt"), "{:?}", app.scm_files);
        // Three FocusGained in a row: one refresh, after the debounce.
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut dirty = true;
        let mut q: std::collections::VecDeque<Event> = [Event::FocusGained, Event::FocusGained, Event::FocusGained].into();
        for _ in 0..3 {
            app.step(&mut t, &mut dirty, |_| Ok(q.pop_front())).unwrap();
        }
        let due = app.refresh_due.expect("debounced");
        assert!(app.status_since.is_none(), "not yet");
        // Then idle: no timer left, no job, the loop asks for its long wait.
        let start = Instant::now();
        loop {
            let w = app
                .step(&mut t, &mut dirty, |w| {
                    std::thread::sleep(w.min(Duration::from_millis(20)));
                    Ok(None)
                })
                .unwrap();
            if Instant::now() > due && w >= Duration::from_secs(1) {
                assert!(!dirty && app.refresh_due.is_none() && app.loading.is_empty());
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "never went idle");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// Before / after numbers for perf batch 2 (run with `-- --ignored --nocapture`):
    /// frames, bytes and writes through a real crossterm backend, and the UI-thread time
    /// of a focus switch.
    #[test]
    #[ignore = "measurement, prints numbers"]
    fn perf_batch2_measure() {
        use std::cell::RefCell;
        use std::rc::Rc;
        #[derive(Clone, Default)]
        struct Count(Rc<RefCell<(usize, usize, usize)>>);
        impl io::Write for Count {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                let mut c = self.0.borrow_mut();
                c.0 += b.len();
                c.1 += 1;
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.borrow_mut().2 += 1;
                Ok(())
            }
        }
        let n = std::env::var("HC_PERF_FILES").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
        let root = many_files("perf2", n);
        let _ = Command::new("git").arg("-C").arg(&root).args(["add", "-A"]).status();
        let _ = Command::new("git").arg("-C").arg(&root).args(["commit", "-qm", "files"]).status();
        for on in [false, true] {
            BATCH2.with(|b| b.set(on));
            let mut app = App::new(root.clone(), None);
            let c = Count::default();
            let opts = ratatui::TerminalOptions { viewport: ratatui::Viewport::Fixed(Rect::new(0, 0, 120, 40)) };
            let mut t = Terminal::with_options(CrosstermBackend::new(c.clone()), opts).unwrap();
            let mut dirty = true;
            app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
            let l = app.ui.side_list;
            let reset = |c: &Count| *c.0.borrow_mut() = (0, 0, 0);
            let (frames, seen) = (std::cell::Cell::new(0), std::cell::Cell::new(app.last_draw));
            let count = |app: &App| {
                if app.last_draw != seen.get() {
                    seen.set(app.last_draw);
                    frames.set(frames.get() + 1);
                }
            };
            // 1000 wheel ticks, 10 at a time (a gap between bursts), down then up.
            reset(&c);
            for burst in 0..100 {
                let kind = if burst < 50 { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp };
                let mut q: std::collections::VecDeque<Event> = (0..10).map(|_| mev(kind, l.x + 2, l.y + 3)).collect();
                while !q.is_empty() {
                    app.step(&mut t, &mut dirty, |_| Ok(q.pop_front())).unwrap();
                    count(&app);
                }
                app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
                count(&app);
            }
            let (wheel, wheel_frames) = (*c.0.borrow(), frames.replace(0));
            // 200 plain moves over 4 list rows.
            reset(&c);
            for k in 0..200u16 {
                let mut one = Some(mev(MouseEventKind::Moved, l.x + k % 20, l.y + k / 50));
                app.step(&mut t, &mut dirty, |_| Ok(one.take())).unwrap();
                count(&app);
                app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
                count(&app);
            }
            let motion_frames = frames.get();
            // A focus switch: UI-thread time (outside waits) until the new status is in.
            let (mut ui, mut worst) = (Duration::ZERO, Duration::ZERO);
            let mut ev = Some(Event::FocusGained);
            let begin = Instant::now();
            loop {
                let mut waited = Duration::ZERO;
                let s0 = Instant::now();
                app.step(&mut t, &mut dirty, |w| {
                    let w0 = Instant::now();
                    if ev.is_none() {
                        std::thread::sleep(w.min(Duration::from_millis(5)));
                    }
                    waited += w0.elapsed();
                    Ok(ev.take())
                })
                .unwrap();
                let spent = s0.elapsed() - waited;
                ui += spent;
                worst = worst.max(spent);
                if ev.is_none() && app.refresh_due.is_none() && app.loading.is_empty() && begin.elapsed() > Duration::from_millis(10) {
                    break;
                }
            }
            println!(
                "batch2 {} ({n} files): wheel 1000 -> {wheel_frames} frames {} B {} writes | motion 200 -> {motion_frames} frames | focus: longest UI step {:.1} ms (all {:.1} ms), fresh after {:.1} ms",
                if on { "on " } else { "off" },
                wheel.0,
                wheel.1,
                worst.as_secs_f64() * 1000.0,
                ui.as_secs_f64() * 1000.0,
                begin.elapsed().as_secs_f64() * 1000.0
            );
        }
        BATCH2.with(|b| b.set(true));
        fs::remove_dir_all(&root).unwrap();
    }

    /// git never takes optional locks: a status running alongside the user's stage
    /// cannot hold index.lock. With a lock already held it still works and leaves it
    /// alone; and it never rewrites the index, even for a stat-stale file.
    #[test]
    fn status_takes_no_optional_locks() {
        let envs: Vec<_> = git::cmd(Path::new("/")).get_envs().map(|(k, v)| (k.to_owned(), v.map(|v| v.to_owned()))).collect();
        assert!(envs.contains(&("GIT_OPTIONAL_LOCKS".into(), Some("0".into()))), "{envs:?}");
        let root = repo("nolocks");
        let (index, lock) = (root.join(".git/index"), root.join(".git/index.lock"));
        // Startup refreshes the index once (on purpose); then a stat-stale entry, which
        // git status would normally refresh and write back.
        let mut app = App::new(root.clone(), None);
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(root.join("README.md"), "hello\n").unwrap();
        let before = fs::metadata(&index).unwrap().modified().unwrap();
        fs::write(&lock, "held by someone else").unwrap();
        fs::write(root.join("new.txt"), "n\n").unwrap();
        app.refresh();
        wait(&mut app);
        assert!(app.scm_files.iter().any(|f| f.path == "new.txt"), "status worked under a held lock: {:?}", app.scm_files);
        assert_eq!(fs::read_to_string(&lock).unwrap(), "held by someone else", "the lock was left alone");
        fs::remove_file(&lock).unwrap();
        app.refresh();
        wait(&mut app);
        assert!(!lock.exists(), "no lock left behind");
        assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), before, "the index was never rewritten");
        fs::remove_dir_all(&root).unwrap();
    }

    /// A refresh queued behind a running status keeps the spinner on when the older one lands.
    #[test]
    fn a_queued_rerun_keeps_the_spinner() {
        let root = repo("rerun");
        let mut app = App::new(root.clone(), None);
        app.refresh();
        let started = app.status_since.expect("running");
        app.refresh();
        assert!(app.status_again, "queued behind it");
        // Let the first land: the rerun starts and the spinner keeps the first start.
        let t0 = Instant::now();
        while app.status_again && t0.elapsed() < Duration::from_secs(5) {
            app.drain();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.status_since, Some(started), "still the original start: the shown status is stale");
        wait(&mut app);
        assert!(app.status_since.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A stat-stale file (touched, same content) is written back to the index right
    /// after a write op, so later statuses stop re-hashing it.
    #[test]
    fn a_write_op_refreshes_stale_index_stat() {
        let root = repo("statrefresh");
        let stale = |r: &Path| String::from_utf8(Command::new("git").arg("-C").arg(r).args(["diff-files", "--name-only"]).output().unwrap().stdout).unwrap();
        let mut app = App::new(root.clone(), None);
        std::thread::sleep(Duration::from_millis(1100));
        fs::write(root.join("README.md"), "hello\n").unwrap();
        fs::write(root.join("new.txt"), "n\n").unwrap();
        assert_eq!(stale(&root), "README.md\n", "stat-dirty, same content");
        app.refresh();
        wait(&mut app);
        assert_eq!(stale(&root), "README.md\n", "a status alone takes no lock, writes nothing");
        let i = app.scm_files.iter().position(|f| f.path == "new.txt").unwrap();
        app.scm_action(ScmAct::Stage(i));
        wait(&mut app);
        assert_eq!(stale(&root), "", "refreshed right after the stage");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The index writer: 20 quick stages never meet index.lock and all land; stage then
    /// unstage lands in that order; with git made slow (a sleeping fsmonitor hook) the UI
    /// thread only queues work, and steps stay quick while it runs.
    #[test]
    fn index_writes_are_serialized_off_the_ui_thread() {
        let root = repo("writer");
        for i in 0..20 {
            fs::write(root.join(format!("n{i:02}.txt")), "n\n").unwrap();
        }
        let mut app = App::new(root.clone(), None);
        let new: Vec<usize> = (0..app.scm_files.len()).filter(|&i| app.scm_files[i].path.starts_with('n')).collect();
        assert_eq!(new.len(), 20);
        for &i in &new {
            app.scm_action(ScmAct::Stage(i));
        }
        wait(&mut app);
        let staged = app.scm_files.iter().filter(|f| f.section == Section::Staged).count();
        assert!(staged == 20 && !app.msg.contains("index.lock"), "{staged} staged; {}", app.msg);
        // In order: unstage n00 then stage it again ends staged; the reverse ends unstaged.
        let at = |app: &App| app.scm_files.iter().position(|f| f.path == "n00.txt").unwrap();
        let i = at(&app);
        app.scm_action(ScmAct::Unstage(i));
        app.scm_action(ScmAct::Stage(i));
        wait(&mut app);
        assert!(app.scm_files.iter().any(|f| f.path == "n00.txt" && f.section == Section::Staged));
        // A slow git: the UI thread queues and returns; steps stay quick meanwhile.
        let hook = root.join(".git/slow-fsmonitor");
        fs::write(&hook, "#!/bin/sh\nsleep 0.4\nexit 1\n").unwrap();
        let _ = Command::new("chmod").arg("+x").arg(&hook).status();
        let _ = Command::new("git").arg("-C").arg(&root).args(["config", "core.fsmonitor"]).arg(&hook).status();
        let i = at(&app);
        let t0 = Instant::now();
        app.scm_action(ScmAct::Unstage(i));
        assert!(t0.elapsed() < Duration::from_millis(20), "queued, not run: {:?}", t0.elapsed());
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let (mut dirty, mut worst, start) = (true, Duration::ZERO, Instant::now());
        while app.gitui.busy() || !app.loading.is_empty() {
            let s0 = Instant::now();
            let mut slept = Duration::ZERO;
            app.step(&mut t, &mut dirty, |w| {
                let w0 = Instant::now();
                std::thread::sleep(w.min(Duration::from_millis(10)));
                slept += w0.elapsed();
                Ok(None)
            })
            .unwrap();
            worst = worst.max(s0.elapsed() - slept);
            assert!(start.elapsed() < Duration::from_secs(10));
        }
        assert!(start.elapsed() >= Duration::from_millis(400), "git really was slow");
        assert!(worst < Duration::from_millis(30), "a UI step took {worst:?} while git ran");
        assert!(app.scm_files.iter().any(|f| f.path == "n00.txt" && f.section == Section::Changes));
        fs::remove_dir_all(&root).unwrap();
    }

    fn slow_write(app: &mut App, ms: u64) {
        app.gitui.write(move || {
            std::thread::sleep(Duration::from_millis(ms));
            crate::scm::Msg::Wrote(Ok(String::new()), false)
        });
    }

    fn slow_git(root: &Path) {
        let hook = root.join(".git/slow-fsmonitor");
        fs::write(&hook, "#!/bin/sh\nsleep 0.4\nexit 1\n").unwrap();
        let _ = Command::new("chmod").arg("+x").arg(&hook).status();
        let _ = Command::new("git").arg("-C").arg(root).args(["config", "core.fsmonitor"]).arg(&hook).status();
    }

    /// Quitting with git still writing asks first, [Wait] by default: it quits once the
    /// writes are done. [Quit Anyway] leaves at once, and `finish_writes` (what run()
    /// does after the UI) lets the running git complete. HUP/TERM gets ~2 s of that.
    #[test]
    fn quitting_never_drops_queued_git_writes() {
        let root = repo("quitwrites");
        let mut app = App::new(root.clone(), None);
        wait(&mut app);
        slow_write(&mut app, 300);
        app.request_quit();
        let p = app.prompt.as_ref().expect("asks");
        assert!(p.text[0].contains("Git is still writing (1 operation)") && p.buttons[p.sel].0 == "Wait", "{:?}", p.text);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.quit && app.gitui.quit_when_idle);
        wait(&mut app);
        assert!(app.quit, "quit once git was done");
        // Quit Anyway: a slow real stage still lands.
        slow_git(&root);
        fs::write(root.join("new.txt"), "n\n").unwrap();
        let mut app = App::new(root.clone(), None);
        wait(&mut app);
        let i = app.scm_files.iter().position(|f| f.path == "new.txt").unwrap();
        app.scm_action(ScmAct::Stage(i));
        app.request_quit();
        app.prompt.as_mut().unwrap().sel = 1;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.quit && app.gitui.writes() > 0, "left with git still writing");
        crate::scm::finish_writes(&mut app, None);
        let staged = Command::new("git").arg("-C").arg(&root).args(["diff", "--cached", "--name-only"]).output().unwrap().stdout;
        assert_eq!(String::from_utf8_lossy(&staged), "new.txt\n", "the write completed");
        // HUP/TERM: a short grace, not forever.
        let mut app = App::new(root.clone(), None);
        slow_write(&mut app, 4000);
        let t0 = Instant::now();
        crate::scm::finish_writes(&mut app, Some(Duration::from_secs(2)));
        assert!(t0.elapsed() >= Duration::from_millis(1900) && t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        fs::remove_dir_all(&root).unwrap();
    }

    /// A discard confirmed behind a slow op checks the files again before acting: saved
    /// edits made meanwhile are never thrown away.
    #[test]
    fn a_queued_discard_refuses_files_changed_since_confirming() {
        let root = repo("stalediscard");
        fs::write(root.join("README.md"), "changed\n").unwrap();
        let mut app = App::new(root.clone(), None);
        wait(&mut app);
        slow_write(&mut app, 500);
        let j = app.scm_files.iter().position(|f| f.path == "README.md").unwrap();
        app.scm_action(ScmAct::Discard(j));
        app.prompt.as_mut().unwrap().sel = 0;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        fs::write(root.join("README.md"), "newer edits, saved after confirming\n").unwrap();
        wait(&mut app);
        assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "newer edits, saved after confirming\n");
        assert!(app.msg.contains("discard cancelled"), "{}", app.msg);
        fs::remove_dir_all(&root).unwrap();
    }

    /// The peek stages its change only if the file still has those lines there.
    #[test]
    fn peek_stage_refuses_a_different_change_at_the_same_lines() {
        let root = repo("peekcontent");
        fs::write(root.join("README.md"), "1\n2\n3\n").unwrap();
        let _ = Command::new("git").arg("-C").arg(&root).args(["commit", "-qam", "three"]).status();
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "README.md", false);
        wait(&mut app);
        app.focus = Focus::Editor;
        let (b, t) = app.ed().unwrap();
        t.view.anchor = Some(Pos::new(1, 0));
        t.view.cursor = Pos::new(1, 1);
        b.insert(&mut t.view, "two");
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        app.update_qdiff();
        app.show_peek(0, "file:README.md".into(), 0);
        // Someone else writes another change to the same line before Stage runs.
        fs::write(root.join("README.md"), "1\nTWO\n3\n").unwrap();
        app.peek_button(PeekBtn::Stage);
        wait(&mut app);
        // Range overlap alone would have staged TWO; the content check refused it.
        let index = Command::new("git").arg("-C").arg(&root).args(["show", ":README.md"]).output().unwrap().stdout;
        assert_eq!(String::from_utf8_lossy(&index), "1\n2\n3\n", "nothing staged");
        fs::remove_dir_all(&root).unwrap();
    }
}
