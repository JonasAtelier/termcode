//! VS Code-style session restore, per workspace root. Kept in
//! `<state dir>/sessions/<hash of the root>.json`:
//! - the editor groups and their tabs (pinned or preview, the active ones), each
//!   with its cursor, selection and scroll;
//! - the sidebar (view, shown or hidden, width), the Explorer's open folders and
//!   scroll, the Search query and toggles (not the results), and the Source
//!   Control layout;
//! - the view state of recently closed files (`workbench.editor.restoreViewState`).
//!
//! It is saved about 1 s after a change (an event that can change it: no writes
//! per frame or per keystroke), on quit, on HUP/TERM and, best effort, after a
//! panic. On start it is restored as far as it still makes sense: missing files,
//! changes no longer pending and commits gone are skipped, and positions are
//! clamped. Tabs reopen lazily: a document is read when its tab is first drawn.
//! Unsaved text is not here; it comes back from the hot-exit backups.

use crate::editor::{Pos, View};
use crate::git::Section;
use crate::panel::{App, Doc, Group, Side, Tab, MAX_GROUPS};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const VERSION: u64 = 1;
/// A larger session file is ignored, and none that large is written.
const MAX_BYTES: u64 = 1 << 20;
/// Closed files whose cursor and scroll are remembered.
const MAX_VIEWS: usize = 64;
const MAX_TABS: usize = 100;
const MAX_FOLDERS: usize = 500;
const DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct State {
    /// None: sessions off (tests unless set).
    pub file: Option<PathBuf>,
    /// The debounced save.
    pub due: Option<Instant>,
    /// What was last written (or read): an unchanged session is not rewritten.
    written: Option<String>,
    /// Closed files' view states, the most recent last.
    views: Vec<(String, View)>,
    /// Restored tabs whose document is not open yet, or whose view still needs
    /// clamping to it.
    pending: HashSet<String>,
    /// Of those, the ones whose document was asked for (opened once, not per frame).
    fetching: HashSet<String>,
    /// Reopened files with a hot-exit backup, offered one prompt at a time.
    offers: std::collections::VecDeque<String>,
}

/// The session file of launch folder `key` in the plugin state directory.
pub fn path(state_dir: &Path, key: &Path) -> PathBuf {
    state_dir.join("sessions").join(format!("{}.json", crate::parse::lock_key("session", &key.to_string_lossy())))
}

/// An event that may change the session: save it a second after the last one.
pub fn touch(app: &mut App) {
    if app.session.file.is_some() {
        app.session.due = Some(Instant::now() + DEBOUNCE);
    }
}

/// From the event loop: the debounced save, and the next queued backup offer once
/// no prompt is open (true: a prompt to draw).
pub fn tick(app: &mut App) -> bool {
    if app.session.due.is_some_and(|t| Instant::now() >= t) {
        save(app);
    }
    if app.prompt.is_some() {
        return false;
    }
    match app.session.offers.pop_front() {
        Some(key) => {
            app.offer_restore(&key);
            true
        }
        None => false,
    }
}

/// A reopened file's backup: offered now, or after the prompt that is open.
pub fn offer(app: &mut App, key: &str) {
    if app.prompt.is_none() {
        app.offer_restore(key);
    } else if !app.session.offers.iter().any(|k| k == key) {
        app.session.offers.push_back(key.to_string());
    }
}

/// Write the session now (0600, atomic) unless it is unchanged or too large.
pub fn save(app: &mut App) {
    app.session.due = None;
    let Some(file) = app.session.file.clone() else { return };
    let v = snapshot(app);
    let text = v.to_string();
    if text.len() as u64 > MAX_BYTES || app.session.written.as_deref() == Some(&text) {
        return;
    }
    // ponytail: two panels on one root write the same file; the last save wins.
    if file.parent().is_some_and(|d| crate::editor::write_record(d, &file, &v).is_ok()) {
        app.session.written = Some(text);
    }
}

/// `save` that survives a broken state (after a panic): best effort, never a second panic.
pub fn save_guarded(app: &mut App) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| save(app)));
}

/// Keys of documents a session can reopen: files, working-tree and index diffs, commit diffs.
fn restorable(key: &str) -> bool {
    ["file:", "diff:staged:", "diff:changes:", "cdiff:"].iter().any(|p| key.starts_with(p))
}

fn view_json(v: &View) -> Value {
    json!([v.cursor.line, v.cursor.col, v.anchor.map(|a| [a.line, a.col]), v.scroll, v.hscroll])
}

fn view_of(v: &Value) -> Option<View> {
    let n = |i: usize| v.get(i)?.as_u64().map(|x| x as usize);
    let anchor = v.get(2).and_then(|a| Some(Pos::new(a.get(0)?.as_u64()? as usize, a.get(1)?.as_u64()? as usize)));
    Some(View { cursor: Pos::new(n(0)?, n(1)?), anchor, scroll: n(3)?, hscroll: n(4)?, ..Default::default() })
}

fn side_name(s: Side) -> &'static str {
    match s {
        Side::Explorer => "explorer",
        Side::Search => "search",
        Side::Scm => "scm",
    }
}

pub fn snapshot(app: &App) -> Value {
    let groups: Vec<Value> = app
        .groups
        .iter()
        .map(|g| {
            let tabs: Vec<Value> = g.tabs.iter().filter(|t| restorable(&t.key)).map(|t| json!({ "key": t.key, "preview": t.preview, "view": view_json(&t.view) })).collect();
            json!({ "active": g.tabs.iter().take(g.active).filter(|t| restorable(&t.key)).count(), "tabs": tabs })
        })
        .collect();
    let open: Vec<&str> = app.tree.iter().filter(|n| n.dir && n.open).map(|n| n.path.as_str()).take(MAX_FOLDERS).collect();
    let views: Vec<Value> = app.session.views.iter().map(|(k, v)| json!([k, view_json(v)])).collect();
    json!({
        "version": VERSION,
        "side": side_name(app.side),
        "sidebar_visible": app.side_visible,
        "sidebar_width": app.side_w,
        "explorer": { "open": open, "scroll": app.off[Side::Explorer as usize], "sel": app.sel[Side::Explorer as usize] },
        "search": { "query": app.query, "case": app.s_opts.0, "word": app.s_opts.1, "regex": app.s_opts.2 },
        "scm": app.gitui.layout(),
        "groups": groups,
        "active_group": app.ag,
        "views": views,
    })
}

/// The session file's JSON, if it is one of ours: small enough, valid, this version.
fn read(file: &Path) -> Option<Value> {
    let mut b = Vec::new();
    std::fs::File::open(file).ok()?.take(MAX_BYTES + 1).read_to_end(&mut b).ok()?;
    if b.len() as u64 > MAX_BYTES {
        return None;
    }
    let v: Value = serde_json::from_slice(&b).ok()?;
    (v["version"].as_u64() == Some(VERSION)).then_some(v)
}

/// Commits of `hashes` the repo no longer has (one `git cat-file --batch-check`).
fn missing_commits(app: &App, hashes: &[&str]) -> HashSet<String> {
    let mut gone: HashSet<String> = hashes.iter().map(|h| h.to_string()).collect();
    let Some(g) = app.git_root.as_ref().filter(|_| !hashes.is_empty()) else { return gone };
    let Ok(mut c) = crate::git::cmd(g).args(["cat-file", "--batch-check"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return gone };
    if let Some(mut i) = c.stdin.take() {
        for h in hashes {
            let _ = writeln!(i, "{h}^{{commit}}");
        }
    }
    // stdin is closed: git answers every line and exits.
    if let Ok(o) = c.wait_with_output() {
        // "<oid> commit <size>" for each one there, "<name> missing" for the others.
        for l in String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.ends_with(" missing")) {
            let oid = l.split(' ').next().unwrap_or_default();
            gone.retain(|h| !oid.starts_with(h.as_str()));
        }
    }
    gone
}

/// A commit id as the panel writes them: 7 to 64 hex digits.
fn is_hash(h: &str) -> bool {
    (7..=64).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A relative path that stays put: plain names only, no `..`, no root.
fn plain(p: &str) -> bool {
    let p = Path::new(p);
    !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// `p`, symlinks resolved, is inside `dir` (canonical).
fn under(p: &Path, dir: &Path) -> bool {
    p.canonicalize().is_ok_and(|c| c.starts_with(dir))
}

/// A session file is not trusted: a file key may only name a file inside the
/// workspace (relative, no `..`, no symlink out of it), or an absolute path in
/// the git root above a subfolder workspace (how its other repo files are keyed).
fn file_ok(app: &App, rel: &str) -> bool {
    let p = Path::new(rel);
    let inside = if p.is_absolute() { app.git_root.as_ref().is_some_and(|g| under(p, g)) } else { plain(rel) && under(&app.root.join(p), &app.root) };
    inside && app.root.join(p).is_file()
}

/// A repo path of a diff: plain, and (when it is on disk) not a symlink out of the repo.
fn repo_path_ok(app: &App, path: &str) -> bool {
    let Some(g) = app.git_root.as_ref() else { return false };
    let full = g.join(path);
    plain(path) && (full.symlink_metadata().is_err() || under(&full, g))
}

/// Whether `key`'s document can still be reopened, and may be.
fn usable(app: &App, key: &str, gone: &HashSet<String>) -> bool {
    if let Some(rel) = key.strip_prefix("file:") {
        return file_ok(app, rel);
    }
    if let Some((sec, path)) = key.strip_prefix("diff:").and_then(|r| r.split_once(':')) {
        // As `open_diff` names them: "staged", or "changes" for every other section.
        return repo_path_ok(app, path) && app.scm_files.iter().any(|f| f.path == path && (f.section == Section::Staged) == (sec == "staged"));
    }
    match key.strip_prefix("cdiff:").and_then(|r| r.split_once(':')) {
        // Read from the commit, not the disk: a plain path is enough.
        Some((hash, path)) => is_hash(hash) && plain(path) && app.git_root.is_some() && !gone.contains(hash),
        None => false,
    }
}

/// Bring the saved session back: per `termcode.restoreSession` the groups,
/// tabs and sidebar; per `workbench.editor.restoreViewState` the cursors and
/// scroll (of the tabs, and of files reopened later). Anything off or invalid is
/// left as a fresh start.
pub fn restore(app: &mut App) {
    let (layout, views) = (app.settings.restore_session, app.settings.restore_view_state);
    let Some(v) = app.session.file.as_deref().filter(|_| layout || views).and_then(read) else { return };
    if views {
        // Closed files, then the tabs that were open (newer): reopened by hand, they come back where they were.
        let open_tabs = v["groups"].as_array().into_iter().flatten().flat_map(|g| g["tabs"].as_array().into_iter().flatten()).map(|t| json!([t["key"], t["view"]]));
        let all: Vec<Value> = v["views"].as_array().into_iter().flatten().cloned().chain(open_tabs).collect();
        let entry = |e: &Value| Some((e.get(0)?.as_str()?.to_string(), view_of(e.get(1)?)?));
        for (k, view) in all.iter().filter_map(entry).filter(|(k, _)| restorable(k)) {
            remember(app, &k, &view);
        }
    }
    if !layout {
        return;
    }
    app.side = match v["side"].as_str() {
        Some("search") => Side::Search,
        Some("scm") => Side::Scm,
        _ => Side::Explorer,
    };
    app.side_visible = v["sidebar_visible"].as_bool().unwrap_or(app.side_visible);
    if let Some(w) = v["sidebar_width"].as_u64() {
        app.side_w = Some(w.clamp(12, 200) as u16);
    }
    // Open folders, parents before their children.
    let mut open: Vec<&str> = v["explorer"]["open"].as_array().into_iter().flatten().filter_map(Value::as_str).take(MAX_FOLDERS).collect();
    open.sort_by_key(|p| p.matches('/').count());
    let root = app.root.clone();
    for p in open.into_iter().filter(|p| plain(p) && under(&root.join(p), &root)) {
        if let Some(i) = app.tree.iter().position(|n| n.dir && !n.open && n.path == p) {
            app.toggle(i);
        }
    }
    let e = Side::Explorer as usize;
    let last = app.tree.len().saturating_sub(1);
    app.off[e] = (v["explorer"]["scroll"].as_u64().unwrap_or(0) as usize).min(last);
    app.sel[e] = (v["explorer"]["sel"].as_u64().unwrap_or(0) as usize).min(last);
    let s = &v["search"];
    app.query = s["query"].as_str().unwrap_or_default().chars().take(1000).collect();
    app.s_opts = (s["case"] == true, s["word"] == true, s["regex"] == true);
    app.gitui.restore(&v["scm"]);
    // Groups: only tabs that can still open; a group left without any is dropped.
    let saved: Vec<&Value> = v["groups"].as_array().into_iter().flatten().take(MAX_GROUPS).collect();
    let tab_list = |g: &&Value| g["tabs"].as_array().cloned().unwrap_or_default();
    let hashes: Vec<String> = saved
        .iter()
        .flat_map(tab_list)
        .filter_map(|t| Some(t["key"].as_str()?.strip_prefix("cdiff:")?.split_once(':')?.0.to_string()))
        .filter(|h| is_hash(h))
        .collect();
    let gone = missing_commits(app, &hashes.iter().map(String::as_str).collect::<Vec<_>>());
    let (mut groups, mut ag) = (Vec::new(), 0);
    for (gi, g) in saved.iter().enumerate() {
        let want = g["active"].as_u64().unwrap_or(0) as usize;
        let mut tabs: Vec<Tab> = Vec::new();
        let mut active = 0;
        for (i, t) in tab_list(g).iter().enumerate().take(MAX_TABS) {
            let Some(key) = t["key"].as_str().filter(|k| restorable(k)) else { continue };
            if tabs.iter().any(|x| x.key == key) || !usable(app, key, &gone) {
                continue;
            }
            if i <= want {
                active = tabs.len(); // the saved active tab, or the nearest one before it
            }
            let view = if views { view_of(&t["view"]).unwrap_or_default() } else { View::default() };
            tabs.push(Tab { key: key.to_string(), preview: t["preview"] == true, view });
        }
        if tabs.is_empty() {
            continue;
        }
        if gi as u64 == v["active_group"].as_u64().unwrap_or(0) {
            ag = groups.len();
        }
        app.session.pending.extend(tabs.iter().map(|t| t.key.clone()));
        groups.push(Group { tabs, active });
    }
    if !groups.is_empty() {
        app.groups = groups;
        app.ag = ag;
        app.focus = crate::panel::Focus::Editor;
    }
    app.session.written = Some(snapshot(app).to_string());
}

/// A document dropped (`App::drop_doc`): no restored tab waits for it any more.
pub fn forget(app: &mut App, key: &str) {
    app.session.pending.remove(key);
    app.session.fetching.remove(key);
}

/// A closed tab's view, for when the file is opened again.
pub fn remember(app: &mut App, key: &str, view: &View) {
    let vs = &mut app.session.views;
    vs.retain(|(k, _)| k != key);
    vs.push((key.to_string(), view.clone()));
    if vs.len() > MAX_VIEWS {
        vs.remove(0);
    }
}

/// The remembered view of `key` for a new tab (`workbench.editor.restoreViewState`),
/// clamped once its document is there.
pub fn recall(app: &mut App, key: &str) -> View {
    if !app.settings.restore_view_state {
        return View::default();
    }
    let Some(v) = app.session.views.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone()) else { return View::default() };
    app.session.pending.insert(key.to_string());
    v
}

/// Before group `g` is drawn: open its active tab's document if a restore left it
/// closed, and clamp a restored view to the document once it is there.
pub fn materialize(app: &mut App, g: usize) {
    let Some(key) = app.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map(|t| t.key.clone()) else { return };
    if !app.session.pending.contains(&key) {
        return;
    }
    if !app.docs.contains_key(&key) && !app.loading.contains_key(&key) && app.session.fetching.insert(key.clone()) {
        app.open_restored(&key);
    }
    let Some(doc) = app.docs.get(&key) else { return };
    app.session.pending.remove(&key);
    app.session.fetching.remove(&key);
    for grp in app.groups.iter_mut() {
        for t in grp.tabs.iter_mut().filter(|t| t.key == key) {
            match doc {
                Doc::Text(b) => b.clamp_view(&mut t.view),
                Doc::Diff(d) => {
                    let last = d.rows.len().saturating_sub(1);
                    t.view.scroll = t.view.scroll.min(last);
                    t.view.cursor.line = t.view.cursor.line.min(last);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scm::ui_tests::{repo, screen, settle, Tmp};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use std::fs;

    /// a..d.txt (5 lines), long.txt (50), sub/deep/x.txt, f00..f19.txt; a.txt then changed.
    fn setup(tag: &str) -> (Tmp, PathBuf) {
        repo(tag, "mkdir -p sub/deep; for f in a b c d; do seq 1 5 > $f.txt; done; seq 1 50 > long.txt; echo x > sub/deep/x.txt; \
                   for i in $(seq -w 0 19); do echo $i > f$i.txt; done; git add .; git commit -qm init; echo changed >> a.txt")
    }

    fn open(t: &Tmp, r: &Path) -> App {
        let mut app = App::new(r.to_path_buf(), Some(t.0.join("state/layout.json")));
        app.session.file = Some(path(&t.0.join("state"), r));
        app
    }

    fn tabs(app: &App) -> Vec<Vec<(String, bool)>> {
        app.groups.iter().map(|g| g.tabs.iter().map(|t| (t.key.clone(), t.preview)).collect()).collect()
    }

    fn term() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(140, 30)).unwrap()
    }

    fn write(app: &App, v: &Value) {
        let f = app.session.file.clone().unwrap();
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, v.to_string()).unwrap();
    }

    fn wait_loads(app: &mut App) {
        for _ in 0..300 {
            app.drain();
            if app.loading.is_empty() && !app.gitui.busy() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn groups_tabs_views_and_the_sidebar_round_trip_and_reopen_lazily() {
        let (t, r) = setup("sess-rt");
        let mut app = open(&t, &r);
        app.open_file(0, "a.txt", false);
        app.open_file(0, "long.txt", false);
        app.groups.push(Group::default());
        app.open_file(1, "b.txt", true);
        let i = app.scm_files.iter().position(|f| f.path == "a.txt").unwrap();
        app.open_diff(i, false); // into group 1
        let long = &mut app.groups[0].tabs[1].view;
        *long = View { cursor: Pos::new(30, 1), anchor: Some(Pos::new(28, 0)), scroll: 20, hscroll: 2, ..Default::default() };
        (app.ag, app.groups[0].active) = (0, 1);
        app.side = Side::Search;
        (app.query, app.s_opts, app.side_w) = ("needle".into(), (true, false, true), Some(40));
        for p in ["sub", "sub/deep"] {
            let i = app.tree.iter().position(|n| n.path == p).unwrap();
            app.toggle(i);
        }
        save(&mut app);
        let file = app.session.file.clone().unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);

        let mut b = open(&t, &r);
        restore(&mut b);
        assert_eq!(tabs(&b), tabs(&app));
        assert_eq!(tabs(&b)[1], [("file:b.txt".to_string(), true), ("diff:changes:a.txt".to_string(), false)], "pinned and preview kept");
        assert_eq!((b.ag, b.groups[0].active, b.groups[1].active), (0, 1, 1));
        assert_eq!(b.groups[0].tabs[1].view.cursor, Pos::new(30, 1));
        assert_eq!((b.groups[0].tabs[1].view.anchor, b.groups[0].tabs[1].view.scroll, b.groups[0].tabs[1].view.hscroll), (Some(Pos::new(28, 0)), 20, 2));
        assert_eq!((b.side, b.query.as_str(), b.s_opts, b.side_w), (Side::Search, "needle", (true, false, true), Some(40)));
        assert!(b.hits.is_empty(), "the query, not its results");
        assert!(b.tree.iter().any(|n| n.path == "sub/deep/x.txt"), "open folders reopened");
        // Lazily: nothing is read before the first frame, then only the shown tabs.
        assert!(b.docs.is_empty());
        screen(&mut b, &mut term());
        assert!(b.docs.contains_key("file:long.txt") && !b.docs.contains_key("file:a.txt"));
        wait_loads(&mut b);
        assert!(matches!(b.docs.get("diff:changes:a.txt"), Some(Doc::Diff(_))), "group 1's active diff tab");
        // Reopened tabs hold their caches the normal way, and drop_doc clears them.
        screen(&mut b, &mut term());
        assert!(b.diff_minis.contains_key("diff:changes:a.txt"), "the diff's minimap buffer");
        b.close_tab(1, 1, false);
        assert!(!b.diff_minis.contains_key("diff:changes:a.txt") && !b.docs.contains_key("diff:changes:a.txt"));
        b.close_tab(0, 0, false); // a.txt, never shown
        assert!(!b.session.pending.contains("file:a.txt") && !b.session.fetching.contains("file:a.txt"));
    }

    #[test]
    fn missing_files_stale_diffs_and_gone_commits_are_skipped_and_views_clamped() {
        let (t, r) = setup("sess-skip");
        let head = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&r).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        let head = head.trim();
        let mut app = open(&t, &r);
        let tab = |k: &str, view: Value| json!({ "key": k, "preview": false, "view": view });
        let v0 = json!([0, 0, null, 0, 0]);
        write(&app, &json!({ "version": 1, "active_group": 0, "groups": [{ "active": 2, "tabs": [
            tab("file:gone.txt", v0.clone()),
            tab("file:a.txt", json!([999, 999, [500, 3], 999, 0])),
            tab("cdiff:0123456789abcdef0123456789abcdef01234567:a.txt", v0.clone()),
            tab(&format!("cdiff:{head}:b.txt"), json!([0, 0, null, 999, 0])),
            tab("diff:staged:b.txt", v0.clone()),
            tab("diff:changes:a.txt", v0.clone()),
            tab("conflict:a.txt:3", v0.clone()),
            tab("file:../outside.txt", v0),
        ]}]}));
        restore(&mut app);
        let keys: Vec<String> = app.groups[0].tabs.iter().map(|t| t.key.clone()).collect();
        assert_eq!(keys, ["file:a.txt".to_string(), format!("cdiff:{head}:b.txt"), "diff:changes:a.txt".into()]);
        assert_eq!(app.groups[0].active, 0, "the saved active tab is gone: the one before it");
        let mut tm = term();
        screen(&mut app, &mut tm);
        let Some(Doc::Text(b)) = app.docs.get("file:a.txt") else { panic!() };
        let (last, v) = (b.lines.len() - 1, &app.groups[0].tabs[0].view);
        assert_eq!((v.cursor, v.anchor.map(|a| a.line), v.scroll), (Pos::new(last, b.lines[last].len()), Some(last), last), "clamped to the file");
        // The commit diff opens when shown, off the UI thread, and its scroll is clamped.
        app.groups[0].active = 1;
        screen(&mut app, &mut tm);
        wait_loads(&mut app);
        screen(&mut app, &mut tm);
        let Some(Doc::Diff(d)) = app.docs.get(&keys[1]) else { panic!("{:?}", app.docs.keys().collect::<Vec<_>>()) };
        assert!(!d.rows.is_empty() && app.groups[0].tabs[1].view.scroll < d.rows.len());
    }

    /// A session file is not trusted: nothing outside the repo opens, nothing panics.
    #[test]
    fn a_crafted_session_opens_nothing_outside_the_workspace_and_never_panics() {
        let (t, r) = setup("sess-evil");
        fs::write(t.0.join("secret.txt"), "secret\n").unwrap();
        fs::create_dir_all(t.0.join("outdir")).unwrap();
        fs::write(t.0.join("outdir/inner.txt"), "x\n").unwrap();
        std::os::unix::fs::symlink(t.0.join("secret.txt"), r.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(t.0.join("outdir"), r.join("outlink")).unwrap();
        let head = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&r).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        let mut app = open(&t, &r);
        let tab = |k: String| json!({ "key": k, "preview": false, "view": [0, 0, null, 0, 0] });
        let keys = [
            format!("file:{}", t.0.join("secret.txt").display()), // absolute, outside the repo
            "file:../secret.txt".into(),
            "file:sub/../../secret.txt".into(),
            "file:link.txt".into(), // a symlink out of the workspace
            "file:outlink/inner.txt".into(),
            "cdiff:éééé:a.txt".into(), // not a hash (and not ASCII)
            "cdiff:zzzzzzz:a.txt".into(),
            format!("cdiff:{}:../a.txt", head.trim()),
            "diff:changes:../a.txt".into(),
            "file:a.txt".into(),
        ];
        write(&app, &json!({ "version": 1, "explorer": { "open": ["outlink", "../", "/", "sub"] }, "groups": [{ "active": 0, "tabs": keys.map(tab) }] }));
        restore(&mut app);
        let kept: Vec<&str> = app.groups[0].tabs.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(kept, ["file:a.txt"]);
        assert!(app.tree.iter().any(|n| n.path == "sub/deep") && !app.tree.iter().any(|n| n.path.starts_with("outlink/")), "folders out of the workspace stay closed");
        screen(&mut app, &mut term());
        settle(&mut app);
        // Defence in depth: a commit diff load given a non-ASCII "hash" slices nothing apart.
        app.groups[0].tabs.push(Tab { key: "cdiff:éééééééé:a.txt".into(), preview: false, view: View::default() });
        crate::scm::load_commit_diff(&mut app, "cdiff:éééééééé:a.txt", "éééééééé", "a.txt");
        settle(&mut app);
        assert!(matches!(app.docs.get("cdiff:éééééééé:a.txt"), Some(Doc::Diff(d)) if d.note.is_some()), "a note, not a panic");
    }

    /// A subfolder workspace keys the repo's other files by absolute path: those may
    /// reopen (inside the git root), nothing else absolute.
    #[test]
    fn a_subfolder_workspace_reopens_repo_files_by_absolute_path_only() {
        let (t, r) = setup("sess-subroot");
        fs::write(t.0.join("secret.txt"), "secret\n").unwrap();
        let root = r.join("sub");
        let mut app = App::new(root.clone(), Some(t.0.join("state/layout.json")));
        app.session.file = Some(path(&t.0.join("state"), &root));
        let tab = |k: String| json!({ "key": k, "preview": false, "view": [0, 0, null, 0, 0] });
        let keys = [format!("file:{}", r.join("a.txt").display()), format!("file:{}", t.0.join("secret.txt").display()), "file:deep/x.txt".into(), "file:../b.txt".into()];
        write(&app, &json!({ "version": 1, "groups": [{ "active": 0, "tabs": keys.clone().map(tab) }] }));
        restore(&mut app);
        let kept: Vec<String> = app.groups[0].tabs.iter().map(|t| t.key.clone()).collect();
        assert_eq!(kept, [keys[0].clone(), keys[2].clone()]);
    }

    #[test]
    fn backups_of_reopened_tabs_are_offered_one_after_another() {
        let (t, r) = setup("sess-offer");
        let mut app = open(&t, &r);
        app.focus = crate::panel::Focus::Editor;
        app.open_file(0, "b.txt", false);
        crate::scm::ui_tests::typ(&mut app, "x");
        app.groups.push(Group::default());
        app.open_file(1, "c.txt", false);
        crate::scm::ui_tests::typ(&mut app, "y");
        app.sync_backups();
        save(&mut app);
        drop(app); // the panel dies: both backups are left behind
        let mut b = open(&t, &r);
        restore(&mut b);
        let mut tm = term();
        screen(&mut b, &mut tm); // both groups' tabs open: one prompt, the other queued
        assert!(b.prompt.as_ref().is_some_and(|p| p.text[0].starts_with("b.txt")), "{:?}", b.prompt.as_ref().map(|p| &p.text));
        b.prompt = None; // answered
        let mut dirty = false;
        b.step(&mut tm, &mut dirty, |_| Ok(None)).unwrap();
        assert!(b.prompt.as_ref().is_some_and(|p| p.text[0].starts_with("c.txt")), "the queued one follows");
        let shown: String = { let buf = tm.backend().buffer(); buf.content().iter().map(|c| c.symbol()).collect() };
        assert!(shown.contains("c.txt has unsaved changes"), "drawn in that same turn");
    }

    #[test]
    fn a_corrupt_unknown_or_huge_session_is_ignored() {
        let (t, r) = setup("sess-bad");
        let good = |v: u64| json!({ "version": v, "groups": [{ "active": 0, "tabs": [{ "key": "file:a.txt", "preview": false, "view": [0, 0, null, 0, 0] }] }] });
        let mut app = open(&t, &r);
        let f = app.session.file.clone().unwrap();
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        let huge = format!("{{\"version\":1,\"pad\":\"{}\"}}", "x".repeat(MAX_BYTES as usize));
        for bad in ["not json".to_string(), "".into(), "[1,2]".into(), good(99).to_string(), huge, json!({ "version": 1, "groups": "x", "views": 3, "explorer": [] }).to_string()] {
            fs::write(&f, bad).unwrap();
            restore(&mut app);
            assert_eq!(tabs(&app), vec![Vec::<(String, bool)>::new()], "a fresh start");
        }
        write(&app, &good(1));
        restore(&mut app);
        assert_eq!(tabs(&app)[0][0].0, "file:a.txt");
    }

    #[test]
    fn the_settings_turn_the_layout_and_the_views_off() {
        let (t, r) = setup("sess-off");
        let mut app = open(&t, &r);
        app.open_file(0, "long.txt", false);
        app.groups[0].tabs[0].view.cursor = Pos::new(40, 0);
        app.close_tab(0, 0, false); // remembered as closed
        app.open_file(0, "b.txt", false);
        app.groups[0].tabs[0].view.cursor = Pos::new(2, 1);
        save(&mut app);
        // Both off: nothing comes back.
        let mut b = open(&t, &r);
        (b.settings.restore_session, b.settings.restore_view_state) = (false, false);
        restore(&mut b);
        assert_eq!(tabs(&b), vec![Vec::<(String, bool)>::new()]);
        b.open_file(0, "long.txt", false);
        assert_eq!(b.groups[0].tabs[0].view.cursor, Pos::new(0, 0));
        // Only the views: no tabs, but a file opened again is where it was.
        let mut c = open(&t, &r);
        c.settings.restore_session = false;
        restore(&mut c);
        assert_eq!(tabs(&c), vec![Vec::<(String, bool)>::new()]);
        c.open_file(0, "long.txt", false);
        screen(&mut c, &mut term());
        assert_eq!(c.groups[0].tabs[0].view.cursor, Pos::new(40, 0), "closed before the save");
        c.open_file(0, "b.txt", false);
        screen(&mut c, &mut term());
        assert_eq!(c.groups[0].tabs[1].view.cursor, Pos::new(2, 1), "open at the save");
        // Only the layout: the tabs, at the top.
        let mut d = open(&t, &r);
        d.settings.restore_view_state = false;
        restore(&mut d);
        assert_eq!(tabs(&d)[0], [("file:b.txt".to_string(), false)]);
        d.open_file(0, "long.txt", false);
        assert_eq!(d.groups[0].tabs[1].view.cursor, Pos::new(0, 0));
    }

    #[test]
    fn file_line_opens_on_top_of_the_restored_session() {
        let (t, r) = setup("sess-req");
        let mut app = open(&t, &r);
        app.open_file(0, "b.txt", false);
        app.open_file(0, "long.txt", false);
        save(&mut app);
        let mut b = open(&t, &r);
        restore(&mut b);
        b.request(&format!("open:{}:20:3", r.join("c.txt").display()));
        let names: Vec<&str> = b.groups[0].tabs.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(names, ["file:b.txt", "file:long.txt", "file:c.txt"]);
        assert_eq!((b.groups[0].active, b.focus), (2, crate::panel::Focus::Editor));
        assert_eq!(b.groups[0].tabs[2].view.cursor, Pos::new(4, 1), "line 20 of a 5-line file: its last line, clamped");
    }

    #[test]
    fn saves_are_debounced_not_per_keystroke_and_skipped_when_unchanged() {
        let (t, r) = setup("sess-deb");
        let mut app = open(&t, &r);
        app.open_file(0, "long.txt", false);
        app.focus = crate::panel::Focus::Editor;
        let file = app.session.file.clone().unwrap();
        let mut tm = term();
        let mut dirty = true;
        for _ in 0..20 {
            let mut ev = Some(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
            app.step(&mut tm, &mut dirty, |_| Ok(ev.take())).unwrap();
            assert!(!file.exists(), "no write per keystroke");
        }
        let due = app.session.due.expect("a save is pending");
        assert!(due > Instant::now() + Duration::from_millis(500), "about a second after the last key");
        app.session.due = Some(Instant::now());
        app.step(&mut tm, &mut dirty, |_| Ok(None)).unwrap();
        let saved = fs::read_to_string(&file).unwrap();
        assert!(saved.contains("[20,0,null,"), "the cursor 20 lines down: {saved}");
        // Nothing changed since: a due save does not rewrite the file.
        fs::write(&file, "sentinel").unwrap();
        touch(&mut app);
        app.session.due = Some(Instant::now());
        app.step(&mut tm, &mut dirty, |_| Ok(None)).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "sentinel");
    }

    #[test]
    fn twenty_restored_tabs_read_one_file_for_the_first_frame() {
        let (t, r) = setup("sess-lazy");
        let mut app = open(&t, &r);
        for i in 0..20 {
            app.open_file(0, &format!("f{i:02}.txt"), false);
        }
        app.groups[0].active = 7;
        save(&mut app);
        let mut b = open(&t, &r);
        let t0 = Instant::now();
        restore(&mut b);
        screen(&mut b, &mut term());
        assert_eq!(b.groups[0].tabs.len(), 20);
        assert_eq!(b.docs.keys().collect::<Vec<_>>(), ["file:f07.txt"], "only the active tab, in {:?}", t0.elapsed());
        settle(&mut b);
    }
}
