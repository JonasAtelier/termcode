//! Source Control as drawn and clicked: the panes, the graph, cards and the commit
//! dialog's helpers (other modules' tests use them).

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
    let t = Tmp(std::env::temp_dir().join(format!("termcode-scm-{tag}-{}", std::process::id())));
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
             GIT_AUTHOR_DATE=\"@$(($(date +%s) - 3700)) +0000\" git commit -qm 'termcode: tighter rows for the narrow sidebar layout'; \
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
        assert!(seen.contains("termcode: tight") && seen.contains("with_a_long") && seen.contains("needle again"), "width {w}:\n{seen}");
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
    assert_eq!(app.ui.activity.iter().filter(|(_, s)| !matches!(s, Side::GitHub | Side::Stm32)).count(), 3, "no separate History item");
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
fn git_run_in_a_shell_shows_in_the_graph_and_timeline_at_the_next_status() {
    let (t, r) = repo("outside", "echo 1 > f.txt; git add f.txt; git commit -qm first");
    let mut app = app(&t, &r);
    let mut tm = term();
    app.open_file(0, "f.txt", false);
    app.show_side(Side::Explorer, false);
    screen(&mut app, &mut tm);
    settle(&mut app);
    app.show_side(Side::Scm, false);
    settle(&mut app);
    let subjects = |app: &App| app.gitui.graph.commits.iter().map(|c| c.subject.clone()).collect::<Vec<_>>();
    assert_eq!(subjects(&app), ["first"]);
    let status = |app: &mut App| {
        app.refresh();
        settle(app);
    };
    // A commit on the same branch: HEAD's name is the same, its commit is not.
    sh(&r, "echo 2 > f.txt; git commit -qam second");
    status(&mut app);
    assert_eq!(subjects(&app), ["second", "first"]);
    // A tag, and a remote branch as a fetch leaves it: no new commit, new labels.
    sh(&r, "git tag v9; git update-ref refs/remotes/origin/main HEAD~1");
    status(&mut app);
    let refs = |i: usize| app.gitui.graph.commits[i].refs.clone();
    assert!(refs(0).contains(&gitops::RefLabel::Tag("v9".into())), "{:?}", refs(0));
    assert!(refs(1).contains(&gitops::RefLabel::Remote("origin/main".into())), "{:?}", refs(1));
    // The file's TIMELINE follows too.
    assert_eq!(app.gitui.timeline.items.iter().map(|c| c.commit.subject.as_str()).collect::<Vec<_>>(), ["second", "first"]);
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
fn a_branch_colours_its_line_from_its_commit_down() {
    let (t, r) = repo(
        "tip-dots",
        "git commit -q --allow-empty -m one; git tag v1; git commit -q --allow-empty -m two; git branch side
         git commit -q --allow-empty -m plain; git commit -q --allow-empty -m three; git update-ref refs/remotes/origin/main HEAD
         git commit -q --allow-empty -m four",
    );
    let mut app = app(&t, &r);
    let mut tm = term();
    app.show_side(Side::Scm, false);
    settle(&mut app);
    screen(&mut app, &mut tm);
    let pal = pal(&app);
    let dot = |tm: &Terminal<TestBackend>, subject: &str| {
        let (x, y) = at(tm, subject);
        let b = tm.backend().buffer();
        (0..x).map(|x| &b[(x, y)]).find(|c| matches!(c.symbol(), "●" | "○")).map(|c| c.fg).unwrap()
    };
    assert_eq!(dot(&tm, "four"), pal.ref_local, "HEAD's branch");
    assert_eq!(dot(&tm, "three"), pal.ref_remote, "a remote branch takes over");
    assert_eq!(dot(&tm, "plain"), pal.ref_remote, "and keeps the line below it");
    assert_eq!(dot(&tm, "two"), P.badge_bg, "a local branch");
    assert_eq!(dot(&tm, "one"), P.badge_bg, "a tag changes nothing");
}

#[test]
fn the_focused_card_has_the_blue_border() {
    let (t, r) = repo("focus-border", "echo a > a.txt; git add .; git commit -qm one");
    let mut app = app(&t, &r);
    let mut tm = term();
    app.settings.blocks = crate::blocks::BlockStyle::Cards;
    app.open_file(0, "a.txt", false);
    app.show_side(Side::Explorer, false);
    settle(&mut app);
    app.focus = Focus::Editor;
    // Border colours of the activity bar, sidebar and editor cards (their top-left corner).
    let borders = |app: &mut App, tm: &mut Terminal<TestBackend>| {
        screen(app, tm);
        let (side, ed) = (app.ui.sidebar, app.ui.editor_area);
        let b = tm.backend().buffer();
        let lit = |x: u16, y: u16| b[(x, y)].fg == P.focus_border;
        [lit(0, side.y), lit(side.x, side.y), lit(ed.x, ed.y)]
    };
    assert_eq!(borders(&mut app, &mut tm), [false, false, true], "the editor");
    app.focus = Focus::Find;
    assert_eq!(borders(&mut app, &mut tm), [false, false, true], "Find is in the editor");
    let list = app.ui.side_list;
    click(&mut app, (list.x + 2, list.y));
    assert_eq!(app.focus, Focus::Side);
    assert_eq!(borders(&mut app, &mut tm), [false, true, false], "the Explorer");
    let search = app.ui.activity.iter().find(|(_, s)| *s == Side::Search).unwrap().0;
    click(&mut app, (search.x + 1, search.y + 1));
    assert_eq!(borders(&mut app, &mut tm), [true, false, false], "the activity bar, just clicked");
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(borders(&mut app, &mut tm), [false, true, false], "a key: back to where it went");
}

#[test]
fn references_follow_the_cursor_and_open_where_clicked() {
    let (t, r) = repo(
        "refs",
        "printf 'int foo(void);\\nint bar(void) { return foo(); }\\n' > a.c; printf 'int foo(void) { return 1; }\\n' > b.c
         git add .; git commit -qm one",
    );
    let mut app = app(&t, &r);
    let mut tm = term();
    app.open_file(0, "a.c", false);
    app.show_side(Side::Search, false);
    settle(&mut app);
    app.focus = Focus::Editor;
    let s = screen(&mut app, &mut tm);
    assert!(s.contains("REFERENCES") && s.contains("Place the cursor on a symbol"), "{s}");
    let wait = |app: &mut App, sym: &str| {
        let end = Instant::now() + Duration::from_secs(10);
        while app.gitui.refs.sym != sym || app.gitui.refs.busy() || app.gitui.refs.items.is_empty() {
            assert!(Instant::now() < end, "no references to {sym}");
            tick(app);
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    app.ed().unwrap().1.view.cursor = crate::editor::Pos::new(0, 5);
    assert!(!tick(&mut app) && app.gitui.refs.sym.is_empty(), "not before the cursor rests");
    wait(&mut app, "foo");
    let s = screen(&mut app, &mut tm);
    assert!(s.contains("REFERENCES foo"), "the symbol in the header: {s}");
    assert!(s.contains("a.c") && s.contains("b.c"), "{s}");
    assert_eq!(app.gitui.refs.items.len(), 3, "{:?}", app.gitui.refs.items);
    // Off a symbol, they stay; onto another, it follows.
    app.ed().unwrap().1.view.cursor = crate::editor::Pos::new(0, 3);
    app.ed().unwrap().1.view.cursor = crate::editor::Pos::new(0, 14);
    tick(&mut app);
    assert_eq!(app.gitui.refs.items.len(), 3);
    // A file row opens its first reference.
    click(&mut app, at(&tm, "b.c"));
    let key = &app.groups[app.ag].tabs[app.groups[app.ag].active].key;
    assert_eq!(key, "file:b.c");
    assert_eq!(app.ed().unwrap().1.view.cursor, crate::editor::Pos::new(0, 4));
    assert_eq!(app.focus, Focus::Editor);
    // The header folds it, and the fold is kept.
    click(&mut app, at(&tm, "REFERENCES"));
    assert!(!app.gitui.refs.open && !app.gitui.layout()["references_open"].as_bool().unwrap());
    let s = screen(&mut app, &mut tm);
    assert!(s.contains("REFERENCES") && app.gitui.hits.refs.height == 0, "just its header: {s}");
}

#[test]
fn the_editor_caret_hides_while_a_commit_card_is_up() {
    let (t, r) = repo("card-caret", "seq 1 40 > a.txt; git add .; git commit -qm 'add a'");
    let mut app = app(&t, &r);
    let mut tm = term();
    app.open_file(0, "a.txt", false);
    app.show_side(Side::Scm, false);
    settle(&mut app);
    app.focus = Focus::Editor;
    screen(&mut app, &mut tm);
    mouse(&mut app, MouseEventKind::Moved, at(&tm, "add a"));
    std::thread::sleep(HOVER + Duration::from_millis(50));
    assert!(tick(&mut app));
    settle(&mut app);
    screen(&mut app, &mut tm);
    assert!(app.gitui.card.is_some() && app.focus == Focus::Editor);
    let (card, text) = (app.gitui.hits.card, app.ui.groups[app.ag].text);
    // Draw with the caret on `line` (column 0); where the terminal cursor ends up.
    let mut caret_on = |app: &mut App, line: u16| {
        app.ed().unwrap().1.view.cursor = crate::editor::Pos::new(line as usize, 0);
        tm.set_cursor_position((0, 0)).unwrap();
        screen(app, &mut tm);
        tm.get_cursor_position().unwrap()
    };
    let under = Position::new(text.x, card.y + 1);
    assert!(card.contains(under), "{card:?} {text:?}");
    assert_eq!(caret_on(&mut app, under.y - text.y), (0, 0).into(), "no caret drawn over the card");
    let below = Position::new(text.x, card.bottom() + 1);
    assert!(below.y < text.bottom());
    assert_eq!(caret_on(&mut app, below.y - text.y), below, "one off the card stays");
    app.gitui.card = None;
    assert_eq!(caret_on(&mut app, under.y - text.y), under, "back once it closes");
}

#[test]
fn a_long_commit_message_scrolls_in_its_card_with_the_buttons_kept() {
    let (t, r) = repo(
        "card-long",
        "echo a > a.txt; git add .; { echo 'long one'; echo; seq -f 'body line %g' 1 40; echo; printf 'w%.0s' $(seq 150); echo TAIL; } > .git/MSG; git commit -qF .git/MSG",
    );
    let mut app = app(&t, &r);
    let mut tm = term();
    app.show_side(Side::Scm, false);
    settle(&mut app);
    screen(&mut app, &mut tm);
    let hash = app.gitui.graph.commits[0].hash.clone();
    open_card(&mut app, hash, Rect::new(5, 5, 10, 1));
    settle(&mut app);
    let s = screen(&mut app, &mut tm);
    assert!(s.contains("body line 12") && !s.contains("body line 40") && s.contains("Copy hash"), "{s}");
    // The wheel over the card scrolls the message: all of it, its long line wrapped.
    let card = app.gitui.hits.card;
    for _ in 0..30 {
        mouse(&mut app, MouseEventKind::ScrollDown, (card.x + 5, card.y + 3));
    }
    let s = screen(&mut app, &mut tm);
    assert!(s.contains("body line 40") && s.contains("TAIL") && s.contains("1 file changed") && s.contains("Copy hash"), "{s}");
    assert!(!s.contains("body line 12"), "scrolled past the top: {s}");
    for _ in 0..30 {
        mouse(&mut app, MouseEventKind::ScrollUp, (card.x + 5, card.y + 3));
    }
    assert!(screen(&mut app, &mut tm).contains("Tester"), "back at the top");
}

#[test]
fn a_message_with_tabs_and_escapes_keeps_its_card_whole() {
    let (t, r) = repo("card-tabs", r#"echo a > a.txt; git add .; printf 'json in it\n\n[\n\t{\n\t\tkey: 1\n\t}\n]\n\033[31mred\n' > .git/MSG; git commit -qF .git/MSG"#);
    let mut app = app(&t, &r);
    let mut tm = term();
    app.show_side(Side::Scm, false);
    settle(&mut app);
    screen(&mut app, &mut tm);
    let hash = app.gitui.graph.commits[0].hash.clone();
    open_card(&mut app, hash, Rect::new(5, 5, 10, 1));
    settle(&mut app);
    let s = screen(&mut app, &mut tm);
    let tab = crate::editor::tab();
    let line = |w: &str| s.lines().find(|l| l.contains(w)).unwrap_or_else(|| panic!("{w}: {s}")).to_string();
    let (outer, inner) = (line("{"), line("key: 1"));
    let col = |l: &str, w: &str| l[..l.find(w).unwrap()].chars().count();
    assert_eq!(col(&inner, "key") - col(&outer, "{"), tab, "a tab is spaces to the next stop");
    let card = app.gitui.hits.card;
    let right = |l: &str| l.chars().nth(card.right() as usize - 1);
    assert_eq!((right(&outer), right(&inner)), (right(&line("json in it")), right(&line("json in it"))), "the border stays put");
    assert!(tm.backend().buffer().content.iter().all(|c| !c.symbol().chars().any(char::is_control)), "no control character reaches the terminal");
    assert!(s.contains("[31mred"), "an escape's text shows, not its effect");
}

#[test]
fn a_cards_text_selects_by_drag_and_double_click_and_ctrl_c_copies_it() {
    let (t, r) = repo("card-sel", "echo a > a.txt; git add .; git commit -qm 'add a' -m 'the body line'");
    let mut app = app(&t, &r);
    let mut tm = term();
    app.show_side(Side::Scm, false);
    settle(&mut app);
    screen(&mut app, &mut tm);
    let hash = app.gitui.graph.commits[0].hash.clone();
    open_card(&mut app, hash, Rect::new(5, 5, 10, 1));
    settle(&mut app);
    screen(&mut app, &mut tm);
    let text = app.gitui.hits.card_text;
    let c = app.gitui.card.as_ref().unwrap();
    let row = c.rows.iter().position(|r| r.starts_with("the body line")).unwrap();
    let y = text.y + (row - c.scroll) as u16;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (text.x, y));
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (text.x + 8, y));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (text.x + 8, y));
    screen(&mut app, &mut tm);
    assert_eq!(tm.backend().buffer()[(text.x + 2, y)].bg, P.selection, "lit");
    assert_ne!(tm.backend().buffer()[(text.x + 9, y)].bg, P.selection);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "the body");
    app.last_click = None;
    click(&mut app, (text.x + 10, y));
    click(&mut app, (text.x + 10, y));
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "line", "a double-click takes the word");
    assert!(app.gitui.card.is_some(), "the card stays up");
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
    // Its buttons light grey under the pointer, like the commit dialog's.
    let copy = at(&tm, "Copy hash");
    let moved = |app: &mut App, p: (u16, u16)| app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: p.0, row: p.1, modifiers: KeyModifiers::NONE });
    assert!(moved(&mut app, copy), "onto a button: redraw");
    screen(&mut app, &mut tm);
    assert_eq!(tm.backend().buffer()[copy].bg, P.button2_hover, "lit");
    let open = at(&tm, "Open changes");
    assert_ne!(tm.backend().buffer()[open].bg, P.button2_hover, "only the one under it");
    assert!(moved(&mut app, open), "onto the other: redraw");
    screen(&mut app, &mut tm);
    assert_eq!(tm.backend().buffer()[open].bg, P.button2_hover);
    assert_ne!(tm.backend().buffer()[copy].bg, P.button2_hover, "not left lit");
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
    let row = s.lines().find(|l| l.contains("Open changes")).unwrap();
    assert!(row.find(&app.branch) > row.find("Open changes"), "the branch right after Open changes: {row}");
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
