//! Git commands for the UI. The operation runner does network and branch
//! work off the UI thread: progress shows in the status bar, a click there
//! cancels it, and errors go to a notification and the Git Output. Also here:
//! the "Git:" palette commands, the branch and stash
//! pickers, confirmations (Cancel is the default for destructive ones), the
//! status bar items (branch, sync / Publish Branch, blame), the editor's
//! conflict actions and blame ghost text, and the quick-diff hunk API.

use crate::diff;
use crate::editor::{self, Buffer, Pos};
use crate::gitops::{self, BlameLine, ConflictRegion, Head, RefKind, StashOpts, SyncStatus};
use crate::panel::{base, App, DiffDoc, Doc, Focus, PAct, Prompt, Side};
use crate::scm::{ago, now, GitAct, Msg as ScmMsg};
use crate::theme::{icons, Rgba, P};
use crate::ui::{centered, fit, input, popup_block, right, row, sp, width};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::Clear;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// Blame waits this long after the cursor settles.
const BLAME_DELAY: Duration = Duration::from_millis(300);
/// The Git Output document.
pub const OUTPUT_KEY: &str = "output:git";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitCmd {
    Pull,
    Push,
    Fetch,
    FetchPrune,
    Sync,
    Publish,
    Checkout,
    CreateBranch,
    CreateBranchFrom,
    DeleteBranch,
    MergeBranch,
    RebaseBranch,
    Stash,
    StashUntracked,
    StashStaged,
    ApplyLatestStash,
    PopLatestStash,
    ApplyStash,
    PopStash,
    DropStash,
    UndoLastCommit,
    CommitAmend,
    AbortMerge,
    AbortRebase,
    ContinueRebase,
    ShowOutput,
    Cancel,
}

/// The F1 palette's Git commands.
pub const GIT_COMMANDS: &[(&str, GitCmd)] = &[
    ("Git: Pull", GitCmd::Pull),
    ("Git: Push", GitCmd::Push),
    ("Git: Fetch", GitCmd::Fetch),
    ("Git: Fetch (Prune)", GitCmd::FetchPrune),
    ("Git: Sync", GitCmd::Sync),
    ("Git: Publish Branch", GitCmd::Publish),
    ("Git: Checkout to...", GitCmd::Checkout),
    ("Git: Create Branch...", GitCmd::CreateBranch),
    ("Git: Create Branch From...", GitCmd::CreateBranchFrom),
    ("Git: Delete Branch...", GitCmd::DeleteBranch),
    ("Git: Merge Branch...", GitCmd::MergeBranch),
    ("Git: Rebase Branch...", GitCmd::RebaseBranch),
    ("Git: Stash", GitCmd::Stash),
    ("Git: Stash (Include Untracked)", GitCmd::StashUntracked),
    ("Git: Stash Staged", GitCmd::StashStaged),
    ("Git: Apply Latest Stash", GitCmd::ApplyLatestStash),
    ("Git: Pop Latest Stash", GitCmd::PopLatestStash),
    ("Git: Apply Stash...", GitCmd::ApplyStash),
    ("Git: Pop Stash...", GitCmd::PopStash),
    ("Git: Drop Stash...", GitCmd::DropStash),
    ("Git: Undo Last Commit", GitCmd::UndoLastCommit),
    ("Git: Commit (Amend)", GitCmd::CommitAmend),
    ("Git: Abort Merge", GitCmd::AbortMerge),
    ("Git: Abort Rebase", GitCmd::AbortRebase),
    ("Git: Continue Rebase", GitCmd::ContinueRebase),
    ("Git: Show Git Output", GitCmd::ShowOutput),
    ("Git: Cancel Running Operation", GitCmd::Cancel),
];

/// Work for the runner.
#[derive(Clone, Debug)]
pub enum Op {
    Fetch(bool),
    Pull,
    Push,
    Publish,
    Sync,
    Checkout(String),
    CheckoutRemote(String),
    CreateBranch(String, Option<String>),
    /// Name, force (`-D`).
    DeleteBranch(String, bool),
    Merge(String),
    Rebase(String),
    StashPush(String, StashOpts),
    /// Index, and the hash it had when picked (checked again before acting).
    StashApply(usize, String),
    StashPop(usize, String),
    StashDrop(usize, String),
    /// Undo Last Commit; true allows a merge commit.
    Undo(bool),
    MergeAbort,
    RebaseAbort,
    RebaseContinue,
}

impl Op {
    /// The status bar's progress text.
    pub fn label(&self) -> String {
        match self {
            Op::Fetch(_) => "Fetching…".into(),
            Op::Pull => "Pulling…".into(),
            Op::Push => "Pushing…".into(),
            Op::Publish => "Publishing…".into(),
            Op::Sync => "Syncing…".into(),
            Op::Checkout(r) | Op::CheckoutRemote(r) => format!("Checking out {r}…"),
            Op::CreateBranch(n, _) => format!("Creating {n}…"),
            Op::DeleteBranch(n, _) => format!("Deleting {n}…"),
            Op::Merge(r) => format!("Merging {r}…"),
            Op::Rebase(r) => format!("Rebasing onto {r}…"),
            Op::StashPush(..) => "Stashing…".into(),
            Op::StashApply(..) | Op::StashPop(..) => "Applying stash…".into(),
            Op::StashDrop(..) => "Dropping stash…".into(),
            Op::Undo(_) => "Undoing the last commit…".into(),
            Op::MergeAbort | Op::RebaseAbort => "Aborting…".into(),
            Op::RebaseContinue => "Continuing the rebase…".into(),
        }
    }

    pub fn run(&self, root: &Path) -> Result<String, String> {
        let ok = |s: String| move |()| s;
        match self {
            Op::Fetch(prune) => gitops::fetch(root, *prune).map(ok("Fetched".into())),
            Op::Pull => gitops::pull(root).map(ok("Pulled".into())),
            Op::Push => gitops::push(root).map(ok("Pushed".into())),
            Op::Publish => {
                let remote = gitops::default_remote(root).ok_or("No remote to publish to: add one first")?;
                gitops::publish(root, &remote).map(ok(format!("Published the branch to {remote}")))
            }
            Op::Sync => gitops::sync(root).map(ok("Synced".into())),
            Op::Checkout(r) => gitops::checkout(root, r).map(ok(format!("Checked out {r}"))),
            Op::CheckoutRemote(r) => gitops::checkout_remote(root, r).map(ok(format!("Checked out {r}"))),
            Op::CreateBranch(n, from) => gitops::create_branch(root, n, from.as_deref()).map(ok(format!("Created {n}"))),
            Op::DeleteBranch(n, force) => gitops::delete_branch(root, n, *force).map(ok(format!("Deleted {n}"))),
            Op::Merge(r) => gitops::merge(root, r).map(ok(format!("Merged {r}"))),
            Op::Rebase(r) => gitops::rebase(root, r).map(ok(format!("Rebased onto {r}"))),
            Op::StashPush(m, o) => gitops::stash_push(root, m, *o).map(ok("Stashed".into())),
            Op::StashApply(i, h) => same_stash(root, *i, h).and_then(|_| gitops::stash_apply(root, *i)).map(ok(format!("Applied stash@{{{i}}}"))),
            Op::StashPop(i, h) => same_stash(root, *i, h).and_then(|_| gitops::stash_pop(root, *i)).map(ok(format!("Popped stash@{{{i}}}"))),
            Op::StashDrop(i, h) => same_stash(root, *i, h).and_then(|_| gitops::stash_drop(root, *i)).map(ok(format!("Dropped stash@{{{i}}}"))),
            Op::Undo(allow) => gitops::undo_last_commit(root, *allow).map(ok("Undid the last commit".into())),
            Op::MergeAbort => gitops::merge_abort(root).map(ok("Merge aborted".into())),
            Op::RebaseAbort => gitops::rebase_abort(root).map(ok("Rebase aborted".into())),
            Op::RebaseContinue => gitops::rebase_continue(root).map(ok("Rebase continued".into())),
        }
    }
}

/// stash@{i} still is the stash the user picked: indexes shift when stashes come and go.
fn same_stash(root: &Path, i: usize, hash: &str) -> Result<(), String> {
    match gitops::stash_list(root)?.get(i) {
        Some(s) if s.hash == hash => Ok(()),
        _ => Err("The stash list changed; pick the stash again".into()),
    }
}

impl Op {
    /// Cancel may close its pipes: nothing local is written (a pure fetch).
    /// Every other op is detached instead, so git never stops half-way through
    /// a merge, rebase, stash or commit.
    fn closable(&self) -> bool {
        matches!(self, Op::Fetch(_))
    }
}

pub struct Running {
    pub label: String,
    pub since: Instant,
    cancel: Arc<AtomicBool>,
    /// Matches its `Msg::Done`; a reply for another id is a detached op finishing.
    id: u64,
    closable: bool,
}

/// What a picker row does.
#[derive(Clone, Debug)]
pub enum Pick {
    Op(Op),
    /// Ask for a new branch's name (from this ref, else HEAD).
    NewBranch(Option<String>),
    /// Pick the ref to branch from.
    BranchFrom,
    /// Confirm (text, button) before the op.
    Confirm(Vec<String>, String, Op),
}

/// What Enter does with the typed text in an input box.
#[derive(Clone, Debug)]
pub enum Input {
    Branch(Option<String>),
    Stash(StashOpts),
}

pub struct Item {
    pub label: String,
    pub detail: String,
    pub pick: Pick,
}

/// A quick pick (VS Code's QuickPick): a filter box over rows, or a plain input box.
pub struct Picker {
    pub title: String,
    pub query: String,
    pub items: Vec<Item>,
    pub sel: usize,
    pub off: usize,
    pub input: Option<Input>,
}

impl Picker {
    pub fn shown(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        (0..self.items.len()).filter(|&i| self.input.is_some() || self.items[i].label.to_lowercase().contains(&q)).collect()
    }
}

#[derive(Default)]
struct Blame {
    /// The cursor spot waiting out the delay: (doc key, version, line), since.
    want: Option<((String, u64, usize), Instant)>,
    asked: Option<(String, u64, usize)>,
    /// One blame at a time: the next one waits for this reply.
    inflight: bool,
    line: Option<((String, u64, usize), BlameLine)>,
}

#[derive(Default)]
pub struct State {
    pub op: Option<Running>,
    /// A notification (git's error), until dismissed.
    pub toast: Option<String>,
    pub picker: Option<Picker>,
    pub sync: Option<SyncStatus>,
    blame: Blame,
    next_id: u64,
    /// Parsed conflict markers per document version.
    pub(crate) conflicts: HashMap<String, (u64, Vec<ConflictRegion>)>,
    out_gen: u64,
}

impl State {
    /// The blame delay (not while a blame runs: its reply wakes the loop).
    pub fn due(&self) -> Option<Instant> {
        self.blame.want.as_ref().filter(|_| !self.blame.inflight).map(|w| w.1 + BLAME_DELAY)
    }
}

pub enum Msg {
    Done(u64, Op, Result<String, String>),
    Blame((String, u64, usize), Result<BlameLine, String>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Accept {
    Current,
    Incoming,
    Both,
    Compare,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Branch,
    Sync,
    Publish,
    /// The running op: a click cancels it.
    Op,
    Blame,
    ToastOutput,
    ToastClose,
    Pick(usize),
    /// Group, region, action.
    Conflict(usize, usize, Accept),
}

// ---------------------------------------------------------------- runner

/// The repo git runs in; without one, a note instead (and no git at all).
fn repo(app: &mut App) -> Option<std::path::PathBuf> {
    let g = app.git_root.clone();
    if g.is_none() {
        app.msg = app.no_git_msg();
    }
    g
}

pub fn run_op(app: &mut App, op: Op) {
    let Some(root) = repo(app) else { return };
    if app.gitui.cmds.op.is_some() {
        app.msg = "A git operation is already running".into();
        return;
    }
    let flag = Arc::new(AtomicBool::new(false));
    let c = &mut app.gitui.cmds;
    c.next_id += 1;
    let id = c.next_id;
    c.op = Some(Running { label: op.label(), since: Instant::now(), cancel: flag.clone(), id, closable: op.closable() });
    // Every op on the index writer (fetch too: it writes refs), in order with stages.
    let label = op.label();
    app.gitui.write(&label, move || {
        let r = gitops::with_cancel(flag, || op.run(&root));
        if !op.closable() {
            gitops::refresh_index(&root);
        }
        ScmMsg::Cmd(Msg::Done(id, op, r))
    });
}

/// Cancel: a fetch has its pipes closed (git ends at its next write). Any
/// other op is detached: the UI stops waiting while its thread keeps reading
/// git's output until git exits, then the panel refreshes.
pub fn cancel(app: &mut App) {
    match app.gitui.cmds.op.take() {
        Some(r) if r.closable => {
            r.cancel.store(true, Ordering::SeqCst);
            app.gitui.cmds.op = Some(Running { label: "Cancelling…".into(), ..r });
        }
        Some(_) => app.msg = "Cancelled; git is finishing in the background".into(),
        None => app.msg = "No git operation is running".into(),
    }
}

/// A confirmation through the app's prompt. `destructive`: Cancel is the default.
pub fn confirm(app: &mut App, text: Vec<String>, button: &str, op: Op, destructive: bool) {
    app.prompt = Some(Prompt {
        text,
        buttons: vec![(button.to_string(), PAct::Git(GitAct::Run(op))), ("Cancel".into(), PAct::Cancel)],
        sel: usize::from(destructive),
    });
}

pub fn drain(app: &mut App, m: Msg) {
    match m {
        Msg::Done(id, op, r) if app.gitui.cmds.op.as_ref().is_none_or(|o| o.id != id) => {
            // A detached op finished: report it, then show the state it left.
            match r {
                Ok(m) => app.msg = format!("{m} (finished in the background)"),
                Err(e) => {
                    app.msg = format!("{} failed in the background", op.label().trim_end_matches('…'));
                    app.gitui.cmds.toast = Some(e);
                }
            }
            app.refresh();
            if app.side == Side::Scm && app.side_visible {
                crate::scm::load_graph(app, true);
            }
        }
        Msg::Done(_, op, r) => {
            app.gitui.cmds.op = None;
            match r {
                Ok(m) => app.msg = m,
                Err(e) if e == "Cancelled" => app.msg = "Cancelled".into(),
                Err(e) => match &op {
                    Op::DeleteBranch(n, false) if e.contains("not fully merged") => {
                        let t = vec![format!("The branch '{n}' is not fully merged."), "Delete it anyway?".into()];
                        confirm(app, t, "Delete Branch", Op::DeleteBranch(n.clone(), true), true);
                    }
                    Op::Undo(false) if e.contains("merge commit") => {
                        let t = vec!["The last commit is a merge commit.".into(), "Undo it anyway? Its other parents are dropped.".into()];
                        confirm(app, t, "Undo", Op::Undo(true), true);
                    }
                    _ => {
                        app.msg = e.lines().next().unwrap_or("").to_string();
                        app.gitui.cmds.toast = Some(e);
                    }
                },
            }
            app.refresh();
            // Fetch and push move remote refs without moving HEAD: redraw the graph's badges too.
            if app.side == Side::Scm && app.side_visible {
                crate::scm::load_graph(app, true);
            }
        }
        // A reply for a spot the cursor (or the buffer) has left is dropped.
        Msg::Blame(at, r) => {
            app.gitui.cmds.blame.inflight = false;
            if app.gitui.cmds.blame.asked.as_ref() == Some(&at) && cursor_spot(app).as_ref() == Some(&at) {
                app.gitui.cmds.blame.line = r.ok().map(|b| (at, b));
            }
        }
    }
}

/// Timers: blame after the cursor settles; true while the spinner turns.
pub fn tick(app: &mut App) -> bool {
    let slow_status = [app.status_since, app.gitui.write_since].iter().flatten().any(|t| t.elapsed() >= crate::panel::STATUS_SLOW);
    let mut redraw = app.gitui.cmds.op.is_some() || slow_status;
    let s = &app.settings;
    let cur = if (s.blame_status || s.blame_decoration) && app.git_root.is_some() { cursor_spot(app) } else { None };
    let bl = &mut app.gitui.cmds.blame;
    let Some(cur) = cur else {
        redraw |= bl.line.take().is_some();
        bl.want = None;
        return redraw;
    };
    if bl.asked.as_ref() == Some(&cur) {
        // Back on the line already blamed: a pending wait is moot (left set, its
        // past deadline would keep the loop from ever sleeping).
        bl.want = None;
        return redraw;
    }
    match &bl.want {
        Some((w, t)) if *w == cur => {
            // One in flight at a time; a newer spot waits for that reply.
            if t.elapsed() >= BLAME_DELAY && !bl.inflight {
                bl.want = None;
                bl.asked = Some(cur.clone());
                spawn_blame(app, cur);
            }
        }
        _ => bl.want = Some((cur, Instant::now())),
    }
    redraw
}

/// (doc key, version, line) of the active editor's cursor in a file of the repo.
fn cursor_spot(app: &App) -> Option<(String, u64, usize)> {
    let grp = app.groups.get(app.ag)?;
    let tab = grp.tabs.get(grp.active)?;
    app.repo_path(tab.key.strip_prefix("file:")?)?;
    let Some(Doc::Text(b)) = app.docs.get(&tab.key) else { return None };
    Some((tab.key.clone(), b.version(), b.clamp(tab.view.cursor).line))
}

fn spawn_blame(app: &mut App, at: (String, u64, usize)) {
    let (Some(root), Some(rel)) = (app.git_root.clone(), app.repo_path(&at.0["file:".len()..])) else { return };
    let Some(Doc::Text(b)) = app.docs.get(&at.0) else { return };
    let text = b.text().into_bytes();
    app.gitui.cmds.blame.inflight = true;
    app.gitui.spawn(move || {
        let r = gitops::blame_line(&root, &rel, at.2, &text);
        ScmMsg::Cmd(Msg::Blame(at, r))
    });
}

/// The blame of the cursor line, when it is for this exact spot.
fn current_blame(app: &App) -> Option<&BlameLine> {
    let cur = cursor_spot(app)?;
    app.gitui.cmds.blame.line.as_ref().filter(|(at, _)| *at == cur).map(|(_, b)| b)
}

fn blame_text(b: &BlameLine) -> String {
    match &b.hash {
        None => "You, not committed yet".into(),
        Some(_) => format!("{}, {}", b.author, ago(b.author_time, now())),
    }
}

// ---------------------------------------------------------------- commands and pickers

pub fn run_cmd(app: &mut App, c: GitCmd) {
    match c {
        GitCmd::Pull => run_op(app, Op::Pull),
        GitCmd::Push => run_op(app, Op::Push),
        GitCmd::Fetch => run_op(app, Op::Fetch(false)),
        GitCmd::FetchPrune => run_op(app, Op::Fetch(true)),
        GitCmd::Sync => sync_clicked(app),
        GitCmd::Publish => run_op(app, Op::Publish),
        GitCmd::Checkout => branch_picker(app),
        GitCmd::CreateBranch => ask(app, "Branch name", Input::Branch(None)),
        GitCmd::CreateBranchFrom => refs_picker(app, "Select a ref to create the branch from", |r| Pick::NewBranch(Some(r.to_string())), true),
        GitCmd::DeleteBranch => delete_picker(app),
        GitCmd::MergeBranch => refs_picker(app, "Select a branch to merge from", |r| Pick::Op(Op::Merge(r.to_string())), false),
        GitCmd::RebaseBranch => {
            let cur = app.branch.clone();
            refs_picker(
                app,
                "Select a branch to rebase onto",
                move |r| Pick::Confirm(vec![format!("Rebase '{cur}' onto '{r}'?"), "This rewrites the commits of the current branch.".into()], "Rebase".into(), Op::Rebase(r.to_string())),
                false,
            )
        }
        GitCmd::Stash => ask(app, "Stash message (optional)", Input::Stash(StashOpts::default())),
        GitCmd::StashUntracked => ask(app, "Stash message (optional)", Input::Stash(StashOpts { include_untracked: true, ..Default::default() })),
        GitCmd::StashStaged => ask(app, "Stash message (optional)", Input::Stash(StashOpts { staged_only: true, ..Default::default() })),
        GitCmd::ApplyLatestStash | GitCmd::PopLatestStash => match repo(app).map_or(Err(String::new()), |g| app.gitui.logged(|| gitops::stash_list(&g))) {
            Err(e) if e.is_empty() => {}
            Ok(l) if l.is_empty() => app.msg = "There are no stashes in the repository.".into(),
            Ok(l) => {
                let h = l[0].hash.clone();
                run_op(app, if c == GitCmd::PopLatestStash { Op::StashPop(0, h) } else { Op::StashApply(0, h) })
            }
            Err(e) => app.msg = e,
        },
        GitCmd::ApplyStash | GitCmd::PopStash | GitCmd::DropStash => stash_picker(app, c),
        GitCmd::UndoLastCommit => run_op(app, Op::Undo(false)),
        GitCmd::CommitAmend => crate::commitdlg::open_amend(app),
        GitCmd::AbortMerge => confirm(app, vec!["Abort the merge?".into(), "Conflict resolutions made so far are lost.".into()], "Abort Merge", Op::MergeAbort, true),
        GitCmd::AbortRebase => confirm(app, vec!["Abort the rebase?".into(), "Conflict resolutions made so far are lost.".into()], "Abort Rebase", Op::RebaseAbort, true),
        GitCmd::ContinueRebase => run_op(app, Op::RebaseContinue),
        GitCmd::ShowOutput => show_output(app),
        GitCmd::Cancel => cancel(app),
    }
}

fn open_picker(app: &mut App, title: &str, items: Vec<Item>, input: Option<Input>) {
    app.gitui.card = None;
    app.gitui.cmds.picker = Some(Picker { title: title.into(), query: String::new(), items, sel: 0, off: 0, input });
}

fn ask(app: &mut App, title: &str, what: Input) {
    open_picker(app, title, Vec::new(), Some(what));
}

/// VS Code's row detail: "short-hash · current branch / branch / remote branch / tag".
fn ref_detail(r: &gitops::RefInfo) -> String {
    let short = &r.hash[..r.hash.len().min(8)];
    let what = match r.kind {
        RefKind::Branch if r.current => "current branch",
        RefKind::Branch => "branch",
        RefKind::Remote => "remote branch",
        RefKind::Tag => "tag",
    };
    format!("{short} · {what}")
}

/// VS Code's order: the current branch, then local branches, remote branches, tags.
fn ordered(mut refs: Vec<gitops::RefInfo>) -> Vec<gitops::RefInfo> {
    refs.sort_by_key(|r| match r.kind {
        _ if r.current => 0,
        RefKind::Branch => 1,
        RefKind::Remote => 2,
        RefKind::Tag => 3,
    });
    refs
}

/// The status bar branch item / "Checkout to...": create, then every ref.
pub fn branch_picker(app: &mut App) {
    let Some(g) = repo(app) else { return };
    let plus = if crate::theme::nerd() { "\u{ea60}" } else { "+" };
    let mut items = vec![
        Item { label: format!("{plus} Create new branch..."), detail: String::new(), pick: Pick::NewBranch(None) },
        Item { label: format!("{plus} Create new branch from..."), detail: String::new(), pick: Pick::BranchFrom },
    ];
    match app.gitui.logged(|| gitops::refs(&g)).map(ordered) {
        Ok(refs) => items.extend(refs.iter().map(|r| Item {
            label: r.name.clone(),
            detail: ref_detail(r),
            pick: Pick::Op(if r.kind == RefKind::Remote { Op::CheckoutRemote(r.name.clone()) } else { Op::Checkout(r.name.clone()) }),
        })),
        Err(e) => app.msg = e,
    }
    open_picker(app, "Select a ref to checkout", items, None);
}

fn refs_picker(app: &mut App, title: &str, pick: impl Fn(&str) -> Pick, with_current: bool) {
    let Some(g) = repo(app) else { return };
    let refs = ordered(app.gitui.logged(|| gitops::refs(&g)).unwrap_or_default());
    let items = refs
        .iter()
        .filter(|r| with_current || !r.current)
        .map(|r| Item { label: r.name.clone(), detail: ref_detail(r), pick: pick(&r.name) })
        .collect();
    open_picker(app, title, items, None);
}

fn delete_picker(app: &mut App) {
    let Some(g) = repo(app) else { return };
    let refs = app.gitui.logged(|| gitops::refs(&g)).unwrap_or_default();
    let items = refs
        .iter()
        .filter(|r| r.kind == RefKind::Branch && !r.current)
        .map(|r| Item { label: r.name.clone(), detail: ref_detail(r), pick: Pick::Op(Op::DeleteBranch(r.name.clone(), false)) })
        .collect();
    open_picker(app, "Select a branch to delete", items, None);
}

fn stash_picker(app: &mut App, c: GitCmd) {
    let Some(g) = repo(app) else { return };
    let list = match app.gitui.logged(|| gitops::stash_list(&g)) {
        Ok(l) if l.is_empty() => {
            app.msg = "There are no stashes in the repository.".into();
            return;
        }
        Ok(l) => l,
        Err(e) => {
            app.msg = e;
            return;
        }
    };
    let items = list
        .iter()
        .map(|s| {
            let op = match c {
                GitCmd::PopStash => Op::StashPop(s.index, s.hash.clone()),
                GitCmd::DropStash => Op::StashDrop(s.index, s.hash.clone()),
                _ => Op::StashApply(s.index, s.hash.clone()),
            };
            let pick = if c == GitCmd::DropStash {
                Pick::Confirm(vec![format!("Are you sure you want to drop the stash: {}?", s.message), "This is IRREVERSIBLE!".into()], "Drop".into(), op)
            } else {
                Pick::Op(op)
            };
            Item { label: format!("stash@{{{}}}", s.index), detail: s.message.clone(), pick }
        })
        .collect();
    let verb = match c {
        GitCmd::PopStash => "pop",
        GitCmd::DropStash => "drop",
        _ => "apply",
    };
    open_picker(app, &format!("Choose a stash to {verb}"), items, None);
}

/// Sync item / "Git: Sync": VS Code's confirmation, then pull and push.
pub fn sync_clicked(app: &mut App) {
    if repo(app).is_none() {
        return;
    }
    // The status job's sync state (no git here); None: no upstream (or no HEAD).
    let up = app.gitui.cmds.sync.as_ref().and_then(|s| s.upstream.clone());
    match up {
        Some(_) if !app.settings.confirm_sync => run_op(app, Op::Sync),
        // OK is the default, as in VS Code (the user's decision).
        Some(u) => {
            app.prompt = Some(Prompt {
                text: vec![format!("This action will pull and push commits from and to '{u}'.")],
                buttons: vec![
                    ("OK".into(), PAct::Git(GitAct::Run(Op::Sync))),
                    ("OK, Don't Show Again".into(), PAct::Git(GitAct::SyncAlways)),
                    ("Cancel".into(), PAct::Cancel),
                ],
                sel: 0,
            });
        }
        None => run_op(app, Op::Publish),
    }
}

/// "OK, Don't Show Again": `git.confirmSync: false` in settings.json, then sync.
pub fn sync_always(app: &mut App) {
    app.settings.confirm_sync = false;
    run_op(app, Op::Sync);
    // After run_op, so its status line is not overwritten by a refusal here.
    if let Some(p) = app.settings_file.clone() {
        if let Err(e) = crate::settings::write_bool(&p, "git.confirmSync", false) {
            app.msg = e;
        }
    }
}

fn pick(app: &mut App, i: usize) {
    let Some(p) = app.gitui.cmds.picker.take() else { return };
    if let Some(what) = p.input {
        let text = p.query.trim().to_string();
        return match what {
            Input::Branch(from) if text.is_empty() => {
                app.msg = "Please provide a branch name".into();
                app.gitui.cmds.picker = Some(Picker { input: Some(Input::Branch(from)), ..p });
            }
            Input::Branch(from) => {
                let name = text.replace(' ', "-");
                let ok = app.git_root.as_ref().map_or(Err(String::new()), |g| app.gitui.logged(|| gitops::valid_branch_name(g, &name)));
                match ok {
                    Ok(()) => run_op(app, Op::CreateBranch(name, from)),
                    Err(e) => {
                        app.msg = e;
                        app.gitui.cmds.picker = Some(Picker { query: text, input: Some(Input::Branch(from)), ..p });
                    }
                }
            }
            Input::Stash(o) => run_op(app, Op::StashPush(text, o)),
        };
    }
    let Some(&k) = p.shown().get(i) else { return };
    match p.items[k].pick.clone() {
        Pick::Op(op) => run_op(app, op),
        Pick::NewBranch(from) => ask(app, &format!("Branch name{}", from.as_ref().map_or(String::new(), |f| format!(" (from {f})"))), Input::Branch(from)),
        Pick::BranchFrom => run_cmd(app, GitCmd::CreateBranchFrom),
        Pick::Confirm(text, button, op) => confirm(app, text, &button, op, true),
    }
}

pub fn picker_key(app: &mut App, k: KeyEvent) {
    let modded = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    let Some(p) = app.gitui.cmds.picker.as_mut() else { return };
    let n = p.shown().len();
    match k.code {
        KeyCode::Esc => app.gitui.cmds.picker = None,
        KeyCode::Enter => {
            let i = p.sel;
            pick(app, i);
        }
        KeyCode::Up => p.sel = p.sel.saturating_sub(1),
        KeyCode::Down => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
        KeyCode::Backspace => {
            p.query.pop();
            p.sel = 0;
        }
        KeyCode::Char(c) if !modded => {
            p.query.push(c);
            p.sel = 0;
        }
        _ => {}
    }
}

pub fn picker_paste(app: &mut App, text: &str) {
    if let Some(p) = app.gitui.cmds.picker.as_mut() {
        p.query.push_str(text.lines().next().unwrap_or(""));
        p.sel = 0;
    }
}

// ---------------------------------------------------------------- clicks

/// A click on something this module drew (picker rows outside: closes it).
pub fn click(app: &mut App, p: Position) -> bool {
    let hit = app.gitui.hits.cmds.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, h)| *h);
    if app.gitui.cmds.picker.is_some() {
        match hit {
            Some(Hit::Pick(i)) => pick(app, i),
            _ if !app.gitui.hits.picker.contains(p) => app.gitui.cmds.picker = None,
            _ => {}
        }
        return true;
    }
    let Some(hit) = hit else { return false };
    match hit {
        Hit::Branch => branch_picker(app),
        Hit::Sync => sync_clicked(app),
        Hit::Publish => run_op(app, Op::Publish),
        Hit::Op => cancel(app),
        Hit::Blame => {
            if let Some(h) = current_blame(app).and_then(|b| b.hash.clone()) {
                let anchor = app.gitui.hits.cmds.iter().find(|x| x.1 == Hit::Blame).map_or(Rect::new(p.x, p.y, 1, 1), |x| x.0);
                crate::scm::open_card(app, h, anchor);
            }
        }
        Hit::ToastOutput => {
            app.gitui.cmds.toast = None;
            show_output(app);
        }
        Hit::ToastClose => app.gitui.cmds.toast = None,
        Hit::Pick(_) => {}
        Hit::Conflict(g, i, a) => resolve(app, g, i, a),
    }
    true
}

// ---------------------------------------------------------------- Git Output

fn stamp((s, ms): (i64, u32)) -> String {
    let tt = s as libc::time_t;
    // SAFETY: tm is plain data; localtime_r only writes into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{ms:03}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// The Git Output text: every git call of this panel, exit code, duration and stderr.
pub fn output_text(log: &gitops::OutputLog) -> String {
    let log = log.entries();
    let mut s = String::new();
    let ascii = crate::theme::ascii();
    // The command early, so it stays readable in a narrow editor: `12:47:01 ✓ git fetch (0.21 s)`.
    for e in log {
        let (mark, tail) = match e.code {
            Some(0) => (if ascii { "+" } else { "✓" }, String::new()),
            Some(c) => (if ascii { "x" } else { "✗" }, format!(", exit {c}")),
            None => (if ascii { "-" } else { "…" }, ", abandoned".to_string()),
        };
        let time = stamp(e.when);
        let hms = time.split(' ').nth(1).and_then(|t| t.split('.').next()).unwrap_or("");
        s.push_str(&format!("{hms} {mark} git {} ({:.2} s{tail})\n", e.args, e.ms as f64 / 1000.0));
        for l in e.stderr.lines().filter(|l| !l.is_empty()) {
            s.push_str(&format!("    {l}\n"));
        }
    }
    s
}

fn output_doc(log: &gitops::OutputLog) -> Doc {
    Doc::Text(Buffer::from_text(&output_text(log), "Plain Text", "Git Output: read-only"))
}

pub fn show_output(app: &mut App) {
    app.show_tab(app.ag, OUTPUT_KEY.into(), false);
    app.docs.insert(OUTPUT_KEY.into(), output_doc(&app.gitui.log));
    app.gitui.cmds.out_gen = app.gitui.log.gen();
    follow_output(app);
    app.focus = Focus::Editor;
}

/// Keep the views of the Git Output at its end.
fn follow_output(app: &mut App) {
    let last = match app.docs.get(OUTPUT_KEY) {
        Some(Doc::Text(b)) => b.lines.len().saturating_sub(1),
        _ => return,
    };
    for g in &mut app.groups {
        for t in g.tabs.iter_mut().filter(|t| t.key == OUTPUT_KEY) {
            t.view.cursor = Pos::new(last, 0);
            t.view.scroll = last.saturating_sub(10);
        }
    }
}

/// An open Git Output follows new calls. True when it changed.
pub fn sync_output(app: &mut App) -> bool {
    let gen = app.gitui.log.gen();
    if gen == app.gitui.cmds.out_gen || !app.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == OUTPUT_KEY)) {
        return false;
    }
    app.gitui.cmds.out_gen = gen;
    app.docs.insert(OUTPUT_KEY.into(), output_doc(&app.gitui.log));
    follow_output(app);
    true
}

// ---------------------------------------------------------------- conflicts in the editor

fn conflict_regions(app: &mut App, key: &str) -> Vec<ConflictRegion> {
    let Some(Doc::Text(b)) = app.docs.get(key) else { return Vec::new() };
    let ver = b.version();
    if let Some((v, r)) = app.gitui.cmds.conflicts.get(key) {
        if *v == ver {
            return r.clone();
        }
    }
    let regions = if b.lines.iter().any(|l| l.starts_with("<<<<<<<")) { gitops::parse_conflict_markers(&b.text()) } else { Vec::new() };
    app.gitui.cmds.conflicts.insert(key.to_string(), (ver, regions.clone()));
    regions
}

/// Accept Current / Incoming / Both: replace the block with those lines, as one undoable edit.
/// Compare: the two sides in a diff tab.
pub fn resolve(app: &mut App, g: usize, i: usize, a: Accept) {
    let Some(key) = app.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map(|t| t.key.clone()) else { return };
    let Some(r) = conflict_regions(app, &key).get(i).cloned() else { return };
    let Some(Doc::Text(b)) = app.docs.get(&key) else { return };
    let side = |rg: &std::ops::Range<usize>| b.lines[rg.clone()].to_vec();
    if a == Accept::Compare {
        let (cur, inc) = (side(&r.current).join("\n"), side(&r.incoming).join("\n"));
        let title = format!("{}: Current ↔ Incoming", base(&key));
        let file = crate::git::ScmFile { section: crate::git::Section::Changes, path: key["file:".len()..].to_string(), orig: None, letter: '!' };
        let doc = DiffDoc { title, rows: diff::align(&format!("{cur}\n"), &format!("{inc}\n")), note: None, file, id: Default::default() };
        let ck = format!("conflict:{}:{}", &key["file:".len()..], r.start);
        app.show_tab(app.ag, ck.clone(), true);
        app.docs.insert(ck, Doc::Diff(doc));
        return;
    }
    let mut lines = match a {
        Accept::Current => side(&r.current),
        Accept::Incoming => side(&r.incoming),
        _ => [side(&r.current), side(&r.incoming)].concat(),
    };
    let last = r.end + 1 >= b.lines.len();
    let end = if last { Pos::new(r.end, b.lines[r.end].len()) } else { Pos::new(r.end + 1, 0) };
    if !last && !lines.is_empty() {
        lines.push(String::new());
    }
    let text = lines.join("\n");
    let Some(tab) = app.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) else { return };
    let Some(Doc::Text(b)) = app.docs.get_mut(&key) else { return };
    tab.view.anchor = Some(Pos::new(r.start, 0));
    tab.view.cursor = end;
    b.insert(&mut tab.view, &text);
    tab.view.anchor = None;
    tab.preview = false;
    app.msg = "Conflict resolved in the editor: save, then stage the file to mark it resolved".into();
}

fn merge_colors(app: &App) -> ([Color; 6], Color) {
    let empty = serde_json::Map::new();
    let t = app.settings.theme.json["colors"].as_object().unwrap_or(&empty);
    let e = P.editor_bg;
    let get = |k: &str, d: &str| crate::theme::lookup(t, k).or_else(|| Rgba::parse(d)).map_or(P.editor_fg, |c| c.over(e));
    (
        [
            get("merge.currentHeaderBackground", "#40C8AE80"),
            get("merge.currentContentBackground", "#40C8AE33"),
            get("merge.incomingHeaderBackground", "#40A6FF80"),
            get("merge.incomingContentBackground", "#40A6FF33"),
            get("merge.commonHeaderBackground", "#60606066"),
            get("merge.commonContentBackground", "#60606029"),
        ],
        get("editorCodeLens.foreground", "#999999"),
    )
}

/// Over a drawn text editor: conflict colours and actions, the blame ghost text.
pub fn editor_overlay(app: &mut App, f: &mut Frame, g: usize) {
    let Some(text) = app.ui.groups.get(g).map(|gh| gh.text) else { return };
    let Some(tab) = app.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)) else { return };
    let (key, hscroll, cursor) = (tab.key.clone(), tab.view.hscroll, tab.view.cursor);
    if !matches!(app.docs.get(&key), Some(Doc::Text(_))) || text.width < 4 {
        return;
    }
    let regions = conflict_regions(app, &key);
    let (merge, lens) = merge_colors(app);
    let Some(Doc::Text(b)) = app.docs.get(&key) else { return };
    let h = text.height as usize;
    let area = f.area();
    let ascii = crate::theme::ascii();
    for (ri, r) in regions.iter().enumerate() {
        for i in (0..h).filter_map(|row| app.line_at(g, text.y + row as u16)).filter(|i| (r.start..=r.end).contains(i)) {
            let c = if i == r.start {
                merge[0]
            } else if r.current.contains(&i) {
                merge[1]
            } else if r.base.as_ref().is_some_and(|x| x.contains(&i) || x.start == i + 1) {
                merge[if r.base.as_ref().is_some_and(|x| x.contains(&i)) { 5 } else { 4 }]
            } else if r.incoming.contains(&i) {
                merge[3]
            } else if i == r.end {
                merge[2]
            } else {
                continue;
            };
            let Some(y) = app.line_y(g, i) else { continue };
            let buf = f.buffer_mut();
            for x in text.x..text.right().min(area.right()) {
                let cell = &mut buf[(x, y)];
                if cell.bg == P.editor_bg || cell.bg == P.cur_line {
                    cell.set_bg(c);
                }
            }
        }
        // The actions, on the `<<<<<<<` line after its text (VS Code's CodeLens row).
        let Some(y) = app.line_y(g, r.start) else { continue };
        let used = editor::display_col(&b.lines[r.start], b.lines[r.start].len()).saturating_sub(hscroll) as u16 + 2;
        let mut x = text.x + used.min(text.width);
        let labels: [(&str, &str, Accept); 4] = [
            ("Accept Current Change", "Current", Accept::Current),
            ("Accept Incoming Change", "Incoming", Accept::Incoming),
            ("Accept Both Changes", "Both", Accept::Both),
            ("Compare Changes", "Compare", Accept::Compare),
        ];
        let long: usize = labels.iter().map(|l| l.0.len() + 3).sum();
        let short = (text.right().saturating_sub(x) as usize) < long;
        for (k, (l, s, a)) in labels.iter().enumerate() {
            let t = if short { *s } else { *l };
            let sep = if k == 0 { "" } else if ascii { " | " } else { " │ " };
            let w = (sep.width() + t.width()) as u16;
            if x + w > text.right() {
                break;
            }
            f.render_widget(Span::styled(sep, Style::new().fg(lens)), Rect::new(x, y, sep.width() as u16, 1));
            let br = Rect::new(x + sep.width() as u16, y, t.width() as u16, 1);
            f.render_widget(Span::styled(t.to_string(), Style::new().fg(lens).bg(merge[0])), br);
            app.gitui.hits.cmds.push((br, Hit::Conflict(g, ri, *a)));
            x += w;
        }
    }
    // Blame ghost text after the cursor line.
    if !app.settings.blame_decoration || g != app.ag {
        return;
    }
    let Some(bl) = current_blame(app) else { return };
    let Some(Doc::Text(b)) = app.docs.get(&key) else { return };
    let line = b.clamp(cursor).line;
    let ghost = match &bl.hash {
        None => "You, not committed yet".to_string(),
        Some(_) => format!("{}, {} • {}", bl.author, ago(bl.author_time, now()), bl.summary),
    };
    let x = text.x + (editor::display_col(&b.lines[line], b.lines[line].len()).saturating_sub(hscroll) as u16 + 3).min(text.width);
    let room = text.right().saturating_sub(x) as usize;
    if room > 4 {
        let Some(y) = app.line_y(g, line) else { return };
        f.render_widget(Span::styled(fit(&ghost, room), Style::new().fg(lens).italic()), Rect::new(x, y, room as u16, 1));
    }
}

// ---------------------------------------------------------------- status bar and popups

fn spinner() -> &'static str {
    let frames: &[&str] = if crate::theme::ascii() { &["|", "/", "-", "\\"] } else { &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] };
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() / 100);
    frames[t as usize % frames.len()]
}

/// Status bar, left side: branch, then sync / Publish Branch, or the running op.
/// Returns the x after the items.
pub fn status_left(app: &mut App, f: &mut Frame, r: Rect, edge: u16, gap: u16) -> u16 {
    // No repository: no branch item, as in VS Code.
    if app.git_root.is_none() {
        return r.x + edge;
    }
    let st = Style::new().fg(P.status_fg).bg(P.status_bg);
    let ig = sp(app.settings.spacing.icon_gap);
    let dirty = app.scm_files.iter().any(|f| f.section == crate::git::Section::Changes);
    let staged = app.scm_files.iter().any(|f| f.section == crate::git::Section::Staged);
    let marks = format!("{}{}", if dirty { "*" } else { "" }, if staged { "+" } else { "" });
    let branch_icon = icons().branch;
    let bi = if branch_icon.is_empty() { String::new() } else { format!("{branch_icon}{ig}") };
    let mut items: Vec<(String, Hit)> = vec![(format!("{bi}{}{marks}", app.branch), Hit::Branch)];
    // A slow `git status` or index write: what shows is the last status, about to change.
    let slow = |t: Option<Instant>| t.is_some_and(|t| t.elapsed() >= crate::panel::STATUS_SLOW);
    if slow(app.status_since) || slow(app.gitui.write_since) {
        items.push((spinner().to_string(), Hit::Branch));
    }
    if let Some(op) = &app.gitui.cmds.op {
        let secs = op.since.elapsed().as_secs();
        let t = if secs >= 2 { format!(" {secs}s") } else { String::new() };
        items.push((format!("{} {}{t}", spinner(), op.label), Hit::Op));
    } else if let Some(s) = &app.gitui.cmds.sync {
        let on_branch = matches!(s.head, Head::Branch(_));
        match &s.upstream {
            Some(_) => {
                let icon = if crate::theme::nerd() { "\u{ea77} " } else { "" };
                items.push((format!("{icon}{}↓ {}↑", s.behind, s.ahead), Hit::Sync));
            }
            None if on_branch => {
                let icon = if crate::theme::nerd() { "\u{eac3} " } else { "" };
                items.push((format!("{icon}Publish Branch"), Hit::Publish));
            }
            None => {}
        }
    }
    let mut x = r.x + edge;
    for (k, (t, hit)) in items.into_iter().enumerate() {
        if k > 0 {
            x += gap;
        }
        let w = (t.width() as u16).min(r.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        let ir = Rect::new(x, r.y, w, 1);
        let item = if app.hover.is_some_and(|p| ir.contains(p)) { st.bg(P.status_hover) } else { st };
        f.render_widget(Span::styled(t, item), ir);
        app.gitui.hits.cmds.push((ir, hit));
        x += w;
    }
    x
}

/// Status bar: the cursor line's blame, right-aligned in `r`. Returns where it starts
/// (`r`'s right edge when there is none), so the message ends before it.
pub fn status_blame(app: &mut App, f: &mut Frame, r: Rect, gap: u16) -> u16 {
    if !app.settings.blame_status {
        return r.right();
    }
    let Some(b) = current_blame(app) else { return r.right() };
    let (t, hash) = (blame_text(b), b.hash.clone());
    let w = t.width() as u16 + gap;
    if r.width < w + 10 {
        return r.right();
    }
    let br = Rect::new(r.right() - w, r.y, w - gap, 1);
    f.render_widget(Span::styled(t, Style::new().fg(P.status_fg).bg(P.status_bg)), br);
    app.gitui.hits.cmds.push((br, Hit::Blame));
    if let Some(h) = hash {
        app.gitui.hits.push_hover(br, h);
    }
    br.x
}

/// Popups: the picker over everything but the prompt, and the notification.
pub fn draw_popups(app: &mut App, f: &mut Frame, area: Rect) {
    draw_toast(app, f, area);
    draw_picker(app, f, area);
}

fn draw_toast(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(t) = &app.gitui.cmds.toast else { return };
    let w = 60.min(area.width.saturating_sub(2));
    let lines: Vec<&str> = t.lines().take(4).collect();
    let h = lines.len() as u16 + 3 + 2;
    if w < 24 || area.height < h + 2 {
        return;
    }
    let r = Rect::new(area.right() - w - 1, area.bottom() - 1 - h, w, h);
    f.render_widget(Clear, r);
    let block = popup_block(P.widget_bg, P.widget_fg, (1, 0));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let icon = match crate::theme::icon_set() {
        crate::theme::IconSet::Nerd => "\u{ea87}",
        crate::theme::IconSet::Unicode => "✗",
        crate::theme::IconSet::Ascii => "!",
    };
    let mut text = vec![Line::from(vec![Span::styled(format!("{icon} "), Style::new().fg(P.error_fg)), Span::styled("Git: the command failed", Style::new().fg(P.widget_fg).bold())])];
    text.extend(lines.iter().map(|l| Line::styled(fit(l, inner.width as usize), Style::new().fg(P.widget_fg))));
    f.render_widget(ratatui::widgets::Paragraph::new(text), inner);
    let by = inner.bottom() - 1;
    let bp = sp(app.settings.spacing.button_padding.min(1));
    let mut x = inner.x;
    for (label, hit, primary) in [("Show Git Output", Hit::ToastOutput, true), ("Close", Hit::ToastClose, false)] {
        let t = format!("{bp}{label}{bp}");
        let br = Rect::new(x, by, t.width() as u16, 1).intersection(inner);
        let st = crate::ui::button_style(primary, app.hover.is_some_and(|p| br.contains(p)));
        f.render_widget(Span::styled(t.clone(), st), br);
        app.gitui.hits.cmds.push((br, hit));
        x += t.width() as u16 + 1;
    }
    app.gitui.hits.toast = r;
}

fn draw_picker(app: &mut App, f: &mut Frame, area: Rect) {
    let comfy = !app.settings.compact;
    let (ph, pv) = app.settings.spacing.popup_padding;
    let Some(p) = app.gitui.cmds.picker.as_mut() else { return };
    let extra = if comfy { 3 + 2 * pv } else { 0 };
    let pop = centered(area, (area.width * 6 / 10).max(40), (area.height * 6 / 10).max(6 + extra), true);
    if pop.width < 10 || pop.height < 3 {
        return;
    }
    f.render_widget(Clear, pop);
    let (inp, list) = if comfy {
        let block = popup_block(P.quick_bg, P.quick_fg, (ph, pv));
        let inner = block.inner(pop);
        f.render_widget(block, pop);
        (Rect::new(inner.x, inner.y, inner.width, 1).intersection(pop), Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2)).intersection(pop))
    } else {
        f.buffer_mut().set_style(pop, Style::new().bg(P.quick_bg).fg(P.quick_fg));
        (Rect::new(pop.x + 1, pop.y, pop.width.saturating_sub(2), 1).intersection(pop), Rect::new(pop.x, pop.y + 1, pop.width, pop.height.saturating_sub(1)).intersection(pop))
    };
    input(f, inp, &p.query, &p.title, true);
    f.set_cursor_position((inp.x + 1 + (p.query.width() as u16).min(inp.width.saturating_sub(2)), inp.y));
    let shown = p.shown();
    let lh = list.height.max(1) as usize;
    p.sel = p.sel.min(shown.len().saturating_sub(1));
    if p.sel < p.off {
        p.off = p.sel;
    } else if p.sel >= p.off + lh {
        p.off = p.sel + 1 - lh;
    }
    let mut hits = Vec::new();
    if p.input.is_some() {
        let hint = "Press Enter to confirm or Escape to cancel";
        row(f, Rect::new(list.x, list.y, list.width, 1).intersection(list), P.quick_bg, vec![Span::styled(fit(&format!(" {hint}"), list.width as usize), Style::new().fg(P.dim))]);
    }
    for (k, &i) in shown.iter().enumerate().skip(p.off).take(if p.input.is_some() { 0 } else { lh }) {
        let rr = Rect::new(list.x, list.y + (k - p.off) as u16, list.width, 1).intersection(list);
        if rr.is_empty() {
            break;
        }
        let on = k == p.sel;
        let (bg, fg) = if on { (P.quick_sel_bg, P.quick_sel_fg) } else { (P.quick_bg, P.quick_fg) };
        let it = &p.items[i];
        row(f, rr, bg, vec![Span::styled(fit(&format!(" {}", it.label), rr.width as usize), Style::new().fg(fg))]);
        if !it.detail.is_empty() {
            let room = (rr.width as usize).saturating_sub(it.label.width() + 4);
            if room > 4 {
                right(f, rr, vec![Span::styled(format!("{} ", fit(&it.detail, room - 1)), Style::new().fg(P.dim))]);
            }
        }
        hits.push((rr, Hit::Pick(k)));
    }
    let empty = shown.is_empty() && p.input.is_none();
    app.gitui.hits.picker = pop;
    app.gitui.hits.cmds.extend(hits);
    if empty {
        let w = width(&[Span::raw(" No matching results")]);
        f.render_widget(Span::styled(" No matching results", Style::new().fg(P.dim)), Rect::new(list.x, list.y, w.min(list.width), 1).intersection(list));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scm::ui_tests::{app, at, click, key, repo, screen, settle, sh, term, typ};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    const REMOTE: &str = "cd ..; git init -q --bare -b main remote.git; cd r; git remote add origin ../remote.git; \
                          echo a > a.txt; git add .; git commit -qm one; git push -q -u origin main 2>/dev/null";

    fn key_of(app: &App) -> String {
        active_key(app)
    }

    fn active_key(app: &App) -> String {
        app.groups[app.ag].tabs[app.groups[app.ag].active].key.clone()
    }

    #[test]
    fn status_bar_branch_picker_sync_confirm_and_publish() {
        let (t, r) = repo("status", &format!("{REMOTE}; git tag v1; git branch other; echo b >> a.txt; git commit -qam two"));
        let mut app = app(&t, &r);
        let mut tm = term();
        app.refresh();
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("main") && s.contains("0↓ 1↑"), "{s}");
        // The sync item asks first (VS Code's text); OK pulls and pushes.
        click(&mut app, at(&tm, "0↓ 1↑"));
        let p = app.prompt.as_ref().expect("sync confirm");
        assert_eq!(p.text[0], "This action will pull and push commits from and to 'origin/main'.");
        assert_eq!(p.buttons[p.sel].0, "OK", "OK by default, as in VS Code");
        assert_eq!(p.buttons[1].0, "OK, Don't Show Again");
        app.prompt.as_mut().unwrap().sel = 2;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.prompt.is_none() && app.gitui.cmds.op.is_none(), "Cancel did nothing");
        // "OK, Don't Show Again" writes git.confirmSync: false and syncs.
        let cfg = t.0.join("cfg/settings.json");
        app.settings_file = Some(cfg.clone());
        click(&mut app, at(&tm, "0↓ 1↑"));
        app.prompt.as_mut().unwrap().sel = 1;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let saved = crate::settings::parse_jsonc(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(saved["git.confirmSync"], false);
        assert!(!app.settings.confirm_sync);
        assert!(app.gitui.cmds.op.is_some(), "runs off the UI thread");
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("Syncing…"), "spinner while it runs: {s}");
        settle(&mut app);
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("0↓ 0↑") && app.msg == "Synced", "{} {s}", app.msg);
        // The branch item: a picker of create actions and refs; picking checks out.
        click(&mut app, at(&tm, "main"));
        let s = screen(&mut app, &mut tm);
        for want in ["Create new branch...", "Create new branch from...", "other", "origin/main", "v1"] {
            assert!(s.contains(want), "{want}: {s}");
        }
        // VS Code's order: create actions, the current branch, branches, remote branches, tags.
        let rows: Vec<(String, String)> = app.gitui.cmds.picker.as_ref().unwrap().items.iter().map(|i| (i.label.clone(), i.detail.clone())).collect();
        let names: Vec<&str> = rows.iter().skip(2).map(|r| r.0.as_str()).collect();
        assert_eq!(names, ["main", "other", "origin/main", "v1"], "{rows:?}");
        let details: Vec<&str> = rows.iter().skip(2).map(|r| r.1.rsplit(" · ").next().unwrap()).collect();
        assert_eq!(details, ["current branch", "branch", "remote branch", "tag"]);
        typ(&mut app, "othe");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut app);
        assert_eq!(app.branch, "other");
        // An invalid name is refused by git's own check, and the box stays open.
        run_cmd(&mut app, GitCmd::CreateBranch);
        typ(&mut app, "bad..name");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.msg.contains("not a valid branch name") && app.gitui.cmds.picker.is_some(), "{}", app.msg);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // Create branch… asks for a name; a branch without upstream shows Publish Branch.
        run_cmd(&mut app, GitCmd::CreateBranch);
        typ(&mut app, "feature x");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut app);
        settle(&mut app);
        assert_eq!(app.branch, "feature-x");
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("Publish Branch"), "{s}");
        click(&mut app, at(&tm, "Publish Branch"));
        settle(&mut app);
        settle(&mut app);
        assert_eq!(gitops::sync_status(&r).unwrap().upstream.as_deref(), Some("origin/feature-x"));
    }

    #[test]
    fn destructive_confirms_default_to_cancel() {
        let (t, r) = repo("confirm", "echo a > a.txt; git add .; git commit -qm one; git checkout -q -b gone; echo x > x.txt; git add .; git commit -qm x; git checkout -q main; echo s > s.txt; git stash -q -u");
        let mut app = app(&t, &r);
        let cancel_is_default = |app: &App| {
            let p = app.prompt.as_ref().expect("a confirm");
            p.buttons[p.sel].0 == "Cancel"
        };
        for c in [GitCmd::AbortMerge, GitCmd::AbortRebase] {
            run_cmd(&mut app, c);
            assert!(cancel_is_default(&app), "{c:?}");
            app.prompt = None;
        }
        // Drop Stash...: pick, then confirm (Cancel by default keeps it).
        run_cmd(&mut app, GitCmd::DropStash);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(cancel_is_default(&app) && app.prompt.as_ref().unwrap().text[0].contains("drop the stash"));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(gitops::stash_list(&r).unwrap().len(), 1, "Cancel kept the stash");
        // Rebase Branch...: pick, then confirm.
        run_cmd(&mut app, GitCmd::RebaseBranch);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(cancel_is_default(&app) && app.prompt.as_ref().unwrap().text[0].starts_with("Rebase 'main' onto"));
        app.prompt = None;
        // Delete Branch...: an unmerged branch asks to force it, Cancel by default.
        run_cmd(&mut app, GitCmd::DeleteBranch);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut app);
        assert!(cancel_is_default(&app) && app.prompt.as_ref().unwrap().text[0].contains("not fully merged"));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(gitops::refs(&r).unwrap().iter().any(|x| x.name == "gone"), "Cancel kept the branch");
        // Undo Last Commit on a merge asks too.
        sh(&r, "git merge -q --no-ff --no-edit gone");
        run_cmd(&mut app, GitCmd::UndoLastCommit);
        settle(&mut app);
        assert!(cancel_is_default(&app) && app.prompt.as_ref().unwrap().text[0].contains("merge commit"));
        app.prompt.as_mut().unwrap().sel = 0;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut app);
        assert_eq!(gitops::last_commit_message(&r).unwrap(), "one", "{}", app.msg);
    }

    #[test]
    fn palette_output_toast_and_cancel() {
        let (t, r) = repo("cmds", "echo a > a.txt; git add .; git commit -qm one");
        let mut app = app(&t, &r);
        let mut tm = term();
        settle(&mut app); // the startup sync-state job, so the log below is in a known order
        // F1 lists the Git commands; picking one runs it.
        let found = crate::panel::filter_commands("git push");
        assert_eq!(found.first().map(String::as_str), Some("Git: Push"));
        assert!(crate::panel::filter_commands("Git:").len() >= GIT_COMMANDS.len());
        // Push with no upstream fails: a notification, and the Git Output has the call.
        run_cmd(&mut app, GitCmd::Push);
        settle(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(app.gitui.cmds.toast.as_deref().is_some_and(|e| e.contains("no upstream")), "{s}");
        assert!(s.contains("Show Git Output"), "{s}");
        click(&mut app, at(&tm, "Show Git Output"));
        assert_eq!(active_key(&app), OUTPUT_KEY);
        // The log is this panel's own: push looked up HEAD and the upstream, then stopped.
        let log = app.gitui.log.entries();
        let args: Vec<&str> = log.iter().map(|e| e.args.as_str()).collect();
        let i = args.iter().position(|a| a.starts_with("for-each-ref")).expect("push's lookup");
        assert_eq!(
            &args[i - 2..=i],
            ["symbolic-ref -q HEAD", "rev-parse --verify -q HEAD", "for-each-ref --format=%(upstream:remotename)%1f%(upstream:remoteref) refs/heads/main"]
        );
        assert_eq!(log[i].code, Some(0));
        assert!(!args.iter().any(|a| a.starts_with("push")), "no push without an upstream");
        let Some(Doc::Text(b)) = app.docs.get(OUTPUT_KEY) else { panic!() };
        assert_eq!(b.text(), output_text(&app.gitui.log), "the tab shows exactly this log");
        // Another panel in the same process keeps its own log.
        let (t2, r2) = repo("cmds2", "echo b > b.txt; git add .; git commit -qm two");
        let mut other = crate::scm::ui_tests::app(&t2, &r2);
        run_cmd(&mut other, GitCmd::Fetch);
        settle(&mut other);
        assert!(other.gitui.log.entries().iter().any(|e| e.args == "fetch"));
        assert!(!app.gitui.log.entries().iter().any(|e| e.args == "fetch"), "not in this panel's log");
        // New calls appear in an open Git Output.
        run_cmd(&mut app, GitCmd::Fetch);
        settle(&mut app);
        let Some(Doc::Text(b)) = app.docs.get(OUTPUT_KEY) else { panic!() };
        assert!(b.text().contains(" git fetch ("), "follows new calls: {}", b.text());
        let line = b.lines.iter().find(|l| l.contains(" git fetch (")).unwrap();
        assert!(line.len() > 11 && line[..8].chars().all(|c| c.is_ascii_digit() || c == ':') && line[9..].contains(" git fetch"), "time, mark, then the command: {line}");
        // A stalled fetch is cancelled from the status bar: pipes closed, no signal.
        sh(&r, "cd ..; git init -q --bare slow.git; cd r; git remote add origin ../slow.git; git config remote.origin.uploadpack 'sleep 3; git-upload-pack'");
        run_cmd(&mut app, GitCmd::Fetch);
        std::thread::sleep(Duration::from_millis(300));
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "Fetching…"));
        let t0 = Instant::now();
        settle(&mut app);
        assert!(t0.elapsed() < Duration::from_secs(2), "cancel did not wait for git");
        assert_eq!(app.msg, "Cancelled");
        assert!(output_text(&app.gitui.log).contains(" git fetch (") && output_text(&app.gitui.log).contains("abandoned)"));
    }

    #[test]
    fn blame_status_item_and_ghost_text() {
        let (t, r) = repo("blame", "printf 'one\\ntwo\\n' > f.txt; git add .; git commit -qm first");
        let mut app = app(&t, &r);
        let mut tm = term();
        app.settings.blame_decoration = true;
        app.open_file(0, "f.txt", false);
        app.focus = Focus::Editor;
        let wait_blame = |app: &mut App| {
            tick(app);
            std::thread::sleep(BLAME_DELAY + Duration::from_millis(30));
            tick(app);
            settle(app);
        };
        wait_blame(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("Tester, now") || s.contains("Tester, 1 minute ago"), "status item: {s}");
        assert!(s.contains("• first"), "ghost text: {s}");
        // An edited line is not committed yet.
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        typ(&mut app, "!");
        wait_blame(&mut app);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("You, not committed yet"), "{s}");
        // A reply for a line the cursor has left is dropped, never shown on the wrong line.
        let stale = (key_of(&app), 0, 99);
        let line = BlameLine { hash: Some("abc".into()), author: "Stale".into(), ..Default::default() };
        drain(&mut app, Msg::Blame(stale, Ok(line)));
        let s = screen(&mut app, &mut tm);
        assert!(!s.contains("Stale") && s.contains("You, not committed yet"), "{s}");
        // The setting turns the status item off.
        app.settings.blame_status = false;
        app.settings.blame_decoration = false;
        let s = screen(&mut app, &mut tm);
        assert!(!s.contains("not committed yet"), "{s}");
    }

    #[test]
    fn conflict_actions_resolve_as_one_undoable_edit() {
        let (t, r) = repo(
            "resolve",
            "printf 'a\\nbase\\nz\\n' > f.txt; git add .; git commit -qm base; git checkout -q -b side; printf 'a\\ntheirs\\nz\\n' > f.txt; \
             git commit -qam theirs; git checkout -q main; printf 'a\\nours\\nz\\n' > f.txt; git commit -qam ours; git merge -q side || true",
        );
        let mut app = app(&t, &r);
        let mut tm = term();
        app.open_file(0, "f.txt", false);
        let s = screen(&mut app, &mut tm);
        assert!(s.contains("<<<<<<< HEAD  Current │ Incoming │ Both │ Compare"), "short labels when narrow: {s}");
        let mut tm = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 30)).unwrap();
        let s = screen(&mut app, &mut tm);
        for want in ["Accept Current Change", "Accept Incoming Change", "Accept Both Changes", "Compare Changes"] {
            assert!(s.contains(want), "{want}: {s}");
        }
        let (mx, my) = at(&tm, "<<<<<<<");
        let (merge, _) = merge_colors(&app);
        assert_eq!(tm.backend().buffer()[(mx, my)].bg, merge[0], "current header colour");
        let (ox, oy) = at(&tm, "ours");
        assert_eq!(tm.backend().buffer()[(ox, oy)].bg, merge[1], "current content colour");
        click(&mut app, at(&tm, "Compare Changes"));
        assert!(active_key(&app).starts_with("conflict:f.txt:"));
        app.groups[0].active = 0;
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "Accept Incoming Change"));
        let text = |app: &App| match app.docs.get("file:f.txt") {
            Some(Doc::Text(b)) => b.text(),
            _ => String::new(),
        };
        assert_eq!(text(&app), "a\ntheirs\nz\n");
        app.focus = Focus::Editor;
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(text(&app).contains("<<<<<<< HEAD") && text(&app).contains(">>>>>>> side"), "one undo step: {}", text(&app));
        screen(&mut app, &mut tm);
        click(&mut app, at(&tm, "Accept Both Changes"));
        assert_eq!(text(&app), "a\nours\ntheirs\nz\n");
    }

    #[test]
    fn dont_show_again_edits_settings_json_keeping_comments() {
        use crate::settings::{parse_jsonc, set_bool};
        let t = "{\n    // my theme\n    \"workbench.density\": \"compact\",\n}\n";
        let added = set_bool(t, "git.confirmSync", false);
        assert!(added.contains("// my theme"), "{added}");
        assert_eq!(parse_jsonc(&added).unwrap()["git.confirmSync"], false);
        let flipped = set_bool(&set_bool(&added, "git.confirmSync", true), "git.confirmSync", false);
        assert_eq!(flipped.matches("git.confirmSync").count(), 1, "{flipped}");
        assert_eq!(parse_jsonc(&set_bool("", "git.confirmSync", false)).unwrap()["git.confirmSync"], false);
    }

    /// The settings.json rewrite either edits exactly the real key or leaves the file byte-identical.
    #[test]
    fn settings_rewrite_is_safe_or_refused() {
        use crate::settings::{parse_jsonc, write_bool};
        let (t, _r) = repo("setfile", "true");
        let f = t.0.join("settings.json");
        let check = |before: &[u8]| -> (Result<(), String>, Vec<u8>) {
            std::fs::write(&f, before).unwrap();
            let r = write_bool(&f, "git.confirmSync", false);
            (r, std::fs::read(&f).unwrap())
        };
        // The key only in a comment: the real key is added, the comment is untouched.
        let before = "{\n    // \"git.confirmSync\": true,\n    \"a\": 1,\n}\n";
        let (r, after) = check(before.as_bytes());
        let after = String::from_utf8(after).unwrap();
        assert!(r.is_ok() && after.contains("// \"git.confirmSync\": true,"), "{r:?} {after}");
        assert_eq!(parse_jsonc(&after).unwrap()["git.confirmSync"], false);
        // `{` in a comment and in a string before the key: still the object's own brace, real value flipped.
        let before = "// see {docs}\n{\n    \"x\": \"a{b\\\"c\",\n    \"git.confirmSync\": true\n}\n";
        let (r, after) = check(before.as_bytes());
        let after = String::from_utf8(after).unwrap();
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(after, before.replace("\"git.confirmSync\": true", "\"git.confirmSync\": false"));
        // Not UTF-8, or not parseable: refused, byte-identical.
        for bad in [&b"{ \"a\": \"\xff\" }"[..], &b"{ \"a\": }"[..]] {
            let (r, after) = check(bad);
            assert!(r.as_ref().is_err_and(|e| e.contains("manually")), "{r:?}");
            assert_eq!(after, bad);
        }
        // Unreadable: refused, untouched (file mode kept).
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&f, "{ \"a\": 1 }\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = write_bool(&f, "git.confirmSync", false);
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        if std::fs::metadata(&f).is_ok() && unsafe { libc::geteuid() } != 0 {
            assert!(r.is_err(), "{r:?}");
            assert_eq!(std::fs::read(&f).unwrap(), b"{ \"a\": 1 }\n");
        }
        // A symlinked settings.json stays a link; its target gets the key. A missing file is created.
        let target = t.0.join("real.json");
        std::fs::write(&target, "{\n    \"a\": 1,\n}\n").unwrap();
        let link = t.0.join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_bool(&link, "git.confirmSync", false).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(parse_jsonc(&std::fs::read_to_string(&target).unwrap()).unwrap()["git.confirmSync"], false);
        let fresh = t.0.join("new/settings.json");
        write_bool(&fresh, "git.confirmSync", false).unwrap();
        assert_eq!(parse_jsonc(&std::fs::read_to_string(&fresh).unwrap()).unwrap()["git.confirmSync"], false);
    }

    /// Cancel never interrupts a local write: it detaches, git completes, then the panel refreshes.
    #[test]
    fn cancel_detaches_a_merge_stuck_in_a_slow_hook() {
        let (t, r) = repo(
            "detach",
            "echo a > a.txt; git add .; git commit -qm one; git checkout -q -b side; echo s > s.txt; git add .; git commit -qm s; \
             git checkout -q main; echo m > m.txt; git add .; git commit -qm m; mkdir hooks; \
             printf '#!/bin/sh\\nsleep 1.5\\n' > hooks/pre-merge-commit; chmod +x hooks/pre-merge-commit; git config core.hooksPath hooks",
        );
        let mut app = app(&t, &r);
        settle(&mut app);
        run_op(&mut app, Op::Merge("side".into()));
        std::thread::sleep(Duration::from_millis(300));
        cancel(&mut app);
        assert!(app.gitui.cmds.op.is_none(), "the UI stopped waiting");
        assert_eq!(app.msg, "Cancelled; git is finishing in the background");
        settle(&mut app);
        assert!(app.msg.contains("finished in the background"), "{}", app.msg);
        assert!(!r.join(".git/MERGE_HEAD").exists(), "not left half-merged");
        assert_eq!(gitops::commit_hover(&r, "HEAD").unwrap().commit.parents.len(), 2, "the merge completed");
    }

    #[test]
    fn one_blame_in_flight_and_late_replies_dropped() {
        let (t, r) = repo("blame1", "printf 'one\\ntwo\\n' > f.txt; git add .; git commit -qm first");
        let mut app = app(&t, &r);
        settle(&mut app);
        app.open_file(0, "f.txt", false);
        app.focus = Focus::Editor;
        tick(&mut app);
        std::thread::sleep(BLAME_DELAY + Duration::from_millis(30));
        tick(&mut app);
        assert!(app.gitui.cmds.blame.inflight);
        // The cursor moves on while that blame runs: nothing new spawns, the reply is dropped.
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        tick(&mut app);
        std::thread::sleep(BLAME_DELAY + Duration::from_millis(30));
        assert!(app.gitui.cmds.due().is_none(), "no timer while one is in flight");
        settle(&mut app);
        assert!(app.gitui.cmds.blame.line.is_none(), "the line-0 reply was dropped");
        tick(&mut app);
        settle(&mut app);
        assert_eq!(app.gitui.cmds.blame.line.as_ref().map(|l| l.0 .2), Some(1), "then line 1, once");
    }

    #[test]
    fn stash_ops_refuse_when_the_stash_list_changed() {
        let (t, r) = repo("stashhash", "echo a > a.txt; git add .; git commit -qm one; echo b > b.txt; git stash -q -u");
        let mut app = app(&t, &r);
        settle(&mut app);
        let picked = gitops::stash_list(&r).unwrap()[0].hash.clone();
        sh(&r, "echo c > c.txt; git stash -q -u");
        run_op(&mut app, Op::StashDrop(0, picked));
        settle(&mut app);
        assert!(app.gitui.cmds.toast.as_deref().is_some_and(|e| e.contains("stash list changed")), "{:?}", app.gitui.cmds.toast);
        assert_eq!(gitops::stash_list(&r).unwrap().len(), 2, "nothing dropped");
    }

    #[test]
    fn a_panicking_job_still_counts_down() {
        let (t, r) = repo("panicjob", "true");
        let mut app = app(&t, &r);
        settle(&mut app);
        assert!(!app.gitui.busy());
        app.gitui.spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
            panic!("job panics on purpose")
        });
        assert!(app.gitui.busy(), "counted while it runs");
        settle(&mut app);
        assert!(!app.gitui.busy(), "the Reply guard's Msg::Lost counted it back down");
        assert_eq!(app.msg, "a background git job failed");
    }

    /// No timer outlives its point: after any input settles, `due()` is None or in the future.
    #[test]
    fn timers_never_stay_past_due() {
        let (t, r) = repo("timers", "printf 'one\\ntwo\\nthree\\n' > f.txt; git add .; git commit -qm first");
        let mut app = app(&t, &r);
        let mut tm = term();
        settle(&mut app);
        // scm::tick is what the loop runs: the hover timer, then gitcmds::tick (blame).
        let ok = |app: &mut App| {
            crate::scm::tick(app);
            settle(app);
            crate::scm::tick(app);
            app.gitui.due().is_none_or(|d| d > Instant::now())
        };
        app.open_file(0, "f.txt", false);
        app.focus = Focus::Editor;
        // Blame line 0, then Down and back Up within the delay (the spin hc-sup5 found).
        tick(&mut app);
        std::thread::sleep(BLAME_DELAY + Duration::from_millis(30));
        tick(&mut app);
        settle(&mut app);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        tick(&mut app);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        std::thread::sleep(BLAME_DELAY + Duration::from_millis(30));
        assert!(ok(&mut app), "blame back on the blamed line");
        assert!(app.gitui.due().is_none(), "nothing left to wait for");
        // A resting pointer: the card opens and its timer is gone; leaving clears a fresh one.
        app.show_side(Side::Scm, false);
        settle(&mut app);
        screen(&mut app, &mut tm);
        let row = at(&tm, "first");
        crate::scm::ui_tests::mouse(&mut app, ratatui::crossterm::event::MouseEventKind::Moved, row);
        std::thread::sleep(crate::scm::HOVER + Duration::from_millis(30));
        assert!(ok(&mut app), "hover fired");
        assert!(app.gitui.card.is_some());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        crate::scm::ui_tests::mouse(&mut app, ratatui::crossterm::event::MouseEventKind::Moved, row);
        crate::scm::ui_tests::mouse(&mut app, ratatui::crossterm::event::MouseEventKind::Moved, (110, 1));
        std::thread::sleep(crate::scm::HOVER + Duration::from_millis(30));
        assert!(ok(&mut app), "hover left before its delay");
        // Blame with the cursor moving while one is in flight.
        if let Some(i) = app.groups[app.ag].tabs.iter().position(|t| t.key == "file:f.txt") {
            app.groups[app.ag].active = i;
        }
        app.focus = Focus::Editor;
        for _ in 0..3 {
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            tick(&mut app);
        }
        std::thread::sleep(BLAME_DELAY + Duration::from_millis(30));
        assert!(ok(&mut app), "after moves during a blame");
    }

    #[test]
    fn tiny_terminals_never_panic_with_pickers_toasts_and_status_items() {
        let (t, r) = repo("tinycmds", &format!("{REMOTE}; echo b >> a.txt"));
        for compact in [false, true] {
            let mut app = app(&t, &r);
            app.settings.compact = compact;
            app.refresh();
            settle(&mut app);
            app.open_file(0, "a.txt", false);
            for w in 20..=44 {
                for h in 3..=16 {
                    let mut tm = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                    let mut draw = |app: &mut App| {
                        tm.draw(|f| crate::ui::draw(app, f)).unwrap();
                    };
                    app.gitui.cmds.toast = Some("error: one\nfatal: two\nthree".into());
                    draw(&mut app);
                    branch_picker(&mut app);
                    draw(&mut app);
                    app.gitui.cmds.picker = None;
                    ask(&mut app, "Branch name", Input::Branch(None));
                    draw(&mut app);
                    app.gitui.cmds.picker = None;
                    sync_clicked(&mut app);
                    draw(&mut app);
                    app.prompt = None;
                    app.side = Side::Scm;
                    draw(&mut app);
                    app.side = Side::Explorer;
                    app.gitui.cmds.toast = None;
                }
            }
        }
    }
}
