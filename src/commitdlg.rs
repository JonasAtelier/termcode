//! The commit dialog, modelled on git-gui: Unstaged Changes over Staged
//! Changes (Will Commit) on the left, the selected file's side-by-side diff on
//! the right, and below them New Commit / Amend Last Commit, the message
//! editor (subject hint at 50, ruler at 72) and [Rescan] [Sign Off] [Commit]
//! [Commit & Push] [Cancel]. The draft survives Cancel, Esc and restarts.
//! The splitters between the panes drag (VS Code's sashes); their positions
//! are kept as ratios in layout.json, and a double click resets one.

use crate::diff;
use crate::editor::{self, Buffer, Pos, View};
use crate::git::{self, ScmFile, Section};
use crate::gitops::{self, CommitOpts};
use crate::panel::{App, DiffDoc, PAct, Prompt};
use crate::scm::{GitAct, Msg as ScmMsg};
use crate::theme::{git_color, P};
use crate::ui::{centered, diff_view, fill, fit, popup_block, render_line, row, sash, sp};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::Clear;
use std::fs;
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

/// git-gui's soft limits: the subject line, and the body ruler.
pub const SUBJECT: usize = 50;
pub const RULER: usize = 72;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pane {
    Unstaged,
    Staged,
    Message,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Btn {
    Stage,
    StageChanged,
    Unstage,
    New,
    Amend,
    Rescan,
    SignOff,
    Commit,
    CommitPush,
    Cancel,
}

/// The splitters: lists | diff, Unstaged / Staged, upper panes / message.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sash {
    Left,
    Lists,
    Message,
}

pub const SASHES: [Sash; 3] = [Sash::Left, Sash::Lists, Sash::Message];

impl Sash {
    /// Its key in layout.json's `scm.commit_dialog`.
    pub fn name(self) -> &'static str {
        match self {
            Sash::Left => "left",
            Sash::Lists => "lists",
            Sash::Message => "message",
        }
    }
}

/// What a drag must leave: file rows per list, diff columns, message rows, list columns.
const MIN_LIST_ROWS: i32 = 3;
const MIN_DIFF_W: i32 = 20;
const MIN_MSG_H: i32 = 4;
const MIN_LEFT_W: i32 = 12;
/// A file pane: header, rows, buttons.
const MIN_PANE: i32 = MIN_LIST_ROWS + 2;
/// Unstaged, a splitter, Staged.
const MIN_UPPER: i32 = 2 * MIN_PANE + 1;
/// Fixed rows: the message splitter, the mode row, buttons, note.
const FIXED_ROWS: i32 = 4;

/// Splitter positions as fractions of their span (`SASHES` order); None: the default.
pub type Ratios = [Option<f64>; 3];

/// `a` of `total` cells near `want`, leaving `min_a` and `min_b`; with no room
/// for both, shared in proportion to them (a drag then changes nothing).
fn split(total: i32, want: i32, min_a: i32, min_b: i32) -> i32 {
    let total = total.max(0);
    if min_a + min_b > total {
        return (total * min_a + (min_a + min_b) / 2) / (min_a + min_b);
    }
    want.clamp(min_a, total - min_b)
}

fn want(r: Option<f64>, total: i32, default: i32) -> i32 {
    r.map_or(default, |f| (f * total as f64).round() as i32)
}

/// The dialog's panes inside `inner` for the splitter `ratios`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub unstaged: Rect,
    pub staged: Rect,
    pub diff: Rect,
    pub mode: Rect,
    pub msg: Rect,
    pub buttons: Rect,
    pub note: Rect,
    pub sashes: [Rect; 3],
}

pub fn geometry(inner: Rect, ratios: Ratios) -> Geo {
    let (w, h) = (inner.width as i32, inner.height as i32);
    let rows = h - FIXED_ROWS;
    let msg_h = split(rows, want(ratios[2], rows, (h / 3).clamp(1, 8)), MIN_MSG_H, MIN_UPPER);
    let top_h = (rows - msg_h).max(0);
    let lw = split(w - 1, want(ratios[0], w - 1, (w * 2 / 5).max(w.min(24))), MIN_LEFT_W, MIN_DIFF_W);
    let avail = top_h - 1;
    let un_h = split(avail, want(ratios[1], avail, avail.max(0) / 2), MIN_PANE, MIN_PANE);
    let r = |x: i32, y: i32, rw: i32, rh: i32| Rect::new(inner.x + x as u16, inner.y + y as u16, rw.max(0) as u16, rh.max(0) as u16).intersection(inner);
    let row_at = |y: i32| r(0, y, w, 1);
    Geo {
        unstaged: r(0, 0, lw, un_h),
        staged: r(0, un_h + 1, lw, avail - un_h),
        diff: r(lw + 1, 0, w - lw - 1, top_h),
        mode: row_at(top_h + 1),
        msg: r(0, top_h + 2, w, msg_h),
        buttons: r(1, top_h + 2 + msg_h, w - 1, 1),
        note: row_at(top_h + 3 + msg_h),
        sashes: [r(lw, 0, 1, top_h), r(0, un_h, lw, avail.min(1)), row_at(top_h)],
    }
}

/// The ratio that puts `s` at `p` (clamped as `geometry` draws it), to 3 places.
pub fn ratio_at(inner: Rect, ratios: Ratios, s: Sash, p: Position) -> f64 {
    let (w, h) = (inner.width as i32, inner.height as i32);
    let (dx, dy) = (p.x as i32 - inner.x as i32, p.y as i32 - inner.y as i32);
    let (a, total) = match s {
        Sash::Left => (split(w - 1, dx, MIN_LEFT_W, MIN_DIFF_W), w - 1),
        Sash::Lists => {
            let g = geometry(inner, ratios);
            let avail = g.sashes[0].height as i32 - 1;
            (split(avail, dy, MIN_PANE, MIN_PANE), avail)
        }
        Sash::Message => {
            let rows = h - FIXED_ROWS;
            (split(rows, rows - dy, MIN_MSG_H, MIN_UPPER), rows)
        }
    };
    (a as f64 / total.max(1) as f64 * 1000.0).round() / 1000.0
}

#[derive(Default)]
pub struct DlgHits {
    pub btns: Vec<(Rect, Btn)>,
    /// Unstaged and Staged list rows.
    pub lists: [Rect; 2],
    pub msg: Rect,
    pub diff: Rect,
    /// The dialog inside its border, and its splitters (`SASHES` order).
    pub inner: Rect,
    pub sashes: [Rect; 3],
}

pub enum Msg {
    /// The diff of (list, path).
    Diff(usize, String, DiffDoc),
    /// Stage / unstage finished.
    Staged(Result<String, String>),
    /// The last commit's message, for Amend.
    Amend(Result<String, String>),
    /// The Signed-off-by line, for Sign Off.
    SignOff(Result<String, String>),
    /// The commit (and push) finished: a summary, or git's error.
    Done(Result<String, String>),
}

pub struct CommitDlg {
    pub amend: bool,
    pub msg: Buffer,
    pub view: View,
    /// The new-commit message that Amend replaced; New Commit puts it back.
    stash: Option<String>,
    /// 0: Unstaged Changes, 1: Staged Changes.
    pub lists: [Vec<ScmFile>; 2],
    pub sel: [usize; 2],
    pub off: [usize; 2],
    pub focus: Pane,
    /// The list the diff follows.
    pub side: usize,
    pub diff: Option<DiffDoc>,
    pub diff_view: View,
    shown: Option<(usize, String)>,
    /// Errors, or the progress of a running commit.
    pub note: String,
    pub busy: bool,
    pub hits: DlgHits,
    /// The splitter being dragged.
    pub drag: Option<Sash>,
}

fn editable(text: &str) -> (Buffer, View) {
    let mut b = Buffer::from_text(text, "Git Commit", "");
    b.readonly = None;
    let v = View { cursor: Pos::new(0, b.lines[0].len()), ..Default::default() };
    (b, v)
}

fn draft_path(app: &App) -> Option<PathBuf> {
    let dir = app.layout_file.as_ref()?.parent()?;
    let key = crate::parse::lock_key("commit-draft", &app.git_root.as_ref().unwrap_or(&app.root).to_string_lossy());
    Some(dir.join("drafts").join(format!("{key}.txt")))
}

/// Keep the draft in memory and in the state dir (an empty one is removed).
fn set_draft(app: &mut App, text: &str) {
    app.gitui.draft = Some(text.to_string());
    let Some(p) = draft_path(app) else { return };
    if text.trim().is_empty() {
        let _ = fs::remove_file(&p);
    } else if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir).and_then(|_| fs::write(&p, text));
    }
}

fn draft(app: &App) -> String {
    app.gitui.draft.clone().or_else(|| draft_path(app).and_then(|p| fs::read_to_string(p).ok())).unwrap_or_default()
}

/// "Add commit": open the dialog with the saved draft.
pub fn open(app: &mut App) {
    if app.git_root.is_none() {
        app.msg = "The folder currently open doesn't have a git repository.".into();
        return;
    }
    let (msg, view) = editable(&draft(app));
    app.gitui.card = None;
    app.gitui.dlg = Some(CommitDlg {
        amend: false,
        msg,
        view,
        stash: None,
        lists: [Vec::new(), Vec::new()],
        sel: [0; 2],
        off: [0; 2],
        focus: Pane::Message,
        side: 0,
        diff: None,
        diff_view: View::default(),
        shown: None,
        note: String::new(),
        busy: false,
        hits: DlgHits::default(),
        drag: None,
    });
    relist(app);
    if let Some(d) = app.gitui.dlg.as_mut() {
        d.side = usize::from(d.lists[0].is_empty() && !d.lists[1].is_empty());
    }
    show_diff(app);
}

/// "Git: Commit (Amend)": the dialog in Amend Last Commit mode.
pub fn open_amend(app: &mut App) {
    open(app);
    set_amend(app, true);
}

/// Refill both lists from the app's status, keeping the selected paths.
pub fn relist(app: &mut App) {
    let pick = |s: Section| app.scm_files.iter().filter(|f| f.section == s).cloned().collect::<Vec<_>>();
    let new = [pick(Section::Changes), pick(Section::Staged)];
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    for (k, files) in new.into_iter().enumerate() {
        let keep = d.lists[k].get(d.sel[k]).map(|f| f.path.clone());
        let at = keep.and_then(|p| files.iter().position(|f| f.path == p)).unwrap_or(d.sel[k]);
        d.sel[k] = at.min(files.len().saturating_sub(1));
        d.lists[k] = files;
    }
    d.shown = None;
    show_diff(app);
}

fn rescan(app: &mut App) {
    app.refresh();
    if let Some(d) = app.gitui.dlg.as_mut() {
        d.note.clear();
    }
}

/// Load the diff of the selected file of the list the diff follows.
fn show_diff(app: &mut App) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    let side = d.side;
    let Some(f) = d.lists[side].get(d.sel[side]).cloned() else {
        d.diff = None;
        d.shown = None;
        return;
    };
    let want = (side, f.path.clone());
    if d.shown.as_ref() == Some(&want) {
        return;
    }
    d.shown = Some(want);
    d.diff_view = View::default();
    let Some(root) = app.git_root.clone() else { return };
    app.gitui.spawn(move || {
        let (old, new) = git::diff_sides(&root, &f);
        let binary = |b: &[u8]| b[..b.len().min(8000)].contains(&0);
        let (rows, note) = if binary(&old) || binary(&new) {
            (Vec::new(), Some("binary file: no text diff".to_string()))
        } else {
            (diff::align(&String::from_utf8_lossy(&old), &String::from_utf8_lossy(&new)), None)
        };
        let path = f.path.clone();
        ScmMsg::Dlg(Msg::Diff(side, path, DiffDoc { title: f.path.clone(), rows, note, file: f, id: Default::default() }))
    });
}

pub fn drain(app: &mut App, m: Msg) {
    match m {
        Msg::Diff(side, path, doc) => {
            if let Some(d) = app.gitui.dlg.as_mut() {
                if d.shown.as_ref() == Some(&(side, path)) {
                    d.diff = Some(doc);
                }
            }
        }
        Msg::Done(Ok(summary)) => {
            app.gitui.dlg = None;
            set_draft(app, "");
            app.msg = summary;
            app.refresh();
        }
        Msg::Done(Err(e)) => {
            if let Some(d) = app.gitui.dlg.as_mut() {
                d.busy = false;
                d.note = e;
            }
            app.refresh();
        }
        Msg::Staged(r) => {
            if let Err(e) = r {
                app.msg = e;
            }
            app.refresh();
        }
        Msg::Amend(r) => {
            let Some(d) = app.gitui.dlg.as_mut() else { return };
            match r {
                Ok(m) if !d.amend => {
                    d.stash = Some(d.msg.text());
                    set_text(d, &m);
                    d.amend = true;
                }
                Ok(_) => {}
                Err(_) => d.note = "There is no commit to amend.".into(),
            }
        }
        Msg::SignOff(Ok(line)) => add_signoff(app, &line),
        Msg::SignOff(Err(e)) => {
            if let Some(d) = app.gitui.dlg.as_mut() {
                d.note = e;
            }
        }
    }
}

/// Run `job` in the repo on a thread; its message comes back through `drain`.
fn job(app: &mut App, job: impl FnOnce(&std::path::Path) -> Msg + Send + 'static) {
    let Some(g) = app.git_root.clone() else { return };
    app.gitui.spawn(move || ScmMsg::Dlg(job(&g)));
}

/// The same for a job that writes the index: on the index writer, in order.
fn write_job(app: &mut App, label: &str, job: impl FnOnce(&std::path::Path) -> Msg + Send + 'static) {
    let Some(g) = app.git_root.clone() else { return };
    app.gitui.write(label, move || ScmMsg::Dlg(job(&g)));
}

/// Esc / Cancel: close, keeping the draft (a New Commit message, not the amended one).
fn cancel(app: &mut App) {
    let Some(d) = app.gitui.dlg.take() else { return };
    let text = if d.amend { d.stash.unwrap_or_default() } else { d.msg.text() };
    set_draft(app, &text);
}

fn set_text(d: &mut CommitDlg, text: &str) {
    d.msg.select_all(&mut d.view);
    if text.is_empty() {
        d.msg.backspace(&mut d.view);
    } else {
        d.msg.insert(&mut d.view, text);
    }
    d.view.cursor = Pos::new(0, d.msg.lines[0].len());
    d.view.anchor = None;
}

/// Amend: the last message arrives from a job (`Msg::Amend`); New Commit puts the draft back.
fn set_amend(app: &mut App, on: bool) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    if on == d.amend {
        return;
    }
    if on {
        return job(app, |g| Msg::Amend(gitops::last_commit_message(g)));
    }
    let back = d.stash.take().unwrap_or_default();
    set_text(d, &back);
    d.amend = false;
}

/// git-gui's Sign Off: the committer identity comes from a job (`Msg::SignOff`).
fn sign_off(app: &mut App) {
    job(app, |g| Msg::SignOff(gitops::signoff_trailer(g)));
}

/// Add the `Signed-off-by:` line to the message, once.
fn add_signoff(app: &mut App, line: &str) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    let text = d.msg.text();
    if text.lines().any(|l| l == line) {
        return;
    }
    let body = text.trim_end();
    // A blank line before the trailer, unless the last paragraph is trailers already.
    let last = body.rsplit("\n\n").next().unwrap_or("");
    let trailers = body.contains("\n\n") && last.lines().all(|l| l.split_once(": ").is_some_and(|(k, _)| !k.contains(' ')));
    let sep = if trailers { "\n" } else { "\n\n" };
    d.msg.select_all(&mut d.view);
    d.msg.insert(&mut d.view, &format!("{body}{sep}{line}"));
    let draft = if d.amend { None } else { Some(d.msg.text()) };
    if let Some(t) = draft {
        set_draft(app, &t);
    }
}

/// Stage (0) or unstage (1) the selected file in a job; the lists follow on `Msg::Staged`.
fn stage_selected(app: &mut App, k: usize) {
    let Some(f) = app.gitui.dlg.as_ref().and_then(|d| d.lists[k].get(d.sel[k]).cloned()) else { return };
    write_job(app, if k == 0 { "stage" } else { "unstage" }, move |g| {
        let r = if k == 0 { git::stage(g, &[f]) } else { git::unstage(g, &[f]) };
        gitops::refresh_index(g);
        Msg::Staged(r)
    });
}

fn button(app: &mut App, b: Btn) {
    match b {
        Btn::Stage => stage_selected(app, 0),
        Btn::Unstage => stage_selected(app, 1),
        Btn::StageChanged => {
            // Modified and deleted tracked files; untracked and conflicted ones stay.
            let files: Vec<ScmFile> = app.gitui.dlg.as_ref().map_or(Vec::new(), |d| d.lists[0].iter().filter(|f| !matches!(f.letter, 'U' | '!')).cloned().collect());
            if !files.is_empty() {
                write_job(app, "stage", move |g| {
                    let r = git::stage(g, &files);
                    gitops::refresh_index(g);
                    Msg::Staged(r)
                });
            }
        }
        Btn::New => set_amend(app, false),
        Btn::Amend => set_amend(app, true),
        Btn::Rescan => rescan(app),
        Btn::SignOff => sign_off(app),
        Btn::Commit => commit(app, false),
        Btn::CommitPush => commit(app, true),
        Btn::Cancel => cancel(app),
    }
}

/// Commit (and push): refuse an empty message; with nothing staged offer to stage everything.
fn commit(app: &mut App, push: bool) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    if d.msg.text().trim().is_empty() {
        d.note = "Please supply a commit message.".into();
        d.focus = Pane::Message;
        return;
    }
    if !d.amend && d.lists[1].is_empty() {
        if d.lists[0].iter().all(|f| f.letter == '!') {
            d.note = "No changes to commit.".into();
            return;
        }
        app.prompt = Some(Prompt {
            text: vec!["There are no staged changes to commit.".into(), "Stage all your changes and commit them?".into()],
            buttons: vec![("Yes".into(), PAct::Git(GitAct::StageAllCommit { push })), ("Cancel".into(), PAct::Cancel)],
            // It would commit files the user may not have meant to include.
            sel: 1,
        });
        return;
    }
    run_commit(app, push, false);
}

/// Commit on a thread; the dialog shows progress and stays modal until it ends.
pub fn run_commit(app: &mut App, push: bool, stage_all: bool) {
    let Some(root) = app.git_root.clone() else { return };
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    let text = d.msg.text();
    let amend = d.amend;
    let files: Vec<ScmFile> = if stage_all { d.lists[0].iter().filter(|f| f.letter != '!').cloned().collect() } else { Vec::new() };
    d.busy = true;
    d.note = if push { "Committing and pushing…" } else { "Committing…" }.into();
    app.gitui.write(if push { "commit and push" } else { "commit" }, move || {
        let run = || -> Result<String, String> {
            if !files.is_empty() {
                git::stage(&root, &files)?;
            }
            gitops::commit(&root, &text, CommitOpts { amend, ..Default::default() })?;
            gitops::refresh_index(&root);
            let short = gitops::log_page(&root, 0, 1).ok().and_then(|p| p.items.first().map(|c| c.short.clone())).unwrap_or_default();
            let done = format!("Committed {short}: {}", text.lines().next().unwrap_or(""));
            if push {
                return Ok(match gitops::push(&root) {
                    Ok(()) => format!("{done} (pushed)"),
                    Err(e) => format!("{done}; push failed: {}", e.lines().next().unwrap_or("")),
                });
            }
            Ok(done)
        };
        ScmMsg::Dlg(Msg::Done(run()))
    });
}

// ---------------------------------------------------------------- input

fn next_pane(p: Pane, back: bool) -> Pane {
    let order = [Pane::Message, Pane::Unstaged, Pane::Staged];
    let i = order.iter().position(|x| *x == p).unwrap_or(0);
    order[(i + if back { 2 } else { 1 }) % 3]
}

pub fn key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    if d.busy {
        return;
    }
    match k.code {
        KeyCode::Esc => return cancel(app),
        // Ctrl+Enter is git-gui's commit key; legacy terminals send it as Enter, so Alt+Enter too.
        KeyCode::Enter if ctrl || alt => return commit(app, false),
        KeyCode::F(5) => return rescan(app),
        KeyCode::Char('s') if ctrl => return sign_off(app),
        KeyCode::Char('q') if ctrl => {
            cancel(app);
            return app.request_quit();
        }
        KeyCode::Tab | KeyCode::BackTab => {
            d.focus = next_pane(d.focus, k.code == KeyCode::BackTab);
            if d.focus != Pane::Message {
                d.side = usize::from(d.focus == Pane::Staged);
                show_diff(app);
            }
            return;
        }
        KeyCode::PageUp | KeyCode::PageDown if d.focus != Pane::Message => {
            let page = d.hits.diff.height.max(1) as isize;
            let dir = if k.code == KeyCode::PageUp { -page } else { page };
            let n = d.diff.as_ref().map_or(0, |x| x.rows.len());
            d.diff_view.scroll = (d.diff_view.scroll as isize + dir).clamp(0, n.saturating_sub(1) as isize) as usize;
            return;
        }
        _ => {}
    }
    if d.focus != Pane::Message {
        let l = usize::from(d.focus == Pane::Staged);
        let n = d.lists[l].len();
        match k.code {
            KeyCode::Up => d.sel[l] = d.sel[l].saturating_sub(1),
            KeyCode::Down => d.sel[l] = (d.sel[l] + 1).min(n.saturating_sub(1)),
            KeyCode::Home => d.sel[l] = 0,
            KeyCode::End => d.sel[l] = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char(' ') => return stage_selected(app, l),
            _ => return,
        }
        d.side = l;
        return show_diff(app);
    }
    let (b, v) = (&mut d.msg, &mut d.view);
    let before = b.version();
    match k.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            'a' => b.select_all(v),
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
        KeyCode::Enter => b.insert(v, "\n"),
        KeyCode::Backspace => b.backspace(v),
        KeyCode::Delete => b.delete(v),
        KeyCode::Left => b.left(v, shift, ctrl),
        KeyCode::Right => b.right(v, shift, ctrl),
        KeyCode::Up => b.vertical(v, -1, shift),
        KeyCode::Down => b.vertical(v, 1, shift),
        KeyCode::Home => b.home(v, shift),
        KeyCode::End => b.end_of_line(v, shift),
        _ => return,
    }
    if b.version() != before && !d.amend {
        let t = b.text();
        set_draft(app, &t);
    }
}

pub fn paste(app: &mut App, text: &str) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    if d.busy || d.focus != Pane::Message {
        return;
    }
    d.msg.insert(&mut d.view, text);
    if !d.amend {
        let t = d.msg.text();
        set_draft(app, &t);
    }
}

pub fn click(app: &mut App, p: Position, double: bool) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    if d.busy {
        return;
    }
    if let Some(i) = d.hits.sashes.iter().position(|r| r.contains(p)) {
        if double {
            app.gitui.dlg_split[i] = None; // back to the default
            return app.save_layout();
        }
        d.drag = Some(SASHES[i]);
        return;
    }
    if let Some(&(_, b)) = d.hits.btns.iter().find(|(r, _)| r.contains(p)) {
        return button(app, b);
    }
    for k in 0..2 {
        let r = d.hits.lists[k];
        if r.contains(p) {
            let i = d.off[k] + (p.y - r.y) as usize;
            if i < d.lists[k].len() {
                d.sel[k] = i;
                d.side = k;
                d.focus = if k == 0 { Pane::Unstaged } else { Pane::Staged };
                if double {
                    return stage_selected(app, k);
                }
                show_diff(app);
            }
            return;
        }
    }
    let m = d.hits.msg;
    if m.contains(p) {
        d.focus = Pane::Message;
        let line = (d.view.scroll + (p.y - m.y) as usize).min(d.msg.lines.len() - 1);
        let x = d.view.hscroll + p.x.saturating_sub(m.x + 1) as usize;
        d.view.cursor = Pos::new(line, editor::byte_at_display(&d.msg.lines[line], x));
        d.view.anchor = None;
        d.view.want_x = None;
    }
}

/// Dragging a splitter: true while one is held.
pub fn drag(app: &mut App, p: Position) -> bool {
    let Some(d) = app.gitui.dlg.as_ref() else { return false };
    let Some(s) = d.drag else { return false };
    let r = ratio_at(d.hits.inner, app.gitui.dlg_split, s, p);
    app.gitui.dlg_split[s as usize] = Some(r);
    true
}

/// Releasing a splitter keeps its position in layout.json.
pub fn release(app: &mut App) -> bool {
    if app.gitui.dlg.as_mut().and_then(|d| d.drag.take()).is_none() {
        return false;
    }
    app.save_layout();
    true
}

pub fn wheel(app: &mut App, p: Position, delta: isize) {
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    let step = |v: usize, n: usize| (v as isize + delta).clamp(0, n.saturating_sub(1) as isize) as usize;
    if d.hits.diff.contains(p) {
        let n = d.diff.as_ref().map_or(0, |x| x.rows.len());
        d.diff_view.scroll = step(d.diff_view.scroll, n);
    } else if d.hits.msg.contains(p) {
        d.view.scroll = step(d.view.scroll, d.msg.lines.len());
    } else {
        for k in 0..2 {
            if d.hits.lists[k].contains(p) {
                let h = d.hits.lists[k].height as usize;
                d.off[k] = step(d.off[k], d.lists[k].len().saturating_sub(h) + 1);
            }
        }
    }
}

// ---------------------------------------------------------------- drawing

fn button_spans(label: &str, primary: bool, pad: u16) -> Span<'static> {
    let st = if primary { Style::new().fg(P.button_fg).bg(P.button_bg) } else { Style::new().fg(P.button2_fg).bg(P.button2_bg) };
    Span::styled(format!("{}{label}{}", sp(pad), sp(pad)), st)
}

/// Buttons left to right from `x`; each is drawn and hit only where it fits.
fn buttons(f: &mut Frame, hits: &mut Vec<(Rect, Btn)>, r: Rect, items: &[(&str, Btn, bool)], pad: u16) {
    let mut x = r.x;
    for (label, b, primary) in items {
        let s = button_spans(label, *primary, pad);
        let br = Rect::new(x, r.y, s.width() as u16, 1).intersection(r);
        if br.width < s.width() as u16 {
            break;
        }
        f.render_widget(s, br);
        hits.push((br, *b));
        x += br.width + 1;
    }
}

fn file_pane(f: &mut Frame, d: &mut CommitDlg, k: usize, r: Rect, title: &str, btns: &[(&str, Btn, bool)], pad: u16) {
    if r.height == 0 {
        return;
    }
    let focused = d.focus == if k == 0 { Pane::Unstaged } else { Pane::Staged };
    let hdr = Rect::new(r.x, r.y, r.width, 1);
    let st = Style::new().fg(if focused { P.focus_border } else { P.widget_fg }).bold();
    row(f, hdr, P.section_bg, vec![Span::styled(fit(&format!(" {title}"), r.width as usize), st)]);
    let list = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(2));
    d.hits.lists[k] = list;
    let h = list.height as usize;
    if d.sel[k] < d.off[k] {
        d.off[k] = d.sel[k];
    } else if h > 0 && d.sel[k] >= d.off[k] + h {
        d.off[k] = d.sel[k] + 1 - h;
    }
    for (i, file) in d.lists[k].iter().enumerate().skip(d.off[k]).take(h) {
        let rr = Rect::new(list.x, list.y + (i - d.off[k]) as u16, list.width, 1);
        let bg = match (i == d.sel[k], focused) {
            (true, true) => P.list_active_bg,
            (true, false) => P.list_inactive_bg,
            _ => P.widget_bg,
        };
        let c = git_color(file.letter);
        row(f, rr, bg, vec![Span::styled(format!(" {} ", file.letter), Style::new().fg(c)), Span::styled(fit(&file.path, (rr.width as usize).saturating_sub(4)), Style::new().fg(c))]);
    }
    if d.lists[k].is_empty() && h > 0 {
        row(f, Rect::new(list.x, list.y, list.width, 1), P.widget_bg, vec![Span::styled(" (none)", Style::new().fg(P.dim))]);
    }
    if r.height >= 2 {
        buttons(f, &mut d.hits.btns, Rect::new(r.x + 1, r.bottom() - 1, r.width.saturating_sub(1), 1), btns, pad);
    }
}

pub fn draw(app: &mut App, f: &mut Frame, area: Rect) {
    let pad = app.settings.spacing.button_padding.min(1);
    let comfy = !app.settings.compact;
    let gap = app.settings.spacing.editor_padding;
    let (ratios, hover) = (app.gitui.dlg_split, app.hover);
    let Some(d) = app.gitui.dlg.as_mut() else { return };
    d.hits = DlgHits::default();
    let outer = centered(area, area.width.saturating_sub(4).min(160), area.height.saturating_sub(2), false);
    if outer.width < 24 || outer.height < 8 {
        return;
    }
    f.render_widget(Clear, outer);
    let block = popup_block(P.widget_bg, P.widget_fg, (1, 0)).title(" Commit ");
    let inner = block.inner(outer);
    f.render_widget(block, outer);
    // Lists | diff over the message splitter, the mode row, message, buttons, note.
    let g = geometry(inner, ratios);
    let diff = g.diff;
    d.hits.inner = inner;
    d.hits.sashes = g.sashes;
    for (s, r) in SASHES.into_iter().zip(g.sashes) {
        sash(f, r, d.drag == Some(s) || hover.is_some_and(|p| r.contains(p)), P.widget_border, P.widget_bg);
    }
    file_pane(f, d, 0, g.unstaged, "Unstaged Changes", &[("Stage →", Btn::Stage, false), ("Stage Changed", Btn::StageChanged, false)], pad);
    file_pane(f, d, 1, g.staged, "Staged Changes (Will Commit)", &[("← Unstage", Btn::Unstage, false)], pad);
    if diff.width > 4 && diff.height > 1 {
        let name = d.lists[d.side].get(d.sel[d.side]).map_or(String::new(), |f| f.path.clone());
        let which = if d.side == 0 { "Working Tree" } else { "Index" };
        let title = if name.is_empty() { " No file selected".to_string() } else { format!(" {name} ({which})") };
        row(f, Rect::new(diff.x, diff.y, diff.width, 1), P.section_bg, vec![Span::styled(fit(&title, diff.width as usize), Style::new().fg(P.widget_fg))]);
        let body = Rect::new(diff.x, diff.y + 1, diff.width, diff.height - 1);
        d.hits.diff = body;
        fill(f, body, P.editor_bg);
        if let Some(doc) = &d.diff {
            diff_view(f, doc, &mut d.diff_view, body, P.slider, u16::from(comfy), gap, &[], None);
        }
    }
    let mode = g.mode;
    let radio = |on: bool| if on { "(•)" } else { "( )" };
    let mut x = mode.x + 1;
    for (label, b, on) in [("New Commit", Btn::New, !d.amend), ("Amend Last Commit", Btn::Amend, d.amend)] {
        let t = format!("{} {label}", radio(on));
        let r = Rect::new(x, mode.y, t.width() as u16, 1).intersection(mode);
        f.render_widget(Span::styled(t.clone(), Style::new().fg(P.widget_fg)), r);
        d.hits.btns.push((r, b));
        x += t.width() as u16 + 3;
    }
    let subject = d.msg.lines[0].chars().count();
    let hint = format!("Subject {subject}/{SUBJECT} ");
    let hc = if subject > SUBJECT { P.git_modified } else { P.dim };
    if mode.width as usize > x.saturating_sub(mode.x) as usize + hint.width() {
        f.render_widget(Span::styled(hint.clone(), Style::new().fg(hc)), Rect::new(mode.right() - hint.width() as u16, mode.y, hint.width() as u16, 1));
    }
    message(f, d, g.msg);
    let items = [
        ("Rescan", Btn::Rescan, false),
        ("Sign Off", Btn::SignOff, false),
        ("Commit", Btn::Commit, true),
        ("Commit & Push", Btn::CommitPush, false),
        ("Cancel", Btn::Cancel, false),
    ];
    if g.buttons.height > 0 {
        buttons(f, &mut d.hits.btns, g.buttons, &items, pad);
    }
    if g.note.height > 0 && !d.note.is_empty() {
        let c = if d.busy { P.dim } else { P.error_fg };
        f.render_widget(Span::styled(fit(&format!(" {}", d.note), inner.width as usize), Style::new().fg(c)), g.note);
    }
}

/// The message editor: selection, caret, and a faint ruler at column 72.
fn message(f: &mut Frame, d: &mut CommitDlg, r: Rect) {
    d.hits.msg = r;
    fill(f, r, P.input_bg);
    if r.height == 0 || r.width < 3 {
        return;
    }
    let (b, v) = (&d.msg, &mut d.view);
    let c = b.clamp(v.cursor);
    let (h, w) = (r.height as usize, (r.width - 1) as usize);
    if c.line < v.scroll {
        v.scroll = c.line;
    } else if c.line >= v.scroll + h {
        v.scroll = c.line + 1 - h;
    }
    let cx = editor::display_col(&b.lines[c.line], c.col);
    if cx < v.hscroll {
        v.hscroll = cx;
    } else if cx >= v.hscroll + w {
        v.hscroll = cx + 1 - w;
    }
    if b.text().is_empty() {
        let ph = "Message (subject, blank line, body)";
        f.render_widget(Span::styled(fit(&format!(" {ph}"), r.width as usize), Style::new().fg(P.placeholder)), r);
    }
    let sel = v.selection();
    for k in 0..h {
        let i = v.scroll + k;
        let Some(line) = b.lines.get(i) else { break };
        let mut overlays = Vec::new();
        if let Some((a, e)) = sel.filter(|(a, e)| a.line <= i && i <= e.line) {
            let s = if a.line == i { a.col } else { 0 };
            let t = if e.line == i { e.col } else { line.len() };
            overlays.push((s, t, P.selection));
        }
        let lr = Rect::new(r.x + 1, r.y + k as u16, r.width - 1, 1);
        if !line.is_empty() {
            f.render_widget(render_line(line, None, &overlays, None, v.hscroll, w, Style::new().fg(P.input_fg).bg(P.input_bg)), lr);
        }
        if RULER >= v.hscroll && RULER - v.hscroll < w {
            let cell = &mut f.buffer_mut()[(lr.x + (RULER - v.hscroll) as u16, lr.y)];
            if cell.symbol() == " " {
                cell.set_symbol("│").set_fg(P.guide);
            }
        }
    }
    if d.focus == Pane::Message && c.line >= v.scroll && c.line < v.scroll + h {
        f.set_cursor_position((r.x + 1 + (cx - v.hscroll) as u16, r.y + (c.line - v.scroll) as u16));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scm::ui_tests::{app, click, mouse, repo, screen, settle};
    use crate::theme::P;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn splitter_geometry_defaults_minimums_and_tiny_dialogs() {
        let inner = Rect::new(2, 1, 156, 40);
        let g = geometry(inner, [None; 3]);
        // git-gui's defaults: lists 2/5 wide, halved; the message up to 8 rows.
        assert_eq!((g.unstaged, g.staged), (Rect::new(2, 1, 62, 13), Rect::new(2, 15, 62, 14)));
        assert_eq!(g.diff, Rect::new(65, 1, 93, 28));
        assert_eq!(g.sashes, [Rect::new(64, 1, 1, 28), Rect::new(2, 14, 62, 1), Rect::new(2, 29, 156, 1)]);
        assert_eq!((g.mode.y, g.msg, g.buttons.y, g.note.y), (30, Rect::new(2, 31, 156, 8), 39, 40));
        // Dragged to either end: 3 file rows a list, 20 diff columns, 4 message rows, 12 list columns.
        let lo = geometry(inner, [Some(0.0); 3]);
        assert_eq!((lo.unstaged.width, lo.unstaged.height, lo.msg.height), (12, 5, 4));
        let hi = geometry(inner, [Some(1.0); 3]);
        assert_eq!((hi.diff.width, hi.staged.height, hi.unstaged.height, hi.msg.height), (20, 5, 5, 25));
        // The ratio for a pointer position lands on the clamped size.
        assert_eq!(ratio_at(inner, [None; 3], Sash::Left, Position::new(0, 0)), (12.0f64 / 155.0 * 1000.0).round() / 1000.0);
        assert_eq!(geometry(inner, [Some(ratio_at(inner, [None; 3], Sash::Left, Position::new(42, 9))), None, None]).unstaged.width, 40);
        assert_eq!(geometry(inner, [None, None, Some(ratio_at(inner, [None; 3], Sash::Message, Position::new(9, 20)))]).sashes[2].y, 20);
        // Too small for the minimums: shared in proportion, every pane inside, nothing overlaps.
        for h in 0..20 {
            for w in [0u16, 5, 31, 40] {
                let inner = Rect::new(3, 2, w, h);
                for r in [[None; 3], [Some(0.0); 3], [Some(1.0); 3]] {
                    let g = geometry(inner, r);
                    for x in [g.unstaged, g.staged, g.diff, g.mode, g.msg, g.buttons, g.note].iter().chain(&g.sashes) {
                        assert_eq!(x.intersection(inner), *x, "{w}x{h} {r:?}: {x:?}");
                    }
                    assert!(g.unstaged.height + g.sashes[1].height + g.staged.height <= g.sashes[0].height, "{w}x{h}: the lists fit their column");
                    if h >= 4 {
                        assert_eq!(g.sashes[0].height + g.msg.height + 4, h, "{w}x{h}: upper, message and 4 fixed rows");
                    }
                    if h >= 6 {
                        assert!(g.msg.height >= 1, "{w}x{h}: a message row");
                    }
                }
            }
        }
    }

    #[test]
    fn splitters_drag_clamp_persist_and_double_click_reset() {
        let (t, r) = repo("sash", "echo a > a.txt; git add .; git commit -qm first; echo b >> a.txt; echo u > u.txt; echo s > s.txt; git add s.txt");
        let mut app = app(&t, &r);
        let mut tm = Terminal::new(TestBackend::new(120, 40)).unwrap();
        app.commit();
        settle(&mut app);
        screen(&mut app, &mut tm);
        let hits = |app: &App| {
            let h = &app.gitui.dlg.as_ref().unwrap().hits;
            (h.sashes, h.inner, h.lists, h.diff, h.msg)
        };
        let mid = |r: Rect| (r.x + r.width / 2, r.y + r.height / 2);
        let moved = |app: &mut App, (x, y): (u16, u16)| app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: x, row: y, modifiers: KeyModifiers::NONE });
        let (s, inner, ..) = hits(&app);
        // Hovering a splitter redraws it in sash.hoverBorder; leaving it puts the border back.
        assert!(moved(&mut app, mid(s[0])), "entering a splitter redraws");
        screen(&mut app, &mut tm);
        let cell = |tm: &Terminal<TestBackend>, p: (u16, u16)| tm.backend().buffer()[p].clone();
        assert_eq!((cell(&tm, mid(s[0])).symbol(), cell(&tm, mid(s[0])).fg), ("│", P.sash_hover));
        assert_eq!((cell(&tm, mid(s[2])).symbol(), cell(&tm, mid(s[2])).fg), ("─", P.widget_border));
        assert!(moved(&mut app, (inner.x + 1, inner.y + 1)), "leaving it redraws");
        screen(&mut app, &mut tm);
        assert_eq!(cell(&tm, mid(s[0])).fg, P.widget_border);
        // Lists | diff: to the far left the lists keep 12 columns, to the far right the diff keeps 20.
        let drag = |app: &mut App, tm: &mut Terminal<TestBackend>, k: usize, to: &[(u16, u16)]| {
            mouse(app, MouseEventKind::Down(MouseButton::Left), mid(hits(app).0[k]));
            let mut out = Vec::new();
            for &p in to {
                mouse(app, MouseEventKind::Drag(MouseButton::Left), p);
                screen(app, tm);
                out.push(hits(app));
                assert_eq!(cell(tm, mid(hits(app).0[k])).fg, P.sash_hover, "highlighted while dragged");
            }
            mouse(app, MouseEventKind::Up(MouseButton::Left), *to.last().unwrap());
            out
        };
        let (y, x) = (mid(s[0]).1, mid(s[1]).0);
        let got = drag(&mut app, &mut tm, 0, &[(0, y), (119, y), (40, y)]);
        assert_eq!(got[0].2[0].width, 12);
        assert_eq!(got[1].3.width, 20);
        assert_eq!(got[2].2[0].width, 40 - inner.x, "follows the pointer between the limits");
        // Unstaged / Staged: each list keeps 3 file rows.
        let got = drag(&mut app, &mut tm, 1, &[(x, 0), (x, 39)]);
        assert_eq!((got[0].2[0].height, got[1].2[1].height), (3, 3));
        // Upper panes / message: the message keeps 4 rows, the lists 3 each.
        let got = drag(&mut app, &mut tm, 2, &[(x, 39), (x, 0)]);
        assert_eq!(got[0].4.height, 4);
        assert_eq!((got[1].2[0].height, got[1].2[1].height), (3, 3));
        // A splitter press whose release never came: the next press forgets it.
        let before = app.gitui.dlg_split;
        let (s, ..) = hits(&app);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), mid(s[0]));
        assert_eq!(app.gitui.dlg.as_ref().unwrap().drag, Some(Sash::Left));
        let m = hits(&app).4;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (m.x + 2, m.y));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (inner.x + 15, m.y));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (inner.x + 15, m.y));
        assert_eq!(app.gitui.dlg_split, before, "the stale sash drag moved nothing");
        // Kept in layout.json: a restart reads the same ratios.
        let file = t.0.join("state/layout.json");
        let saved = fs::read_to_string(&file).unwrap();
        assert!(saved.contains("\"commit_dialog\""), "{saved}");
        let fresh = App::new(r.clone(), Some(file.clone()));
        assert!(app.gitui.dlg_split.iter().all(Option::is_some));
        assert_eq!(fresh.gitui.dlg_split, app.gitui.dlg_split);
        // A double click puts one splitter back to its default.
        let (s, inner, ..) = hits(&app);
        click(&mut app, mid(s[0]));
        click(&mut app, mid(s[0]));
        assert_eq!(app.gitui.dlg_split[0], None);
        screen(&mut app, &mut tm);
        assert_eq!(hits(&app).2[0].width, geometry(inner, [None; 3]).unstaged.width);
        let saved = fs::read_to_string(&file).unwrap();
        assert!(!saved.contains("\"left\"") && saved.contains("\"lists\""), "{saved}");
    }
}
