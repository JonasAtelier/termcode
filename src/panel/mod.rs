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
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime};

// `impl App`, by topic.
mod qdiff;
mod gitfs;
pub(crate) mod rename;
mod search;
mod outline;
mod tabs;
mod find;
mod sidebar;
mod quick;
mod keys;
mod mouse;
pub(crate) use mouse::diff_pos_in;

/// A file's (mtime, length), or None when it does not exist.
type Stamp = Option<(SystemTime, u64)>;
/// Quick-diff recompute delay after an edit.
const QDIFF_DEBOUNCE: Duration = Duration::from_millis(200);
pub const MAX_HITS: usize = 5_000;
/// A group splits while each half keeps at least MIN_GROUP_W columns and MIN_GROUP_H
/// rows: a wide or tall monitor takes more. MAX_GROUPS only bounds it.
pub const MIN_GROUP_W: u16 = 30;
pub const MIN_GROUP_H: u16 = 7;
pub const MAX_GROUPS: usize = 64;

/// Where a group sits in its view's editor area, in fractions of it: left, top, right,
/// bottom. A split halves a group's own (what a drop's preview shows) and leaves the others
/// as they are; a group closed gives its own to the neighbours along one side. Halves are
/// exact in binary, so two neighbours share the very same edge (and the same cell).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frac {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Default for Frac {
    fn default() -> Self {
        Frac { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }
    }
}

impl Frac {
    /// Its halves: left and right, or (`down`) top and bottom.
    pub fn halves(self, down: bool) -> (Frac, Frac) {
        if down {
            let m = (self.y0 + self.y1) / 2.0;
            (Frac { y1: m, ..self }, Frac { y0: m, ..self })
        } else {
            let m = (self.x0 + self.x1) / 2.0;
            (Frac { x1: m, ..self }, Frac { x0: m, ..self })
        }
    }

    /// Its cells in `r`: an edge two groups share lands on the same cell for both.
    pub fn cells(self, r: Rect) -> Rect {
        let x = |f: f64| r.x + (f * f64::from(r.width)).round() as u16;
        let y = |f: f64| r.y + (f * f64::from(r.height)).round() as u16;
        let (x0, y0) = (x(self.x0), y(self.y0));
        Rect::new(x0, y0, x(self.x1).saturating_sub(x0), y(self.y1).saturating_sub(y0))
    }

    pub fn size(self) -> f64 {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }
}

/// Close enough to be the same edge (fractions from halvings are exact; columns of an old
/// session's thirds are not).
fn same(p: f64, q: f64) -> bool {
    (p - q).abs() < 1e-9
}

/// Do `fracs` tile the whole area: inside it, none empty, none overlapping, all of it?
pub fn tiles(fracs: &[Frac]) -> bool {
    let inside = fracs.iter().all(|f| f.x0 > -1e-9 && f.y0 > -1e-9 && f.x1 < 1.0 + 1e-9 && f.y1 < 1.0 + 1e-9 && f.x1 > f.x0 + 1e-9 && f.y1 > f.y0 + 1e-9);
    let apart = fracs.iter().enumerate().all(|(i, a)| fracs[i + 1..].iter().all(|b| a.x1 <= b.x0 + 1e-9 || b.x1 <= a.x0 + 1e-9 || a.y1 <= b.y0 + 1e-9 || b.y1 <= a.y0 + 1e-9));
    inside && apart && same(fracs.iter().map(|f| f.size()).sum(), 1.0)
}

/// Space `sp`'s groups laid out as columns (equal widths), each column's groups top down
/// (equal heights); `heads` says which group starts a column (an old session's layout).
pub fn columns_layout(groups: &mut [Group], sp: Space, heads: &[bool]) {
    let idx: Vec<usize> = (0..groups.len()).filter(|&i| groups[i].space == sp).collect();
    let mut cols: Vec<Vec<usize>> = Vec::new();
    for (k, &i) in idx.iter().enumerate() {
        match cols.last_mut() {
            Some(c) if !heads.get(k).copied().unwrap_or(true) => c.push(i),
            _ => cols.push(vec![i]),
        }
    }
    let n = cols.len() as f64;
    for (ci, col) in cols.iter().enumerate() {
        let m = col.len() as f64;
        for (ri, &i) in col.iter().enumerate() {
            groups[i].frac = Frac { x0: ci as f64 / n, x1: (ci + 1) as f64 / n, y0: ri as f64 / m, y1: (ri + 1) as f64 / m };
            groups[i].path.clear();
        }
    }
}

/// Group `g`'s area given back to the half it was split from (by their paths): that
/// half's groups along the cut grow over it, and all of them forget that halving. False
/// when its path says nothing (an older session's layout).
fn give_back(groups: &mut [Group], g: usize) -> bool {
    let (a, sp, path) = (groups[g].frac, groups[g].space, groups[g].path.clone());
    let Some(&(down, second)) = path.last() else { return false };
    let d = path.len() - 1;
    let mut sib = path[..d].to_vec();
    sib.push((down, !second));
    let half: Vec<usize> = (0..groups.len()).filter(|&i| i != g && groups[i].space == sp && groups[i].path.starts_with(&sib)).collect();
    // Along the cut: those of the other half that touch it.
    let edge = |f: &Frac| match (down, second) {
        (false, true) => same(f.x1, a.x0),
        (false, false) => same(f.x0, a.x1),
        (true, true) => same(f.y1, a.y0),
        (true, false) => same(f.y0, a.y1),
    };
    let cut: Vec<usize> = half.iter().copied().filter(|&i| edge(&groups[i].frac)).collect();
    let span = |f: &Frac| if down { f.x1 - f.x0 } else { f.y1 - f.y0 };
    if cut.is_empty() || !same(cut.iter().map(|&i| span(&groups[i].frac)).sum(), span(&a)) {
        return false;
    }
    for &i in &cut {
        let f = &mut groups[i].frac;
        match (down, second) {
            (false, true) => f.x1 = a.x1,
            (false, false) => f.x0 = a.x0,
            (true, true) => f.y1 = a.y1,
            (true, false) => f.y0 = a.y0,
        }
    }
    for &i in &half {
        groups[i].path.remove(d);
    }
    true
}

/// A rectangle a halving makes: each side a power of two of the whole, starting at a
/// multiple of it.
fn halving(f: Frac) -> bool {
    let fits = |lo: f64, hi: f64| {
        let w = hi - lo;
        w > 0.0 && w.log2().fract() == 0.0 && (lo / w).fract() == 0.0
    };
    fits(f.x0, f.x1) && fits(f.y0, f.y1)
}

/// Group `g` removed from `groups`, its area given to its neighbours along a side they
/// cover exactly: the side whose groups, grown over it, are halvings again (its other
/// half, the group it was split from), else the fewest. A layout of halvings always has
/// such a side; anything else is laid out again as columns.
pub fn remove_group_in(groups: &mut Vec<Group>, g: usize) {
    if give_back(groups, g) {
        groups.remove(g);
        return;
    }
    let (a, sp) = (groups[g].frac, groups[g].space);
    let others: Vec<usize> = (0..groups.len()).filter(|&i| i != g && groups[i].space == sp).collect();
    // ((not halvings again, how many), (side, those groups))
    type Pick = ((bool, usize), (usize, Vec<usize>));
    let mut best: Option<Pick> = None;
    for side in 0..4 {
        // 0: those right of it, 1: left, 2: below, 3: above.
        let span = |f: &Frac| if side < 2 { (f.y0, f.y1) } else { (f.x0, f.x1) };
        let touch = |f: &Frac| match side {
            0 => same(f.x0, a.x1),
            1 => same(f.x1, a.x0),
            2 => same(f.y0, a.y1),
            _ => same(f.y1, a.y0),
        };
        let (lo, hi) = span(&a);
        let set: Vec<usize> = others.iter().copied().filter(|&i| touch(&groups[i].frac) && span(&groups[i].frac).0 > lo - 1e-9 && span(&groups[i].frac).1 < hi + 1e-9).collect();
        let covered: f64 = set.iter().map(|&i| span(&groups[i].frac).1 - span(&groups[i].frac).0).sum();
        if set.is_empty() || !same(covered, hi - lo) {
            continue;
        }
        let grown = |f: Frac| match side {
            0 => Frac { x0: a.x0, ..f },
            1 => Frac { x1: a.x1, ..f },
            2 => Frac { y0: a.y0, ..f },
            _ => Frac { y1: a.y1, ..f },
        };
        // Better: halvings again; then the fewest groups.
        let rank = (!set.iter().all(|&i| halving(grown(groups[i].frac))), set.len());
        if best.as_ref().is_none_or(|b| rank < b.0) {
            best = Some((rank, (side, set)));
        }
    }
    match best.map(|b| b.1) {
        Some((side, set)) => {
            for i in set {
                let f = &mut groups[i].frac;
                match side {
                    0 => f.x0 = a.x0,
                    1 => f.x1 = a.x1,
                    2 => f.y0 = a.y0,
                    _ => f.y1 = a.y1,
                }
            }
            groups.remove(g);
        }
        None => {
            groups.remove(g);
            if groups.iter().any(|x| x.space == sp) {
                columns_layout(groups, sp, &[]);
            }
        }
    }
}
pub(crate) const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Set for the process a reload execs: the session comes back whatever the settings say.
const RELOAD_ENV: &str = "TERMCODE_RELOADED";
/// How long a dragged item rests on a closed folder before it opens.
const SPRING: Duration = Duration::from_millis(700);
/// Changes made outside the panel (an agent, a shell) show without a focus or view
/// switch: `git status` reruns this often, or ten times as long as it last took.
const STATUS_POLL: Duration = Duration::from_secs(2);
const STATUS_POLL_MAX: Duration = Duration::from_secs(30);
/// How often a running panel looks for a newly installed binary.
const UPGRADE_CHECK: Duration = Duration::from_secs(2);

/// A new termcode installed over the running one (`cargo install` renames a new file
/// over it): seen once its size and mtime hold still between two checks.
#[derive(Default)]
pub struct Upgrade {
    /// Only the real panel looks (`run`), never a test.
    pub watch: bool,
    next: Option<Instant>,
    seen: Option<(Option<SystemTime>, u64)>,
    /// "reloads once saved" was said: said once.
    told: bool,
}

/// The installed path when `/proc/self/exe` says the running binary was replaced.
fn replaced(link: &Path) -> Option<PathBuf> {
    link.to_str()?.strip_suffix(" (deleted)").map(PathBuf::from)
}

/// Search on type: the pause after the last change to the query before it runs
/// (VS Code's `search.searchOnTypeDebouncePeriod`).
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// How long the pointer rests on a cut Explorer name before its tooltip shows.
const TIP_DELAY: Duration = Duration::from_millis(500);

impl Tip {
    /// The row grown to show the whole name: from the name's column, one blank after it,
    /// up to `right` (the screen's edge); nothing when the name was not cut.
    pub fn ext(&self, right: u16) -> Rect {
        let Some(text) = &self.text else { return Rect::default() };
        let w = (unicode_width::UnicodeWidthStr::width(text.as_str()) as u16 + 1).min(right.saturating_sub(self.x));
        Rect::new(self.x, self.rect.y, w, 1)
    }

    /// Shown now: a file's MD5 starts on a thread, `tick` takes it in; the tooltip going
    /// stops it (`Drop`).
    fn show(&mut self, root: &Path) {
        self.shown = true;
        // A link: where it points, as written (missing: said so).
        if let Some(rel) = &self.link {
            let p = root.join(rel);
            let raw = fs::read_link(&p).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
            self.link_to = Some(if p.exists() { raw } else { format!("{raw}  (missing)") });
        }
        let Some(path) = self.file.as_ref().map(|f| root.join(f)) else { return };
        let (tx, rx) = mpsc::channel();
        let stop = self.stop.clone();
        std::thread::spawn(move || match crate::md5::file(&path, &stop) {
            Ok(Some(sum)) => drop(tx.send(Ok(sum))),
            Ok(None) => {} // nobody waits for it any more
            Err(e) => drop(tx.send(Err(format!("unreadable: {e}")))),
        });
        self.md5_rx = Some(rx);
    }

    /// The MD5 once it came; true when it just did.
    fn tick(&mut self) -> bool {
        let Some(got) = self.md5_rx.as_ref().and_then(|rx| rx.try_recv().ok()) else { return false };
        (self.md5, self.md5_rx) = (Some(got), None);
        true
    }
}

/// An Explorer name being edited in place (double-click a file or folder): the item, the new
/// name, the caret and the other end of the selection (bytes).
pub struct Rename {
    pub path: String,
    pub text: String,
    pub cur: usize,
    pub anchor: Option<usize>,
}

impl Rename {
    /// The name with a file's stem (a folder's whole name) selected, as VS Code starts a rename.
    fn new(path: &str, name: &str, dir: bool) -> Rename {
        let stem = name.rfind('.').filter(|&i| i > 0 && !dir).unwrap_or(name.len());
        Rename { path: path.to_string(), text: name.to_string(), cur: stem, anchor: Some(0) }
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor.filter(|&a| a != self.cur)?;
        Some((a.min(self.cur), a.max(self.cur)))
    }

    /// Pasted text replaces the selection; a name is one line, so only the first counts.
    pub(crate) fn paste(&mut self, text: &str) {
        let line = text.lines().next().unwrap_or("").trim_end_matches('\r');
        self.take_selection();
        self.text.insert_str(self.cur, line);
        self.cur += line.len();
    }

    fn take_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.text.replace_range(a..b, "");
        (self.cur, self.anchor) = (a, None);
        true
    }

    /// The byte the caret goes to for screen column `x` of the box `(field, scrolled)`.
    fn caret_at(&self, (field, skip): (Rect, usize), x: u16) -> usize {
        editor::byte_at_display(&self.text, skip + x.clamp(field.x, field.right()).saturating_sub(field.x) as usize)
    }

    /// Move the caret to `to`; with `extend` the selection grows from where it was.
    fn go(&mut self, to: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cur);
        } else {
            self.anchor = None;
        }
        self.cur = to;
    }

    /// One key of the field; false for keys it does not use.
    pub(crate) fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let prev = |s: &str, i: usize| s[..i].chars().next_back().map_or(i, |c| i - c.len_utf8());
        let next = |s: &str, i: usize| s[i..].chars().next().map_or(i, |c| i + c.len_utf8());
        match k.code {
            KeyCode::Char('a') if ctrl => (self.anchor, self.cur) = (Some(0), self.text.len()),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                self.take_selection();
                self.text.insert(self.cur, c);
                self.cur += c.len_utf8();
            }
            KeyCode::Backspace => {
                if !self.take_selection() && self.cur > 0 {
                    let p = prev(&self.text, self.cur);
                    self.text.replace_range(p..self.cur, "");
                    self.cur = p;
                }
            }
            KeyCode::Delete => {
                if !self.take_selection() && self.cur < self.text.len() {
                    let n = next(&self.text, self.cur);
                    self.text.replace_range(self.cur..n, "");
                }
            }
            // Shift extends the selection; without it an arrow first collapses one.
            KeyCode::Left => {
                let to = match self.selection().filter(|_| !shift) {
                    Some((a, _)) => a,
                    None => prev(&self.text, self.cur),
                };
                self.go(to, shift);
            }
            KeyCode::Right => {
                let to = match self.selection().filter(|_| !shift) {
                    Some((_, b)) => b,
                    None => next(&self.text, self.cur),
                };
                self.go(to, shift);
            }
            KeyCode::Home => self.go(0, shift),
            KeyCode::End => self.go(self.text.len(), shift),
            _ => return false,
        }
        true
    }
}

/// The tooltip of the Explorer row under the pointer: its row, a cut name's full name,
/// since when; a file's MD5 (with a Copy button) on the line under it.
pub struct Tip {
    pub rect: Rect,
    pub text: Option<String>,
    /// The column the name starts at: a cut one is drawn in full from there.
    pub x: u16,
    since: Instant,
    pub shown: bool,
    /// The file's path, its MD5 (or why there is none) once `md5sum` gave it.
    pub file: Option<String>,
    /// A symlink's path, and (once shown) where it points.
    pub link: Option<String>,
    pub link_to: Option<String>,
    pub md5: Option<Result<String, String>>,
    md5_rx: Option<mpsc::Receiver<Result<String, String>>>,
    /// Set when the tooltip goes: its MD5 stops.
    stop: Arc<AtomicBool>,
}

impl Drop for Tip {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
/// /proc/<pid>/comm of a running panel (herdr-code: its name before termcode): its
/// backups are its own.
const PANEL_COMM: &[&str] = &["termcode", "herdr-code"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Explorer,
    Search,
    Scm,
    /// A driver's view (ext.rs), by its number: Serial, the Programmer, GitHub, the
    /// Extensions view (in-tree), then the installed extensions.
    View(u16),
}
impl Side {
    pub const PROGRAMMER: Side = Side::View(crate::ext::PROGRAMMER);
    pub const GITHUB: Side = Side::View(crate::ext::GITHUB);

    /// Its place in the per-view arrays (`sel`, `off`, which only the first three use).
    pub fn idx(self) -> usize {
        match self {
            Side::Explorer => 0,
            Side::Search => 1,
            Side::Scm => 2,
            Side::View(_) => 3,
        }
    }
}
/// Activity bar order (VS Code's).
pub const SIDES: [Side; 3] = [Side::Explorer, Side::Search, Side::Scm];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Focus {
    Side,
    Editor,
    SearchInput,
    Find,
    /// The integrated terminal: keys go to its shell.
    Term,
    /// The Outline's list.
    Outline,
}

/// An Outline row as last drawn: where its symbol is, its tree path, its depth, whether
/// symbols nest in it and whether they show.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    pub to: editor::Pos,
    pub path: String,
    pub depth: usize,
    pub kids: bool,
    pub open: bool,
}

/// An Explorer row with a tooltip: its rect, a cut name's full name, the column the name
/// starts at, a file's path, a symlink's path.
pub type TipRow = (Rect, Option<String>, u16, Option<String>, Option<String>);

pub struct Node {
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub dir: bool,
    pub open: bool,
    /// A symlink: where it points, as written, and whether that is there.
    pub link: Option<(String, bool)>,
}

/// A file tab's other look, from either of its keys: a Markdown file's preview, a
/// patch's diff. The file and the look's key prefix.
pub(crate) fn other_look(key: &str) -> Option<(&str, &'static str)> {
    if let Some(rel) = key.strip_prefix("file:") {
        return if crate::symbols::is_markdown(rel) {
            Some((rel, "preview:"))
        } else if crate::patch::is_patch(rel) {
            Some((rel, "patch:"))
        } else if rel.to_ascii_lowercase().ends_with(".map") {
            Some((rel, "usage:"))
        } else if crate::logview::is_log(rel) {
            Some((rel, "log:"))
        } else {
            None
        };
    }
    ["preview:", "patch:", "usage:", "log:"].into_iter().find_map(|look| key.strip_prefix(look).map(|r| (r, look)))
}

pub struct DiffDoc {
    pub title: String,
    pub rows: Vec<diff::Row>,
    pub note: Option<String>,
    pub file: ScmFile,
    /// Keys this diff's syntax and minimap buffers in the panel.
    pub id: DiffId,
    /// A patch file's view: its files' and hunks' heads, its own line numbers.
    pub patch: Option<Box<crate::patch::PatchRows>>,
}

/// One DiffDoc, unique per construction (`Default` draws the next number): the
/// editor buffers are not Send, so they live in the panel, not in the doc.
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
    /// Its place in its view's editor area (fractions of it), and the halvings that made
    /// it, outermost first: (down, its second half). Closed, its other half takes it back.
    pub frac: Frac,
    pub path: Vec<(bool, bool)>,
    /// The space it belongs to: only the current view's groups show.
    pub space: Space,
    /// Its tab bar scrolled: the first tab shown; the active tab last brought into view
    /// (the bar follows the active tab when that changes, else stays where it was put).
    pub tab_off: usize,
    pub tab_follow: Option<usize>,
}

/// What the editor area is for: code (the Explorer, Search, Source Control), serial
/// streams, GitHub's pages, the Programmer's. Each keeps its own groups and tabs; a view
/// shows its space's, and a tab opens in its own space (switching to it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Space {
    #[default]
    Code,
    /// A driver's view's (ext.rs).
    View(u16),
}

impl Space {

    pub fn of_side(s: Side) -> Space {
        match s {
            Side::View(n) => Space::View(n),
            Side::Explorer | Side::Search | Side::Scm => Space::Code,
        }
    }

    /// The space a tab belongs to, by its key: its driver's, else code's.
    pub fn of_key(key: &str) -> Space {
        crate::ext::owner(key).map_or(Space::Code, Space::View)
    }
}

pub struct Find {
    pub query: String,
    pub replace: Option<String>,
    /// 0 = find field, 1 = replace field.
    pub field: usize,
    /// The focused field's whole text is selected (Ctrl+A, a double-click).
    pub all: bool,
    pub matches: Vec<(Pos, Pos)>,
    pub cur: Option<usize>,
    /// Match Case / Match Whole Word toggles.
    pub case: bool,
    pub word: bool,
    /// Use Regular Expression; `invalid` when the query does not compile.
    pub regex: bool,
    pub invalid: bool,
    /// Diff side chosen when opening Find; None for a regular editor.
    pub diff_side: Option<u8>,
    stamp: (String, u64, String, bool, bool, bool, Option<u8>),
}

impl Find {
    /// The document and its version the matches are for.
    pub fn of(&self) -> (&str, u64) {
        (&self.stamp.0, self.stamp.1)
    }

    /// The matches of the query in `lines`; `invalid` when its regular expression is.
    fn search<'a>(&mut self, lines: impl IntoIterator<Item = (usize, &'a str)>) {
        self.invalid = false;
        self.matches = if self.regex && !self.query.is_empty() {
            match editor::find_regex(&self.query, self.case, self.word) {
                Ok(re) => editor::find_re(lines, &re),
                Err(_) => {
                    self.invalid = true;
                    Vec::new()
                }
            }
        } else {
            editor::find_in(lines, &self.query, self.case, self.word)
        };
    }

    /// The regular expression in use, when the toggle is on and the query compiles.
    fn compiled(&self) -> Option<fancy_regex::Regex> {
        self.regex.then(|| editor::find_regex(&self.query, self.case, self.word).ok()).flatten()
    }
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
    /// Quit although a program runs in the terminal (it ends with the window).
    QuitEndTerminal,
    /// Close terminal N although a program runs in it.
    KillTerminal(usize),
    /// An Explorer item dropped on a folder ("" the root): move it there.
    Move(Vec<String>, String),
    /// Delete an Explorer item: to the Trash, or (`true`) for good.
    Delete(Vec<String>, bool),
    /// A change on GitHub, confirmed.
    GitHub(crate::github::Act),
    /// An STM32 erase, confirmed.
    Prog(crate::programmer::Op),
    /// A document and the backup file chosen for it; the backup file to drop.
    RestoreBackup(String, PathBuf),
    DiscardBackup(PathBuf),
    /// An API client's request: 0 deny, 1 allow once, 2 allow while it runs.
    Api(u8),
    /// An API client's question (`ui.prompt` number), button picked.
    ApiPick(u64, usize),
    /// A Rust workspace: checked with cargo on save (true), or its syntax only.
    RustCheck(PathBuf, bool),
    /// The extension fetched in the Extensions view: installed (true), or not.
    ExtInstall(bool),
    /// An extension removed (the Extensions view), by id.
    ExtRemove(String),
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
    close: bool,
    row: Option<usize>,
    sticky: Option<usize>,
    git_row: Option<u16>,
    tab: Option<(usize, usize)>,
    fold: Option<(usize, u16)>,
    scrollbar: Option<usize>,
    minimap: Option<usize>,
    peek: Option<usize>,
    /// A commit-dialog or sidebar splitter (usize::MAX): sash.hoverBorder.
    sash: Option<usize>,
    /// The Outline row under the pointer (its screen row).
    outline: Option<u16>,
    /// The GitHub view's cell under the pointer (its rows and buttons light up).
    gh: Option<(u16, u16)>,
    /// An activity bar item (its icon lights up).
    activity: Option<usize>,
    /// A commit-dialog button, a commit card's button, or (usize::MAX) Source Control's
    /// "Add commit": lit grey.
    button: Option<usize>,
    /// The status bar item under the pointer (its column): statusBarItem.hoverBackground.
    status: Option<u16>,
    /// A tab bar's button under the pointer (its cell): a scroll arrow (lit white), split,
    /// terminal, outline.
    bar_btn: Option<(u16, u16)>,
}

/// Events the loop batches before one frame: wheel ticks and plain pointer motion.
fn coalescable(ev: &Event) -> bool {
    matches!(ev, Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight | MouseEventKind::Moved))
}

/// (mtime, length) of each file, None for a missing one: what a confirmed discard re-checks.
fn file_stamps(g: &Path, files: &[ScmFile]) -> Vec<Option<(SystemTime, u64)>> {
    files.iter().map(|f| fs::symlink_metadata(g.join(&f.path)).ok().and_then(|m| Some((m.modified().ok()?, m.len())))).collect()
}

/// The branch label, `git status -z` and the sync state (HEAD, upstream, ahead/behind).
type Status = (String, Vec<u8>, Option<crate::gitops::SyncStatus>);

/// The `Status` of the repo at `g` (runs git: off the UI thread, but for the first frame).
fn read_status(g: &Path) -> Status {
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
    let raw = run(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]).unwrap_or_default();
    (branch, raw, crate::gitops::sync_status(g).ok())
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
    /// `:` (Go to Line) and `@` (Go to Symbol): where each row goes, and its detail.
    pub goto: Vec<(Pos, String)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cmd {
    /// An STM32 Programmer job.
    Prog(crate::programmer::Op),
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
    OpenSettings,
    SettingsPanel,
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
    RefreshSettings,
    /// Focus the pane beside the focused one: 0 left, 1 right, 2 up, 3 down.
    Navigate(u8),
    /// Close the active terminal; the last one: the panel hidden, its shell kept.
    CloseTerminal,
    /// The active tab again in a new group: to the right, or (true) below.
    Split(bool),
    Nav(crate::navui::Act),
    Git(crate::gitcmds::GitCmd),
    GotoLine,
    GotoSymbol,
    ToggleComment,
    ToggleWrap,
    ToggleHex,
    ToggleBreadcrumbs,
    ToggleTerminal,
    NewTerminal,
    KillTerminal,
    /// Terminal: Focus Next (true) / Previous Terminal.
    NextTerminal(bool),
    TermFind,
    ToggleOutline,
    FocusOutline,
    /// Markdown: Open Preview; `true` to the side.
    MdPreview(bool),
    ReloadWindow,
    ReportIssue,
    ShowSide(Side),
    Quit,
    /// Find, or Find and Replace.
    Find(bool),
    /// An action the default keys do: the key, as if pressed (no keybinding in between).
    Keys(KeyCode, KeyModifiers),
}

/// The F1 command palette: label, command, key hint.
pub const COMMANDS: &[(&str, Cmd, &str)] = &[
    ("Fold", Cmd::Fold, "Ctrl+Shift+["),
    ("Unfold", Cmd::Unfold, "Ctrl+Shift+]"),
    ("Fold All", Cmd::FoldAll, "Ctrl+K Ctrl+0"),
    ("Unfold All", Cmd::UnfoldAll, "Ctrl+K Ctrl+J"),
    ("Toggle Line Comment", Cmd::ToggleComment, "Ctrl+/"),
    ("Preferences: Open Settings (JSON)", Cmd::OpenSettings, ""),
    ("Preferences: Open Settings", Cmd::SettingsPanel, ""),
    ("View: Toggle Indent Rainbow", Cmd::ToggleRainbow, ""),
    ("View: Toggle Minimap Ruler", Cmd::ToggleRuler, ""),
    ("View: Toggle Indent Guides", Cmd::ToggleGuides, ""),
    ("View: Toggle Primary Side Bar Visibility", Cmd::ToggleSidebar, ""),
    ("View: Toggle Minimap", Cmd::ToggleMinimap, ""),
    ("View: Toggle Rulers", Cmd::ToggleRulers, ""),
    ("View: Toggle Word Wrap", Cmd::ToggleWrap, "Alt+Z"),
    ("View: Toggle Hexdump", Cmd::ToggleHex, ""),
    ("View: Toggle Breadcrumbs", Cmd::ToggleBreadcrumbs, ""),
    ("View: Toggle Terminal", Cmd::ToggleTerminal, "Ctrl+`"),
    ("Terminal: Create New Terminal", Cmd::NewTerminal, "Ctrl+Shift+`"),
    ("Terminal: Kill the Active Terminal Instance", Cmd::KillTerminal, ""),
    ("Terminal: Focus Next Terminal", Cmd::NextTerminal(true), "Ctrl+PgDn"),
    ("Terminal: Focus Previous Terminal", Cmd::NextTerminal(false), "Ctrl+PgUp"),
    ("Terminal: Find", Cmd::TermFind, "Ctrl+F"),
    ("View: Toggle Outline", Cmd::ToggleOutline, ""),
    ("View: Focus Outline", Cmd::FocusOutline, ""),
    ("Go to Next Change", Cmd::NextChange, "Alt+F5 / F7"),
    ("Go to Previous Change", Cmd::PrevChange, "Shift+Alt+F5 / Shift+F7"),
    ("Go to File...", Cmd::QuickOpen, "Ctrl+P"),
    ("Go to Line/Column...", Cmd::GotoLine, "Ctrl+P :"),
    ("Go to Symbol in Editor...", Cmd::GotoSymbol, "Ctrl+Shift+O"),
    ("File: Save", Cmd::Save, "Ctrl+S"),
    ("Developer: Reload Settings", Cmd::ReloadSettings, ""),
    ("Preferences: Refresh Settings File", Cmd::RefreshSettings, ""),
    ("Terminal: Close Terminal", Cmd::CloseTerminal, "Ctrl+T X"),
    ("View: Navigate Left", Cmd::Navigate(0), "Ctrl+T ←"),
    ("View: Navigate Right", Cmd::Navigate(1), "Ctrl+T →"),
    ("View: Navigate Up", Cmd::Navigate(2), "Ctrl+T ↑"),
    ("View: Navigate Down", Cmd::Navigate(3), "Ctrl+T ↓"),
    ("Developer: Reload Window", Cmd::ReloadWindow, ""),
    ("Help: Report Issue", Cmd::ReportIssue, ""),
    ("Go to Definition", Cmd::Nav(crate::navui::Act::Definition), "F12"),
    ("Peek Definition", Cmd::Nav(crate::navui::Act::Peek), "Alt+F12"),
    ("Find All References", Cmd::Nav(crate::navui::Act::References), "Shift+F12"),
    ("Go Back", Cmd::Nav(crate::navui::Act::Back), "Alt+←"),
    ("Go Forward", Cmd::Nav(crate::navui::Act::Forward), "Alt+→"),
    ("Markdown: Toggle Preview", Cmd::MdPreview(false), "Ctrl+Shift+V"),
    ("Markdown: Open Preview to the Side", Cmd::MdPreview(true), "Ctrl+K V"),
    ("View: Show Explorer", Cmd::ShowSide(Side::Explorer), "Alt+1, Ctrl+E"),
    ("View: Show Search", Cmd::ShowSide(Side::Search), "Alt+4, F4"),
    ("View: Show Source Control", Cmd::ShowSide(Side::Scm), "Alt+2, F2, Ctrl+G"),
    ("View: Show GitHub", Cmd::ShowSide(Side::GITHUB), ""),
    ("View: Show Programmer", Cmd::ShowSide(Side::PROGRAMMER), ""),
    ("View: Split Editor Right", Cmd::Split(false), "Ctrl+T \\"),
    ("View: Split Editor Down", Cmd::Split(true), "Ctrl+T -"),
    ("STM32: Connect", Cmd::Prog(crate::programmer::Op::Connect), ""),
    ("STM32: Start Programming", Cmd::Prog(crate::programmer::Op::Program), ""),
    ("STM32: Reset Target", Cmd::Prog(crate::programmer::Op::Reset), ""),
    ("STM32: Read Option Bytes", Cmd::Prog(crate::programmer::Op::ReadOb), ""),
    ("View: Open Next Editor", Cmd::Keys(KeyCode::PageDown, KeyModifiers::CONTROL), "Ctrl+PgDn"),
    ("View: Open Previous Editor", Cmd::Keys(KeyCode::PageUp, KeyModifiers::CONTROL), "Ctrl+PgUp"),
    ("View: Focus First Editor Group", Cmd::Keys(KeyCode::Char('1'), KeyModifiers::CONTROL), "Ctrl+1"),
    ("View: Focus Second Editor Group", Cmd::Keys(KeyCode::Char('2'), KeyModifiers::CONTROL), "Ctrl+2"),
    ("View: Focus Third Editor Group", Cmd::Keys(KeyCode::Char('3'), KeyModifiers::CONTROL), "Ctrl+3"),
    ("View: Close Editor", Cmd::Keys(KeyCode::Char('w'), KeyModifiers::CONTROL), "Ctrl+W"),
    ("File: Quit", Cmd::Quit, "Ctrl+Q"),
    ("Find", Cmd::Find(false), "Ctrl+F"),
    ("Replace", Cmd::Find(true), "Ctrl+H"),
    ("Add Cursor Above", Cmd::Keys(KeyCode::Up, KeyModifiers::CONTROL.union(KeyModifiers::SHIFT)), "Ctrl+Shift+↑"),
    ("Add Cursor Below", Cmd::Keys(KeyCode::Down, KeyModifiers::CONTROL.union(KeyModifiers::SHIFT)), "Ctrl+Shift+↓"),
];

/// Recently used palette commands kept (in the layout file).
const MAX_RECENT: usize = 10;

/// Every palette command: the app's, then the Git ones.
pub fn all_commands() -> impl Iterator<Item = (&'static str, Cmd)> {
    COMMANDS.iter().map(|c| (c.0, c.1)).chain(crate::gitcmds::GIT_COMMANDS.iter().map(|(l, g)| (*l, Cmd::Git(*g))))
}

/// Commands matching `q`, best tier first, palette order within a tier.
#[cfg(test)]
pub fn filter_commands(q: &str) -> Vec<String> {
    filter_commands_recent(q, &[], &[])
}

/// `filter_commands`, with the recently used ones first (newest first) while nothing is
/// typed, as VS Code lists them.
pub fn filter_commands_recent(q: &str, recent: &[String], extra: &[String]) -> Vec<String> {
    let mut v: Vec<(usize, usize, &str)> =
        all_commands().map(|c| c.0).chain(extra.iter().map(String::as_str)).enumerate().filter_map(|(i, c)| parse::match_score(c, q).map(|s| (s / 100_000, i, c))).collect();
    v.sort();
    let mut out: Vec<String> = v.into_iter().map(|(_, _, l)| l.to_string()).collect();
    if q.is_empty() {
        let mut front: Vec<String> = recent.iter().filter(|r| out.contains(r)).cloned().collect();
        out.retain(|l| !front.contains(l));
        front.append(&mut out);
        out = front;
    }
    out
}

/// Something with a scrollbar.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ScrollTarget {
    Editor(usize),
    EditorHorizontal(usize),
    Side,
    Quick,
    /// Source Control lists: 0 = GRAPH, 1 = Explorer TIMELINE.
    Git(u8),
    /// A PDF's pages in group `g` (in points).
    Media(usize),
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
    /// The editor's first row when it was drawn (folded lines do not count).
    pub scroll: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    Left,
    Right,
    /// The top or bottom edge: a new group in its upper or lower half.
    Up,
    Down,
    Center,
    /// A dragged tab over a tab bar: inserted before tab n (n = the count: at the end).
    Tab(usize),
}

pub enum Drag {
    None,
    Close,
    /// Released inside Close; paint its hover state before starting the quit procedure.
    ClosePending,
    Divider,
    /// The terminal panel's top border, its height's handle.
    TermSash,
    /// Selecting in the terminal from this (row, column) of its screen.
    TermSelect((u16, u16)),
    /// A press the program in the terminal is getting (its drag and release go there too).
    TermMouse,
    /// The Outline's left border, its width's handle.
    OutlineSash,
    /// A line in a driver's view (between the Serial view's ports and channels, GitHub's
    /// panes, an extension's sections): its `drag_to` follows the pointer.
    View(u16),
    /// An Explorer item (`dir`: a folder): onto an editor (a file), or into a folder.
    File { rel: String, from: Position, active: bool, dir: bool, keep: bool },
    /// Tab `i` of group `g` (VS Code's tab drag and drop); active once moved 2 cells.
    Tab { g: usize, i: usize, from: Position, active: bool },
    Select(usize),
    /// Selecting in the rename box.
    Rename,
    /// Scrollbar slider grabbed `grab` rows below its top.
    Scroll(ScrollHit, u16),
    /// Minimap slider grabbed `grab` rows below its top.
    Minimap(MiniHit, u16),
    /// Selecting a PDF's text.
    Media,
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
    ReplaceOne,
    ReplaceAll,
    Case,
    Word,
    Regex,
    /// Search view toggles share this hit list.
    SearchCase,
    SearchWord,
    SearchRegex,
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
    /// Explorer rows with a tooltip: a cut name's full name, where the name starts, a file's
    /// path (its tooltip adds its MD5), a symlink's path (where it points).
    pub tips: Vec<TipRow>,
    /// The hovered cut row grown to its whole name (part of that row for the mouse).
    pub tip_ext: Rect,
    /// The hovered row's lines under it (a link's target, a file's MD5), and the MD5's Copy
    /// button.
    pub tip_md5: Rect,
    pub tip_copy: Rect,
    /// The read-only banner's way-to-show buttons: each one, and whether it is the hexdump.
    pub view_btns: Vec<(Rect, crate::media::As)>,
    /// A Markdown tab's preview and source buttons: group, the preview's.
    pub md_btns: Vec<(Rect, usize, bool)>,
    /// The Explorer row being renamed, its box and the display columns scrolled off it.
    pub rename: Rect,
    pub rename_field: (Rect, usize),
    pub close: Rect,
    pub settings_btn: Rect,
    pub activity: Vec<(Rect, Side)>,
    pub divider: Rect,
    pub side_list: Rect,
    pub side_input: Rect,
    pub commit_btn: Rect,
    /// The status bar row, its items on the right (each lit under the pointer), and
    /// Ln/Col among them (a click: Go to Line).
    pub status_bar: Rect,
    pub status_items: Vec<Rect>,
    pub status_line: Rect,
    /// The settings.json error notification and its Close button.
    pub notice_close: Rect,
    pub scm_btns: Vec<(Rect, ScmAct)>,
    pub search_folds: Vec<(Rect, usize)>,
    pub groups: Vec<GroupHit>,
    pub editor_area: Rect,
    pub find_btns: Vec<(Rect, FindBtn)>,
    pub find_inputs: Vec<(Rect, usize)>,
    pub find_area: Rect,
    pub prompt_btns: Vec<(Rect, usize)>,
    /// "Split Editor Right" button per group.
    pub split_btns: Vec<(Rect, usize, bool)>,
    /// A drop's preview as drawn (where the split it makes goes).
    pub drop_preview: Rect,
    /// The terminal: its toggle button per group, its panel's top border (the height
    /// handle) and its screen.
    pub term_btns: Vec<Rect>,
    pub term_sash: Rect,
    pub term: Rect,
    /// The panel's border: a tab per terminal (when more than one), + and ×.
    pub term_tabs: Vec<(Rect, usize)>,
    pub term_new: Rect,
    pub term_kill: Rect,
    /// The Outline: its toggle button per group, its left border (the width handle), its
    /// list, and each drawn row with where it goes.
    pub outline_btns: Vec<Rect>,
    pub outline_sash: Rect,
    pub outline: Rect,
    pub outline_rows: Vec<(Rect, usize)>,
    /// Every row the Outline shows (scrolled out of view too), and each drawn chevron.
    pub outline_items: Vec<OutlineItem>,
    pub outline_chevs: Vec<(Rect, usize)>,
    /// Empty editor shortcuts: group and optional sidebar (None opens Quick Open).
    pub welcome: Vec<(Rect, usize, Option<Side>)>,
    pub quick: Rect,
    pub quick_list: Rect,
    pub scrollbars: Vec<ScrollHit>,
    /// The sidebar, and how its list was laid out: items that fit, blank rows between items.
    pub sidebar: Rect,
    pub side_rows: usize,
    pub side_gap: u16,
    /// Sticky scroll: the list items pinned over slots 0, 1, …, how many may be, the
    /// rows they and their separator line take, and the breadcrumb's names.
    pub sticky: Vec<usize>,
    pub sticky_cap: usize,
    pub sticky_h: u16,
    pub sticky_segs: Vec<(Rect, usize)>,
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

    /// The pinned (sticky) item drawn at screen row `y`: its slot's item row and gap
    /// row; the separator line under them counts as the last one.
    pub fn sticky_at(&self, y: u16) -> Option<usize> {
        let rel = y.checked_sub(self.side_list.y).filter(|&r| r < self.sticky_h)?;
        let slot = (rel / (1 + self.side_gap)) as usize;
        self.sticky.get(slot.min(self.sticky.len().saturating_sub(1))).copied()
    }
}

#[derive(Default, Clone)]
pub struct GroupHit {
    pub area: Rect,
    pub tabbar: Rect,
    /// (title area, close-button cell) per tab drawn, from tab `tab_first`; the arrows
    /// shown when tabs are hidden left or right of them.
    pub tabs: Vec<(Rect, Rect)>,
    pub tab_first: usize,
    pub tab_prev: Rect,
    pub tab_next: Rect,
    /// Text area right of the gutter.
    pub text: Rect,
    /// The quick-diff marker column.
    pub marks_x: Option<u16>,
    pub folds_x: Option<u16>,
    /// The peek zone's rows (first row, height): text rows below it are shifted down.
    pub zone: Option<(u16, u16)>,
    /// A side-by-side diff: each side's text area (original, modified), for selecting.
    pub diff_text: Option<[Rect; 2]>,
    /// With word wrap: the (line, first display column) each text row shows, `None` on
    /// the peek zone's rows. Empty without wrap (a row is `scroll` + its offset).
    pub rows: Vec<Option<(usize, usize)>>,
    /// The breadcrumbs row (a click opens Go to Symbol).
    pub crumbs: Rect,
    /// A Markdown preview's rendered rows, for scrolling it.
    pub preview_rows: usize,
}

enum Load {
    Diff(DiffDoc),
    /// `git status` and the sync state, off the UI thread.
    Status(Status),
    /// Quick-diff base (the index blob) for a `file:` document.
    Base(Option<Vec<u8>>),
    Cancelled,
}

pub struct App {
    /// The workspace root: Explorer, Search, Quick Open and document paths.
    pub root: PathBuf,
    /// The git worktree holding the root (VS Code's parent-repo detection); None = no git.
    pub git_root: Option<PathBuf>,
    /// The repository git is kept off in until the user trusts it, and its settings
    /// that run programs (`trust`).
    pub untrusted: Option<(PathBuf, Vec<String>)>,
    /// The root relative to git_root, "" or "sub/dir/": git paths under the root start with it.
    prefix: String,
    /// Launcher requests carry this (see `on_paste`); None = take none.
    pub token: Option<String>,
    pub repo: String,
    pub branch: String,
    pub side: Side,
    pub side_visible: bool,
    /// Dragged sidebar width; None = `termcode.spacing.sidebarWidth`.
    pub side_w: Option<u16>,
    /// `side_w` came from a layout.json before version 2: a content width, not an outer one.
    pub side_w_legacy: bool,
    /// The integrated terminals (their shells kept while the panel is hidden), the one
    /// shown, whether the panel shows, and its dragged height (None: a third of the editor
    /// area).
    pub terms: Vec<crate::term::Term>,
    pub term_at: usize,
    /// Find in the shown terminal's scrollback.
    pub term_find: Option<crate::term::TermFind>,
    pub term_open: bool,
    pub term_h: Option<u16>,
    /// The Outline right of the editors: shown, its dragged width (None: 30 columns),
    /// its scroll, and the symbol it last followed the cursor to.
    /// Quit was confirmed over a program running in the terminal.
    pub term_quit_ok: bool,
    /// The folder an Explorer drag would drop into ("" the root), lit while it is over it;
    /// the item Ctrl+C (or Ctrl+X: `true`) took for Ctrl+V.
    pub tree_drop: Option<String>,
    pub tree_clip: Option<(Vec<String>, bool)>,
    /// The Explorer's selected items beyond the cursor's (Ctrl+click, Shift+click, Shift+↑↓).
    pub tree_marks: Vec<String>,
    /// Markdown previews as last rendered, by file: (what they were rendered from, lines).
    pub(crate) md_cache: HashMap<String, (u64, crate::mdpreview::Rendered)>,
    /// A Markdown tab's other look (source or preview), its view as it was, by its key.
    pub(crate) md_views: HashMap<String, View>,
    /// A patch's diff, by key: what it was made from (patch.rs).
    pub(crate) patch_stamps: HashMap<String, u64>,
    /// Where a Shift+click or Shift+↑↓ range starts.
    pub tree_anchor: Option<String>,
    /// A dragged item resting on this closed folder opens it at this time.
    pub tree_spring: Option<(String, Instant)>,
    /// A paste running on its thread.
    pub file_job: Option<rename::FileJob>,
    /// The last mouse event's modifiers (Ctrl+click, a Ctrl+drag's copy).
    pub mouse_mods: KeyModifiers,
    /// The Trash Delete moves to (freedesktop's: $XDG_DATA_HOME/Trash).
    pub(crate) trash: PathBuf,
    /// Explorer file operations Ctrl+Z can take back, and those it took back (Ctrl+Y).
    pub file_undo: Vec<Vec<rename::FileOp>>,
    pub file_redo: Vec<Vec<rename::FileOp>>,
    /// The GitHub view and its status bar item.
    pub gh: crate::github::GitHub,
    /// The STM32 Programmer view, its page and its log.
    pub prog: crate::programmer::Programmer,
    /// The Serial view's ports and open channels.
    /// The extensions installed (ext.rs).
    pub exts: crate::ext::Exts,
    /// This termcode's socket (`termcode PATH` from its terminals); a herdr tab's pane.
    pub api: Option<crate::api::Server>,
    /// What API clients may do, as the user answered.
    pub api_gate: crate::api::Gate,
    /// Each space's active group when last left; the code view last shown (to go back to).
    space_ag: std::collections::HashMap<Space, usize>,
    code_side: Side,
    pub tab: bool,
    /// A switch to another folder asked for: termcode starts again there (this argument).
    pub reroot: Option<String>,
    /// The editor last shown in the Explorer (explorer.autoReveal).
    revealed: Option<String>,
    /// Pictures and PDFs in editor tabs: their zoom, pages, pixels.
    pub media: crate::media::State,
    /// Log files' log looks: their lines' index, filters (logview.rs).
    pub logs: crate::logview::State,
    pub outline_open: bool,
    pub outline_w: Option<u16>,
    pub outline_off: usize,
    pub outline_cur: Option<usize>,
    /// The Outline's selected row (its tree path), and the folded ones ("key\npath").
    pub outline_sel: Option<String>,
    pub outline_folded: std::collections::HashSet<String>,
    pub(crate) layout_file: Option<PathBuf>,
    pub focus: Focus,
    pub status: Vec<StatusEntry>,
    pub marks: HashMap<String, char>,
    pub dirty_dirs: HashMap<String, char>,
    /// Staged files first, then Changes.
    pub scm_files: Vec<ScmFile>,
    /// Folded Source Control groups: Merge Changes, Staged Changes, Changes.
    pub scm_folded: [bool; 3],
    pub tree: Vec<Node>,
    pub query: String,
    /// The whole Search query is selected (Ctrl+A, a double-click): copied by Ctrl+C,
    /// cut by Ctrl+X, replaced by typing or a paste.
    pub query_all: bool,
    /// Search toggles (Match Case, Whole Word, Regex); all off like VS Code.
    pub s_opts: (bool, bool, bool),
    pub hits: Vec<Hit>,
    pub search_closed: HashSet<String>,
    pub search: Option<Child>,
    /// Tells the running search's reader thread to stop.
    search_stop: Arc<AtomicBool>,
    /// rg processes not yet reaped (running or abandoned), Search and nav lookups alike.
    pub(crate) live_rg: Arc<AtomicUsize>,
    /// A search waits until fewer than MAX_LIVE_RG rg processes are left.
    pub queued_search: Option<(String, (bool, bool, bool))>,
    /// The query changed: it runs at this time unless it changes again first.
    search_due: Option<Instant>,
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
    /// The prefix (termcode.prefix) pressed: the next key is a quick key.
    pub prefix_armed: bool,
    /// A minimap slider held at (or past) its edge: how far (rows, up negative) and the
    /// last step; the editor scrolls on by itself, faster the farther (`minimap_auto`).
    pub(crate) mini_auto: Option<(i32, Instant)>,
    pub drop: Option<(usize, Zone)>,
    pub(crate) last_click: Option<(Instant, Position)>,
    pub tip: Option<Tip>,
    pub rename: Option<Rename>,
    /// Clicks in a row on one spot (2: a word, 3: a line, in a diff).
    clicks: u8,
    pub hover: Option<Position>,
    /// A button held down (a tab bar's arrow): drawn pressed until the mouse is let go.
    pub pressed: Option<Rect>,
    pending_k: bool,
    /// Running a `Cmd::Keys`: its key does what it does by default, past the keybindings.
    replaying: bool,
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
    /// The activity bar was clicked last: its card has the focus border, until the next
    /// key or click elsewhere hands it back to `focus`'s card.
    pub activity_lit: bool,
    /// Poll `git status` (the real panel only, never a test), when next, how long the
    /// last one took, whether the running one is a poll, and what the last one said.
    pub poll_status: bool,
    status_poll_due: Option<Instant>,
    status_took: Duration,
    status_polled: bool,
    status_seen: Option<(String, Vec<u8>, String)>,
    pub(crate) clip: String,
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
    /// Quit to exec the installed binary in place (`run`), not to end.
    pub reload: bool,
    pub upgrade: Upgrade,
    pub ui: Hits,
    pub settings: Settings,
    pub settings_dlg: Option<crate::settingsui::Dialog>,
    /// settings.json; None in tests unless set.
    pub settings_file: Option<PathBuf>,
    /// mtimes of settings.json (and VS Code's, when followed) at the last load.
    settings_stamp: Option<(Stamp, Stamp)>,
    /// The last load failed: shown in the status bar until a good save.
    pub settings_err: Option<String>,
    /// The settings error the user closed the notification of (shown again if it changes).
    pub settings_err_closed: Option<String>,
    /// The theme last installed, so an unrelated settings change does not recolour.
    installed: ThemeData,
    /// Command-palette toggles, reset from settings.json on each load.
    pub rainbow_on: bool,
    pub guides_on: bool,
    pub ruler_on: bool,
    pub minimap_on: bool,
    pub rulers_on: bool,
    pub breadcrumbs_on: bool,
    /// Alt+Z's word wrap, over `editor.wordWrap` until settings reload.
    pub wrap_on: Option<bool>,
    /// Palette commands, most recent first.
    pub recent_cmds: Vec<String>,
    pub peek: Option<Peek>,
    pub qdiff: HashMap<String, QDiff>,
    qdiff_due: Option<Instant>,
    /// Each Explorer row's parent folder row (usize::MAX at the top), for sticky
    /// scroll; empty until needed and after the tree changes.
    tree_parents: std::cell::RefCell<Vec<usize>>,
    /// Per diff tab: the modified side as a buffer (one line per row) and each
    /// row's mark, for its minimap; rebuilt when the doc is replaced.
    pub diff_minis: HashMap<String, (DiffId, Buffer, Vec<Option<Mark>>)>,
    pub diff_syntax: HashMap<String, crate::ui::DiffSyntax>,
    /// Go to Definition / Peek / References state.
    pub nav: crate::navui::Nav,
    /// Source Control view, Timeline, hover card and commit dialog state.
    pub gitui: crate::scm::GitUi,
    /// Problems from shellcheck and the C/C++ compiler (`lint`).
    pub lints: crate::lint::Lints,
    /// The saved session of the launch folder (`session`).
    pub session: crate::session::State,
}

/// The TUI on this process's terminal for `root`, first opening `req`
/// (`open:<path>:<line>:<col>`, see `request`). `tab`: the plugin's tab pane,
/// which also takes requests from `termcode --tab FILE`; in place it does not.
/// Every exit restores the terminal, so in place the shell gets it back.
pub fn run(root: PathBuf, key: PathBuf, req: Option<String>, tab: bool) -> Result<(), String> {
    // A termcode started in another's terminal is its own: its shells get its socket.
    std::env::remove_var(crate::api::ENV);
    let state = crate::data_dir(true);
    let mut private: Vec<PathBuf> = vec![state.clone(), crate::data_dir(false)];
    private.extend(["backup", "drafts", "sessions", "terminals"].map(|d| state.join(d)));
    crate::private_dirs(&private)?;
    let mut app = App::new(root, Some(state.join("layout.json")));
    app.tab = tab;
    app.api = crate::api::start(&crate::api::dir());
    if let Some(s) = &app.api {
        crate::api::arm(s);
    }
    app.settings_file = Some(settings::path());
    if app.untrusted.is_some() {
        app.msg = "Git is off: this repository's own settings run programs (see Source Control).".into();
    }
    if take_crash(&state) {
        app.msg = "termcode hit an error last time. Please report it: F1 Help: Report Issue copies the details.".into();
    }
    // A first start leaves a settings.json to edit, every option listed at its default.
    let _ = settings::create(&settings::path());
    app.check_settings();
    // In place and in a tab alike: the same state dir, one session per launch folder
    // (`key`: where termcode was started; the root itself for a PATH).
    app.session.file = Some(crate::session::path(&state, &key));
    // A reload brings the session back whatever the settings say, and is not a new launch:
    // its FILE:L was opened already.
    let mut reloaded = std::env::var_os(RELOAD_ENV).is_some();
    std::env::remove_var(RELOAD_ENV); // not for git and rg
    // A herdr-code (termcode's old name) reloading into termcode is a reload too, and its
    // session, saved just now, is in the old folder.
    if std::env::var_os("HERDR_CODE_RELOADED").is_some() {
        std::env::remove_var("HERDR_CODE_RELOADED");
        reloaded = true;
        if let Some(new) = &app.session.file {
            let _ = fs::copy(crate::session::path(&crate::old_data_dir(true), &key), new);
        }
    }
    let wanted = (app.settings.restore_session, app.settings.restore_view_state);
    if reloaded {
        app.session.from = app.session.file.as_deref().map(crate::session::reload_file).filter(|f| f.exists());
        (app.settings.restore_session, app.settings.restore_view_state) = (true, true);
    }
    crate::session::restore(&mut app);
    crate::session::clean_stale(&state);
    (app.settings.restore_session, app.settings.restore_view_state) = wanted;
    app.upgrade.watch = true;
    if let Ok(v) = std::env::var(RELOAD_TERMS_ENV) {
        std::env::remove_var(RELOAD_TERMS_ENV); // not for the shells started later
        let dir = crate::data_dir(true).join("terminals");
        let mut parts = v.splitn(3, '|');
        let shown = parts.next();
        let open = matches!(shown, Some("1" | "2"));
        let at = parts.next().and_then(|a| a.parse().ok()).unwrap_or(0usize);
        app.terms = parts.next().unwrap_or("").split(';').filter_map(|s| crate::term::Term::adopt(s, &dir)).collect();
        app.term_at = at.min(app.terms.len().saturating_sub(1));
        app.term_open = open && !app.terms.is_empty();
        app.reap_strays();
        if app.term_open && shown == Some("2") {
            app.focus = Focus::Term;
        }
    } else if std::env::var_os(RELOAD_TERM_ENV).is_some() {
        // From a termcode before 0.3: new shells, as many.
        let n = std::env::var(RELOAD_TERM_ENV).ok().and_then(|n| n.parse().ok()).unwrap_or(1usize);
        std::env::remove_var(RELOAD_TERM_ENV); // not for the new shells
        let focus = app.focus;
        for _ in 0..n.clamp(1, 16) {
            app.new_terminal();
        }
        app.focus = focus;
    }
    app.poll_status = true;
    if reloaded {
        app.msg = "reloaded".into();
    }
    // `termcode FILE:L` opens on top of the restored tabs.
    if let Some(req) = req.filter(|_| !reloaded) {
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
        let name = std::thread::current().name().unwrap_or("job").to_string();
        log_panic(&crate::data_dir(true), &format!("thread '{name}' {info}"));
        if panic_restores(main) {
            restore();
            hook(info);
            // Not eprintln!: with the terminal gone that panics inside this hook (an abort).
            let _ = writeln!(
                io::stderr(),
                "\ntermcode crashed. Unsaved files are kept and offered back when you open them again.\n\
                 Please report it: {}/issues/new/choose (F1 Help: Report Issue in termcode copies the details).",
                crate::ISSUES
            );
        } else {
            JOB_PANICKED.store(true, Ordering::SeqCst);
        }
    }));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // One write per frame (ratatui flushes the backend after each draw): herdr then
        // gets whole frames, not ~14 line-buffered pieces.
        let out = io::BufWriter::with_capacity(1 << 16, Gate { w: io::stdout(), off: &RESTORED });
        Terminal::new(CrosstermBackend::new(out)).and_then(|mut t| {
            let r = app.event_loop(&mut t);
            // ratatui's drop shows the cursor and, failing (the terminal gone: its window
            // closed, its tmux session killed), eprintln!s that, which panics with no
            // stderr either. Shown here; failing, its drop is skipped (restore() resets).
            if t.show_cursor().is_err() {
                std::mem::forget(t);
            }
            r
        })
    }));
    // Keys typed into the terminal as it switched or reloaded, read already: to its shell,
    // not lost with this process (the next command typed right after `termcode FOLDER`).
    if app.reload && app.focus == Focus::Term && matches!(res, Ok(Ok(()))) {
        while matches!(event::poll(Duration::ZERO), Ok(true)) {
            match event::read() {
                Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => app.term_key(k),
                Ok(Event::Paste(text)) => app.paste_text(&text),
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    // The terminal first: saving backups and the session never holds the shell back.
    restore();
    finish(&mut app);
    // Extensions told to end, and gone, before this process quits or becomes another.
    crate::ext::shutdown(&mut app);
    leave(&mut app, token_file, SIGNALED.load(Ordering::SeqCst) || res.is_err());
    // A reload: this process becomes the installed binary, same arguments, same pane.
    if app.reload && matches!(res, Ok(Ok(()))) && !SIGNALED.load(Ordering::SeqCst) {
        // The shells go on running: their PTYs cross the exec, their screens a file each.
        if !app.terms.is_empty() {
            let dir = crate::data_dir(true).join("terminals");
            let _ = std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700).create(&dir);
            let specs: Vec<String> = app.terms.iter_mut().filter_map(|t| t.hand_over(&dir).ok()).collect();
            // 1: the panel showed; 2: and the terminal had the keys (`termcode FOLDER` typed there).
            let shown = if app.term_open && app.focus == Focus::Term { 2 } else { u8::from(app.term_open) };
            std::env::set_var(RELOAD_TERMS_ENV, format!("{shown}|{}|{}", app.term_at, specs.join(";")));
        }
        // A switch to another folder: termcode as launched there (its own session), not a
        // reload of this one.
        let reroot = app.reroot.take();
        let own = if reroot.is_none() { crate::session::save_for_reload(&app) } else { None };
        let e = reexec(reroot);
        if let Some(f) = own {
            let _ = fs::remove_file(f);
        }
        return Err(format!("reload failed: {e}"));
    }
    // Quitting just ends this process: herdr then closes the tab. Nothing here
    // signals or closes anything else.
    crate::api::remove();
    match res {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(r) => r.map_err(|e| e.to_string()),
    }
}

/// Exec the installed binary (the path the running one was started from) with this
/// process's arguments (a switch: `--new FOLDER`, as a launch there); returns only when
/// that fails.
fn reexec(reroot: Option<String>) -> io::Error {
    use std::os::unix::process::CommandExt;
    let exe = match fs::read_link("/proc/self/exe") {
        Ok(l) => replaced(&l).unwrap_or(l),
        Err(e) => return e,
    };
    let mut args = std::env::args_os();
    let arg0 = args.next().unwrap_or_else(|| exe.clone().into_os_string());
    if let Some(arg) = reroot {
        return Command::new(&exe).arg0(arg0).args(["--new".into(), arg]).exec();
    }
    Command::new(&exe).arg0(arg0).args(args).env(RELOAD_ENV, "1").exec()
}

/// Set across a reload by a termcode before 0.3 when its terminal showed: it opens again
/// (new shells).
const RELOAD_TERM_ENV: &str = "TERMCODE_RELOAD_TERMINAL";
/// Set across a reload: whether the panel showed, the one shown, and each terminal kept
/// ("fd,pid,rows,cols;…", see `Term::hand_over`).
const RELOAD_TERMS_ENV: &str = "TERMCODE_RELOAD_TERMINALS";

/// Every exit path (quit, HUP/TERM, an error, a panic) leaves the unsaved edits
/// and the session on disk.
fn finish(app: &mut App) {
    app.sync_backups();
    crate::session::save_guarded(app);
}

/// The exit once the terminal is restored. Git writes still queued or running finish
/// before the process ends (after HUP/TERM or a panic, `hurry`: 2 s at most), everything
/// else done first: Ctrl+C in that wait ends the process (SIGINT is not caught).
fn leave(app: &mut App, token_file: Option<PathBuf>, hurry: bool) {
    app.stop_search();
    if let Some(p) = token_file.filter(|_| app.token.is_some()) {
        let _ = fs::remove_file(p);
    }
    if app.gitui.writes() > 0 {
        crate::scm::finish_writes(app, hurry.then_some(Duration::from_secs(2)));
    }
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

/// A panic, appended to `panic.log` in `dir` with the time and version (best effort).
fn log_panic(dir: &Path, msg: &str) {
    let _ = fs::create_dir_all(dir);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join("panic.log")) {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let _ = writeln!(f, "{} termcode {}: {msg}", crate::scm::local_time(now), env!("CARGO_PKG_VERSION"));
    }
}

/// A job thread panicked since the last look (the UI says so once).
static JOB_PANICKED: AtomicBool = AtomicBool::new(false);

/// At start: whether the last run left errors in `panic.log`. They move to
/// `panic-reported.log` (replacing older ones), so this is said once and the log stays small.
fn take_crash(dir: &Path) -> bool {
    let log = dir.join("panic.log");
    let had = fs::metadata(&log).is_ok_and(|m| m.len() > 0);
    if had {
        let _ = fs::rename(&log, dir.join("panic-reported.log"));
    }
    had
}

/// The newest error in `dir`'s logs, for a bug report: where it happened and its message
/// (one line each, cut short).
fn last_error(dir: &Path) -> Option<String> {
    let text = ["panic.log", "panic-reported.log"].iter().find_map(|f| fs::read_to_string(dir.join(f)).ok().filter(|t| !t.trim().is_empty()))?;
    let start = text.rfind(" termcode ").and_then(|i| text[..i].rfind('\n').map(|n| n + 1).or(Some(0)))?;
    let entry: Vec<&str> = text[start..].lines().take(2).collect();
    Some(entry.iter().map(|l| l.chars().take(160).collect::<String>()).collect::<Vec<_>>().join(" / "))
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

/// Whether a key acts at every cursor (or adds one). Any other key leaves one cursor.
fn keeps_cursors(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> bool {
    match code {
        KeyCode::Char('v') if ctrl => !alt && !shift,
        KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete => !ctrl && !alt,
        KeyCode::Left | KeyCode::Right => !alt && !shift,
        KeyCode::Up | KeyCode::Down => !alt && ctrl == shift,
        KeyCode::Home | KeyCode::End => !ctrl && !alt && !shift,
        _ => false,
    }
}

/// What a wait on the terminal's input found. `Shell`: no input, but the integrated
/// terminal's shell wrote (or closed it).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Input {
    Ready,
    Idle,
    Gone,
    Shell,
}

/// Wait up to `wait` for input on `fd`, or for the integrated terminal's shell. A signal
/// ends the wait early, as `Idle`.
/// termcode with no terminal (`--headless`): the same App and event loop, its frames
/// drawn offscreen (120 × 40: `/ui/screen` reads them), driven through the API alone: its
/// socket, and (`stdio`) JSON-RPC on stdin and stdout. Ends on File: Quit
/// (`command.run file.quit`), stdin closed (with `stdio`), a hang-up or TERM.
pub fn run_headless(root: PathBuf, stdio: bool) -> Result<(), String> {
    HEADLESS.store(true, Ordering::SeqCst);
    std::env::remove_var(crate::api::ENV);
    let state = crate::data_dir(true);
    let mut private: Vec<PathBuf> = vec![state.clone(), crate::data_dir(false)];
    private.extend(["backup", "drafts", "sessions", "terminals"].map(|d| state.join(d)));
    crate::private_dirs(&private)?;
    let mut app = App::new(root, None);
    app.api = crate::api::start(&crate::api::dir());
    let Some(srv) = &app.api else { return Err("the API's socket could not be made".into()) };
    crate::api::arm(srv);
    if stdio {
        crate::api::serve_stdio(srv);
    }
    // (stderr: stdout is the API's, with stdio.)
    let _ = writeln!(io::stderr(), "termcode headless: pid {} on {} (termcode api --to {} …)", std::process::id(), app.root.display(), std::process::id());
    app.settings_file = Some(settings::path());
    app.check_settings();
    app.poll_status = true;
    for sig in [libc::SIGHUP, libc::SIGTERM] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(sig, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    let mut t = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).map_err(|e| e.to_string())?;
    let mut dirty = true;
    while !app.quit && !SIGNALED.load(Ordering::SeqCst) && !crate::api::END.load(Ordering::SeqCst) {
        app.step(&mut t, &mut dirty, |wait| {
            wait_fds(wait);
            Ok(None)
        })
        .map_err(|e| e.to_string())?;
    }
    finish(&mut app);
    crate::ext::shutdown(&mut app);
    crate::api::remove();
    Ok(())
}

/// No terminal here (`--headless`): nothing may be written to stdout but the API's
/// replies (no terminal escapes, no clipboard by OSC 52), and no one answers windows.
pub static HEADLESS: AtomicBool = AtomicBool::new(false);

/// Headless's wait: for a terminal's or a port's output, a request, or `wait`.
fn wait_fds(wait: Duration) {
    let mut polls: Vec<libc::pollfd> = crate::term::fds().into_iter().chain(crate::ext::fds()).chain(crate::api::wake_fd()).map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let ms = wait.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
    // SAFETY: valid pollfds, as many as given.
    unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, ms) };
}

fn wait_input(fd: libc::c_int, wait: Duration) -> Input {
    // The input, then every terminal's PTY.
    // (The socket's wake-up pipe counts as a shell's output: the loop turns at once.)
    let mut polls: Vec<libc::pollfd> = std::iter::once(fd).chain(crate::term::fds()).chain(crate::ext::fds()).chain(crate::api::wake_fd()).map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let ms = wait.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
    // SAFETY: valid pollfds, as many as given.
    if unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, ms) } <= 0 {
        return Input::Idle;
    }
    let shell = polls[1..].iter().any(|p| p.revents != 0);
    match polls[0].revents {
        r if r & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 => Input::Gone,
        0 if shell => Input::Shell,
        0 => Input::Idle,
        _ => Input::Ready,
    }
}

/// The next input event, waiting up to `wait`. crossterm is only ever asked without
/// waiting, after a look at the terminal: once the terminal is gone its read loop never
/// returns. A vanished terminal ends the event loop the way HUP does.
fn next_event(wait: Duration) -> io::Result<Option<Event>> {
    for wait in [Duration::ZERO, wait] {
        let got = wait_input(libc::STDIN_FILENO, wait);
        if got == Input::Gone {
            SIGNALED.store(true, Ordering::SeqCst);
            return Ok(None);
        }
        if event::poll(Duration::ZERO)? {
            return event::read().map(Some);
        }
        // The shell's output: the loop reads and draws it, then waits again.
        if got == Input::Shell {
            return Ok(None);
        }
    }
    Ok(None)
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
    // Tests never touch the user's clipboard (they read `App::clip`).
    if cfg!(test) {
        return "test";
    }
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
    if HEADLESS.load(Ordering::SeqCst) {
        return "nothing (headless)";
    }
    let mut out = io::stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", parse::base64(text.as_bytes()));
    let _ = out.flush();
    "OSC 52"
}

/// System clipboard text via wl-paste or xclip, each given 500 ms.
pub(crate) fn clipboard_get() -> Option<String> {
    // Tests never read the user's clipboard either (`App::clip` stands in).
    if cfg!(test) {
        return None;
    }
    // The tool that worked last time first: a missing or hung first one is not waited for
    // on every paste.
    static LAST: AtomicUsize = AtomicUsize::new(0);
    let tools = [&["wl-paste", "-n"][..], &["xclip", "-o", "-selection", "clipboard"][..]];
    let first = LAST.load(Ordering::Relaxed);
    for k in [first, 1 - first] {
        let argv = tools[k];
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
                        LAST.store(k, Ordering::Relaxed);
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

/// Row `i`'s text on `side` of a diff (0 original, 1 modified); None on a filler row.
pub fn diff_cell(d: &DiffDoc, i: usize, side: u8) -> Option<&str> {
    let r = d.rows.get(i)?;
    (if side == 0 { &r.left } else { &r.right }).as_ref().map(|(_, t)| t.as_str())
}

/// A diff selection's text: that side's text of its rows, filler rows skipped, no
/// line numbers or +/- signs.
pub fn diff_selected_text(d: &DiffDoc, v: &View) -> Option<String> {
    let (a, b) = v.selection()?;
    let rows = (a.line..=b.line).filter_map(|i| {
        let t = diff_cell(d, i, v.side)?;
        let start = if i == a.line { a.col.min(t.len()) } else { 0 };
        let end = if i == b.line { b.col.min(t.len()) } else { t.len() };
        Some(t.get(start..end.max(start)).unwrap_or(""))
    });
    Some(rows.collect::<Vec<_>>().join("\n"))
}

pub fn base(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

impl App {
    /// Why termcode must not switch to another folder now (`termcode FOLDER` from its
    /// terminal), if anything would be cut short or lost: a flash, a git write, unsaved
    /// edits.
    pub fn busy_for_switch(&self) -> Option<String> {
        if self.prog.busy_job() {
            return Some("a Programmer job is running".into());
        }
        if self.gitui.writes() > 0 {
            return Some("Git is still writing".into());
        }
        let dirty = self.docs.values().filter(|d| matches!(d, Doc::Text(b) if b.dirty())).count();
        (dirty > 0).then(|| format!("{dirty} unsaved file{}: save {} first", if dirty == 1 { "" } else { "s" }, if dirty == 1 { "it" } else { "them" }))
    }

    pub fn new(root: PathBuf, layout_file: Option<PathBuf>) -> App {
        let (tx, rx) = mpsc::sync_channel(MAX_HITS + 1);
        let (load_tx, load_rx) = mpsc::channel();
        let repo = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut git_root = crate::repo_root(&root);
        // A repository whose own settings run programs gets no git until it is trusted.
        let untrusted = git_root.as_ref().and_then(|g| {
            let risky = crate::trust::risky(g);
            (!risky.is_empty() && !crate::trust::trusted(g, &risky)).then(|| (g.clone(), risky))
        });
        if untrusted.is_some() {
            git_root = None;
        }
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
        let gh_here = git_root.as_deref().and_then(crate::github::detect);
        let mut a = App {
            root,
            git_root,
            untrusted,
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
            terms: Vec::new(),
            term_at: 0,
            term_find: None,
            term_open: false,
            term_h: saved["terminal_height"].as_u64().map(|h| h.clamp(3, 500) as u16),
            term_quit_ok: false,
            tree_drop: None,
            tree_clip: None,
            tree_marks: Vec::new(),
            md_cache: HashMap::new(),
            md_views: HashMap::new(),
            patch_stamps: HashMap::new(),
            tree_anchor: None,
            tree_spring: None,
            file_job: None,
            mouse_mods: KeyModifiers::NONE,
            // Tests never reach the user's Trash.
            file_undo: Vec::new(),
            file_redo: Vec::new(),
            trash: if cfg!(test) { std::env::temp_dir().join(format!("termcode-test-trash-{}", std::process::id())) } else { crate::data_home().join("Trash") },
            prog: Default::default(),
            api: None,
            api_gate: crate::api::Gate::from_env(),
            space_ag: std::collections::HashMap::new(),
            code_side: Side::Explorer,
            exts: crate::ext::Exts::load(),
            tab: false,
            reroot: None,
            revealed: None,
            media: Default::default(),
            logs: Default::default(),
            gh: crate::github::GitHub::new(
                gh_here,
                saved["github_repo"].as_str().map(String::from),
                saved["github_split"].as_u64().map(|h| h.clamp(1, 500) as u16),
            ),
            outline_open: false,
            outline_w: saved["outline_width"].as_u64().map(|w| w.clamp(16, 500) as u16),
            outline_off: 0,
            outline_cur: None,
            outline_sel: None,
            outline_folded: Default::default(),
            layout_file,
            focus: Focus::Side,
            status: Vec::new(),
            marks: HashMap::new(),
            dirty_dirs: HashMap::new(),
            scm_files: Vec::new(),
            scm_folded: [false; 3],
            tree: Vec::new(),
            query: String::new(),
            query_all: false,
            s_opts: (false, false, false),
            hits: Vec::new(),
            search_closed: HashSet::new(),
            search: None,
            search_stop: Arc::new(AtomicBool::new(false)),
            live_rg: Arc::new(AtomicUsize::new(0)),
            queued_search: None,
            search_due: None,
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
            prefix_armed: false,
            mini_auto: None,
            drop: None,
            last_click: None,
            tip: None,
            rename: None,
            clicks: 0,
            hover: None,
            pressed: None,
            pending_k: false,
            replaying: false,
            pending_ev: None,
            last_draw: None,
            refresh_due: None,
            status_again: false,
            status_since: None,
            activity_lit: false,
            poll_status: false,
            status_poll_due: None,
            status_took: Duration::ZERO,
            status_polled: false,
            status_seen: None,
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
            reload: false,
            upgrade: Upgrade::default(),
            ui: Hits::default(),
            settings: Settings::default(),
            settings_dlg: None,
            settings_file: None,
            settings_stamp: None,
            settings_err: None,
            settings_err_closed: None,
            installed: Settings::default().theme,
            rainbow_on: true,
            guides_on: true,
            ruler_on: true,
            minimap_on: true,
            rulers_on: true,
            breadcrumbs_on: true,
            wrap_on: None,
            recent_cmds: saved["recent_commands"].as_array().map_or_else(Vec::new, |a| {
                a.iter().filter_map(|c| c.as_str().map(str::to_string)).take(MAX_RECENT).collect()
            }),
            peek: None,
            qdiff: HashMap::new(),
            qdiff_due: None,
            tree_parents: Default::default(),
            diff_minis: HashMap::new(),
            diff_syntax: HashMap::new(),
            nav: Default::default(),
            gitui: crate::scm::GitUi::new(&saved),
            lints: Default::default(),
            session: Default::default(),
        };
        a.backup_dir = a.layout_file.as_ref().and_then(|p| p.parent()).map(|d| d.join("backup"));
        // The first frame shows the real status: read it here, once, synchronously.
        a.refresh_files();
        if let Some(g) = a.git_root.clone() {
            // The index stat refresh is the writer's first job (slow on a big repo).
            let gw = g.clone();
            a.gitui.write("refresh the index", move || {
                crate::gitops::refresh_index(&gw);
                crate::scm::Msg::Wrote(Ok(String::new()), false)
            });
            a.apply_status(a.gitui.logged(|| read_status(&g)));
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
        if self.settings_dlg.is_some() { return; }
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
        theme::set_icons(s.icons.unwrap_or_else(theme::auto_icons));
        crate::colors::set(s.colors);
        // Tests run no checker unless one turns it on (`lint::Lints`).
        self.lints.on = s.error_checking && !cfg!(test);
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
        self.breadcrumbs_on = s.breadcrumbs;
        self.wrap_on = None;
        if !s.warnings.is_empty() {
            self.msg = format!("settings.json: {}", s.warnings.join("; "));
        }
        self.settings = s;
    }

    /// "Help: Report Issue": what a bug report needs about this setup, and nothing private
    /// (no paths, file or repository names).
    fn report_details(&self) -> String {
        let os = fs::read_to_string("/etc/os-release").ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_else(|| "unknown".into());
        let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "-".into());
        // "git 2.53.0": the first word of `--version` that starts with a digit.
        let tool = |prog: &str| {
            let v = Command::new(prog).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())
                .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().find(|w| w.starts_with(|c: char| c.is_ascii_digit())).map(str::to_string));
            format!("{prog} {}", v.as_deref().unwrap_or("not installed"))
        };
        let inside: Vec<&str> = [("HERDR_ENV", "herdr"), ("TMUX", "tmux"), ("SSH_CONNECTION", "ssh")]
            .into_iter().filter(|(k, _)| std::env::var_os(k).is_some()).map(|(_, n)| n).collect();
        let s = &self.settings;
        let error = if cfg!(test) { None } else { last_error(&crate::data_dir(true)) };
        let icons = match s.icons { Some(i) => format!("{i:?}"), None => format!("auto ({:?})", theme::icon_set()) };
        format!(
            "termcode {}\nOS: {os}, Linux {}, {}\nTerminal: TERM={} TERM_PROGRAM={} {}\nInside: {}\n\
             Settings: icons {icons}, colors {:?}, density {}, blocks {:?}, error checking {}\nTools: {}, {}, {}, {}\n{}",
            env!("CARGO_PKG_VERSION"),
            kernel.trim(),
            std::env::consts::ARCH,
            env("TERM"),
            env("TERM_PROGRAM"),
            env("TERM_PROGRAM_VERSION"),
            if inside.is_empty() { "a plain terminal".to_string() } else { inside.join(", ") },
            crate::colors::depth(),
            if s.compact { "compact" } else { "comfortable" },
            s.blocks,
            if s.error_checking { "on" } else { "off" },
            tool("git"),
            tool("rg"),
            tool("shellcheck"),
            tool("clang"),
            error.map_or(String::new(), |e| format!("Last error: {e}\n")),
        )
    }

    /// Why git is not running here, for a git command asked for anyway.
    pub fn no_git_msg(&self) -> String {
        if self.untrusted.is_some() {
            "Git is off until you trust this repository (see Source Control).".into()
        } else {
            "The folder currently open doesn't have a git repository.".into()
        }
    }

    /// Trust the repository git was kept off in, then reload so git starts with it.
    pub fn trust_repo(&mut self) {
        let Some((repo, risky)) = self.untrusted.clone() else { return };
        if let Err(e) = crate::trust::trust(&repo, &risky) {
            self.msg = format!("could not save the trust: {e}");
        } else if self.unsaved() > 0 {
            self.msg = "Trusted. Save your files, then F1 Developer: Reload Window to turn git on.".into();
        } else {
            (self.reload, self.quit) = (true, true);
        }
    }

    /// "Preferences: Open Settings (JSON)": the file, created from a commented template.
    fn open_settings(&mut self) {
        let Some(path) = self.settings_file.clone() else {
            self.msg = "no settings file in this session".into();
            return;
        };
        if let Err(e) = settings::create(&path) {
            self.msg = format!("{}: {e}", path.display());
            return;
        }
        self.open_file(self.ag, &path.to_string_lossy(), false);
        self.focus = Focus::Editor;
    }

    pub(crate) fn run_command(&mut self, c: Cmd) {
        let on_off = |b: bool| if b { "on" } else { "off" };
        match c {
            Cmd::ShowSide(side) => self.show_side(side, side == Side::Search),
            Cmd::Prog(op) => crate::programmer::command(self, op),
            Cmd::Quit => self.request_quit(),
            Cmd::Find(replace) => self.open_find(replace),
            Cmd::Keys(code, mods) => {
                self.replaying = true;
                self.on_key(KeyEvent::new(code, mods));
                self.replaying = false;
            }
            Cmd::ReportIssue => {
                let text = self.report_details();
                let how = clipboard_set(&text);
                self.msg = format!("Report details copied ({how}): paste them at {}/issues/new/choose", crate::ISSUES);
                self.clip = text;
            }
            Cmd::Fold | Cmd::Unfold | Cmd::FoldAll | Cmd::UnfoldAll => self.fold_command(c, None),
            Cmd::OpenSettings => self.open_settings(),
            Cmd::SettingsPanel => crate::settingsui::open(self),
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
            Cmd::ToggleTerminal => self.toggle_terminal(),
            Cmd::NewTerminal => self.new_terminal(),
            Cmd::KillTerminal => self.kill_terminal(self.term_at, true),
            Cmd::NextTerminal(next) => {
                let n = self.terms.len().max(1);
                self.switch_terminal((self.term_at + if next { 1 } else { n - 1 }) % n);
            }
            Cmd::TermFind => self.term_find_open(),
            Cmd::ToggleOutline => {
                self.outline_open = !self.outline_open;
                if !self.outline_open && self.focus == Focus::Outline {
                    self.focus = Focus::Editor;
                }
            }
            Cmd::FocusOutline => (self.outline_open, self.focus) = (true, Focus::Outline),
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
            Cmd::GotoLine => self.open_goto(":"),
            Cmd::GotoSymbol => self.open_goto("@"),
            Cmd::ToggleComment => self.toggle_comment(),
            Cmd::ToggleHex => self.toggle_hex(),
            Cmd::ToggleWrap => {
                let on = !self.active_tab_ref().is_some_and(|t| self.wraps(&t.key));
                self.wrap_on = Some(on);
                self.msg = format!("word wrap {}", on_off(on));
                self.reveal();
            }
            Cmd::ToggleBreadcrumbs => {
                self.breadcrumbs_on = !self.breadcrumbs_on;
                self.msg = format!("breadcrumbs {}", on_off(self.breadcrumbs_on));
            }
            Cmd::MdPreview(side) => self.md_preview(side),
            Cmd::ReloadWindow => {
                if self.unsaved() > 0 {
                    self.msg = "save the unsaved files first: a reload starts without undo history".into();
                } else {
                    (self.reload, self.quit) = (true, true);
                }
            }
            Cmd::Navigate(dir) => self.navigate(dir),
            Cmd::Split(down) => self.split(self.ag, down),
            Cmd::CloseTerminal if self.terms.len() > 1 => self.kill_terminal(self.term_at, true),
            Cmd::CloseTerminal => {
                if self.term_open {
                    self.hide_terminal();
                }
            }
            Cmd::RefreshSettings => {
                let path = crate::settings::path();
                match crate::settingsui::refresh(&path) {
                    Ok(backup) => {
                        self.settings_stamp = None;
                        self.check_settings();
                        self.open_settings();
                        self.msg = format!("settings.json rewritten, your values kept (the old one: {})", backup.display());
                    }
                    Err(e) => self.msg = format!("settings.json left as it was: {e}"),
                }
            }
            Cmd::ReloadSettings => {
                self.settings_stamp = None;
                self.check_settings();
                if self.settings_err.is_none() {
                    self.msg = "settings reloaded".into();
                }
            }
        }
    }

    /// View: Toggle Hexdump: the active file again, as a hexdump or as text.
    fn toggle_hex(&mut self) {
        use crate::media::As;
        let Some((hex, elf, media)) = self.ed().map(|(b, _)| (b.binary, b.elf_file, b.path.as_deref().and_then(crate::media::kind))) else { return };
        // Back from the hexdump: to the picture, the pages or readelf's report; else text.
        self.show_as(match () {
            _ if !hex => As::Hex,
            _ if elf || media.is_some() => As::Main,
            _ => As::Text,
        });
    }

    /// How the active file shows now (the banner's lit button).
    pub(crate) fn shown_as(b: &Buffer) -> crate::media::As {
        use crate::media::As;
        match () {
            _ if b.binary => As::Hex,
            _ if b.size_view => As::Size,
            _ if b.pdf_text => As::Text,
            _ if b.media.is_some() || b.elf => As::Main,
            _ => As::Text,
        }
    }

    /// The active file again, shown `how`: as it is meant to be (a picture, a PDF's pages,
    /// readelf's report), as text (a PDF: pdftotext's), as a hexdump. The banner's
    /// buttons and View: Toggle Hexdump.
    pub(crate) fn show_as(&mut self, how: crate::media::As) {
        use crate::media::As;
        let Some((b, _)) = self.ed() else { return };
        let Some(path) = b.path.clone() else { return };
        if Self::shown_as(b) == how {
            return;
        }
        if b.dirty() {
            self.msg = "save it first: a hexdump cannot hold unsaved edits".into();
            return;
        }
        let pdf = crate::media::kind(&path) == Some(crate::media::Kind::Pdf);
        let new = match how {
            As::Hex => Buffer::open_as(&path, Some(true)),
            As::Text if pdf => Buffer::pdf_text(&path, crate::media::pdf_text(self, &path)),
            As::Text => Buffer::open_as(&path, Some(false)),
            As::Main => Buffer::open_as(&path, None),
            As::Size => match Buffer::elf_size(&path) {
                Some(b) => b,
                None => {
                    self.msg = "no size without readelf (sudo apt install binutils)".into();
                    return;
                }
            },
        };
        let Some((b, tab)) = self.ed() else { return };
        *b = new;
        tab.view = View::default();
        self.msg = match how {
            As::Hex => "shown as a hexdump".into(),
            As::Text => "shown as text".into(),
            As::Main if b.elf => "shown as readelf's report".into(),
            As::Size => "shown as where its bytes go".into(),
            As::Main => "shown as it is".into(),
        };
    }

    fn unsaved(&self) -> usize {
        self.docs.values().filter(|d| matches!(d, Doc::Text(b) if b.dirty())).count()
    }

    /// A newly installed binary at `installed` (None: the running one is current): once
    /// it holds still, reload into it when nothing would be lost (no unsaved file, no
    /// dialog or git write under way); else say so once and try again later.
    fn upgrade_tick(&mut self, installed: Option<PathBuf>) {
        let Some(path) = installed else {
            self.upgrade.seen = None;
            return;
        };
        let stamp = fs::metadata(&path).ok().filter(|m| m.is_file()).map(|m| (m.modified().ok(), m.len()));
        let still = stamp.is_some() && stamp == self.upgrade.seen;
        self.upgrade.seen = stamp;
        if !still {
            return;
        }
        let busy = self.prompt.is_some() || self.quick.is_some() || self.settings_dlg.is_some() || self.gitui.dlg.is_some()
            || self.rename.is_some() || !matches!(self.drag, Drag::None) || self.gitui.writes() > 0 || self.file_job.is_some();
        // The terminals' programs go on across it (`Term::hand_over`).
        if self.unsaved() == 0 && !busy {
            (self.reload, self.quit) = (true, true);
        } else if !std::mem::replace(&mut self.upgrade.told, true) {
            self.msg = "a new termcode is installed: it reloads once every file is saved".into();
        }
    }

    /// `editor.wordWrap` for document `key`: Alt+Z's choice, else the setting, else on for
    /// Markdown only (VS Code's `[markdown]` default). Files only, not diffs.
    pub fn wraps(&self, key: &str) -> bool {
        key.starts_with("file:") && self.wrap_on.or(self.settings.word_wrap).unwrap_or_else(|| crate::symbols::is_markdown(key))
    }

    fn active_tab_ref(&self) -> Option<&Tab> {
        let grp = self.groups.get(self.ag)?;
        grp.tabs.get(grp.active)
    }

    /// Ctrl+/: the language's line comment (or block pair) on the selected lines.
    fn toggle_comment(&mut self) {
        let Some((b, tab)) = self.ed() else { return };
        let path = b.path.as_deref().and_then(Path::to_str).unwrap_or("").to_string();
        let Some(marks) = editor::comment_marks(b.syntax_name(), &path) else {
            self.msg = format!("no comment syntax for {}", b.syntax_name());
            return;
        };
        if let Some(ro) = b.readonly.clone() {
            self.msg = format!("read-only: {ro}");
            return;
        }
        let before = b.version();
        b.toggle_comment(&mut tab.view, marks);
        if b.version() != before {
            tab.preview = false;
        }
        self.reveal();
        self.find_refresh();
    }

    /// Group `g`'s Markdown tab rendered (a patch's: as a diff, `rendered`) or as its source, in place; each
    /// look keeps its view (scroll, cursor) for coming back. The preview's buttons.
    pub(crate) fn md_show(&mut self, g: usize, rendered: bool) {
        let Some(key) = self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map(|t| t.key.clone()) else { return };
        let Some((rel, look)) = other_look(&key).map(|(r, l)| (r.to_string(), l)) else { return };
        let to = if rendered { format!("{look}{rel}") } else { self.file_doc(&rel) };
        match look {
            "patch:" if rendered => crate::patch::ensure(self, &to, &rel),
            "usage:" if rendered => crate::size::ensure(self, &to, &rel),
            _ => {}
        }
        let grp = &mut self.groups[g];
        if key == to {
            return;
        }
        // The other look open in this group already: go to it.
        if let Some(i) = grp.tabs.iter().position(|t| t.key == to) {
            grp.active = i;
        } else {
            let tab = &mut grp.tabs[grp.active];
            let view = self.md_views.remove(&to).unwrap_or_default();
            let old = std::mem::replace(&mut tab.view, view);
            tab.key = to;
            // A log look shown nowhere now: no longer read or watched.
            if !self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key)) {
                self.logs.forget(&key);
            }
            self.md_views.insert(key, old);
        }
        self.find_refresh();
    }

    /// Markdown: Toggle Preview: the active Markdown tab rendered read-only (following its
    /// unsaved edits) or its source again, in place. To the side: the preview in the next
    /// group, the source staying.
    fn md_preview(&mut self, side: bool) {
        let Some(key) = self.active_tab_ref().map(|t| t.key.clone()) else { return };
        let Some((_, look)) = other_look(&key) else {
            self.msg = "Markdown preview: the active editor is not a Markdown file (or a patch)".into();
            return;
        };
        let source = key.starts_with("file:");
        if !side || !source || look == "patch:" {
            return self.md_show(self.ag, source);
        }
        let Some(rel) = key.strip_prefix("file:") else { return };
        let pk = format!("preview:{rel}");
        let from = self.ag;
        // To the side: the group right of it (the one beside it most), else its right half,
        // else here.
        let (f, sp) = (self.groups[from].frac, self.groups[from].space);
        let overlap = |o: &Frac| o.y1.min(f.y1) - o.y0.max(f.y0);
        let next = (0..self.groups.len()).filter(|&i| self.groups[i].space == sp && same(self.groups[i].frac.x0, f.x1) && overlap(&self.groups[i].frac) > 1e-9).max_by(|&a, &b| overlap(&self.groups[a].frac).total_cmp(&overlap(&self.groups[b].frac)));
        let g = match next {
            _ if !side => from,
            Some(n) => n,
            None if self.can_split(from, false) => self.new_group_beside(from, Zone::Right),
            None => from,
        };
        self.show_tab(g, pk, false);
        if side && g != from {
            self.ag = from; // to the side keeps the source focused, as VS Code does
        }
        self.focus = Focus::Editor;
    }

    fn fold_command(&mut self, cmd: Cmd, line: Option<usize>) {
        let Some((b, tab)) = self.ed() else { return };
        let v = &mut tab.view;
        b.clamp_view(v);
        let line = line.unwrap_or(v.cursor.line);
        match cmd {
            Cmd::FoldAll => v.collapsed.extend(v.fold_ends.iter().enumerate().filter_map(|(i, end)| end.map(|_| i))),
            Cmd::UnfoldAll => v.collapsed.clear(),
            _ => {
                let start = (0..=line.min(v.fold_ends.len() - 1)).rev().find(|&i| {
                    v.fold_ends[i].is_some_and(|end| line <= end)
                        && (cmd != Cmd::Unfold || v.collapsed.contains(&i))
                });
                if let Some(start) = start {
                    if cmd == Cmd::Unfold { v.collapsed.remove(&start); }
                    else { v.collapsed.insert(start); }
                }
            }
        }
        v.rebuild_folds();
        if v.hidden(v.cursor.line) {
            v.cursor = Pos::new(v.line_of(v.row_of(v.cursor.line)), 0);
            v.anchor = None;
        }
        v.scroll = v.line_of(v.row_of(v.scroll));
        self.peek = None;
        self.reveal();
    }
}

/// Tests: drain until no git job or index write and no load is in flight. Run it
/// before removing a test repo: a job still running (the startup index refresh, a
/// stage) would create index.lock in a directory being deleted.
#[cfg(test)]
pub(crate) fn wait_idle(app: &mut App) {
    let end = Instant::now() + Duration::from_secs(20);
    while app.gitui.busy() || !app.loading.is_empty() {
        assert!(Instant::now() < end, "still busy after 20 s: {} git job(s), loads {:?}", app.gitui.writes(), app.loading.keys().collect::<Vec<_>>());
        app.drain();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Tests: a dropped panel (one a test lets die) finishes its git jobs first, so
/// none outlives it into a later App or the test's cleanup. Production quits as
/// it always has (with Wait / Quit Anyway for queued writes).
#[cfg(test)]
impl Drop for App {
    fn drop(&mut self) {
        let end = Instant::now() + Duration::from_secs(20);
        while (self.gitui.busy() || !self.loading.is_empty()) && Instant::now() < end {
            self.drain();
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests;
