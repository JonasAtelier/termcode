//! Source Control view, VS Code style: the CHANGES pane (an "Add commit"
//! button over Merge Changes, Staged Changes and Changes) above the GRAPH
//! pane, the Explorer's TIMELINE pane, and the commit hover card. Git work runs
//! on threads; `drain` folds the results in on the UI thread.
//!
//! The CHANGES list reuses the sidebar's list state (`sel`/`off` of
//! `Side::Scm`, `ui.side_list`), so its keys, clicks and scrollbar are the
//! generic ones. GRAPH and TIMELINE keep their own state here.

use crate::commitdlg::{self, CommitDlg};
use crate::diff;
use crate::git::{ScmFile, Section};
use crate::gitops::{self, Commit, FileChange, FileCommit, Hover, Page, RefLabel, PAGE};
use crate::graph::{GraphRow, Lanes};
use crate::panel::{base, App, DiffDoc, Doc, Focus, SRow, ScrollTarget, Side};
use crate::theme::{git_color, icons, Rgba, P};
use crate::ui::{fill, fit, fit_pair, inset, popup_block, right, room_before, row, scm_row, scrollbar, sp, width};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::Clear;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

/// Cells a commit subject keeps before its ref badges collapse into `+N` (and before a
/// TIMELINE time shortens to "1h").
const SUBJECT_MIN: usize = 20;
/// Cells a subject keeps before the short time goes too.
const SUBJECT_FLOOR: usize = 12;

/// How long the pointer rests on a commit before its card opens.
pub const HOVER: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Default, Debug)]
pub struct List {
    pub sel: usize,
    pub off: usize,
}

/// GRAPH pane rows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GRow {
    Commit(usize),
    /// File `j` of the expanded commit `i`.
    File(usize, usize),
    /// "View more": the next 100 commits.
    More,
}

#[derive(Default)]
pub struct Graph {
    pub commits: Vec<Commit>,
    pub rows: Vec<GraphRow>,
    lanes: Lanes,
    pub more: bool,
    pub loading: bool,
    /// A page arrived since the last reset.
    loaded: bool,
    /// The expanded commit and its files (None while they load).
    pub open: Option<(usize, Option<Vec<FileChange>>)>,
    pub list: List,
    /// HEAD as of the last load: a new HEAD reloads the graph.
    head: Option<String>,
    gen: u64,
    /// The commit-graph cache was checked for (`gitops::ensure_commit_graph`).
    cache_checked: bool,
}

#[derive(Default)]
pub struct Timeline {
    pub open: bool,
    /// The file shown (worktree-relative).
    pub path: Option<String>,
    pub items: Vec<FileCommit>,
    pub more: bool,
    pub loading: bool,
    pub list: List,
    gen: u64,
}

/// The commit hover card.
pub struct Card {
    pub hash: String,
    /// None while it loads.
    pub hover: Option<Result<Hover, String>>,
    /// The row it belongs to; the card sits beside it.
    pub anchor: Rect,
    /// When the pointer left both: the card closes CARD_GRACE later unless it is back.
    left: Option<Instant>,
}

/// How long a card outlives the pointer leaving it: time to cross the scrollbar, the
/// border and other rows on the way from the row to the card's buttons.
pub const CARD_GRACE: Duration = Duration::from_secs(1);

/// Clickable things drawn by this module.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    ChangesHead,
    GraphHead,
    /// The line between CHANGES and GRAPH: the splitter (VS Code's sash).
    GraphSash,
    /// GRAPH row (index into `graph_rows`).
    Graph(usize),
    /// The ⓘ at a commit row's end.
    Info(usize),
    TimelineHead,
    TimelineSash,
    Timeline(usize),
    TimelineMore,
    CardCopy,
    CardOpen,
}

#[derive(Default)]
pub struct Hits {
    pub areas: Vec<(Rect, Hit)>,
    /// Rows a resting pointer opens a card for: (row, commit hash).
    hovers: Vec<(Rect, String)>,
    /// CHANGES content, GRAPH list, TIMELINE list, the card.
    pub changes: Rect,
    pub graph: Rect,
    pub timeline: Rect,
    pub card: Rect,
    /// `gitcmds`: status bar items, picker rows, notification, conflict actions.
    pub cmds: Vec<(Rect, crate::gitcmds::Hit)>,
    pub picker: Rect,
    pub toast: Rect,
}

impl Hits {
    /// A row a resting pointer opens `hash`'s card for.
    pub fn push_hover(&mut self, r: Rect, hash: String) {
        self.hovers.push((r, hash));
    }
}

/// Confirmable git actions, run from the app's prompt (`PAct::Git`).
#[derive(Clone, Debug)]
pub enum GitAct {
    /// Nothing was staged: stage every change, then commit (and push).
    StageAllCommit { push: bool },
    /// A confirmed operation (sync, rebase, aborts, drops, force delete...).
    Run(crate::gitcmds::Op),
    /// Sync, and stop asking (`git.confirmSync: false`).
    SyncAlways,
    /// The peek's Stage on an unsaved buffer: save this document, then stage.
    SaveStage(String),
    /// Quitting with git still writing: wait for it, then quit.
    QuitWait,
    /// Quitting with git still writing: leave now; `run` lets git finish first.
    QuitAnyway,
}

pub enum Msg {
    Page(u64, usize, Result<Page<Commit>, String>),
    Files(String, Result<Vec<FileChange>, String>),
    Diff(String, DiffDoc),
    Card(String, Result<Hover, String>),
    /// Open the diff of `path` (or the first file) in a commit: hash, short, files, path.
    Open(String, String, Result<Vec<FileChange>, String>, Option<String>),
    Timeline(u64, String, usize, Result<Page<FileCommit>, String>),
    Dlg(commitdlg::Msg),
    Cmd(crate::gitcmds::Msg),
    /// A job panicked: only the jobs count comes back.
    Lost,
    /// A job of the index writer finished (its message inside).
    Written(Box<Msg>),
    /// An index write (stage, unstage, discard...): a note, or git's error; `reload`
    /// rereads the Explorer too (discard deletes files). Status follows either way.
    Wrote(Result<String, String>, bool),
}

type WriteJob = (String, Box<dyn FnOnce() -> Msg + Send>);

/// Sends `Msg::Lost` if dropped before its job's message went out (a panic
/// unwinding the job), so the jobs count always comes back down.
struct Reply(Option<mpsc::Sender<Msg>>);

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(Msg::Lost);
        }
    }
}

pub struct GitUi {
    pub changes_open: bool,
    pub graph_open: bool,
    /// Rows of CHANGES content while both panes are open, once dragged; None = half.
    pub split: Option<u16>,
    /// Keys go to the GRAPH list rather than CHANGES.
    pub graph_focus: bool,
    pub graph: Graph,
    pub timeline: Timeline,
    /// Timeline content rows, including its header, once resized.
    timeline_height: Option<u16>,
    /// Grabbed border row and original Timeline content height.
    timeline_drag: Option<(u16, u16)>,
    pub card: Option<Card>,
    /// The commit under a resting pointer: hash, its row, since when.
    hover: Option<(String, Rect, Instant)>,
    pub dlg: Option<CommitDlg>,
    /// The commit dialog's dragged splitters (layout.json `commit_dialog`).
    pub dlg_split: commitdlg::Ratios,
    /// The commit message draft, also kept on disk (`commitdlg`).
    pub draft: Option<String>,
    /// GRAPH header (or its sash) pressed at this row; whether it was dragged since (a
    /// sash press counts as one: releasing it never toggles GRAPH); the grabbed row's
    /// distance below the CHANGES list's end.
    drag: Option<(u16, bool, u16)>,
    pub hits: Hits,
    /// Running op, pickers, sync state, blame, notification (`gitcmds`).
    pub cmds: crate::gitcmds::State,
    /// This panel's Git Output: every git call of its jobs and its UI.
    pub log: gitops::OutputLog,
    /// The index writer: one thread runs every write job in order (see `write`).
    writer: Option<mpsc::Sender<WriteJob>>,
    /// The write job running now: its label, since when.
    current: std::sync::Arc<std::sync::Mutex<Option<(String, Instant)>>>,
    /// Write jobs not finished yet, and since when there have been some (a spinner).
    writes: usize,
    pub write_since: Option<Instant>,
    /// Quit once the writes are done ([Wait]); quit despite them ([Quit Anyway]).
    pub quit_when_idle: bool,
    pub quit_anyway: bool,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    jobs: usize,
}

impl GitUi {
    /// Pane state from layout.json's `scm` object.
    pub fn new(saved: &serde_json::Value) -> GitUi {
        let (tx, rx) = mpsc::channel();
        let mut g = GitUi {
            changes_open: true,
            graph_open: true,
            split: None,
            graph_focus: false,
            graph: Graph::default(),
            timeline: Timeline { open: true, ..Default::default() },
            timeline_height: None,
            timeline_drag: None,
            card: None,
            hover: None,
            dlg: None,
            dlg_split: [None; 3],
            draft: None,
            drag: None,
            hits: Hits::default(),
            cmds: crate::gitcmds::State::default(),
            log: gitops::OutputLog::default(),
            writer: None,
            current: Default::default(),
            writes: 0,
            write_since: None,
            quit_when_idle: false,
            quit_anyway: false,
            tx,
            rx,
            jobs: 0,
        };
        g.restore(&saved["scm"]);
        g
    }

    /// Pane state from a `layout()` object (layout.json's `scm`, a session's): what it has.
    pub fn restore(&mut self, s: &serde_json::Value) {
        self.changes_open = s["changes_open"].as_bool().unwrap_or(self.changes_open);
        self.graph_open = s["graph_open"].as_bool().unwrap_or(self.graph_open);
        self.timeline.open = s["timeline_open"].as_bool().unwrap_or(self.timeline.open);
        if let Some(h) = s["timeline_height"].as_u64() {
            self.timeline_height = Some(h.clamp(4, 500) as u16);
        }
        if let Some(v) = s["split"].as_u64() {
            self.split = Some(v.clamp(1, 500) as u16);
        }
        for (k, r) in commitdlg::SASHES.iter().zip(self.dlg_split.iter_mut()) {
            if let Some(f) = s["commit_dialog"][k.name()].as_f64().filter(|f| (0.0..=1.0).contains(f)) {
                *r = Some(f);
            }
        }
    }

    pub fn layout(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "changes_open": self.changes_open,
            "graph_open": self.graph_open,
            "timeline_open": self.timeline.open,
        });
        if let Some(s) = self.split {
            v["split"] = s.into();
        }
        if let Some(h) = self.timeline_height {
            v["timeline_height"] = h.into();
        }
        let dlg: serde_json::Map<String, serde_json::Value> =
            commitdlg::SASHES.iter().zip(self.dlg_split).filter_map(|(k, r)| Some((k.name().to_string(), r?.into()))).collect();
        if !dlg.is_empty() {
            v["commit_dialog"] = dlg.into();
        }
        v
    }

    /// Run `job` on a thread; its message comes back through `drain`.
    pub fn spawn(&mut self, job: impl FnOnce() -> Msg + Send + 'static) {
        self.jobs += 1;
        let (mut reply, log) = (Reply(Some(self.tx.clone())), self.log.clone());
        std::thread::spawn(move || {
            let m = gitops::with_log(&log, job);
            if let Some(tx) = reply.0.take() {
                let _ = tx.send(m);
            }
        });
    }

    /// Queue a job that writes the index or refs. One worker thread runs them in the
    /// order queued, so no two git writers ever race for index.lock and stage A then
    /// stage B land in that order; the UI thread never writes the index itself. A
    /// panicking job is caught there and reported as `Msg::Lost`; the queue goes on.
    /// `label` names it while it runs ("commit", "Pulling…") for the quit wait.
    pub fn write(&mut self, label: &str, job: impl FnOnce() -> Msg + Send + 'static) {
        self.jobs += 1;
        self.writes += 1;
        self.write_since.get_or_insert_with(Instant::now);
        let (tx, log, current) = (self.tx.clone(), self.log.clone(), self.current.clone());
        let q = self.writer.get_or_insert_with(|| {
            let (qtx, qrx) = mpsc::channel::<WriteJob>();
            std::thread::spawn(move || {
                for (label, job) in qrx {
                    *current.lock().unwrap_or_else(|e| e.into_inner()) = Some((label, Instant::now()));
                    let m = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| gitops::with_log(&log, job))).unwrap_or(Msg::Lost);
                    *current.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    if tx.send(Msg::Written(Box::new(m))).is_err() {
                        break;
                    }
                }
            });
            qtx
        });
        let _ = q.send((label.to_string(), Box::new(job)));
    }

    /// The quit wait's line: "waiting for 2 git operations: commit… (3 s)".
    pub fn waiting_line(&self) -> String {
        let n = self.writes;
        let cur = self.current.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let what = cur.map_or(String::new(), |(l, t)| format!(": {}… ({} s)", l.trim_end_matches('…'), t.elapsed().as_secs()));
        format!("waiting for {n} git operation{}{what}; Ctrl+C to stop waiting", if n == 1 { "" } else { "s" })
    }

    /// Write jobs queued or running.
    pub fn writes(&self) -> usize {
        self.writes
    }

    /// Run `f` (git calls on the UI thread) logged in this panel's Git Output.
    pub fn logged<T>(&self, f: impl FnOnce() -> T) -> T {
        gitops::with_log(&self.log, f)
    }

    /// A job is out: the event loop polls often.
    pub fn busy(&self) -> bool {
        self.jobs > 0
    }

    /// When a resting pointer opens the card.
    /// When a resting pointer opens the card, or the cursor's blame is due.
    pub fn due(&self) -> Option<Instant> {
        let hover = self.hover.as_ref().filter(|_| self.card.is_none()).map(|h| h.2 + HOVER);
        let close = self.card.as_ref().and_then(|c| c.left).map(|t| t + CARD_GRACE);
        [hover, close, self.cmds.due()].into_iter().flatten().min()
    }
}

// ---------------------------------------------------------------- colours

/// Source Control colours: the theme's, else VS Code's dark registry defaults.
pub struct Pal {
    pub lanes: [Color; 5],
    pub ref_local: Color,
    pub ref_remote: Color,
    pub label_fg: Color,
    pub added: Color,
    pub deleted: Color,
}

impl Pal {
    pub fn lane(&self, k: usize) -> Color {
        self.lanes[k % self.lanes.len()]
    }
}

pub fn pal(app: &App) -> Pal {
    let empty = serde_json::Map::new();
    let t = app.settings.theme.json["colors"].as_object().unwrap_or(&empty);
    let get = |k: &str, d: &str, bg: Color| crate::theme::lookup(t, k).or_else(|| Rgba::parse(d)).map_or(P.sidebar_fg, |c| c.over(bg));
    let s = P.sidebar_bg;
    Pal {
        lanes: [
            get("scmGraph.foreground1", "#FFB000", s),
            get("scmGraph.foreground2", "#DC267F", s),
            get("scmGraph.foreground3", "#994F00", s),
            get("scmGraph.foreground4", "#40B0A6", s),
            get("scmGraph.foreground5", "#B66DFF", s),
        ],
        ref_local: get("scmGraph.historyItemRefColor", "#3794FF", s),
        ref_remote: get("scmGraph.historyItemRemoteRefColor", "#B180D7", s),
        label_fg: get("scmGraph.historyItemHoverLabelForeground", "#FFFFFF", s),
        added: get("scmGraph.historyItemHoverAdditionsForeground", "#81B88B", s),
        deleted: get("scmGraph.historyItemHoverDeletionsForeground", "#C74E39", s),
    }
}

/// Codicons for refs and the info button (ASCII: none, and `i`).
fn ref_icon(r: &RefLabel) -> &'static str {
    if crate::theme::ascii() {
        return "";
    }
    match r {
        RefLabel::Current(_) | RefLabel::Branch(_) => "\u{ea68} ",
        RefLabel::Remote(_) => "\u{ebaa} ",
        RefLabel::Tag(_) => "\u{ea66} ",
        RefLabel::Head => "\u{ebf8} ",
    }
}

fn info_icon() -> &'static str {
    if crate::theme::ascii() {
        "i"
    } else {
        "\u{ea74}"
    }
}

/// Ref badges within `budget` cells; the rest becomes `+N`.
pub fn badges(refs: &[RefLabel], pal: &Pal, budget: usize) -> Vec<Span<'static>> {
    // HEAD's own ref first, as in VS Code: when they collapse it stays, the rest become +N.
    let mut refs: Vec<&RefLabel> = refs.iter().collect();
    refs.sort_by_key(|r| !matches!(r, RefLabel::Current(_) | RefLabel::Head));
    let mut v = Vec::new();
    let mut used = 0;
    for (k, r) in refs.iter().copied().enumerate() {
        let (name, bg) = match r {
            RefLabel::Current(b) => (b.as_str(), pal.ref_local),
            RefLabel::Remote(b) => (b.as_str(), pal.ref_remote),
            RefLabel::Branch(b) | RefLabel::Tag(b) => (b.as_str(), P.badge_bg),
            RefLabel::Head => ("HEAD", P.badge_bg),
        };
        let fg = if matches!(r, RefLabel::Current(_) | RefLabel::Remote(_)) { pal.label_fg } else { P.badge_fg };
        let mut text = format!(" {}{name} ", ref_icon(r));
        let rest = refs.len() - k;
        // Room kept for the "+N" the refs after this one may need.
        let more = if rest > 1 { format!(" +{}", rest - 1).width() } else { 0 };
        if k == 0 && matches!(r, RefLabel::Current(_) | RefLabel::Head) {
            // Shrunk before it collapses (keeping room for "+N"): the icon goes first, then
            // the name is cut, to 3 letters at least ("mai…"); under that it is a +N too.
            let room = budget.saturating_sub(1 + more);
            if text.width() > room {
                text = format!(" {name} ");
            }
            if text.width() > room && room >= 5 {
                text = fit(&text, room);
            }
        }
        if used + 1 + text.width() + more > budget {
            let plus = format!(" +{rest}");
            if used + plus.width() <= budget {
                v.push(Span::styled(plus, Style::new().fg(P.dim)));
            }
            break;
        }
        used += 1 + text.width();
        v.push(Span::raw(" "));
        v.push(Span::styled(text, Style::new().fg(fg).bg(bg)));
    }
    v
}

// ---------------------------------------------------------------- dates

/// VS Code's `fromNow` with full words: "now", "5 minutes ago", "1 year ago".
pub fn ago(t: i64, now: i64) -> String {
    ago_unit(t, now).map_or("now".into(), |(n, unit, _)| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" }))
}

/// `ago` in a few cells, for a narrow row: "now", "5m", "1h", "3d", "2w", "4mo", "1y".
pub fn ago_short(t: i64, now: i64) -> String {
    ago_unit(t, now).map_or("now".into(), |(n, _, u)| format!("{n}{u}"))
}

/// (count, unit, short unit) of the time since `t`; None under 30 s ("now").
fn ago_unit(t: i64, now: i64) -> Option<(i64, &'static str, &'static str)> {
    let s = (now - t).max(0);
    if s < 30 {
        return None;
    }
    Some(match s {
        s if s < 3600 => ((s / 60).max(1), "minute", "m"),
        s if s < 86_400 => (s / 3600, "hour", "h"),
        s if s < 7 * 86_400 => (s / 86_400, "day", "d"),
        s if s < 30 * 86_400 => (s / (7 * 86_400), "week", "w"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "month", "mo"),
        s => (s / (365 * 86_400), "year", "y"),
    })
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Local "YYYY-MM-DD HH:MM".
pub fn local_time(t: i64) -> String {
    let tt = t as libc::time_t;
    // SAFETY: tm is plain data; localtime_r only writes into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

// ---------------------------------------------------------------- loading

/// Load the first page (`reset`) or the next one ("View more").
pub fn load_graph(app: &mut App, reset: bool) {
    let Some(root) = app.git_root.clone() else { return };
    let g = &mut app.gitui.graph;
    if g.loading && !reset {
        return;
    }
    g.gen += 1;
    g.loading = true;
    let (gen, skip) = (g.gen, if reset { 0 } else { g.commits.len() });
    // Once a session, outside the jobs count: a long first write (a kernel-size
    // history) must not hold up quitting or a reload. Tests skip it so their Git
    // Output stays exact; gitops tests cover the write.
    if !std::mem::replace(&mut g.cache_checked, true) && !cfg!(test) {
        let (r, log) = (root.clone(), app.gitui.log.clone());
        std::thread::spawn(move || gitops::with_log(&log, || gitops::ensure_commit_graph(&r)));
    }
    app.gitui.spawn(move || Msg::Page(gen, skip, gitops::log_page(&root, skip, PAGE)));
}

/// After a status landed (HEAD came with it, in the sync state): a new HEAD reloads a
/// visible graph; an open dialog relists. No git runs here.
pub fn on_refresh(app: &mut App) {
    if app.git_root.is_none() {
        return;
    }
    if app.side == Side::Scm && app.side_visible {
        let head = app.gitui.cmds.sync.as_ref().map(|s| format!("{:?}", s.head));
        let g = &mut app.gitui.graph;
        if head != g.head || !g.loaded && !g.loading {
            g.head = head;
            load_graph(app, true);
        }
    }
    if app.gitui.dlg.is_some() {
        commitdlg::relist(app);
    }
}

fn load_timeline(app: &mut App) {
    let Some(root) = app.git_root.clone() else { return };
    let Some(path) = app.gitui.timeline.path.clone() else { return };
    let Some(rp) = app.repo_path(&path) else { return };
    let t = &mut app.gitui.timeline;
    t.gen += 1;
    t.loading = true;
    let (gen, skip) = (t.gen, t.items.len());
    app.gitui.spawn(move || {
        let page = gitops::file_history(&root, &rp, skip, PAGE);
        Msg::Timeline(gen, path, skip, page)
    });
}

/// Follow the active editor: its file's history, loaded once per file.
fn sync_timeline(app: &mut App) {
    if !app.gitui.timeline.open || app.git_root.is_none() {
        return;
    }
    let active = app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).map(|t| t.key.clone());
    let Some(rel) = active.and_then(|k| k.strip_prefix("file:").map(String::from)) else { return };
    if app.repo_path(&rel).is_none() || app.gitui.timeline.path.as_deref() == Some(rel.as_str()) {
        return;
    }
    let t = &mut app.gitui.timeline;
    *t = Timeline { open: true, path: Some(rel), gen: t.gen, ..Default::default() };
    load_timeline(app);
}

/// Fold finished jobs in; true when something changed.
pub fn drain(app: &mut App) -> bool {
    let mut changed = false;
    while let Ok(m) = app.gitui.rx.try_recv() {
        app.gitui.jobs = app.gitui.jobs.saturating_sub(1);
        changed = true;
        let m = match m {
            Msg::Written(inner) => {
                app.gitui.writes = app.gitui.writes.saturating_sub(1);
                if app.gitui.writes == 0 {
                    app.gitui.write_since = None;
                }
                *inner
            }
            m => m,
        };
        match m {
            Msg::Page(gen, skip, page) => {
                let g = &mut app.gitui.graph;
                if gen != g.gen {
                    continue;
                }
                g.loading = false;
                match page {
                    Ok(p) => {
                        if skip == 0 {
                            let sel = g.list;
                            *g = Graph { head: g.head.take(), gen: g.gen, list: sel, ..Default::default() };
                        }
                        if skip == g.commits.len() {
                            g.rows.extend(g.lanes.rows(&p.items));
                            g.commits.extend(p.items);
                            g.more = p.more;
                            g.loaded = true;
                        }
                    }
                    Err(e) => app.msg = e,
                }
            }
            Msg::Files(hash, files) => {
                let g = &mut app.gitui.graph;
                let Some((i, slot)) = g.open.as_mut() else { continue };
                if g.commits.get(*i).is_none_or(|c| c.hash != hash) {
                    continue;
                }
                match files {
                    Ok(f) => *slot = Some(f),
                    Err(e) => {
                        g.open = None;
                        app.msg = e;
                    }
                }
            }
            Msg::Diff(key, doc) => {
                // A tab closed meanwhile does not resurrect its document.
                if app.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key)) {
                    app.docs.insert(key, Doc::Diff(doc));
                }
            }
            Msg::Card(hash, r) => {
                if let Some(c) = app.gitui.card.as_mut().filter(|c| c.hash == hash) {
                    c.hover = Some(r);
                }
            }
            Msg::Open(hash, short, files, want) => match files {
                Ok(files) => {
                    let f = match &want {
                        Some(p) => files.iter().find(|f| &f.path == p || f.orig_path.as_ref() == Some(p)),
                        None => files.first(),
                    };
                    match f.cloned() {
                        Some(f) => open_commit_diff(app, &hash, &short, f, true),
                        None => app.msg = format!("{short} changes no file"),
                    }
                }
                Err(e) => app.msg = e,
            },
            Msg::Timeline(gen, path, skip, page) => {
                let t = &mut app.gitui.timeline;
                if gen != t.gen || t.path.as_deref() != Some(path.as_str()) || skip != t.items.len() {
                    continue;
                }
                t.loading = false;
                match page {
                    Ok(p) => {
                        t.items.extend(p.items);
                        t.more = p.more;
                    }
                    Err(e) => app.msg = e,
                }
            }
            Msg::Dlg(m) => commitdlg::drain(app, m),
            Msg::Cmd(m) => crate::gitcmds::drain(app, m),
            Msg::Lost => app.msg = "a background git job failed".into(),
            Msg::Written(_) => {}
            Msg::Wrote(r, reload) => {
                match r {
                    Ok(m) if m.is_empty() => {}
                    Ok(m) => app.msg = m,
                    Err(e) => app.msg = e,
                }
                if reload {
                    app.reload_all();
                } else {
                    app.refresh();
                }
            }
        }
    }
    // [Wait] was chosen: quit now that git is done (unsaved files still get asked about).
    if app.gitui.quit_when_idle && app.gitui.writes == 0 {
        app.gitui.quit_when_idle = false;
        app.request_quit();
        changed = true;
    }
    changed | crate::gitcmds::sync_output(app)
}

/// After the UI is gone: let queued and running writes finish (their pipes stay open
/// and are drained, so git never takes SIGPIPE), for at most `limit` if one is given.
/// It shows what runs on stderr. Ctrl+C there just ends this process: every git runs in
/// its own session (no SIGINT) with its output in files (no SIGPIPE), and finishes alone.
pub fn finish_writes(app: &mut App, limit: Option<Duration>) {
    let start = Instant::now();
    let mut shown = String::new();
    while app.gitui.writes > 0 && limit.is_none_or(|l| start.elapsed() < l) {
        let line = app.gitui.waiting_line();
        if line != shown {
            eprint!("\r\x1b[Kherdr-code: {line}");
            shown = line;
        }
        drain(app);
        std::thread::sleep(Duration::from_millis(50));
    }
    if !shown.is_empty() {
        eprintln!();
    }
}

/// The DiffDoc VS Code shows for a file of a commit: first parent left, commit right.
pub fn commit_diff_doc(root: &std::path::Path, hash: &str, short: &str, f: FileChange) -> DiffDoc {
    let title = format!("{} ({short})", base(&f.path));
    let file = ScmFile { section: Section::Changes, path: f.path.clone(), orig: f.orig_path.clone(), letter: f.status };
    let (rows, note) = match gitops::commit_sides(root, hash, &f) {
        Err(e) => (Vec::new(), Some(e)),
        Ok((a, b)) if a.binary || b.binary => (Vec::new(), Some("binary file: no text diff".to_string())),
        Ok((a, b)) => (diff::align(&String::from_utf8_lossy(&a.bytes), &String::from_utf8_lossy(&b.bytes)), None),
    };
    DiffDoc { title, rows, note, file, id: Default::default() }
}

/// Open (or show again) the read-only side-by-side diff of `f` in commit `hash`.
pub fn open_commit_diff(app: &mut App, hash: &str, short: &str, f: FileChange, preview: bool) {
    let Some(root) = app.git_root.clone() else { return };
    let key = format!("cdiff:{hash}:{}", f.path);
    app.show_tab(app.ag, key.clone(), preview);
    app.focus = Focus::Editor;
    if !app.docs.contains_key(&key) {
        let (hash, short) = (hash.to_string(), short.to_string());
        app.gitui.spawn(move || Msg::Diff(key, commit_diff_doc(&root, &hash, &short, f)));
    }
}

/// A restored commit-diff tab's document: `path` of `hash`, built off the UI thread.
pub fn load_commit_diff(app: &mut App, key: &str, hash: &str, path: &str) {
    let Some(root) = app.git_root.clone() else { return };
    let (key, hash, path) = (key.to_string(), hash.to_string(), path.to_string());
    app.gitui.spawn(move || {
        let short = hash.get(..7).unwrap_or(&hash).to_string();
        let f = gitops::commit_files(&root, &hash).ok().and_then(|fs| fs.into_iter().find(|f| f.path == path || f.orig_path.as_deref() == Some(path.as_str())));
        let doc = match f {
            Some(f) => commit_diff_doc(&root, &hash, &short, f),
            None => DiffDoc { title: format!("{} ({short})", base(&path)), rows: Vec::new(), note: Some(format!("{path} is not in {short}")), file: ScmFile { section: Section::Changes, path, orig: None, letter: 'M' }, id: Default::default() },
        };
        Msg::Diff(key, doc)
    });
}

/// Open one file of `hash` (`path`, or the first) once its file list is known.
fn open_commit(app: &mut App, hash: &str, short: &str, path: Option<String>) {
    let Some(root) = app.git_root.clone() else { return };
    let (hash, short) = (hash.to_string(), short.to_string());
    app.gitui.spawn(move || {
        let files = gitops::commit_files(&root, &hash);
        Msg::Open(hash, short, files, path)
    });
}

/// F7 / Shift+F7 in a diff view: the next / previous change, wrapping.
/// `from` is the change row last jumped to (None before the first jump).
pub fn next_change(rows: &[diff::Row], from: Option<usize>, dir: isize) -> Option<usize> {
    let starts: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].kind != diff::Kind::Same && (i == 0 || rows[i - 1].kind == diff::Kind::Same))
        .collect();
    if dir > 0 {
        let next = from.map_or(starts.first(), |f| starts.iter().find(|&&s| s > f).or(starts.first()));
        next.copied()
    } else {
        let prev = from.map_or(starts.last(), |f| starts.iter().rev().find(|&&s| s < f).or(starts.last()));
        prev.copied()
    }
}

/// F7 in the active diff tab. The view's `want_x` marks that a jump happened
/// and `cursor.line` holds its row (diff views have no caret of their own).
pub fn jump_change(app: &mut App, dir: isize) {
    let h = app.ui.groups.get(app.ag).map_or(20, |g| g.text.height as usize);
    let Some(grp) = app.groups.get_mut(app.ag) else { return };
    let Some(tab) = grp.tabs.get_mut(grp.active) else { return };
    let Some(Doc::Diff(d)) = app.docs.get(&tab.key) else { return };
    let from = tab.view.want_x.map(|_| tab.view.cursor.line);
    match next_change(&d.rows, from, dir) {
        Some(i) => {
            tab.view.cursor.line = i;
            tab.view.want_x = Some(0);
            // Revealed centred, as VS Code's diff editor does.
            tab.view.scroll = i.saturating_sub(h / 2);
        }
        None => app.msg = "No changes".into(),
    }
}

// ---------------------------------------------------------------- rows and actions

pub fn graph_rows(g: &Graph) -> Vec<GRow> {
    let mut v = Vec::with_capacity(g.commits.len() + 1);
    for i in 0..g.commits.len() {
        v.push(GRow::Commit(i));
        if let Some((o, Some(files))) = &g.open {
            if *o == i {
                v.extend((0..files.len()).map(|j| GRow::File(i, j)));
            }
        }
    }
    if g.more {
        v.push(GRow::More);
    }
    v
}

/// Click or Enter on GRAPH row `i`: a commit expands or collapses its files,
/// a file opens its diff, "View more" loads the next page.
pub fn activate_graph(app: &mut App, i: usize, pin: bool) {
    match graph_rows(&app.gitui.graph).get(i).copied() {
        Some(GRow::Commit(ci)) => toggle_commit(app, ci),
        Some(GRow::File(ci, j)) => {
            let g = &app.gitui.graph;
            let c = &g.commits[ci];
            let f = g.open.as_ref().and_then(|o| o.1.as_ref()).and_then(|fs| fs.get(j)).cloned();
            if let Some(f) = f {
                let (h, s) = (c.hash.clone(), c.short.clone());
                open_commit_diff(app, &h, &s, f, !pin);
            }
        }
        Some(GRow::More) => load_graph(app, false),
        None => {}
    }
}

fn toggle_commit(app: &mut App, ci: usize) {
    let g = &mut app.gitui.graph;
    if g.open.as_ref().is_some_and(|o| o.0 == ci) {
        g.open = None;
        return;
    }
    let Some(hash) = g.commits.get(ci).map(|c| c.hash.clone()) else { return };
    g.open = Some((ci, None));
    let Some(root) = app.git_root.clone() else { return };
    app.gitui.spawn(move || {
        let files = gitops::commit_files(&root, &hash);
        Msg::Files(hash, files)
    });
}

pub fn open_card(app: &mut App, hash: String, anchor: Rect) {
    if app.gitui.card.as_ref().is_some_and(|c| c.hash == hash) {
        return;
    }
    // Cleared first: a timer left in the past would keep the event loop from sleeping.
    app.gitui.hover = None;
    let Some(root) = app.git_root.clone() else { return };
    app.gitui.card = Some(Card { hash: hash.clone(), hover: None, anchor, left: None });
    app.gitui.spawn(move || {
        let card = gitops::commit_hover(&root, &hash);
        Msg::Card(hash, card)
    });
}

/// The resting-pointer timer fired: open the card; or the pointer stayed off a card for
/// CARD_GRACE: close it. True when either did.
pub fn tick(app: &mut App) -> bool {
    let mut changed = crate::gitcmds::tick(app);
    if app.gitui.card.as_ref().and_then(|c| c.left).is_some_and(|t| t.elapsed() >= CARD_GRACE) {
        app.gitui.card = None;
        changed = true;
    }
    let Some((hash, rect, t)) = app.gitui.hover.clone() else { return changed };
    if app.gitui.card.is_some() || t.elapsed() < HOVER {
        return changed;
    }
    open_card(app, hash, rect);
    true
}

/// Pointer motion (no button): start or stop the hover timer; leaving a card and its
/// row starts its CARD_GRACE, coming back stops it. True when a redraw is needed.
pub fn moved(app: &mut App, p: Position) -> bool {
    let gu = &mut app.gitui;
    if let Some(c) = &mut gu.card {
        if gu.hits.card.contains(p) || c.anchor.contains(p) {
            c.left = None;
            return false;
        }
        c.left.get_or_insert_with(Instant::now);
    }
    match gu.hits.hovers.iter().find(|(r, _)| r.contains(p)) {
        Some((r, h)) => {
            if gu.hover.as_ref().is_none_or(|x| &x.0 != h) {
                gu.hover = Some((h.clone(), *r, Instant::now()));
            }
        }
        None => gu.hover = None,
    }
    false
}

fn save_layout(app: &App) {
    app.save_layout();
}

/// Mouse down: true when this module handled it. The CHANGES list, the
/// "Add commit" button and scrollbars fall through to the generic handlers.
pub fn click(app: &mut App, p: Position, double: bool) -> bool {
    if app.gitui.dlg.is_some() {
        commitdlg::click(app, p, double);
        return true;
    }
    if crate::gitcmds::click(app, p) {
        return true;
    }
    let hit = app.gitui.hits.areas.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, h)| *h);
    if app.gitui.card.is_some() && !app.gitui.hits.card.contains(p) {
        app.gitui.card = None;
        app.gitui.hover = None;
    }
    if app.ui.scrollbars.iter().any(|s| s.track.contains(p)) {
        return false;
    }
    if app.side == Side::Scm && app.ui.side_list.contains(p) {
        app.gitui.graph_focus = false;
        return false;
    }
    let Some(hit) = hit else { return false };
    match hit {
        Hit::ChangesHead => {
            app.gitui.changes_open = !app.gitui.changes_open;
            save_layout(app);
        }
        Hit::GraphHead => app.gitui.drag = Some((p.y, false, between(app.settings.blocks))),
        Hit::GraphSash => app.gitui.drag = Some((p.y, true, 0)),
        Hit::Graph(i) => {
            app.focus = Focus::Side;
            app.gitui.graph_focus = true;
            app.gitui.graph.list.sel = i;
            activate_graph(app, i, double);
        }
        Hit::Info(ci) => {
            let Some(hash) = app.gitui.graph.commits.get(ci).map(|c| c.hash.clone()) else { return true };
            let anchor = app.gitui.hits.hovers.iter().find(|(_, h)| *h == hash).map_or(Rect::new(p.x, p.y, 1, 1), |x| x.0);
            open_card(app, hash, anchor);
        }
        Hit::TimelineSash => app.gitui.timeline_drag = Some((p.y, app.gitui.hits.timeline.height + 1)),
        Hit::TimelineHead => {
            app.gitui.timeline.open = !app.gitui.timeline.open;
            save_layout(app);
        }
        Hit::Timeline(i) => {
            app.gitui.timeline.list.sel = i;
            if let Some(it) = app.gitui.timeline.items.get(i) {
                let (h, s, path) = (it.commit.hash.clone(), it.commit.short.clone(), it.path.clone());
                open_commit(app, &h, &s, Some(path));
            }
        }
        Hit::TimelineMore => load_timeline(app),
        Hit::CardCopy => {
            if let Some(c) = &app.gitui.card {
                let hash = c.hash.clone();
                let how = crate::panel::clipboard_set(&hash);
                app.msg = format!("copied {} via {how}", &hash[..hash.len().min(7)]);
            }
        }
        Hit::CardOpen => {
            if let Some(c) = app.gitui.card.take() {
                let short = c.hash[..c.hash.len().min(7)].to_string();
                // Expanded in the graph too, when it is there.
                if let Some(ci) = app.gitui.graph.commits.iter().position(|x| x.hash == c.hash) {
                    if app.gitui.graph.open.as_ref().is_none_or(|o| o.0 != ci) {
                        toggle_commit(app, ci);
                    }
                }
                open_commit(app, &c.hash, &short, None);
            }
        }
    }
    true
}

/// Forget a drag whose release never arrived: a sidebar or dialog splitter's.
pub fn drop_drags(app: &mut App) {
    app.gitui.drag = None;
    app.gitui.timeline_drag = None;
    if let Some(d) = app.gitui.dlg.as_mut() {
        d.drag = None;
        d.hdrag = None;
    }
}

/// Rows between the CHANGES list and the GRAPH header: the cards' shared border (cards),
/// the shared line (lines), none (none).
fn between(st: crate::blocks::BlockStyle) -> u16 {
    u16::from(st != crate::blocks::BlockStyle::None)
}

/// Dragging a sidebar splitter resizes its panes. True while that drag is on.
pub fn drag(app: &mut App, p: Position) -> bool {
    if commitdlg::drag(app, p) {
        return true;
    }
    if let Some((y, h)) = app.gitui.timeline_drag {
        if app.side == Side::Explorer && app.side_visible && app.gitui.timeline.open {
            app.gitui.timeline_height = Some((i32::from(h) + i32::from(y) - i32::from(p.y)).clamp(4, 500) as u16);
        }
        return true;
    }
    let Some((y0, moved, off)) = app.gitui.drag.as_mut() else { return false };
    if p.y != *y0 {
        *moved = true;
    }
    if *moved && app.gitui.changes_open && app.gitui.graph_open {
        let top = app.gitui.hits.changes.y + *off;
        app.gitui.split = Some(p.y.saturating_sub(top).max(1));
    }
    true
}

/// Releasing the GRAPH header: a click (no drag) collapses or expands it.
pub fn release(app: &mut App) -> bool {
    if commitdlg::release(app) {
        return true;
    }
    if app.gitui.timeline_drag.take().is_some() {
        save_layout(app);
        return true;
    }
    let Some((_, moved, _)) = app.gitui.drag.take() else { return false };
    if !moved {
        app.gitui.graph_open = !app.gitui.graph_open;
    }
    save_layout(app);
    true
}

/// Wheel over GRAPH or TIMELINE (or the dialog). True when handled.
pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    if app.gitui.dlg.is_some() {
        commitdlg::wheel(app, p, d);
        return true;
    }
    let which = if app.gitui.hits.graph.contains(p) {
        0
    } else if app.gitui.hits.timeline.contains(p) {
        1
    } else {
        return false;
    };
    let cur = scroll_of(app, which);
    set_scroll(app, which, (cur as isize + d).max(0) as usize);
    true
}

fn list_len(app: &App, which: u8) -> (usize, usize) {
    match which {
        0 => (graph_rows(&app.gitui.graph).len(), app.gitui.hits.graph.height as usize),
        _ => (timeline_rows(app), app.gitui.hits.timeline.height as usize),
    }
}

fn list_mut(app: &mut App, which: u8) -> &mut List {
    if which == 0 {
        &mut app.gitui.graph.list
    } else {
        &mut app.gitui.timeline.list
    }
}

/// `ScrollTarget::Git(which)`: 0 = GRAPH, 1 = TIMELINE.
pub fn scroll_of(app: &App, which: u8) -> usize {
    if which == 0 {
        app.gitui.graph.list.off
    } else {
        app.gitui.timeline.list.off
    }
}

pub fn set_scroll(app: &mut App, which: u8, off: usize) {
    let (n, h) = list_len(app, which);
    list_mut(app, which).off = off.min(n.saturating_sub(h));
}

fn timeline_rows(app: &App) -> usize {
    let t = &app.gitui.timeline;
    t.items.len() + usize::from(t.more)
}

/// Keys while the Source Control list has focus. True when consumed; CHANGES
/// keys other than Tab fall through to the generic sidebar keys.
pub fn key(app: &mut App, k: KeyEvent) -> bool {
    let gu = &mut app.gitui;
    // Tab / Shift+Tab cycle between the CHANGES and GRAPH lists; Esc leaves for the editor.
    if !gu.graph_focus {
        // With GRAPH collapsed, Tab falls through to the sidebar's own (to the editor).
        if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) && gu.graph_open {
            gu.graph_focus = true;
            return true;
        }
        if k.code == KeyCode::Char('r') {
            load_graph(app, true);
        }
        return false;
    }
    let n = graph_rows(&gu.graph).len();
    let page = (gu.hits.graph.height as isize).max(1);
    let mv = |gu: &mut GitUi, d: isize| {
        let l = &mut gu.graph.list;
        l.sel = (l.sel as isize + d).clamp(0, n.saturating_sub(1) as isize) as usize;
        let h = (gu.hits.graph.height as usize).max(1);
        if l.sel < l.off {
            l.off = l.sel;
        } else if l.sel >= l.off + h {
            l.off = l.sel + 1 - h;
        }
    };
    let sel = gu.graph.list.sel;
    let row_rect = {
        let g = gu.hits.graph;
        Rect::new(g.x, g.y + sel.saturating_sub(gu.graph.list.off) as u16, g.width, 1)
    };
    match k.code {
        KeyCode::Up => mv(gu, -1),
        KeyCode::Down => mv(gu, 1),
        KeyCode::PageUp => mv(gu, -page),
        KeyCode::PageDown => mv(gu, page),
        KeyCode::Home => mv(gu, -(n as isize)),
        KeyCode::End => mv(gu, n as isize),
        KeyCode::Enter => activate_graph(app, sel, true),
        KeyCode::Char(' ') => activate_graph(app, sel, false),
        KeyCode::Left | KeyCode::Right => {
            let rows = graph_rows(&gu.graph);
            let ci = match rows.get(sel) {
                Some(GRow::Commit(ci)) | Some(GRow::File(ci, _)) => *ci,
                _ => return true,
            };
            let open = gu.graph.open.as_ref().is_some_and(|o| o.0 == ci);
            if open == (k.code == KeyCode::Left) {
                if let Some(at) = rows.iter().position(|r| *r == GRow::Commit(ci)) {
                    gu.graph.list.sel = at;
                }
                toggle_commit(app, ci);
            }
        }
        KeyCode::Char('i') => {
            if let Some(GRow::Commit(ci)) = graph_rows(&gu.graph).get(sel) {
                let hash = gu.graph.commits[*ci].hash.clone();
                open_card(app, hash, row_rect);
            }
        }
        KeyCode::Char('r') => {
            load_graph(app, true);
            app.refresh();
        }
        KeyCode::Tab | KeyCode::BackTab => gu.graph_focus = !gu.changes_open,
        KeyCode::Esc if gu.card.is_some() => gu.card = None,
        KeyCode::Esc => {
            gu.graph_focus = false;
            app.focus = Focus::Editor;
        }
        _ => {}
    }
    true
}

/// `PAct::Git` from the app's prompt.
pub fn run_act(app: &mut App, a: GitAct) {
    match a {
        GitAct::StageAllCommit { push } => commitdlg::run_commit(app, push, true),
        GitAct::Run(op) => crate::gitcmds::run_op(app, op),
        GitAct::SyncAlways => crate::gitcmds::sync_always(app),
        GitAct::QuitWait => {
            app.gitui.quit_when_idle = true;
            app.msg = "Waiting for git to finish, then quitting…".into();
        }
        GitAct::QuitAnyway => {
            app.gitui.quit_anyway = true;
            app.request_quit();
        }
        GitAct::SaveStage(key) => {
            if let Err(e) = app.save_doc(&key).and_then(|_| app.stage_peek_hunk()) {
                app.msg = e;
            }
        }
    }
}

// ---------------------------------------------------------------- drawing

/// A pane header; `focused`: its list has the keys. Cards show that on their border;
/// the other styles (no focused border) colour the header `focusBorder`.
#[allow(clippy::too_many_arguments)]
fn pane_header(app: &mut App, f: &mut Frame, r: Rect, title: &str, open: bool, hit: Hit, extra: &str, focused: bool) {
    if r.height == 0 {
        return;
    }
    let s = app.settings.spacing;
    let ic = icons();
    fill(f, r, P.section_bg);
    let chev = if open { ic.chev_down } else { ic.chev_right };
    let fg = if focused && app.settings.blocks != crate::blocks::BlockStyle::Cards { P.focus_border } else { P.section_fg };
    let pad = s.sidebar_padding.saturating_sub(1) / 2;
    let gap = crate::blocks::icon_gap(s.icon_gap) / 2;
    let ir = inset(r, pad);
    // As in the rows: cut once with "…"; the dim file name only while the title is whole.
    let head = format!(" {chev}{}{title}", sp(gap));
    let extra = if extra.is_empty() { String::new() } else { format!(" {extra}") };
    let (head, extra) = fit_pair(&head, &extra, ir.width as usize);
    row(f, ir, P.section_bg, vec![Span::styled(head, Style::new().fg(fg).bold()), Span::styled(extra, Style::new().fg(P.dim))]);
    app.gitui.hits.areas.push((r, hit));
}

/// The Source Control sidebar below its title: CHANGES over GRAPH.
pub fn draw_scm(app: &mut App, f: &mut Frame, r: Rect) {
    let s = app.settings.spacing;
    if app.git_root.is_none() {
        // VS Code's Source Control view for a folder without a repository.
        let msg = "The folder currently open doesn't have a git repository.";
        let r = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, app.focus == Focus::Side);
        let area = inset(r, (s.sidebar_padding.saturating_sub(1) / 2).max(1));
        let p = ratatui::widgets::Paragraph::new(msg).wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(p.style(Style::new().fg(P.sidebar_fg)), area);
        app.ui.side_list = Rect::default();
        return;
    }
    let (co, go) = (app.gitui.changes_open, app.gitui.graph_open);
    // CHANGES and GRAPH are two blocks: cards (sharing one border row), or a shared line.
    let st = app.settings.blocks;
    let between = between(st);
    let frame = if st == crate::blocks::BlockStyle::Cards { 2 } else { 0 };
    let body = r.height.saturating_sub(2 + between + if frame > 0 { 2 } else { 0 });
    let ch = match (co, go) {
        (true, true) => {
            let lo = body.min(3);
            // Nothing to commit: CHANGES keeps its button and header, GRAPH the rest.
            let fit = app.scm_files.is_empty().then_some(2);
            app.gitui.split.or(fit).unwrap_or(body / 2).clamp(lo, body.saturating_sub(3).max(lo))
        }
        (true, false) => body,
        _ => 0,
    };
    let gh = if go { body - ch } else { 0 };
    let keys = app.focus == Focus::Side;
    let gf = app.gitui.graph_focus;
    // CHANGES block (with its top frame), the line row (lines), then the GRAPH block; cards
    // `stack` onto the GRAPH card's top edge.
    let (h1, sep) = (1 + ch + frame / 2, u16::from(st == crate::blocks::BlockStyle::Lines));
    let gy = (r.y + h1 + sep).min(r.bottom());
    let gbot = Rect::new(r.x, gy, r.width, r.bottom() - gy);
    let ctop = Rect::new(r.x, r.y, r.width, (h1 + sep).min(r.height));
    let (ctop, gbot) = crate::blocks::stack(st, ctop, gbot, r.x.saturating_sub(1), r.right());
    let ci = crate::blocks::card(st, f, ctop, P.sidebar_bg, keys && !gf);
    let gi = crate::blocks::card(st, f, gbot, P.sidebar_bg, keys && gf);
    crate::blocks::seam(st, f, ctop, gbot, keys && !gf);
    // The line between is the splitter, lit while hovered or dragged.
    // Cards: only above a framed GRAPH card (a tiny one falls back to unframed).
    if co && go && (st == crate::blocks::BlockStyle::Lines || gi.y > gbot.y) && gi.y > r.y {
        let sash = Rect::new(gi.x, gi.y - 1, gi.width, 1);
        app.gitui.hits.areas.push((sash, Hit::GraphSash));
        if app.gitui.drag.is_some_and(|d| d.2 == 0) || app.hover.is_some_and(|p| sash.contains(p)) {
            crate::blocks::hot(sash);
        }
    }
    let chdr = Rect::new(ci.x, ci.y, ci.width, 1).intersection(ci);
    let crect = Rect::new(ci.x, ci.y + 1, ci.width, ch).intersection(ci);
    let ghdr = Rect::new(gi.x, gi.y, gi.width, 1).intersection(gi);
    let grect = Rect::new(gi.x, gi.y + 1, gi.width, gh).intersection(gi);
    pane_header(app, f, chdr, "WORKING TREE", co, Hit::ChangesHead, "", keys && !gf);
    app.gitui.hits.changes = crect;
    app.ui.side_list = Rect::default();
    if crect.height > 0 {
        let m = (s.sidebar_padding.saturating_sub(1) / 2).min(crect.width / 2);
        let btn = Rect::new(crect.x + m, crect.y, crect.width.saturating_sub(2 * m), 1);
        let label = format!("{}{}Add commit", icons().check, sp(crate::blocks::icon_gap(s.icon_gap) / 2));
        let bg = if app.hover.is_some_and(|p| btn.contains(p)) { P.button_hover } else { P.button_bg };
        fill(f, btn, bg);
        let bx = btn.x + btn.width.saturating_sub(label.width() as u16) / 2;
        f.render_widget(Span::styled(fit(&label, btn.width as usize), Style::new().fg(P.button_fg).bg(bg)), Rect::new(bx, btn.y, btn.right().saturating_sub(bx), 1));
        crate::ui::round_ends(f, btn, bg, P.sidebar_bg);
        app.ui.commit_btn = btn;
        // The list starts right under the button, no blank row between them.
        let top = 1.min(crect.height);
        changes_list(app, f, Rect::new(crect.x, crect.y + top, crect.width, crect.height - top));
    }
    let n = app.gitui.graph.commits.len();
    let extra = if n > 0 { format!("{n}{}", if app.gitui.graph.more { "+" } else { "" }) } else { String::new() };
    pane_header(app, f, ghdr, "GRAPH", go, Hit::GraphHead, &extra, keys && gf);
    app.gitui.hits.graph = grect;
    if grect.height > 0 {
        graph_list(app, f, grect);
    }
}

fn list_bg(selected: bool, focused: bool, hovered: bool) -> Color {
    match (selected, focused, hovered) {
        (true, true, _) => P.list_active_bg,
        (true, false, _) => P.list_inactive_bg,
        (false, _, true) => P.list_hover_bg,
        _ => P.sidebar_bg,
    }
}

/// CHANGES rows on the sidebar's generic list state.
fn changes_list(app: &mut App, f: &mut Frame, list: Rect) {
    let mut lay = app.settings.spacing;
    lay.sidebar_padding = lay.sidebar_padding.saturating_sub(1) / 2;
    app.ui.side_list = list;
    let s = Side::Scm as usize;
    let rows = app.scm_rows();
    let n = rows.len();
    let step = 1 + lay.list_gap;
    let h = (list.height as usize).div_ceil(step as usize);
    app.ui.side_rows = h;
    app.ui.side_gap = lay.list_gap;
    app.sel[s] = app.sel[s].min(n.saturating_sub(1));
    app.off[s] = app.off[s].min(n.saturating_sub(1));
    let hover = app.hover.filter(|p| list.contains(*p)).and_then(|p| app.ui.side_item(p.y, app.off[s]));
    let focused = app.focus == Focus::Side && !app.gitui.graph_focus;
    for k in 0..h {
        let i = app.off[s] + k;
        if i >= n {
            break;
        }
        let rr = Rect::new(list.x, list.y + k as u16 * step, list.width, 1);
        let bg = if matches!(rows[i], SRow::Gap) { P.sidebar_bg } else { list_bg(i == app.sel[s], focused, hover == Some(i)) };
        fill(f, rr, bg);
        let hot = i == app.sel[s] || hover == Some(i);
        scm_row(app, f, inset(rr, lay.sidebar_padding), bg, &rows[i], hot);
    }
    // Sticky scroll: the Merge / Staged / Changes header over its files.
    crate::ui::sticky_rows(app, f, list, lay.sidebar_padding, lay.list_gap, &rows, &[]);
    if n > h && h > 0 {
        let track = Rect::new(list.right() - 1, list.y, 1, list.height);
        scrollbar(app, f, track, ScrollTarget::Side, n, h, app.off[s], n - h);
    }
}

fn graph_list(app: &mut App, f: &mut Frame, list: Rect) {
    let pal = pal(app);
    let rows = graph_rows(&app.gitui.graph);
    let n = rows.len();
    let h = list.height as usize;
    let l = &mut app.gitui.graph.list;
    l.sel = l.sel.min(n.saturating_sub(1));
    l.off = l.off.min(n.saturating_sub(h));
    let (sel, off) = (l.sel, l.off);
    let pad = app.settings.spacing.sidebar_padding.saturating_sub(1) / 2;
    if n == 0 {
        let t = if app.gitui.graph.loading || !app.gitui.graph.loaded { "Loading…" } else { "No commits yet" };
        row(f, inset(Rect::new(list.x, list.y, list.width, 1), pad), P.sidebar_bg, vec![Span::styled(format!(" {t}"), Style::new().fg(P.dim))]);
        return;
    }
    let scroll = n > h;
    let hover = app.hover.filter(|p| list.contains(*p)).map(|p| off + (p.y - list.y) as usize);
    let focused = app.focus == Focus::Side && app.gitui.graph_focus;
    for (k, gr) in rows.iter().enumerate().skip(off).take(h) {
        let y = list.y + (k - off) as u16;
        let rr = Rect::new(list.x, y, list.width.saturating_sub(u16::from(scroll)), 1);
        let hot = k == sel || hover == Some(k);
        let bg = list_bg(k == sel, focused, hover == Some(k));
        fill(f, rr, bg);
        let ir = inset(rr, pad);
        app.gitui.hits.areas.push((rr, Hit::Graph(k)));
        match *gr {
            GRow::Commit(ci) => commit_row(app, f, ir, bg, ci, hot, &pal),
            GRow::File(ci, j) => {
                let indent = app.gitui.graph.rows[ci].cells.len().min(ir.width as usize / 2) + 3;
                let Some(fc) = app.gitui.graph.open.as_ref().and_then(|o| o.1.as_ref()).and_then(|v| v.get(j)).cloned() else { continue };
                file_row(app, f, ir, bg, indent, &fc);
            }
            GRow::More => {
                let t = if app.gitui.graph.loading { "Loading…".to_string() } else { format!("{}View more", sp(1)) };
                row(f, ir, bg, vec![Span::raw(" ".repeat(3)), Span::styled(t, Style::new().fg(P.focus_border))]);
            }
        }
    }
    if scroll {
        let track = Rect::new(list.right() - 1, list.y, 1, list.height);
        scrollbar(app, f, track, ScrollTarget::Git(0), n, h, off, n - h);
    }
}

fn commit_row(app: &mut App, f: &mut Frame, r: Rect, bg: Color, ci: usize, hot: bool, pal: &Pal) {
    let g = &app.gitui.graph;
    let c = &g.commits[ci];
    let head = c.refs.iter().any(|x| matches!(x, RefLabel::Current(_) | RefLabel::Head));
    let maxg = (r.width / 2) as usize;
    let mut spans = vec![Span::raw(" ")];
    for cell in g.rows[ci].cells.iter().take(maxg) {
        let glyph = if cell.glyph == '●' && !head { '○' } else { cell.glyph };
        let glyph = if crate::theme::ascii() && matches!(glyph, '●' | '○') { if head { '*' } else { 'o' } } else { glyph };
        spans.push(Span::styled(glyph.to_string(), Style::new().fg(pal.lane(cell.color))));
    }
    spans.push(Span::raw(sp(crate::blocks::icon_gap(app.settings.spacing.icon_gap) / 2)));
    // The subject keeps at least ~20 cells and 2 blank ones after it; the badges get the
    // rest before they collapse into +N, then go.
    let info = if hot { 3 } else { 0 };
    let keep = c.subject.width().min(SUBJECT_MIN);
    let avail = (r.width as usize).saturating_sub(width(&spans) as usize);
    let mut rs = badges(&c.refs, pal, avail.saturating_sub(keep + 2 + info));
    if hot {
        // A blank cell after it: a Nerd Font glyph often draws wider than its one cell
        // and would spill onto the scrollbar.
        rs.push(Span::styled(format!(" {} ", info_icon()), Style::new().fg(P.icon_fg)));
    }
    let mut st = Style::new().fg(P.sidebar_fg);
    if head {
        st = st.bold();
    }
    let (subject, author) = fit_pair(&c.subject, &format!(" {}", c.author_name), room_before(&rs, avail));
    spans.push(Span::styled(subject, st));
    spans.push(Span::styled(author, Style::new().fg(P.dim)));
    let hash = c.hash.clone();
    row(f, r, bg, spans);
    if !rs.is_empty() {
        right(f, r, rs);
    }
    app.gitui.hits.hovers.push((r, hash));
    if hot {
        app.gitui.hits.areas.push((Rect::new(r.right().saturating_sub(2), r.y, 1, 1), Hit::Info(ci)));
    }
}

fn file_row(app: &App, f: &mut Frame, r: Rect, bg: Color, indent: usize, fc: &FileChange) {
    let (glyph, gfg) = crate::icons::file_icon(base(&fc.path), &app.settings.file_icons);
    let g = crate::blocks::icon_gap(app.settings.spacing.icon_gap) / 2;
    let dir = fc.path.rsplit_once('/').map_or(String::new(), |(d, _)| format!(" {d}"));
    let letter = vec![Span::styled(format!(" {} ", fc.status), Style::new().fg(git_color(fc.status)))];
    let room = room_before(&letter, (r.width as usize).saturating_sub(indent + 1 + g as usize));
    let (name, dir) = fit_pair(base(&fc.path), &dir, room);
    let mut name_st = Style::new().fg(git_color(fc.status));
    if fc.status == 'D' {
        name_st = name_st.add_modifier(Modifier::CROSSED_OUT);
    }
    row(
        f,
        r,
        bg,
        vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(glyph, Style::new().fg(gfg)),
            Span::raw(sp(g)),
            Span::styled(name, name_st),
            Span::styled(dir, Style::new().fg(P.dim)),
        ],
    );
    right(f, r, letter);
}

/// The Explorer split: the tree above, TIMELINE (header, and list when open) below.
pub fn split_timeline(app: &App, r: Rect) -> (Rect, Rect) {
    // A card's frame takes 2 rows: the pane keeps its rows inside it.
    let frame = if app.settings.blocks == crate::blocks::BlockStyle::Cards { 2 } else { 0 };
    let min = if app.gitui.timeline.open { 4 } else { 1 };
    if r.height < frame + min + 8 {
        return (r, Rect::default());
    }
    let h = frame + if app.gitui.timeline.open {
        app.gitui.timeline_height.unwrap_or((r.height / 3).clamp(4, 14)).clamp(4, r.height - frame - 8)
    } else { 1 };
    (Rect::new(r.x, r.y, r.width, r.height - h), Rect::new(r.x, r.bottom() - h, r.width, h))
}

pub fn draw_timeline(app: &mut App, f: &mut Frame, r: Rect) {
    sync_timeline(app);
    fill(f, r, P.sidebar_bg);
    let name = app.gitui.timeline.path.as_deref().map(base).unwrap_or("").to_string();
    let open = app.gitui.timeline.open;
    if open && r.y > app.ui.side_list.bottom() {
        let sash = Rect::new(r.x, r.y - 1, r.width, 1);
        app.gitui.hits.areas.push((sash, Hit::TimelineSash));
        if app.gitui.timeline_drag.is_some() || app.hover.is_some_and(|p| sash.contains(p)) {
            crate::blocks::hot(sash);
        }
    }
    pane_header(app, f, Rect::new(r.x, r.y, r.width, 1), "TIMELINE", open, Hit::TimelineHead, &name, false);
    let list = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    app.gitui.hits.timeline = list;
    if !open || list.height == 0 {
        return;
    }
    let s = app.settings.spacing;
    let pad = s.sidebar_padding.saturating_sub(1) / 2;
    let gap = sp(crate::blocks::icon_gap(s.icon_gap) / 2);
    let dot = if crate::theme::ascii() { "o" } else { "\u{eafc}" };
    // Reserve Explorer's empty fold column so icons and labels share its grid.
    let lead = format!("  {gap}{dot}{gap}");
    let t = &app.gitui.timeline;
    let n = timeline_rows(app);
    if n == 0 {
        let msg = match (&t.path, t.loading) {
            (None, _) => "The active editor cannot provide timeline information.",
            (Some(_), true) => "Loading…",
            (Some(_), false) => "No timeline information.",
        };
        row(f, inset(Rect::new(list.x, list.y, list.width, 1), pad), P.sidebar_bg, vec![Span::styled(fit(&format!(" {msg}"), list.width as usize), Style::new().fg(P.dim))]);
        return;
    }
    let h = list.height as usize;
    let l = &mut app.gitui.timeline.list;
    l.off = l.off.min(n.saturating_sub(h));
    let (sel, off) = (l.sel, l.off);
    let scroll = n > h;
    let hover = app.hover.filter(|p| list.contains(*p)).map(|p| off + (p.y - list.y) as usize);
    let now = now();
    for k in off..(off + h).min(n) {
        let rr = Rect::new(list.x, list.y + (k - off) as u16, list.width.saturating_sub(u16::from(scroll)), 1);
        let bg = list_bg(k == sel, false, hover == Some(k));
        fill(f, rr, bg);
        let ir = inset(rr, pad);
        let Some(it) = app.gitui.timeline.items.get(k) else {
            let t = if app.gitui.timeline.loading { "Loading…" } else { "Load more" };
            row(f, ir, bg, vec![Span::styled(format!("{}{t}", sp(lead.width() as u16)), Style::new().fg(P.focus_border))]);
            app.gitui.hits.areas.push((rr, Hit::TimelineMore));
            continue;
        };
        // The time gives way first: "1 hour ago" while the subject keeps ~20 cells, then
        // "1h" while it keeps 12, then none.
        let (avail, sw, t) = ((ir.width as usize).saturating_sub(lead.width()), it.commit.subject.width(), it.commit.author_time);
        let rs = [(ago(t, now), SUBJECT_MIN), (ago_short(t, now), SUBJECT_FLOOR)]
            .into_iter()
            .map(|(s, keep)| (vec![Span::styled(format!("{s} "), Style::new().fg(P.dim))], keep))
            .find(|(m, keep)| room_before(m, avail) >= sw.min(*keep))
            .map_or(Vec::new(), |(m, _)| m);
        let (subject, author) = fit_pair(&it.commit.subject, &format!(" {}", it.commit.author_name), room_before(&rs, avail));
        row(f, ir, bg, vec![Span::styled(lead.clone(), Style::new().fg(P.icon_fg)), Span::styled(subject, Style::new().fg(P.sidebar_fg)), Span::styled(author, Style::new().fg(P.dim))]);
        right(f, ir, rs);
        let hash = it.commit.hash.clone();
        app.gitui.hits.areas.push((rr, Hit::Timeline(k)));
        app.gitui.hits.hovers.push((rr, hash));
    }
    if scroll {
        let track = Rect::new(list.right() - 1, list.y, 1, list.height);
        scrollbar(app, f, track, ScrollTarget::Git(1), n, h, off, n - h);
    }
}

/// Popups over everything but the app's prompt: the card, the commit dialog.
pub fn draw_popups(app: &mut App, f: &mut Frame, area: Rect) {
    draw_card(app, f, area);
    if app.gitui.dlg.is_some() {
        commitdlg::draw(app, f, area);
    }
    crate::gitcmds::draw_popups(app, f, area);
}

fn card_lines(c: &Card, pal: &Pal, w: usize) -> (Vec<Line<'static>>, Option<usize>) {
    let st = |c: Color| Style::new().fg(c);
    let h = match &c.hover {
        None => return (vec![Line::styled("Loading…", st(P.dim))], None),
        Some(Err(e)) => return (vec![Line::styled(fit(e, w), st(P.error_fg))], None),
        Some(Ok(h)) => h,
    };
    let cm = &h.commit;
    let mut v = vec![
        Line::from(vec![
            Span::styled(fit(&cm.author_name, w), st(P.widget_fg).bold()),
            Span::styled(fit(&format!(" <{}>", cm.author_email), w.saturating_sub(cm.author_name.width())), st(P.dim)),
        ]),
        Line::styled(fit(&format!("{} ({})", ago(cm.author_time, now()), local_time(cm.author_time)), w), st(P.dim)),
        Line::raw(""),
        Line::styled(fit(&cm.subject, w), st(P.widget_fg).bold()),
    ];
    if !cm.body.is_empty() {
        v.push(Line::raw(""));
        let body: Vec<&str> = cm.body.lines().collect();
        for l in body.iter().take(12) {
            v.push(Line::styled(fit(l, w), st(P.widget_fg)));
        }
        if body.len() > 12 {
            v.push(Line::styled("…", st(P.dim)));
        }
    }
    v.push(Line::raw(""));
    let plural = |n: u64, one: &str| if n == 1 { one.to_string() } else { format!("{one}s") };
    v.push(Line::from(vec![
        Span::styled(format!("{} {} changed, ", h.files, plural(h.files as u64, "file")), st(P.widget_fg)),
        Span::styled(format!("{} {}(+)", h.insertions, plural(h.insertions, "insertion")), st(pal.added)),
        Span::styled(", ", st(P.widget_fg)),
        Span::styled(format!("{} {}(-)", h.deletions, plural(h.deletions, "deletion")), st(pal.deleted)),
    ]));
    if !cm.refs.is_empty() {
        let mut b = badges(&cm.refs, pal, w);
        if !b.is_empty() {
            b.remove(0);
        }
        v.push(Line::from(b));
    }
    v.push(Line::raw(""));
    let at = v.len();
    v.push(Line::raw(""));
    (v, Some(at))
}

fn draw_card(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(c) = &app.gitui.card else { return };
    let pal = pal(app);
    let sb = app.ui.sidebar;
    let room_right = area.right().saturating_sub(sb.right() + 1);
    let x = if room_right >= 40 { sb.right() + 1 } else { area.x + 1 };
    let w = 64.min(area.right().saturating_sub(x + 1));
    if w < 20 || area.height < 6 {
        return;
    }
    let (lines, btn_row) = card_lines(c, &pal, (w - 4) as usize);
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let y = c.anchor.y.min(area.bottom().saturating_sub(h + 1)).max(area.y + 1);
    let r = Rect::new(x, y, w, h).intersection(area);
    f.render_widget(Clear, r);
    let block = popup_block(P.widget_bg, P.widget_fg, (1, 0));
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(ratatui::widgets::Paragraph::new(lines), inner);
    app.gitui.hits.card = r;
    let Some(k) = btn_row else { return };
    let by = inner.y + k as u16;
    if by >= inner.bottom() {
        return;
    }
    let short = &c.hash[..c.hash.len().min(7)];
    let bp = sp(app.settings.spacing.button_padding.min(1));
    let mut x = inner.x;
    f.render_widget(Span::styled(short.to_string(), Style::new().fg(P.dim)), Rect::new(x, by, 8.min(inner.width), 1));
    x += 9;
    for (label, hit) in [("Copy hash", Hit::CardCopy), ("Open changes", Hit::CardOpen)] {
        let t = format!("{bp}{label}{bp}");
        let br = Rect::new(x, by, t.width() as u16, 1).intersection(inner);
        if br.is_empty() {
            break;
        }
        f.render_widget(Span::styled(t.clone(), Style::new().fg(P.button2_fg).bg(P.button2_bg)), br);
        app.gitui.hits.areas.push((br, hit));
        x += t.width() as u16 + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Kind, Row};

    #[test]
    fn relative_dates_read_like_vscode() {
        assert_eq!(ago(100, 110), "now");
        assert_eq!(ago(0, 60), "1 minute ago");
        assert_eq!(ago(0, 7200), "2 hours ago");
        assert_eq!(ago(0, 3 * 86_400), "3 days ago");
        assert_eq!(ago(0, 14 * 86_400), "2 weeks ago");
        assert_eq!(ago(0, 400 * 86_400), "1 year ago");
        assert_eq!(local_time(0).len(), 16);
    }

    #[test]
    fn f7_walks_change_blocks_and_wraps() {
        let r = |kind| Row { left: None, right: None, kind };
        let rows = [r(Kind::Same), r(Kind::Change), r(Kind::Change), r(Kind::Same), r(Kind::Ins), r(Kind::Same)];
        assert_eq!(next_change(&rows, None, 1), Some(1));
        assert_eq!(next_change(&rows, Some(1), 1), Some(4));
        assert_eq!(next_change(&rows, Some(4), 1), Some(1), "wraps");
        assert_eq!(next_change(&rows, Some(4), -1), Some(1));
        assert_eq!(next_change(&rows, None, -1), Some(4));
        assert_eq!(next_change(&rows[..1], None, 1), None);
    }

    #[test]
    fn badges_fit_their_budget() {
        let refs = [RefLabel::Current("main".into()), RefLabel::Remote("origin/main".into()), RefLabel::Tag("v1".into())];
        let p = Pal {
            lanes: [Color::Reset; 5],
            ref_local: Color::Blue,
            ref_remote: Color::Magenta,
            label_fg: Color::White,
            added: Color::Green,
            deleted: Color::Red,
        };
        let all: String = badges(&refs, &p, 80).iter().map(|s| s.content.to_string()).collect();
        assert!(all.contains("main") && all.contains("origin/main") && all.contains("v1"), "{all}");
        let few = badges(&refs, &p, 14);
        assert!(width(&few) <= 14, "{few:?}");
        assert!(few.iter().any(|s| s.content.contains('+')), "{few:?}");
        // The current branch stays visible however the refs are ordered or narrow the room.
        let mixed = [RefLabel::Remote("origin/main".into()), RefLabel::Tag("v1".into()), RefLabel::Current("main".into())];
        for budget in [12, 20] {
            let b = badges(&mixed, &p, budget);
            let text: String = b.iter().map(|s| s.content.to_string()).collect();
            assert!(text.contains("main") && !text.starts_with(" +"), "{budget}: {text:?}");
            assert!(usize::from(width(&b)) <= budget, "{budget}: {text:?}");
        }
    }
}

/// TestBackend runs of the Source Control view, the Timeline, the card and the
/// commit dialog, driven by keys and mouse events at the drawn positions.
#[cfg(test)]
pub(crate) mod ui_tests {
    use super::*;
    use crate::panel::ScmAct;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(crate) struct Tmp(pub(crate) PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `<tmp>/r`: a repo on main (hooks and signing off), after `script`.
    pub(crate) fn repo(tag: &str, script: &str) -> (Tmp, PathBuf) {
        let t = Tmp(std::env::temp_dir().join(format!("herdr-code-scm-{tag}-{}", std::process::id())));
        let _ = fs::remove_dir_all(&t.0);
        fs::create_dir_all(&t.0).unwrap();
        let init = "git init -q -b main r; cd r; git config user.name Tester; git config user.email t@example.com; \
                    git config commit.gpgsign false; git config core.hooksPath /dev/null";
        sh(&t.0, &format!("{init}; {script}"));
        let r = t.0.join("r").canonicalize().unwrap();
        (t, r)
    }

    pub(crate) fn sh(dir: &Path, script: &str) {
        let o = Command::new("sh").arg("-ec").arg(script).current_dir(dir).output().unwrap();
        assert!(o.status.success(), "{script}\n{}", String::from_utf8_lossy(&o.stderr));
    }

    pub(crate) fn app(t: &Tmp, root: &Path) -> App {
        App::new(root.to_path_buf(), Some(t.0.join("state/layout.json")))
    }

    pub(crate) fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// The cell where `needle` starts on screen.
    pub(crate) fn at(t: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
        let b = t.backend().buffer();
        for y in 0..b.area.height {
            let cells: Vec<&str> = (0..b.area.width).map(|x| b[(x, y)].symbol()).collect();
            let line: String = cells.concat();
            if let Some(i) = line.find(needle) {
                // Byte offset to cell: count cells whose symbols precede it.
                let mut acc = 0;
                for (x, c) in cells.iter().enumerate() {
                    if acc >= i {
                        return (x as u16, y);
                    }
                    acc += c.len();
                }
            }
        }
        panic!("{needle:?} not on screen");
    }

    pub(crate) fn mouse(app: &mut App, kind: MouseEventKind, (x, y): (u16, u16)) {
        app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
    }

    pub(crate) fn click(app: &mut App, p: (u16, u16)) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), p);
        mouse(app, MouseEventKind::Up(MouseButton::Left), p);
    }

    pub(crate) fn key(app: &mut App, code: KeyCode, m: KeyModifiers) {
        app.on_key(KeyEvent::new(code, m));
    }

    pub(crate) fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            key(app, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    /// Let background git jobs finish and fold them in.
    pub(crate) fn settle(app: &mut App) {
        for _ in 0..500 {
            app.drain();
            if !app.gitui.busy() && app.loading.is_empty() {
                app.drain();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("git jobs never finished");
    }

    pub(crate) fn term() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(120, 40)).unwrap()
    }

    /// Sidebars of 30, 36 and 50 columns (GRAPH, TIMELINE, Changes, search results): a
    /// row's text is cut with one "…" and two blank cells follow it before any right-
    /// aligned meta; a TIMELINE time shortens to "1h" first.
    #[test]
    fn narrow_rows_cut_once_and_keep_a_gap_before_the_meta() {
        let file = "src/a/rather/deep/folder/with_a_long_file_name.rs";
        let (t, r) = repo(
            "narrow",
            &format!(
                "mkdir -p src/a/rather/deep/folder; echo needle > {file}; git add .; \
                 GIT_AUTHOR_DATE=\"@$(($(date +%s) - 3700)) +0000\" git commit -qm 'herdr-code: tighter rows for the narrow sidebar layout'; \
                 git tag v1.0.0; git branch feature-with-a-long-name; echo needle again >> {file}"
            ),
        );
        let mut app = app(&t, &r);
        let mut tm = term();
        app.open_file(0, file, false);
        app.query = "needle".into();
        app.start_search();
        for w in [30u16, 36, 50] {
            app.side_w = Some(w);
            let mut seen = String::new();
            for side in [Side::Explorer, Side::Scm, Side::Search] {
                app.show_side(side, false);
                // Drawing asks for the graph and the timeline: draw, let them land, twice.
                for _ in 0..2 {
                    screen(&mut app, &mut tm);
                    for _ in 0..300 {
                        if app.search.is_none() && app.queued_search.is_none() {
                            break;
                        }
                        app.drain();
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    settle(&mut app);
                }
                let s = screen(&mut app, &mut tm);
                let sb = app.ui.sidebar;
                for row in s.lines().skip(sb.y as usize).take(sb.height as usize) {
                    let row: String = row.chars().skip(sb.x as usize).take(sb.width as usize).collect();
                    // The main label keeps its gap; ref badges may abbreviate independently.
                    for (i, _) in row.match_indices('…').take(1) {
                        let next: Vec<char> = row[i + '…'.len_utf8()..].chars().take(2).collect();
                        assert!(next.iter().all(|&c| c == ' ' || ('\u{2500}'..='\u{257f}').contains(&c)), "width {w}, {side:?}: {row:?}");
                    }
                    seen += &row;
                    seen.push('\n');
                }
            }
            assert!(seen.contains("herdr-code: tight") && seen.contains("with_a_long") && seen.contains("needle again"), "width {w}:\n{seen}");
            let hdr = seen.lines().find(|l| l.contains("TIMELINE")).unwrap_or("");
            assert!(hdr.contains("with_a_long_file_name.rs") || hdr.contains("with_a_lo") && hdr.contains('…'), "width {w}: header {hdr:?}");
            let time = if w == 50 { "  1 hour ago" } else { "  1h" };
            assert!(seen.contains(time), "width {w}: TIMELINE shows {time:?}\n{seen}");
        }
    }

    #[test]
    fn one_view_changes_over_graph_collapsible_and_split_persisted() {
        let (t, r) = repo("panes", "echo a > a.txt; git add .; git commit -qm first; echo b >> a.txt");
        let mut app = app(&t, &r);
        let mut tm = term();
        app.show_side(Side::Scm, false);
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("WORKING TREE") && s.contains("GRAPH") && s.contains("Add commit") && s.contains("first"), "{s}");
        assert_eq!(app.ui.activity.len(), 3, "no separate History item");
        assert!(!s.contains("HISTORY"));
        // CHANGES collapses and expands from its header.
        click(&mut app, at(&tm, "WORKING TREE"));
        let s = screen(&mut app, &mut tm);
        assert!(!s.contains("Add commit") && s.contains("first"), "{s}");
        click(&mut app, at(&tm, "WORKING TREE"));
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("Add commit"), "{s}");
        // Dragging the GRAPH header moves the split; it is kept in layout.json.
        let (gx, gy) = at(&tm, "GRAPH");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (gx, gy));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (gx, gy - 6));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (gx, gy - 6));
        screen(&mut app, &mut tm);
        assert_eq!(at(&tm, "GRAPH").1, gy - 6);
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(t.0.join("state/layout.json")).unwrap()).unwrap();
        let split = saved["scm"]["split"].as_u64().unwrap();
        let mut again = App::new(r.clone(), Some(t.0.join("state/layout.json")));
        assert_eq!(again.gitui.split, Some(split as u16));
        again.show_side(Side::Scm, false);
        settle(&mut again);
        screen(&mut again, &mut tm);
        assert_eq!(at(&tm, "GRAPH").1, gy - 6, "restored");
        // A click without a drag collapses GRAPH.
        click(&mut app, at(&tm, "GRAPH"));
        let s = screen(&mut app, &mut tm);
        assert!(!app.gitui.graph_open && !s.contains("first"), "{s}");
    }

    #[test]
    fn merge_changes_group_holds_conflicts_and_stages_them() {
        let (t, r) = repo(
            "merge",
            "echo base > f.txt; git add .; git commit -qm base; git checkout -q -b side; echo theirs > f.txt; git commit -qam theirs; \
             git checkout -q main; echo ours > f.txt; git commit -qam ours; git merge -q side || true; echo new > n.txt",
        );
        let mut app = app(&t, &r);
        let mut tm = term();
        app.show_side(Side::Scm, false);
        settle(&mut app);
        let rows = app.scm_rows();
        assert!(matches!(rows[0], SRow::MergeHead(1)), "Merge Changes first");
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("Merge Changes") && s.contains("Changes") && s.contains("n.txt"), "{s}");
        // Stage All Changes leaves the conflict alone; the Merge group's + resolves it.
        app.scm_action(ScmAct::StageAll);
        settle(&mut app);
        assert!(app.scm_files.iter().any(|f| f.letter == '!'));
        app.scm_action(ScmAct::StageMerge);
        settle(&mut app);
        assert!(!app.scm_files.iter().any(|f| f.letter == '!'), "{:?}", app.scm_files);
    }

    #[test]
    fn graph_view_more_expand_and_side_by_side_diff() {
        let (t, r) = repo(
            "graph",
            "i=1; while [ $i -le 150 ]; do echo $i > f.txt; git add f.txt; git commit -qm c$i; i=$((i+1)); done; git tag v1 HEAD~1",
        );
        let mut app = app(&t, &r);
        let mut tm = term();
        app.show_side(Side::Scm, false);
        settle(&mut app);
        assert_eq!((app.gitui.graph.commits.len(), app.gitui.graph.more), (100, true));
        assert_eq!(graph_rows(&app.gitui.graph).last(), Some(&GRow::More));
        // Nothing loads on scroll: only the "View more" row does.
        set_scroll(&mut app, 0, 10_000);
        let s = screen(&mut app, &mut tm);
        settle(&mut app);
        assert_eq!(app.gitui.graph.commits.len(), 100);
        assert!(s.contains("View more"), "{s}");
        click(&mut app, at(&tm, "View more"));
        settle(&mut app);
        assert_eq!((app.gitui.graph.commits.len(), app.gitui.graph.more), (150, false));
        // Lanes, badges and the HEAD branch on the first row.
        set_scroll(&mut app, 0, 0);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("● c150") && s.contains("main") && s.contains("v1"), "{s}");
        // Clicking the commit lists its files inline; clicking the file opens the diff.
        click(&mut app, at(&tm, "c150"));
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("f.txt"), "{s}");
        click(&mut app, at(&tm, "f.txt"));
        settle(&mut app);
        let key = app.groups[app.ag].tabs[app.groups[app.ag].active].key.clone();
        assert!(key.starts_with("cdiff:") && key.ends_with(":f.txt"), "{key}");
        let s = screen(&mut app, &mut tm);
        // In the diff editor only: the GRAPH list may show a c149 row above it.
        let ed = app.ui.groups[app.ag].area;
        let in_editor = |needle: &str| {
            let b = tm.backend().buffer();
            (ed.top()..ed.bottom())
                .find_map(|y| {
                    let line: String = (ed.left()..ed.right()).map(|x| b[(x, y)].symbol()).collect();
                    let i = line.find(needle)?;
                    Some((ed.x + line[..i].chars().count() as u16, y))
                })
                .unwrap_or_else(|| panic!("{needle:?} not in the editor: {s}"))
        };
        let (lx, ly) = in_editor("149");
        let (rx, ry) = in_editor("150 ");
        assert_eq!(ly, ry, "rows aligned");
        assert!(rx > lx + 20, "parent left, commit right: {s}");
        assert!(!s.contains("diff --git") && !s.contains("@@"), "no raw patch text: {s}");
        let del = tm.backend().buffer()[(lx, ly)].bg;
        assert_eq!(del, P.diff_del_text.over(P.diff_del_line));
        assert_eq!(tm.backend().buffer()[(rx, ry)].bg, P.diff_ins_text.over(P.diff_ins_line));
        key_f7(&mut app);
        let v = &app.groups[app.ag].tabs[app.groups[app.ag].active].view;
        assert_eq!(v.want_x, Some(0), "F7 jumped to the change");
        app.refresh();
        settle(&mut app);
        assert!(matches!(app.docs.get(&key), Some(Doc::Diff(d)) if d.title.contains("f.txt")), "refresh keeps commit diffs");
    }

    /// Tab / Shift+Tab cycle CHANGES ⇄ GRAPH (whatever had focus before), the focused
    /// header shows it, and Home/End/PgUp/PgDn work in both; End reaches View more.
    #[test]
    fn keyboard_cycles_panes_and_end_reaches_view_more() {
        let (t, r) = repo("keys", "i=1; while [ $i -le 130 ]; do echo $i > f.txt; git add f.txt; git commit -qm c$i; i=$((i+1)); done; echo x >> f.txt; echo y > y.txt");
        let mut app = app(&t, &r);
        let mut tm = term();
        app.show_side(Side::Scm, false);
        settle(&mut app);
        app.gitui.graph_focus = true; // left over from an earlier visit: Tab must not leave the sidebar
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert!(!app.gitui.graph_focus && app.focus == Focus::Side);
        screen(&mut app, &mut tm);
        // Cards show focus on the pane card's border only (the header keeps its colour).
        let (cx, cy) = at(&tm, "WORKING TREE");
        let sx = app.ui.sidebar.x;
        let edge = |tm: &Terminal<TestBackend>, y: u16| tm.backend().buffer()[(sx, y)].fg;
        assert_eq!(edge(&tm, cy), crate::blocks::border(true), "the focused pane's card");
        assert_eq!(tm.backend().buffer()[(cx, cy)].fg, P.section_fg, "no second focus colour");
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.sel[Side::Scm as usize], app.scm_rows().len() - 1, "End in CHANGES");
        key(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        assert!(app.gitui.graph_focus && app.focus == Focus::Side, "Shift+Tab cycles too");
        screen(&mut app, &mut tm);
        let (_, gy) = at(&tm, "GRAPH");
        assert_eq!((edge(&tm, gy), edge(&tm, cy)), (crate::blocks::border(true), crate::blocks::border(false)));
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert!(app.gitui.graph.list.sel > 1);
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.gitui.graph.list.sel, 0);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        let rows = graph_rows(&app.gitui.graph);
        assert_eq!(rows[app.gitui.graph.list.sel], GRow::More, "End lands on View more");
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("View more"), "and it is on screen: {s}");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut app);
        assert_eq!((app.gitui.graph.commits.len(), app.gitui.graph.more), (130, false));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, Focus::Editor, "Esc leaves for the editor");
        // GRAPH collapsed: Tab in CHANGES is not swallowed; it moves on to the editor.
        app.gitui.graph_open = false;
        app.gitui.graph_focus = false;
        app.focus = Focus::Side;
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.focus, Focus::Editor, "Tab falls through with GRAPH collapsed");
    }

    /// Badges take the room the subject leaves (it keeps ~20 cells): all of them
    /// when wide; when narrow, HEAD's stays and the rest collapse into +N.
    #[test]
    fn badges_use_the_free_room_before_collapsing() {
        let (t, r) = repo(
            "badgeroom",
            "cd ..; git init -q --bare -b main remote.git; cd r; git remote add origin ../remote.git; echo a > a.txt; git add .; \
             git commit -qm \"Merge branch 'feature'\"; git tag v1; git push -q -u origin main 2>/dev/null",
        );
        let mut tm = Terminal::new(TestBackend::new(200, 30)).unwrap();
        let row_of = |s: &str| s.lines().find(|l| l.contains("Merge branch")).unwrap_or("").to_string();
        let mut app = app(&t, &r);
        app.side_w = Some(90);
        app.show_side(Side::Scm, false);
        settle(&mut app);
        // The sidebar's columns of that row (cards, lines or none alike).
        let cols = |app: &App, row: String| row.chars().skip(app.ui.sidebar.x as usize).take(app.ui.sidebar.width as usize).collect::<String>();
        let row = row_of(&screen(&mut app, &mut tm));
        let side = cols(&app, row);
        assert!(side.contains("Merge branch 'feature'") && side.contains("main") && side.contains("origin/main") && side.contains("v1"), "{side}");
        assert!(!side.contains('+'), "nothing collapsed: {side}");
        app.side_w = Some(42); // outer width: 40 columns inside the card's frame
        let row = row_of(&screen(&mut app, &mut tm));
        let side = cols(&app, row);
        assert!(side.contains("Merge branch 'featu") && side.contains('+'), "subject first, then +N: {side}");
        assert!(side.contains("ma"), "HEAD's badge stays: {side}");
    }

    fn key_f7(app: &mut App) {
        key(app, KeyCode::F(7), KeyModifiers::NONE);
    }

    #[test]
    fn hover_card_after_a_rest_and_with_i_and_the_info_button() {
        let (t, r) = repo("card", "echo a > a.txt; git add .; git commit -qm 'add a' -m 'the body line'; echo b > b.txt; git add .; git commit -qm second");
        let mut app = app(&t, &r);
        let mut tm = term();
        app.show_side(Side::Scm, false);
        settle(&mut app);
        screen(&mut app, &mut tm);
        let p = at(&tm, "add a");
        mouse(&mut app, MouseEventKind::Moved, p);
        assert!(!tick(&mut app), "not before the delay");
        assert!(app.gitui.due().is_some());
        std::thread::sleep(HOVER + Duration::from_millis(50));
        assert!(tick(&mut app));
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        for want in ["Tester", "<t@example.com>", "add a", "the body line", "1 file changed", "1 insertion(+)", "Copy hash", "Open changes"] {
            assert!(s.contains(want), "{want}: {s}");
        }
        assert!(s.contains("ago") || s.contains("now"), "{s}");
        click(&mut app, at(&tm, "Copy hash"));
        assert!(app.msg.starts_with("copied"), "{}", app.msg);
        // Off the row and the card, it lasts CARD_GRACE: back in time keeps it open.
        let card = app.gitui.hits.card;
        mouse(&mut app, MouseEventKind::Moved, (100, 2));
        assert!(!tick(&mut app) && app.gitui.card.is_some(), "not closed at once");
        mouse(&mut app, MouseEventKind::Moved, (card.x + 1, card.y + 1));
        std::thread::sleep(CARD_GRACE + Duration::from_millis(50));
        assert!(!tick(&mut app) && app.gitui.card.is_some(), "back on the card");
        mouse(&mut app, MouseEventKind::Moved, (100, 2));
        assert!(app.gitui.due().is_some_and(|d| d > Instant::now()), "the close is scheduled");
        std::thread::sleep(CARD_GRACE + Duration::from_millis(50));
        assert!(tick(&mut app));
        assert!(app.gitui.card.is_none());
        // `i` on the selected commit, with no pointer at all.
        app.gitui.graph_focus = true;
        app.gitui.graph.list.sel = 0;
        key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("second") && s.contains("Open changes"), "{s}");
        click(&mut app, at(&tm, "Open changes"));
        settle(&mut app);
        let key = &app.groups[app.ag].tabs[app.groups[app.ag].active].key;
        assert!(key.starts_with("cdiff:") && key.ends_with(":b.txt"), "{key}");
        // The ⓘ at the selected row's end opens it too.
        app.gitui.card = None;
        app.focus = Focus::Side;
        screen(&mut app, &mut tm);
        let info = app.gitui.hits.areas.iter().find(|(_, h)| matches!(h, Hit::Info(_))).map(|x| x.0).unwrap();
        let b = tm.backend().buffer();
        assert_eq!(b[(info.x, info.y)].symbol(), info_icon(), "the hit is on the icon");
        assert_eq!(b[(info.x + 1, info.y)].symbol(), " ", "a blank cell keeps it off the scrollbar");
        click(&mut app, (info.x, info.y));
        assert!(app.gitui.card.is_some());
    }

    #[test]
    fn timeline_border_resizes_and_remembers_its_height() {
        use crate::blocks::BlockStyle;
        let (t, r) = repo("timeline-resize", "echo 1 > f.txt; echo 2 > g.txt; git add .; git commit -qm one");
        for (st, compact) in [(BlockStyle::Cards, false), (BlockStyle::Lines, true)] {
            let state = t.0.join(format!("state-{st:?}/layout.json"));
            let setup = |app: &mut App| {
                app.settings.blocks = st;
                app.settings.compact = compact;
                app.settings.spacing = if compact { crate::settings::Spacing::COMPACT } else { crate::settings::Spacing::COMFORTABLE };
            };
            let mut app = App::new(r.clone(), Some(state.clone()));
            setup(&mut app);
            app.open_file(0, "f.txt", false);
            let mut tm = Terminal::new(TestBackend::new(110, 30)).unwrap();
            screen(&mut app, &mut tm);
            settle(&mut app);
            screen(&mut app, &mut tm);
            let (x, y) = at(&tm, "TIMELINE");
            let border = (x + 3, y - 1);
            let height = app.gitui.hits.timeline.height;
            assert!(app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: border.0, row: border.1, modifiers: KeyModifiers::NONE }), "{st:?}: hovering the border redraws");
            screen(&mut app, &mut tm);
            assert_eq!(tm.backend().buffer()[border].fg, P.sash_hover, "{st:?}: the border lights up");
            click(&mut app, border);
            assert!(app.gitui.timeline.open, "a border click never folds Timeline");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), border);
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (border.0, border.1 - 3));
            screen(&mut app, &mut tm);
            assert_eq!(app.gitui.hits.timeline.height, height + 3, "{st:?}: dragging up grows Timeline");
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (border.0, border.1 - 3));
            let saved: serde_json::Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
            assert_eq!(saved["scm"]["timeline_height"], height + 4, "height includes the header");
            app.open_file(0, "g.txt", false);
            screen(&mut app, &mut tm);
            assert_eq!(app.gitui.hits.timeline.height, height + 3, "switching files retains the split");
            click(&mut app, at(&tm, "TIMELINE"));
            screen(&mut app, &mut tm);
            assert!(!app.gitui.timeline.open);
            click(&mut app, at(&tm, "TIMELINE"));
            screen(&mut app, &mut tm);
            assert_eq!(app.gitui.hits.timeline.height, height + 3, "unfolding restores the split");
            let mut restored = App::new(r.clone(), Some(state));
            setup(&mut restored);
            screen(&mut restored, &mut tm);
            assert_eq!(restored.gitui.hits.timeline.height, height + 3, "reopening restores the split");
            let (x, y) = at(&tm, "TIMELINE");
            mouse(&mut restored, MouseEventKind::Down(MouseButton::Left), (x + 3, y - 1));
            mouse(&mut restored, MouseEventKind::Drag(MouseButton::Left), (x + 3, 500));
            screen(&mut restored, &mut tm);
            assert_eq!(restored.gitui.hits.timeline.height, 3, "Timeline keeps a header and three rows");
            mouse(&mut restored, MouseEventKind::Drag(MouseButton::Left), (x + 3, 0));
            screen(&mut restored, &mut tm);
            assert!(restored.ui.side_list.height >= 4 && !restored.gitui.hits.timeline.is_empty(), "both panes remain usable");
            mouse(&mut restored, MouseEventKind::Up(MouseButton::Left), (x + 3, 0));
            for height in 5..31 {
                let mut tiny = Terminal::new(TestBackend::new(40, height)).unwrap();
                screen(&mut restored, &mut tiny);
                assert!(restored.ui.side_list.bottom() <= height);
            }
            settle(&mut app);
            settle(&mut restored);
        }
    }

    #[test]
    fn timeline_follows_the_active_file() {
        let (t, r) = repo("timeline", "echo 1 > f.txt; git add .; git commit -qm one; echo 2 > f.txt; git commit -qam two; git mv f.txt g.txt; git commit -qm moved");
        let mut app = app(&t, &r);
        let mut tm = term();
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("TIMELINE") && s.contains("The active editor"), "{s}");
        app.open_file(0, "g.txt", false);
        screen(&mut app, &mut tm);
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("moved") && s.contains("two") && s.contains("one"), "{s}");
        click(&mut app, at(&tm, "two"));
        settle(&mut app);
        let key = &app.groups[app.ag].tabs[app.groups[app.ag].active].key;
        assert!(key.starts_with("cdiff:") && key.ends_with(":f.txt"), "before the rename: {key}");
        // The header collapses it (kept in layout.json).
        click(&mut app, at(&tm, "TIMELINE"));
        assert!(!app.gitui.timeline.open);
        let saved = fs::read_to_string(t.0.join("state/layout.json")).unwrap();
        assert!(saved.contains("\"timeline_open\":false"), "{saved}");
    }

    /// Where the dialog drew button `b`.
    fn btn(app: &App, b: commitdlg::Btn) -> (u16, u16) {
        let r = app.gitui.dlg.as_ref().unwrap().hits.btns.iter().find(|x| x.1 == b).unwrap().0;
        (r.x + 1, r.y)
    }

    #[test]
    fn commit_dialog_empty_nothing_staged_signoff_draft_and_amend() {
        let (t, r) = repo("dialog", "echo a > a.txt; git add .; git commit -qm first; echo b >> a.txt; echo u > u.txt");
        let mut app = app(&t, &r);
        let mut tm = term();
        app.show_side(Side::Scm, false);
        settle(&mut app);
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "Add commit"));
        assert!(app.gitui.dlg.is_some());
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        for want in ["Unstaged Changes", "Staged Changes (Will Commit)", "Stage →", "Stage Changed", "← Unstage", "(•) New Commit", "( ) Amend Last Commit", "Rescan", "Sign Off", "Commit & Push", "Cancel", "Subject 0/50"] {
            assert!(s.contains(want), "{want}: {s}");
        }
        assert!(s.contains("a.txt (Working Tree)"), "diff of the selected file: {s}");
        // An empty message is refused.
        let b = btn(&app, commitdlg::Btn::Commit);
        click(&mut app, b);
        assert_eq!(app.gitui.dlg.as_ref().unwrap().note, "Please supply a commit message.");
        // Sign Off adds the trailer once.
        typ(&mut app, "-x subject");
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        settle(&mut app); // the identity comes from a job
        let text = app.gitui.dlg.as_ref().unwrap().msg.text();
        assert_eq!(text, "-x subject\n\nSigned-off-by: Tester <t@example.com>");
        // Esc keeps the draft, in memory and in the state dir; reopening restores it.
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.gitui.dlg.is_none());
        let drafts: Vec<_> = fs::read_dir(t.0.join("state/drafts")).unwrap().flatten().collect();
        assert_eq!(fs::read_to_string(drafts[0].path()).unwrap(), text);
        let mut fresh = App::new(r.clone(), Some(t.0.join("state/layout.json")));
        fresh.commit();
        assert_eq!(fresh.gitui.dlg.as_ref().unwrap().msg.text(), text, "draft survives a restart");
        app.commit();
        // Nothing staged: offer to stage everything; Yes commits all of it.
        screen(&mut app, &mut tm);
        let b = btn(&app, commitdlg::Btn::Commit);
        click(&mut app, b);
        let p = app.prompt.as_ref().expect("stage-all prompt");
        assert!(p.text[0].contains("no staged changes") && p.buttons[p.sel].0 == "Cancel", "Cancel by default: {:?}", p.text);
        key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.gitui.dlg.as_ref().unwrap().busy, "commits off the UI thread");
        settle(&mut app);
        assert!(app.gitui.dlg.is_none() && app.msg.starts_with("Committed"), "{}", app.msg);
        assert!(app.scm_files.is_empty(), "u.txt and a.txt both committed: {:?}", app.scm_files);
        assert!(!t.0.join("state/drafts").read_dir().unwrap().any(|_| true), "draft dropped after the commit");
        // Amend prefills the last message; New Commit puts the draft back.
        app.commit();
        typ(&mut app, "draft");
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "Amend Last Commit"));
        settle(&mut app);
        assert!(app.gitui.dlg.as_ref().unwrap().msg.text().starts_with("-x subject"));
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "New Commit"));
        assert_eq!(app.gitui.dlg.as_ref().unwrap().msg.text(), "draft");
        click(&mut app, at(&tm, "Amend Last Commit"));
        settle(&mut app);
        key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        typ(&mut app, "amended ");
        key(&mut app, KeyCode::Enter, KeyModifiers::ALT);
        settle(&mut app);
        assert!(app.gitui.dlg.is_none(), "{}", app.msg);
        assert!(gitops::last_commit_message(&r).unwrap().starts_with("amended -x subject"));
        assert_eq!(gitops::log_page(&r, 0, 10).unwrap().items.len(), 2, "amend replaced the commit");
        // Stage → and ← Unstage move the selected file between the lists.
        fs::write(r.join("a.txt"), "changed\n").unwrap();
        app.refresh();
        settle(&mut app);
        app.commit();
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "Stage →"));
        settle(&mut app);
        assert_eq!(app.gitui.dlg.as_ref().unwrap().lists[1].len(), 1);
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "← Unstage"));
        settle(&mut app);
        assert_eq!(app.gitui.dlg.as_ref().unwrap().lists[0].len(), 1);
        click(&mut app, at(&tm, "Cancel"));
        assert!(app.gitui.dlg.is_none());
    }

    #[test]
    fn tiny_terminals_never_panic_with_git_popups() {
        let (t, r) = repo("tiny", "echo a > a.txt; git add .; git commit -qm first; echo b >> a.txt");
        for compact in [false, true] {
            let mut app = app(&t, &r);
            app.settings.compact = compact;
            app.show_side(Side::Scm, false);
            settle(&mut app);
            toggle_commit(&mut app, 0);
            settle(&mut app);
            for w in 20..=44 {
                for h in 3..=16 {
                    let mut tm = Terminal::new(TestBackend::new(w, h)).unwrap();
                    let mut draw = |app: &mut App| {
                        tm.draw(|f| crate::ui::draw(app, f)).unwrap();
                    };
                    app.side = Side::Scm;
                    draw(&mut app);
                    app.side = Side::Explorer;
                    draw(&mut app);
                    let hash = app.gitui.graph.commits[0].hash.clone();
                    open_card(&mut app, hash, Rect::new(5, 5, 10, 1));
                    settle(&mut app);
                    draw(&mut app);
                    app.gitui.card = None;
                    app.commit();
                    draw(&mut app);
                    app.prompt = Some(crate::panel::Prompt {
                        text: vec!["There are no staged changes to commit.".into()],
                        buttons: vec![("Yes".into(), crate::panel::PAct::Cancel), ("Cancel".into(), crate::panel::PAct::Cancel)],
                        sel: 0,
                    });
                    draw(&mut app);
                    app.prompt = None;
                    app.gitui.dlg = None;
                }
            }
        }
    }
}
