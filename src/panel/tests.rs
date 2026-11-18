use super::*;
use ratatui::backend::TestBackend;

/// The settled screen: draws again while highlighting catches up, as the event loop
/// does (one 6 ms frame is not enough under a loaded test run).
pub fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
    t.draw(|f| crate::ui::draw(app, f)).unwrap();
    for _ in 0..10_000 {
        if !app.hl_pending() {
            break;
        }
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
    }
    assert!(!app.hl_pending(), "highlighting never settled");
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
    let d = std::env::temp_dir().join(format!("termcode-{tag}-{}", std::process::id()));
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
fn folding_button_padding_is_clickable_and_uses_shared_chevrons() {
    let root = repo("fold-button");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    screen(&mut app, &mut t);
    let gh = app.ui.groups[0].clone();
    let (x, y) = (gh.folds_x.unwrap(), gh.text.y);
    let original = app.ed().unwrap().0.text();
    assert_eq!(gh.marks_x.unwrap() - 1 - gh.area.x, 1, "one blank cell before a single-digit line number");
    assert_eq!(x - (gh.marks_x.unwrap() - 1) - 1, 1, "one cell between the number and the arrow");
    for dx in [0, 1] {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + dx, y);
        assert!(!screen(&mut app, &mut t).contains("println!"), "button column {dx} folds");
        assert_eq!(t.backend().buffer()[(x, y)].symbol(), crate::theme::icons().chev_right);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + dx, y);
        assert!(screen(&mut app, &mut t).contains("println!"), "button column {dx} unfolds");
        assert_eq!(t.backend().buffer()[(x, y)].symbol(), crate::theme::icons().chev_down);
    }
    let normal_bg = t.backend().buffer()[(x, y)].bg;
    assert!(app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: x + 1, row: y, modifiers: KeyModifiers::NONE }));
    screen(&mut app, &mut t);
    assert_ne!(t.backend().buffer()[(x, y)].bg, normal_bg, "hover lights the whole button");
    assert_eq!(gh.marks_x.unwrap() + 1, x, "Git marker remains separate");
    assert_eq!(app.ed().unwrap().0.text(), original);
    assert!(!app.ed().unwrap().0.dirty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folding_hides_body_maps_clicks_and_preserves_text() {
    let root = repo("folding");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    assert!(screen(&mut app, &mut t).contains("println!"));
    let original = app.ed().unwrap().0.text();
    key(&mut app, KeyCode::Char('['), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(!screen(&mut app, &mut t).contains("println!"), "folded body must be hidden");
    let y = app.ui.groups[0].text.y;
    assert_eq!(app.line_at(0, y + 1), Some(2));
    assert_eq!(app.line_y(0, 1), None);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.ed().unwrap().1.view.cursor.line, 2);
    assert_eq!(app.ed().unwrap().0.text(), original);
    assert!(!app.ed().unwrap().0.dirty());
    key(&mut app, KeyCode::Up, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char(']'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(screen(&mut app, &mut t).contains("println!"));
    let x = app.ui.groups[0].folds_x.unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(!screen(&mut app, &mut t).contains("println!"));
    let sb = app.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Editor(0)).unwrap();
    assert_eq!(sb.max_off, 1, "scrollbar counts visible rows");
    app.set_scroll(ScrollTarget::Editor(0), 1);
    assert_eq!(app.ed().unwrap().1.view.scroll, 2);
    app.set_scroll(ScrollTarget::Editor(0), 0);
    let (b, tab) = app.ed().unwrap();
    b.move_to(&mut tab.view, Pos::new(1, 0), false);
    app.reveal();
    assert!(screen(&mut app, &mut t).contains("println!"), "navigation reveals hidden results");
    fs::remove_dir_all(root).unwrap();
}

/// The status bar from narrow to wide: the branch item stays whole (its hit rect on
/// it), the message is cut once with "…" and keeps statusBarGap blanks before the
/// right items, which drop from their left end (Ln/Col first, the language last).
#[test]
fn the_status_message_never_runs_into_the_right_items() {
    let root = repo("statusbar");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    let msg = "opened src/ui.rs:900";
    let gap = " ".repeat(app.settings.spacing.status_gap as usize);
    let mut line = String::new();
    for w in [40u16, 60, 75, 95, 140] {
        let mut t = Terminal::new(TestBackend::new(w, 20)).unwrap();
        app.msg = msg.into();
        line = screen(&mut app, &mut t).lines().last().unwrap().to_string();
        let (r, _) = *app.gitui.hits.cmds.iter().find(|(_, h)| *h == crate::gitcmds::Hit::Branch).unwrap();
        let hit: String = (r.x..r.right()).map(|x| t.backend().buffer()[(x, r.y)].symbol()).collect();
        assert!(hit.contains(&app.branch), "{w}: the branch item and its hit rect: {hit:?} in {line:?}");
        assert!(line.contains("Rust"), "{w}: the language goes last: {line:?}");
        assert!(!line.contains("Ln") || line.contains(&format!("Ln 1, Col 1{gap}")), "{w}: {line:?}");
        if let Some(i) = line.find("opened") {
            let shown = line[i..].split(gap.as_str()).next().unwrap();
            assert!(shown == msg || shown.ends_with('…') && msg.starts_with(shown.trim_end_matches('…')), "{w}: message {shown:?} in {line:?}");
        }
    }
    assert!(line.contains(msg) && line.contains("Ln 1, Col 1"), "room for all: {line:?}");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn title_close_brightens_on_hover_and_only_activates_after_release() {
    let root = repo("title-close-release");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "README.md", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    typ(&mut app, "unsaved");
    assert!(app.ed().unwrap().0.dirty());
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    let r = app.ui.close;
    let p = (r.x + 2, r.y);
    let colours = |t: &Terminal<TestBackend>| (t.backend().buffer()[p].bg, t.backend().buffer()[(r.x, r.y)].bg);
    let brightness = |c: Color| match c { Color::Rgb(r, g, b) => r as u16 + g as u16 + b as u16, _ => 0 };
    let normal = colours(&t);
    assert_eq!(normal.0, normal.1, "the edge follows the button fill");
    assert!(app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: p.0, row: p.1, modifiers: KeyModifiers::NONE }));
    screen(&mut app, &mut t);
    let hover = colours(&t);
    assert_eq!(hover.0, hover.1, "the hovered edge follows the bright fill");
    assert!(brightness(hover.0) > brightness(normal.0), "hover brightens the fill");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), p.0, p.1);
    assert!(app.prompt.is_none() && !app.quit, "pressing must not start closing");
    screen(&mut app, &mut t);
    assert_eq!(colours(&t), normal, "pressed fill and edges return to their original colours");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), r.x - 1, r.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x - 1, r.y);
    assert!(app.prompt.is_none() && !app.quit, "releasing outside cancels");
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), p.0, p.1);
    assert!(app.prompt.is_none() && !app.quit, "release alone cannot activate the button");
    for event in [Event::FocusLost, Event::Resize(110, 30), Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))] {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), p.0, p.1);
        app.handle_event(event);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), p.0, p.1);
        assert!(app.prompt.is_none() && !app.quit, "cancelled presses cannot activate Close");
    }
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), p.0, p.1);
    mouse(&mut app, MouseEventKind::Moved, p.0, p.1);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), p.0, p.1);
    assert!(app.prompt.is_none() && !app.quit, "plain motion recovers a lost mouse release");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), p.0, p.1);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), p.0, p.1);
    assert!(app.prompt.is_none() && !app.quit, "paint the released state before closing");
    let mut dirty = true;
    let delay = app.step(&mut t, &mut dirty, |_| panic!("closing should not wait for another input event")).unwrap();
    assert_eq!(delay, Duration::ZERO);
    assert_eq!(colours(&t), hover, "release restores the bright fill and edges");
    assert!(app.prompt.is_some() && !app.quit, "unsaved changes still prompt");
    assert!(dirty, "draw the close procedure on the next frame");
    assert_eq!(fs::read_to_string(root.join("README.md")).unwrap(), "hello\n");
    let mut clean = App::new(root.clone(), None);
    wait(&mut clean);
    screen(&mut clean, &mut t);
    let r = clean.ui.close;
    mouse(&mut clean, MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
    mouse(&mut clean, MouseEventKind::Up(MouseButton::Left), r.x + 2, r.y);
    assert!(!clean.quit);
    clean.step(&mut t, &mut true, |_| panic!("clean close should not wait")).unwrap();
    assert!(clean.quit, "a clean editor closes after painting the released button");
    wait_idle(&mut app);
    wait_idle(&mut clean);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn welcome_actions_open_existing_views_and_stay_inside_editor() {
    let root = repo("welcome-actions");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    let s = screen(&mut app, &mut t);
    assert!(s.contains("herdr"));
    assert_eq!(app.ui.welcome.len(), 3);
    for (i, expected) in [None, Some(Side::Search), Some(Side::Scm)].into_iter().enumerate() {
        screen(&mut app, &mut t);
        let (r, _, _) = app.ui.welcome[i];
        assert!(app.ui.groups[0].text.contains(Position::new(r.x, r.y)));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x, r.y);
        if let Some(side) = expected {
            assert_eq!(app.side, side);
            assert!(app.side_visible);
        } else {
            assert!(!app.quick.as_ref().unwrap().commands);
            app.quick = None;
        }
    }
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn tab_accent_uses_the_left_edge_without_an_extra_row() {
    let root = repo("accent-row");
    let mut app = App::new(root.clone(), None);
    app.breadcrumbs_on = false; // their row is tested on its own
    app.open_file(0, "src/main.rs", false);
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    screen(&mut app, &mut t);
    let (a, bar) = (app.ui.groups[0].area, app.ui.groups[0].tabbar);
    assert_eq!(bar.y, a.y, "tabs start immediately inside the top border");
    assert_eq!(app.ui.groups[0].text.y, bar.y + 1, "one tab row, then code");
    assert!(matches!(app.drop_target(Position::new(bar.x + 1, a.y), true), Some((0, Zone::Tab(_)))), "a tab dropped on the tab row is inserted");
    assert_eq!(t.backend().buffer()[(a.right() - 3, a.y)].bg, crate::theme::P.tabs_bg, "no editor-coloured band");
    app.focus = Focus::Editor;
    screen(&mut app, &mut t);
    let accent = if crate::theme::ascii() { "|" } else { "▎" };
    for x in a.x..a.right() {
        let cell = &t.backend().buffer()[(x, bar.y - 1)];
        assert_eq!(cell.symbol(), crate::theme::icons().border[4], "continuous border at column {x}");
        assert_eq!(cell.fg, crate::blocks::border(true));
    }
    let cell = &t.backend().buffer()[(bar.x, bar.y)];
    assert_eq!(cell.symbol(), accent, "the accent sits at the active tab's left edge");
    assert!(cell.modifier.contains(ratatui::style::Modifier::BOLD), "the active tab's left edge is heavier");
    assert_eq!(cell.fg, crate::theme::P.tab_active_border, "the accent uses foreground colour");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), bar.x + 1, bar.y);
    assert_eq!(app.focus, Focus::Editor);
    assert!(app.prompt.is_none());
    app.open_file(0, "README.md", false);
    for selected in [1, 0] {
        app.groups[0].active = selected;
        screen(&mut app, &mut t);
        let gh = &app.ui.groups[0];
        for (i, (tab, close)) in gh.tabs.iter().enumerate() {
            let cell = &t.backend().buffer()[(tab.x, tab.y)];
            assert_eq!(cell.symbol(), accent, "every tab has the left edge");
            let edge = if i == selected { crate::theme::P.tab_active_border } else { crate::theme::P.tab_inactive_border };
            assert_eq!(cell.fg, edge, "the shown tab's accent, the others' grey");
            assert_eq!(cell.modifier.contains(ratatui::style::Modifier::BOLD), i == selected, "only the selected tab has a heavy accent line");
            assert_eq!(t.backend().buffer()[(close.x, close.y)].symbol(), crate::theme::icons().tab_close, "every tab shows its cross");
        }
    }
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn dirty_tab_keeps_its_marker_on_hover_and_close_still_prompts() {
    let root = repo("dirty-tab-marker");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    screen(&mut app, &mut t);
    let before = app.ui.groups[0].tabs[0];
    typ(&mut app, "x");
    screen(&mut app, &mut t);
    assert_eq!(app.ui.groups[0].tabs[0], before, "dirty state reserves its own slot");
    let (_, close) = before;
    app.hover = Some(Position::new(close.x, close.y));
    screen(&mut app, &mut t);
    let b = t.backend().buffer();
    let (dot, cross) = (&b[(close.x - 2, close.y)], &b[(close.x, close.y)]);
    assert_eq!((dot.symbol(), dot.fg), (crate::theme::icons().dirty, Color::Rgb(255, 255, 255)), "a white dot");
    assert_eq!(b[(close.x - 1, close.y)].symbol(), " ", "a blank between the dot and the cross");
    assert_eq!(cross.symbol(), crate::theme::icons().tab_close);
    assert!(cross.modifier.contains(Modifier::BOLD), "the cross is bold");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), close.x, close.y);
    assert!(app.prompt.is_some(), "closing an edited file still asks to save");
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
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
    let gap = " ".repeat(app.settings.spacing.icon_gap.div_ceil(2) as usize); // the icon's gap
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
    screen(&mut app, &mut t);
    let (a, b) = (app.ui.groups[0].area, app.ui.groups[1].area);
    assert_eq!(b.x, a.right() + 2, "the two cards' borders touch, no gutter column");

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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// Stacked cards share one border row (├──┤) in both densities: the Explorer over
/// TIMELINE and CHANGES over GRAPH, no gutter row, the view title in the top frame.
/// The CHANGES/GRAPH line is the splitter, lit on hover.
#[test]
fn stacked_cards_share_one_border_and_the_graph_line_is_the_sash() {
    use crate::blocks::BlockStyle;
    use crate::settings::Spacing;
    let root = repo("seam");
    for compact in [false, true] {
        let mut app = App::new(root.clone(), None);
        app.settings.compact = compact;
        app.settings.spacing = if compact { Spacing::COMPACT } else { Spacing::COMFORTABLE };
        app.settings.blocks = BlockStyle::Cards;
        app.open_file(0, "src/main.rs", false); // the Explorer shows TIMELINE under the tree
        let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
        screen(&mut app, &mut t);
        let (l, r) = (app.ui.sidebar.x, app.ui.divider.x - 1);
        let b = t.backend().buffer().clone();
        assert_eq!(b[(l, app.ui.sidebar.y)].symbol(), "╭", "{compact}: top borders share a row");
        let seams: Vec<u16> = (0..30).filter(|&y| b[(l, y)].symbol() == "├").collect();
        assert_eq!(seams.len(), 1, "{compact}: one shared row, Explorer over TIMELINE");
        let y = seams[0];
        assert_eq!((b[(r, y)].symbol(), b[(l + 1, y)].symbol()), ("┤", "─"), "{compact}");
        assert_eq!((b[(l, y - 1)].symbol(), b[(l, y + 1)].symbol()), ("│", "│"), "{compact}: no gutter, no second border");
        // The focused card's colour wins on the shared row.
        app.focus = Focus::Side;
        screen(&mut app, &mut t);
        assert_eq!(t.backend().buffer()[(l + 1, y)].fg, crate::blocks::border(true), "{compact}");

        app.side = Side::Scm;
        app.focus = Focus::Editor;
        screen(&mut app, &mut t);
        let (_, gy) = cell_of(&t, "GRAPH", 0);
        let ly = gy - 1;
        let b = t.backend().buffer();
        assert_eq!((b[(l, ly)].symbol(), b[(l + 1, ly)].symbol(), b[(r, ly)].symbol()), ("├", "─", "┤"), "{compact}: CHANGES over GRAPH");
        assert_eq!(b[(l, ly - 1)].symbol(), "│", "{compact}: no gutter row");
        assert_eq!(b[(l + 1, ly)].fg, crate::blocks::border(false));
        // Hovering the line lights it (and asks for a redraw); dragging it resizes.
        assert!(app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: l + 3, row: ly, modifiers: KeyModifiers::NONE }));
        screen(&mut app, &mut t);
        assert_eq!(t.backend().buffer()[(l + 3, ly)].fg, crate::theme::P.sash_hover, "{compact}: sash.hoverBorder");
        let ch = app.gitui.hits.changes.height;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), l + 3, ly);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), l + 3, ly + 2);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), l + 3, ly + 2);
        screen(&mut app, &mut t);
        assert_eq!(app.gitui.hits.changes.height, ch + 2, "{compact}: the line follows the pointer");
        // A click on the line (no drag) leaves GRAPH open; one on its header folds it.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), l + 3, ly + 2);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), l + 3, ly + 2);
        assert!(app.gitui.graph_open, "{compact}");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), l + 3, ly + 3);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), l + 3, ly + 3);
        assert!(!app.gitui.graph_open, "{compact}: the header still folds");
        // However short the panel, a sash is only ever on a drawn shared row.
        app.gitui.graph_open = true;
        for h in 6..30 {
            let mut t = Terminal::new(TestBackend::new(110, h)).unwrap();
            screen(&mut app, &mut t);
            for (a, _) in app.gitui.hits.areas.iter().filter(|(_, k)| *k == crate::scm::Hit::GraphSash) {
                assert_eq!(t.backend().buffer()[(l, a.y)].symbol(), "├", "{compact}: height {h}");
            }
        }
    }
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_focused_card_is_focus_border_blue() {
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
    // vs-dark: #444444, focused in focusBorder (#007FD4).
    assert_eq!(crate::blocks::border(false), Color::Rgb(0x44, 0x44, 0x44));
    assert_eq!(crate::blocks::border(true), Color::Rgb(0x00, 0x7F, 0xD4));
    wait_idle(&mut app);
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
        // Two rows an item; comfortable puts the icon on the second, one blank row between icons.
        let (ih, off) = if compact { (2, 0) } else { (2, 1) };
        let (items, b) = (app.ui.activity.clone(), t.backend().buffer().clone());
        assert!(items.iter().all(|(r, _)| r.height == ih), "{compact}: one {ih}-row cell per item");
        let views: Vec<_> = items.iter().filter(|(_, s)| !matches!(s, Side::GitHub | Side::Programmer | Side::Serial)).collect();
        assert!(views.windows(2).all(|w| w[1].0.y == w[0].0.bottom()), "the views stacked without gaps");
        // The icon, a blank kept after it (its 2-cell slot).
        let (scm, _) = items[2];
        let iy = scm.y + off;
        let ix = (scm.x..scm.right()).find(|&x| b[(x, iy)].symbol() == crate::theme::icons().scm).expect("the SCM icon");
        assert_eq!(b[(ix + 1, iy)].symbol(), " ");
        // The active item (Explorer): the indicator on the icon's row over the active
        // background, which takes half of each blank row around the icon.
        let (ex, side) = items[0];
        assert_eq!(side, Side::Explorer);
        let lifted = crate::blocks::active_bg(&serde_json::Map::new());
        let ey = ex.y + off;
        assert_eq!((b[(ex.x, ey)].symbol(), b[(ex.x, ey)].fg), ("▎", crate::theme::P.activity_border));
        assert_eq!(b[(ex.x + 1, ey)].bg, lifted);
        // Half of the blank row above (comfortable) and of the one below (the next item's top).
        for y in (ey - off..=ey + 1).filter(|&y| y != ey) {
            let half = if y < ey { "▄" } else { "▀" };
            for x in ex.x..ex.right() {
                assert_eq!((b[(x, y)].symbol(), b[(x, y)].fg, b[(x, y)].bg), (half, lifted, crate::theme::P.activity_bg), "{compact}: ({x}, {y})");
            }
        }
        assert_ne!(b[(scm.x + 1, iy)].bg, lifted, "only the active item is lifted");
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
    wait_idle(&mut app);
    wait_idle(&mut again);
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
                // The editor's left border is the resize handle; no blank gutter.
                assert_eq!(app.ui.editor_area.x, x);
                assert!(["│", "╭", "╰"].contains(&b[(x, y)].symbol()), "{st:?} row {y}: editor border");
                let edge = b[(x - 1, y)].symbol();
                assert!(["│", "╮", "╯", "┤", " "].contains(&edge), "{st:?} row {y}: {edge:?} in the card edge");
            } else {
                // One straight line, joined where a section line meets it.
                let s = b[(x, y)].symbol();
                assert!(["│", "┤", "┼", "├"].contains(&s), "{st:?} row {y}: {s:?}");
            }
        }
        if st == BlockStyle::Cards {
            assert!((1..19).filter(|&y| b[(x - 1, y)].symbol() == "│").count() > 10, "the sidebar card's edge");
        }
        let width = app.ui.sidebar.width;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, 10);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 3, 10);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 3, 10);
        screen(&mut app, &mut t);
        assert_eq!(app.ui.sidebar.width, width + 3, "{st:?}: resize follows the border");
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
    // The gutter: [editorPadding][number][marker] code: the last digit two cells left of it.
    let (_, y) = cell_of(&t, "#define MAX_PORTS", 0);
    let nx = app.ui.groups[0].marks_x.unwrap() - 1;
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
    wait_idle(&mut app);
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
    assert!(app.ed().unwrap().0.text().starts_with("// termcode settings"));
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
    wait_idle(&mut app);
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
    let mark = &t.backend().buffer()[(x - 1, y)];
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn sidebar_titles_share_the_top_frame_without_header_actions() {
    use crate::blocks::BlockStyle;
    use crate::settings::Spacing;
    let root = repo("aligned-sidebar");
    let custom = Spacing::parse(Spacing::COMFORTABLE, &serde_json::json!({"sidebarPadding":3,"iconGap":3}));
    for (compact, spacing) in [(false, Spacing::COMFORTABLE), (true, Spacing::COMPACT), (false, custom)] {
        for style in [BlockStyle::Cards, BlockStyle::Lines, BlockStyle::None] {
            let mut app = App::new(root.clone(), None);
            app.repo = "demo".into();
            app.settings.compact = compact;
            app.settings.spacing = spacing;
            app.settings.blocks = style;
            let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
            screen(&mut app, &mut t);
            // The title, section chevrons and field text share a left edge.
            let title = cell_of(&t, "EXPLORER", 0).0;
            let section = cell_of(&t, "DEMO", 0).0;
            let lead = 1 + crate::blocks::icon_gap(spacing.icon_gap) / 2; // chevron, gap
            let icon = cell_of(&t, "README.md", 0).0 - crate::blocks::icon_gap(spacing.icon_gap) / 2 - 1;
            assert_eq!(title, section - lead, "{compact}/{style:?}: view title on the chevrons' edge");
            assert_eq!(section, icon, "{compact}/{style:?}: root label aligns with file icons");
            assert_eq!(cell_of(&t, "TIMELINE", 0).0, section, "{compact}/{style:?}: Timeline and Explorer headers align");
            println!("{style:?} compact={compact} padding={}: title={title}, section={section}, file_icon={icon}", spacing.sidebar_padding);
            let explorer_title = title;
            let top = app.ui.sidebar.y;
            assert_eq!(cell_of(&t, "EXPLORER", 0).1, top);
            assert!(!app.ui.find_btns.iter().any(|(r, _)| r.y == top));
            if style == BlockStyle::Cards {
                let b = t.backend().buffer();
                for x in [app.ui.activity[0].0.x - 1, app.ui.sidebar.x, app.ui.editor_area.x] {
                    assert_eq!(b[(x, top)].symbol(), "╭", "all three top frames align");
                }
                assert_eq!(b[(app.ui.sidebar.right() - 1, top)].symbol(), "╮");
            }
            app.show_side(Side::Search, true);
            screen(&mut app, &mut t);
            let input = app.ui.side_input;
            let title = cell_of(&t, "SEARCH", 0).0;
            assert_eq!(input.x, app.ui.side_list.x + spacing.sidebar_padding.saturating_sub(1) / 2);
            assert_eq!(title, explorer_title, "Search title matches Explorer");
            assert_eq!(cell_of(&t, "SEARCH", 0).1, top);
            assert!(!app.ui.find_btns.iter().any(|(r, _)| r.y == top));
            if style == BlockStyle::Cards {
                assert_eq!(input.y, top + 1, "input starts directly inside the frame");
                assert_eq!(t.backend().buffer()[(app.ui.sidebar.x, top)].symbol(), "╭");
            }
            assert_eq!(cell_of(&t, "Search", 0).0, title, "{compact}/{style:?}: the field's text on the left edge");
            for (r, b) in app.ui.find_btns.clone() {
                if b == FindBtn::SearchCase {
                    let old = app.s_opts.0;
                    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x, r.y);
                    assert_ne!(app.s_opts.0, old, "toggle hit follows rendered icon");
                }
            }
            app.show_side(Side::Scm, false);
            screen(&mut app, &mut t);
            assert_eq!(cell_of(&t, "SOURCE CONTROL", 0).0, explorer_title, "SCM title matches Explorer");
            assert_eq!(cell_of(&t, "SOURCE CONTROL", 0).1, top);
            assert!(!app.ui.find_btns.iter().any(|(r, _)| r.y == top));
            assert!(!app.gitui.hits.cmds.iter().any(|(r, _)| r.y == top));
            if style == BlockStyle::Cards {
                assert_eq!(t.backend().buffer()[(app.ui.sidebar.x, top)].symbol(), "╭");
            }
            assert_eq!(app.ui.commit_btn.x, app.ui.side_list.x + spacing.sidebar_padding.saturating_sub(1) / 2);
            assert_eq!(app.ui.commit_btn.right(), input.right());
            assert_eq!(cell_of(&t, "WORKING TREE", 0).0, section, "WORKING TREE matches Explorer's section label");
            assert_eq!(cell_of(&t, "GRAPH", 0).0, section, "GRAPH matches Explorer's section label");
            wait_idle(&mut app);
            app.git_root = None;
            let s = screen(&mut app, &mut t);
            assert!(s.contains("repository."), "{s}");
            assert_eq!(cell_of(&t, "SOURCE CONTROL", 0).1, top);
            if style == BlockStyle::Cards {
                assert_eq!(t.backend().buffer()[(app.ui.sidebar.x, top)].symbol(), "╭", "non-Git folders keep the frame");
            }
        }
    }
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn narrow_search_controls_and_text_stay_inside_the_input() {
    let root = repo("narrow-alignment");
    let mut app = App::new(root.clone(), None);
    app.query = "a very long search query".into();
    app.show_side(Side::Search, true);
    for width in [12, 18, 28] {
        for padding in [0, 3, 8] {
            app.side_w = Some(width);
            app.settings.spacing.sidebar_padding = padding;
            let mut t = Terminal::new(TestBackend::new(75, 20)).unwrap();
            screen(&mut app, &mut t);
            let input = app.ui.side_input;
            for (r, b) in &app.ui.find_btns {
                if matches!(b, FindBtn::SearchCase | FindBtn::SearchWord | FindBtn::SearchRegex) {
                    assert_eq!(r.intersection(input), *r, "{width}/{padding}: drawn toggle stays inside input");
                    let icon = match b {
                        FindBtn::SearchCase => crate::theme::icons().case,
                        FindBtn::SearchWord => crate::theme::icons().word,
                        _ => crate::theme::icons().regex,
                    };
                    assert_eq!(t.backend().buffer()[(r.x, r.y)].symbol(), icon);
                }
            }
            assert!(input.x >= app.ui.sidebar.x && input.right() <= app.ui.sidebar.right());
        }
    }
    wait_idle(&mut app);
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
        let gap = crate::blocks::icon_gap(sp.icon_gap) / 2;
        let fr = crate::blocks::frame(st);
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        screen(&mut app, &mut t);
        assert_eq!(app.ui.divider.x - app.ui.sidebar.x, sp.sidebar_width + fr, "{tag}: default width (and frame) until dragged");
        // Cards touch at their borders; lines reserve one separator column.
        assert_eq!(app.ui.sidebar.x, sp.activity_width + fr + u16::from(st == BlockStyle::Lines), "{tag}");
        // The activity icon is drawn inside its hit rect: the second row (the first when
        // compact), its 2-cell slot centred right of the indicator column.
        let (ar, _) = app.ui.activity[2];
        let (ix, iy) = (ar.x + 1 + ar.width.saturating_sub(3) / 2, ar.y + u16::from(!compact));
        assert_eq!(t.backend().buffer()[(ix, iy)].symbol(), crate::theme::icons().scm, "{tag}");
        // Explorer header: the root's name; the tree starts on the next row.
        let (_, hy) = cell_of(&t, &app.repo.to_uppercase(), 0);
        assert_eq!(app.ui.side_list.y, hy + 1, "{tag}: tree rows start below it");
        let (x, y) = cell_of(&t, "README.md", 0);
        // [icon][gap blanks][name]: the icon is one cell, then only blanks (wide icons:
        // the remaining blank accommodates their 2-cell slot).
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
        // Halved tab padding is shared across both edges; the dirty marker keeps its slot.
        let (tr, xr) = app.ui.groups[0].tabs[0];
        let buf = t.backend().buffer();
        for k in 1..=sp.icon_gap.div_ceil(2) {
            assert_eq!(buf[(tr.x + sp.tab_padding.div_ceil(4) + k, tr.y)].symbol(), " ", "{tag}: gap after the tab icon");
            assert_eq!(buf[(xr.x - k, xr.y)].symbol(), " ", "{tag}: gap before the close button");
        }
        assert_eq!(buf[(xr.x, xr.y)].symbol(), crate::theme::icons().tab_close, "{tag}");
        // The title Close button uses half the configured edge padding, rounded up.
        assert_eq!(app.ui.close.right() + sp.title_padding.div_ceil(2), 120, "{tag}");
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
        wait_idle(&mut app);
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
fn minimap_colours_finish_without_scrolling_or_input() {
    let root = repo("minimap-first-colours");
    fs::write(root.join("colours.c"), (0..320).map(|i| format!("// coloured minimap line {i}\n")).collect::<String>()).unwrap();
    for size in [crate::minimap::Size::Proportional, crate::minimap::Size::Fill] {
        for characters in [true, false] {
            let mut app = App::new(root.clone(), None);
            app.open_file(0, "colours.c", false);
            app.settings.minimap.size = size;
            app.settings.minimap.render_characters = characters;
            let mut t = Terminal::new(TestBackend::new(160, 30)).unwrap();
            crate::editor::set_frame_budget(Some(Duration::ZERO));
            let mut dirty = true;
            for step in 0..1000 {
                app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
                if !app.hl_pending() { break; }
                assert!(step < 999, "minimap never settles");
            }
            assert_eq!(app.groups[0].tabs[0].view.scroll, 0, "no scroll is needed");
            let area = app.ui.minimaps[0].area;
            let first_access = t.backend().buffer().clone();
            crate::editor::set_frame_budget(None);
            let (b, _) = app.ed().unwrap();
            b.highlight_upto(b.lines.len());
            t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
            for p in area.positions() {
                assert_eq!(first_access[p], t.backend().buffer()[p], "{size:?}/{characters}: cold minimap differs from fully coloured one at {p:?}");
            }
            wait_idle(&mut app);
        }
    }
    crate::editor::set_frame_budget(Some(crate::editor::FRAME_BUDGET));
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
    let d = std::env::temp_dir().join(format!("termcode-panic-{}", std::process::id()));
    log_panic(&d, "thread 'git' panicked at x.rs:1:1:\nboom");
    log_panic(&d, "thread 'nav' panicked at y.rs:2:2:\nagain");
    let log = fs::read_to_string(d.join("panic.log")).unwrap();
    assert!(log.contains("boom") && log.contains("again"), "{log}");
    assert!(log.contains(&format!(" termcode {}: thread 'git'", env!("CARGO_PKG_VERSION"))), "dated and versioned: {log}");
    // The report carries the newest one; the next start says so once.
    assert_eq!(last_error(&d).as_deref(), Some(&*format!("{} / again", log.lines().nth(2).unwrap())));
    assert!(take_crash(&d) && !take_crash(&d), "said once");
    assert!(!d.join("panic.log").exists() && last_error(&d).is_some_and(|e| e.ends_with("again")), "kept for the report");
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// herdr closes a pane with HUP: the loop ends, and the exit path writes the
/// session next to the backups. (The handler is called as the signal would.)
#[test]
fn hup_ends_the_loop_and_saves_the_session() {
    let root = repo("hup");
    let state = std::env::temp_dir().join(format!("termcode-hup-state-{}", std::process::id()));
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
    app.breadcrumbs_on = false; // these tests measure the editor's own rows
    app.open_file(0, "long.c", false);
    wait(&mut app);
    app.update_qdiff();
    assert_eq!(app.qdiff["file:long.c"].hunks.len(), 3);
    (root, app)
}

#[test]
fn keybindings_run_palette_commands_turn_keys_off_and_show_in_f1() {
    let root = repo("keys");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "README.md", false);
    app.open_file(0, "src/main.rs", false);
    let json = r#"{ "termcode.keybindings": { "ctrl+alt+m": "View: Toggle Minimap", "ctrl+q": "", "f6": "View: Close Editor", "ctrl+w": "Find" } }"#;
    let s = crate::settings::resolve(&crate::settings::parse_jsonc(json).unwrap()).unwrap();
    app.apply_settings(s);
    let on = app.minimap_on;
    key(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL | KeyModifiers::ALT);
    assert_eq!(app.minimap_on, !on, "Ctrl+Alt+M runs its command");
    key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
    assert!(!app.quit && app.prompt.is_none(), "\"\" turns Ctrl+Q off");
    // F6 closes the editor the way Ctrl+W does by default, though Ctrl+W is rebound.
    let tabs = app.groups[0].tabs.len();
    key(&mut app, KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(app.groups[0].tabs.len(), tabs - 1, "F6: View: Close Editor");
    key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
    assert!(app.find.is_some() && app.groups[0].tabs.len() == tabs - 1, "Ctrl+W: Find now");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    // F1 shows the user's key beside its command.
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    app.open_palette();
    typ(&mut app, "toggle minimap");
    let s = screen(&mut app, &mut t);
    assert!(s.lines().any(|l| l.contains("View: Toggle Minimap") && l.contains("Ctrl+Alt+M")), "{s}");
    // A typo is an error, not a key that silently does nothing.
    for (bad, why) in [(r#"{"termcode.keybindings": {"ctrl+m": "View: Toggle Minimapp"}}"#, "no command"),
                       (r#"{"termcode.keybindings": {"hyper+m": "Find"}}"#, "unknown modifier")] {
        let e = crate::settings::resolve(&crate::settings::parse_jsonc(bad).unwrap()).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn report_issue_copies_the_setup_and_names_where_to_report() {
    let root = repo("report");
    let mut app = App::new(root.clone(), None);
    app.run_command(Cmd::ReportIssue);
    assert!(app.msg.contains("https://github.com/JonasAtelier/termcode-releases/issues/new/choose"), "{}", app.msg);
    let text = &app.clip;
    assert!(text.starts_with(&format!("termcode {}\n", env!("CARGO_PKG_VERSION"))), "{text}");
    for part in ["OS: ", "Terminal: TERM=", "Settings: icons ", "Tools: git "] {
        assert!(text.contains(part), "{part}: {text}");
    }
    assert!(!text.contains(&*root.to_string_lossy()) && !text.contains("report-"), "nothing about the files: {text}");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_repository_whose_settings_run_programs_gets_no_git_until_trusted() {
    let root = repo("untrusted");
    fs::write(root.join("a.txt"), "changed\n").unwrap();
    let marker = root.join("../untrusted-ran");
    let _ = fs::remove_file(&marker);
    let hook = root.join(".git/evil.sh");
    fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    let _ = Command::new("chmod").arg("+x").arg(&hook).status();
    let _ = Command::new("git").arg("-C").arg(&root).args(["config", "core.fsmonitor"]).arg(&hook).status();
    let mut app = App::new(root.clone(), None);
    assert!(app.git_root.is_none() && app.untrusted.as_ref().is_some_and(|(_, r)| r[0].starts_with("core.fsmonitor=")));
    // Every view and a refresh: still nothing runs the hook.
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.refresh();
    for side in SIDES {
        app.side = side;
        screen(&mut app, &mut t);
        wait_idle(&mut app);
    }
    app.commit();
    assert_eq!(app.msg, "Git is off until you trust this repository (see Source Control).");
    let s = screen(&mut app, &mut t);
    assert!(s.contains("Git is off in this repository") && s.contains("core.fsmonitor="), "{s}");
    assert!(!marker.exists(), "the repository's program ran");
    // Trust Repository: saved, and the panel reloads with git on.
    let (x, y) = cell_of(&t, "Trust Repository", 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(app.reload && app.quit, "reloads to start git");
    let again = App::new(root.clone(), None);
    assert!(again.git_root.is_some() && again.untrusted.is_none(), "trusted now");
    let _ = fs::remove_file(crate::trust::store());
    fs::remove_dir_all(&root).unwrap();
    let _ = fs::remove_file(&marker);
}

#[test]
fn a_256_or_16_colour_terminal_gets_no_24_bit_colour() {
    use crate::colors::{test_depth, Depth};
    let (root, mut app) = changed_file("depth");
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    for (d, ok) in [(Depth::C256, &(|c: Color| matches!(c, Color::Indexed(16..)) || c == Color::Reset) as &dyn Fn(Color) -> bool),
                    (Depth::C16, &|c: Color| !matches!(c, Color::Rgb(..) | Color::Indexed(_)))] {
        test_depth(Some(d));
        for side in SIDES {
            app.side = side;
            screen(&mut app, &mut t);
            let bad: Vec<Color> = t.backend().buffer().content.iter().flat_map(|c| [c.fg, c.bg]).filter(|&c| !ok(c)).take(3).collect();
            assert!(bad.is_empty(), "{d:?} {side:?}: {bad:?}");
        }
    }
    test_depth(None);
    screen(&mut app, &mut t);
    assert!(t.backend().buffer().content.iter().any(|c| matches!(c.bg, Color::Rgb(..))), "24-bit by default");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn without_a_nerd_font_no_private_use_glyph_is_drawn() {
    use crate::theme::{test_icons, IconSet};
    let (root, mut app) = changed_file("nonerd");
    let mut t = Terminal::new(TestBackend::new(140, 36)).unwrap();
    let nerd = |t: &Terminal<TestBackend>| -> Vec<String> {
        let pua = |ch: char| matches!(ch, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{10ffff}');
        t.backend().buffer().content.iter().map(|c| c.symbol()).filter(|s| s.chars().any(pua)).map(String::from).collect()
    };
    for set in [IconSet::Nerd, IconSet::Unicode, IconSet::Ascii] {
        test_icons(Some(set));
        let mut seen = Vec::new();
        // Every view: file icons, tabs, the SCM badges and pills, the graph, the status bar.
        for side in SIDES {
            app.side = side;
            screen(&mut app, &mut t);
            wait_idle(&mut app);
            screen(&mut app, &mut t);
            seen.extend(nerd(&t));
        }
        if set == IconSet::Nerd {
            assert!(!seen.is_empty(), "the check sees Nerd glyphs");
        } else {
            assert!(seen.is_empty(), "{set:?} drew {seen:?}");
        }
    }
    test_icons(None);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn input_wait_tells_idle_ready_and_a_closed_terminal_apart() {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: valid out pointers; the name, termios and winsize arguments may be null.
    let opened = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) };
    assert_eq!(opened, 0);
    // SAFETY: both descriptors are open. A child forked by another test must not inherit them.
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    let start = Instant::now();
    assert_eq!(wait_input(slave, Duration::from_millis(30)), Input::Idle);
    assert!(start.elapsed() >= Duration::from_millis(25), "it waited");
    // SAFETY: both are open descriptors from openpty and the buffer is 2 bytes.
    unsafe { assert_eq!(libc::write(master, b"x\n".as_ptr().cast(), 2), 2) };
    assert_eq!(wait_input(slave, Duration::from_secs(1)), Input::Ready);
    // SAFETY: each descriptor is closed once.
    unsafe { libc::close(master) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while wait_input(slave, Duration::from_millis(20)) != Input::Gone {
        assert!(Instant::now() < deadline, "the other end closed");
    }
    unsafe { libc::close(slave) };
}

fn regex_app(tag: &str) -> (PathBuf, App) {
    let root = repo(tag);
    fs::write(root.join("a.c"), "foo1 = 1;\nbar\nfoo22 = 2;\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    app.open_find(false);
    (root, app)
}

#[test]
fn the_regex_toggle_switches_find_to_a_regular_expression() {
    let (root, mut app) = regex_app("find-regex");
    typ(&mut app, r"foo\d+");
    assert_eq!(app.find.as_ref().unwrap().matches.len(), 0, "literal text");
    app.find_button(FindBtn::Regex);
    assert_eq!(app.find.as_ref().unwrap().matches.len(), 2);
    typ(&mut app, "(");
    app.find_refresh();
    let f = app.find.as_ref().unwrap();
    assert!(f.invalid && f.matches.is_empty(), "a pattern that does not compile says so");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn replace_all_expands_regex_groups() {
    let (root, mut app) = regex_app("find-regex-replace");
    typ(&mut app, r"foo(\d+)");
    app.find_button(FindBtn::Regex);
    app.find.as_mut().unwrap().replace = Some("bar$1".into());
    app.find_button(FindBtn::ReplaceAll);
    let Some(Doc::Text(b)) = app.docs.get("file:a.c") else { panic!() };
    assert_eq!(b.lines[..3], ["bar1 = 1;", "bar", "bar22 = 2;"]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn alt_arrows_move_lines_and_shift_arrows_still_select() {
    let root = repo("move-lines");
    fs::write(root.join("a.c"), "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    let lines = |app: &App| match app.docs.get("file:a.c") {
        Some(Doc::Text(b)) => b.lines.clone(),
        _ => unreachable!(),
    };
    key(&mut app, KeyCode::Down, KeyModifiers::ALT);
    assert_eq!(lines(&app), ["two", "one", "three"]);
    key(&mut app, KeyCode::Up, KeyModifiers::ALT);
    assert_eq!(lines(&app), ["one", "two", "three"]);
    key(&mut app, KeyCode::Down, KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(lines(&app), ["one", "one", "two", "three"]);
    key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    assert!(app.groups[0].tabs[0].view.selection().is_some(), "Shift+Down extends a selection");
    assert_eq!(lines(&app).len(), 4, "and changes nothing");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_pointer_lights_inactive_tabs_status_items_and_buttons() {
    use crate::theme::P;
    let (root, mut app) = changed_file("hover-bg");
    fs::write(root.join("b.c"), "int b;\n").unwrap();
    app.groups[0].tabs[0].preview = false;
    app.open_file(0, "b.c", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let tabs = app.ui.groups[0].tabs.clone();
    let (idle, active) = if app.groups[0].active == 0 { (tabs[1].0, tabs[0].0) } else { (tabs[0].0, tabs[1].0) };
    let bg = |t: &Terminal<TestBackend>, r: Rect| t.backend().buffer()[(r.x + 1, r.y)].bg;
    assert_eq!(bg(&t, idle), P.tab_inactive_bg, "no pointer: plain");
    app.hover = Some(Position::new(idle.x + 1, idle.y));
    screen(&mut app, &mut t);
    assert_eq!(bg(&t, idle), P.tab_hover);
    app.hover = Some(Position::new(active.x + 1, active.y));
    screen(&mut app, &mut t);
    assert_eq!(bg(&t, active), P.tab_active_bg, "the active tab stays as it is");
    let branch = app.gitui.hits.cmds.iter().find(|(_, h)| matches!(h, crate::gitcmds::Hit::Branch)).unwrap().0;
    app.hover = Some(Position::new(branch.x, branch.y));
    screen(&mut app, &mut t);
    assert_eq!(bg(&t, branch), P.status_hover);
    app.show_side(Side::Scm, false);
    app.hover = None;
    screen(&mut app, &mut t);
    let commit = app.ui.commit_btn;
    assert_eq!(bg(&t, commit), P.button_bg);
    assert_eq!(app.gitui.hits.changes.y + 1, app.ui.side_list.y, "the list starts right under Add commit");
    app.hover = Some(Position::new(commit.x + 2, commit.y));
    screen(&mut app, &mut t);
    assert_eq!(bg(&t, commit), P.button_hover);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ctrl_shift_arrows_edit_several_lines_at_once() {
    let root = repo("multi-cursor");
    fs::write(root.join("a.c"), "aa\nbb\ncc\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    let both = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let lines = |app: &App| match app.docs.get("file:a.c") {
        Some(Doc::Text(b)) => b.lines.clone(),
        _ => unreachable!(),
    };
    key(&mut app, KeyCode::Down, both);
    typ(&mut app, "X");
    assert_eq!(lines(&app), ["Xaa", "Xbb", "cc"]);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let buf = t.backend().buffer();
    let (x, y) = (0..30).flat_map(|y| (0..119).map(move |x| (x, y))).find(|&(x, y)| buf[(x, y)].symbol() == "X" && buf[(x + 1, y)].symbol() == "a").unwrap();
    assert!(buf[(x + 1, y)].modifier.contains(Modifier::REVERSED), "the other cursor is drawn");
    app.paste_text("Z");
    assert_eq!(lines(&app), ["XZaa", "XZbb", "cc"], "a paste goes to both");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.groups[0].tabs[0].view.extra.is_empty(), "Esc leaves one cursor");
    key(&mut app, KeyCode::Up, both);
    assert_eq!(app.groups[0].tabs[0].view.extra.len(), 1);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert!(app.groups[0].tabs[0].view.extra.is_empty(), "a key that cannot apply to all leaves one cursor");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn whitespace_is_drawn_where_the_setting_asks() {
    let root = repo("whitespace");
    fs::write(root.join("a.c"), "aq  bz\tcy dx\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let cell = |t: &Terminal<TestBackend>, x: u16, y: u16| t.backend().buffer()[(x, y)].symbol().to_string();
    let origin = |t: &Terminal<TestBackend>| {
        (0..30).flat_map(|y| (0..119).map(move |x| (x, y))).find(|&(x, y)| cell(t, x, y) == "a" && cell(t, x + 1, y) == "q").unwrap()
    };
    screen(&mut app, &mut t);
    let (x, y) = origin(&t);
    assert_eq!((cell(&t, x + 2, y), cell(&t, x + 3, y)), ("·".to_string(), "·".to_string()), "two spaces");
    assert_eq!(cell(&t, x + 6, y), "→", "the tab");
    assert_eq!(cell(&t, x + 10, y), " ", "a single space between words");
    app.settings.render_whitespace = editor::Whitespace::None;
    screen(&mut app, &mut t);
    assert_eq!(cell(&t, x + 2, y), " ", "off");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn other_uses_of_the_cursor_word_are_highlighted() {
    let root = repo("word-hl");
    fs::write(root.join("a.c"), "foo Foo foobar q\nw foo\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    app.groups[0].tabs[0].view.cursor = Pos::new(0, 1);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let buf = t.backend().buffer();
    let cells: Vec<(u16, u16)> = (0..30).flat_map(|y| (0..120).map(move |x| (x, y))).collect();
    let starts = |text: &str| -> Vec<(u16, u16)> {
        cells.iter().copied().filter(|&(x, y)| x + text.len() as u16 <= 120 && text.chars().enumerate().all(|(k, c)| buf[(x + k as u16, y)].symbol() == c.to_string())).collect()
    };
    let word = crate::theme::P.word_hl;
    let foo = starts("foo ");
    assert_ne!(buf[starts("w foo")[0]].bg, word, "other words stay plain");
    let hit = starts("w foo")[0];
    assert_eq!(buf[(hit.0 + 2, hit.1)].bg, word, "the same word on another line");
    assert_eq!(buf[foo[0]].bg, word, "the word itself");
    assert_ne!(buf[starts("Foo")[0]].bg, word, "case matters");
    assert_ne!(buf[starts("foobar")[0]].bg, word, "whole words only");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_bracket_pair_at_the_cursor_is_highlighted() {
    let root = repo("bracket");
    fs::write(root.join("a.c"), "f(x);\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    app.groups[0].tabs[0].view.cursor = Pos::new(0, 1);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let buf = t.backend().buffer();
    let at = |s: &str| (0..120).flat_map(|x| (0..30).map(move |y| (x, y))).find(|&p| buf[p].symbol() == s).unwrap();
    let (open, close, x) = (at("("), at(")"), at("x"));
    assert_eq!(buf[open].bg, crate::theme::P.bracket);
    assert_eq!(buf[close].bg, crate::theme::P.bracket);
    assert_ne!(buf[x].bg, crate::theme::P.bracket, "only the pair");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ctrl_s_tidies_the_file_it_saves() {
    let root = repo("save-tidy");
    fs::write(root.join("a.c"), "x\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.c", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    typ(&mut app, "y  ");
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(fs::read_to_string(root.join("a.c")).unwrap(), "y  x\n", "spaces inside the line stay");
    let t = &mut app.groups[0].tabs[0].view;
    t.cursor = Pos::new(0, 4);
    typ(&mut app, "  ");
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(fs::read_to_string(root.join("a.c")).unwrap(), "y  x\n", "trailing ones go");
    let _ = fs::remove_dir_all(&root);
}

/// 162 lines whose first block (lines 1..=60) folds away, opened in a proportional minimap.
fn foldable_file(tag: &str) -> (PathBuf, App, Terminal<TestBackend>) {
    let root = repo(tag);
    let mut lines = vec!["void a() {".to_string()];
    lines.extend((1..=60).map(|i| format!("    int body_{i} = {i};")));
    lines.push("}".into());
    lines.extend((1..=100).map(|i| format!("int tail_{i} = {i};")));
    fs::write(root.join("f.c"), lines.join("\n") + "\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.breadcrumbs_on = false;
    app.open_file(0, "f.c", false);
    wait(&mut app);
    app.settings.minimap.size = crate::minimap::Size::Proportional;
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let v = &mut app.groups[0].tabs[0].view;
    v.collapsed.insert(0);
    v.rebuild_folds();
    screen(&mut app, &mut t);
    (root, app, t)
}

#[test]
fn minimap_counts_only_the_rows_a_fold_leaves() {
    let (root, mut app, mut t) = foldable_file("mini-fold");
    // The first cell's 4 dot rows are lines 0, 61, 62, 63; only "}" stops at the first dot column.
    let m = app.ui.minimaps[0];
    assert_eq!(t.backend().buffer()[(m.area.x + 1, m.area.y)].symbol(), "\u{28EF}");
    app.groups[0].tabs[0].view.scroll = 70;
    screen(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    assert_eq!(m.mini.n, 102, "162 lines less the 60 folded away");
    assert_eq!(m.scroll, 10, "line 70: row 0 is line 0, row k is line 60 + k");
    // Row 10 at 4 rows a cell: the slider starts on cell 3 (2.5 rounded).
    let bg = |y: u16| t.backend().buffer()[(m.area.right() - 1, m.area.y + y)].bg;
    assert_eq!(bg(2), crate::theme::P.mini_bg, "above the slider");
    assert_ne!(bg(3), crate::theme::P.mini_bg, "the slider's first cell");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn minimap_click_past_a_fold_leaves_the_fold_closed() {
    let (root, mut app, _t) = foldable_file("mini-fold-click");
    let m = app.ui.minimaps[0];
    let want = m.mini.click_scroll(m.scroll, 8);
    assert!(want >= 1, "lands below the folded header");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), m.area.x + 3, m.area.y + 8);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), m.area.x + 3, m.area.y + 8);
    let v = &app.groups[0].tabs[0].view;
    assert!(v.collapsed.contains(&0), "the click scrolls, it does not unfold");
    assert_eq!(v.scroll, want + 60, "row k is line 60 + k");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_find_match_shows_in_a_squeezed_minimap_wherever_it_falls() {
    let root = repo("mini-find");
    let mut lines: Vec<String> = (0..2000).map(|i| format!("int line_{i} = {i};")).collect();
    lines[1] = "int needle = 1;".into();
    fs::write(root.join("big.c"), lines.join("\n") + "\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "big.c", false);
    wait(&mut app);
    app.settings.minimap.size = crate::minimap::Size::Fill;
    app.settings.minimap.slider_always = false;
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.open_find(false);
    typ(&mut app, "needle");
    app.focus = Focus::Editor;
    screen(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    let cell = &t.backend().buffer()[(m.area.right() - 1, m.area.y)];
    assert_eq!(cell.bg, crate::theme::P.mini_find, "line 1 lies between the sampled lines");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn minimap_draws_git_lane_find_and_slider_and_scrolls() {
    let (root, mut app) = changed_file("mini");
    app.settings.minimap.slider_always = false; // Explicit mouseover mode remains supported.
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    let buf = t.backend().buffer();
    // Git lane: the modified line 5 (index 4) on its row, in minimapGutter.modifiedBackground.
    let row = m.mini.row_of(m.scroll, 4).unwrap();
    let lane = &buf[(m.area.x, m.area.y + row)];
    // Line 5 is its cell's first dot row (lines 5-8).
    assert_eq!((lane.symbol(), lane.fg), ("⠉", crate::theme::P.mini_modified));
    // Braille by default (VS Code's renderCharacters): 4 lines a cell, a dot per run of
    // columns with code ("int line_N = N;": the first two dot columns), nothing past it.
    let p = &crate::theme::P;
    let first = buf[(m.area.x + 1, m.area.y)].clone();
    assert_eq!(first.symbol(), "⣿", "lines 1-4, columns 0..10: all code");
    assert_eq!(buf[(m.area.x + 4, m.area.y)].symbol(), "⠀", "the fourth cell: past every line");
    // Muted: the cell's token colour over the background, never the raw colour.
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
    // Mouseover shows the slider in explicit mouseover mode, translucent: blended
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// VS Code's gutter: [editorPadding][number][marker] code, the marker cell (or a
/// blank one) the single cell between number and code, in the editor, its peek and
/// diffs alike; the numbers right-aligned to the line count's width, 2 digits at least.
#[test]
fn gutters_are_one_cell_and_line_up() {
    let (root, mut app) = changed_file("gutter");
    assert_eq!(app.settings.spacing.editor_padding, 0);
    let mut t = Terminal::new(TestBackend::new(120, 34)).unwrap();
    let sym = |t: &Terminal<TestBackend>, x: u16, y: u16| t.backend().buffer()[(x, y)].symbol().to_string();
    // A short file: a two-digit field gives one blank before a single digit.
    app.open_file(0, "README.md", false);
    screen(&mut app, &mut t);
    let gh = app.ui.groups[0].clone();
    let (x, y) = cell_of(&t, "hello", 0);
    assert_eq!((x, gh.marks_x, gh.text.x), (gh.area.x + 3, Some(gh.area.x + 2), x), "two digits, the marker, the code");
    assert_eq!((sym(&t, gh.area.x, y), sym(&t, gh.area.x + 1, y), sym(&t, x - 1, y)), (" ".into(), "1".into(), " ".into()));
    assert_eq!(gh.text.y, gh.area.y + 1, "the accent uses the border without reserving a row");
    // long.c, 100 lines: the marker of a changed line sits right between number and code.
    app.open_file(0, "long.c", false);
    screen(&mut app, &mut t);
    let gh = app.ui.groups[0].clone();
    let mx = gh.marks_x.unwrap();
    let (x, y) = cell_of(&t, "int line_5 = 50;", 0);
    assert_eq!((x, sym(&t, mx - 1, y), sym(&t, mx, y)), (mx + 1, "5".into(), crate::theme::icons().gutter_bar.into()), "5▎int");
    // The quick-diff peek: its numbers and signs in the editor's columns.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), mx, y);
    screen(&mut app, &mut t);
    let (x, y) = cell_of(&t, "int line_5 = 5;", 0);
    assert_eq!((x, sym(&t, mx, y), sym(&t, mx - 1, y)), (mx + 1, "-".into(), "5".into()), "removed row");
    let (zy, zh) = app.ui.groups[0].zone.unwrap();
    let y = (zy..zy + zh).find(|&y| line_of(&t, y).contains("int line_5 = 50;")).unwrap();
    let x = line_of(&t, y).find("int line_5 = 50;").map(|i| line_of(&t, y)[..i].chars().count() as u16).unwrap();
    assert_eq!((x, sym(&t, mx, y), sym(&t, mx - 1, y)), (mx + 1, "+".into(), "5".into()), "added row");
    app.peek = None;
    // A diff: [pad][number][sign]code on both sides, compact without the pad.
    open_big_diff(&mut app);
    for compact in [false, true] {
        app.settings.compact = compact;
        app.settings.spacing = if compact { crate::settings::Spacing::COMPACT } else { crate::settings::Spacing::COMFORTABLE };
        screen(&mut app, &mut t);
        let (x, y) = cell_of(&t, "line 020 changed", 0);
        let row = line_of(&t, y);
        assert!(row.contains(" 21+line 020 changed") && row.contains(" 21-line 020 "), "compact {compact}: {row}");
        assert_eq!(sym(&t, x - 1, y), "+");
        let gh = &app.ui.groups[0];
        assert_eq!(gh.text.y, gh.area.y + 1, "compact {compact}: only the tab row precedes the editor");
    }
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn rulers_sit_after_n_columns_behind_the_text() {
    let root = repo("rulers");
    // Ruler at 10: tabs expand to 4, CJK is 2 wide.
    let text = format!("\t\tab\n中文中文中\n中文中文中文\n0123456789X\n0123456789\nnext\n{}\n", "x".repeat(120));
    fs::write(root.join("r.txt"), &text).unwrap();
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut a);
    wait_idle(&mut b);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn search_streams_and_stops_at_cap() {
    let tmp = std::env::temp_dir().join(format!("termcode-cap-{}", std::process::id()));
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("many.txt"), "e\n".repeat(MAX_HITS * 3)).unwrap();
    let mut app = App::new(tmp.clone(), None);
    app.query = "e".into();
    app.start_search();
    wait(&mut app);
    assert!(app.search.is_none(), "rg still running");
    assert_eq!(app.hits.len(), MAX_HITS);
    assert!(app.msg.contains("search stopped"), "{}", app.msg);
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// Status, HEAD and the sync state come in one job: a refresh runs no git on the UI
/// thread, and the last sync state shows until the new one lands.
#[test]
fn a_refresh_runs_no_git_on_the_ui_thread() {
    let root = repo("uigit");
    let mut app = App::new(root.clone(), None);
    app.show_side(Side::Scm, false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    screen(&mut app, &mut t);
    let before = git::CALLS.with(|n| n.get());
    app.refresh();
    assert!(app.gitui.cmds.sync.is_some(), "the last sync state shows meanwhile");
    wait(&mut app);
    screen(&mut app, &mut t);
    assert_eq!(git::CALLS.with(|n| n.get()), before, "git ran on the UI thread");
    assert!(app.gitui.cmds.sync.is_some());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_folder_without_git_has_no_scm_and_runs_no_git() {
    let d = std::env::temp_dir().join(format!("termcode-nogit-{}", std::process::id()));
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
    wait_idle(&mut app);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn launcher_requests_open_files_and_other_pastes_insert() {
    let root = repo("req");
    let mut app = App::new(root.clone(), None);
    app.token = Some("t0k".into());
    let main = root.join("src/main.rs");
    let req = |p: &Path, rest: &str| format!("termcode:t0k:open:{}{rest}", p.display());
    // A request opens the file pinned at line:col (col in characters); nothing is inserted.
    app.on_paste(&req(&main, ":2:5"));
    let tab = &app.groups[0].tabs[app.groups[0].active];
    assert_eq!((tab.key.as_str(), tab.preview, tab.view.cursor), ("file:src/main.rs", false, Pos::new(1, 4)));
    assert_eq!(app.focus, Focus::Editor);
    let clean = "fn main() {\n    println!(\"needle\");\n}";
    assert!(text(&app, "file:src/main.rs").starts_with(clean));
    // Refused, never inserted: outside the root, not a regular file, bad numbers, a relative path.
    let out = std::env::temp_dir().join(format!("termcode-req-out-{}", std::process::id()));
    fs::write(&out, "x").unwrap();
    let bad = [
        (req(&out, ":1:1"), "outside"),
        (req(&root.join("src"), ":1:1"), "not a regular file"),
        (req(&main, ":x:1"), "open:"),
        (req(&main, ":1"), "open:"),
        ("termcode:t0k:open:src/main.rs:1:1".into(), "open:"),
        ("termcode:t0k:shell:true".into(), "shell"),
    ];
    for (body, why) in bad {
        app.on_paste(&body);
        assert!(app.msg.starts_with("request refused") && app.msg.contains(why), "{body}: {}", app.msg);
    }
    assert_eq!(app.groups[0].tabs.len(), 1);
    assert!(text(&app, "file:src/main.rs").starts_with(clean));
    // Another token is no request, and any other paste is ordinary text.
    app.on_paste("termcode:bad:open:/x:1:1");
    app.on_paste("hi");
    assert!(text(&app, "file:src/main.rs").contains("    termcode:bad:open:/x:1:1hiprintln!"));
    fs::remove_file(&out).unwrap();
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// A non-git tree arch/{sh/{boards/b00..b59.c, kernel.c}, x86/x.c}, block/{blk,blk2}.c,
/// zz.txt, with arch, sh and boards expanded.
fn deep_tree(tag: &str) -> (PathBuf, App) {
    let d = std::env::temp_dir().join(format!("termcode-sticky-{tag}-{}", std::process::id()));
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
fn explorer_sticky_row_is_the_folder_path() {
    let (d, mut app) = deep_tree("one");
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let (sh, boards, x86, b10, b59) = (at(&app, "arch/sh"), at(&app, "arch/sh/boards"), at(&app, "arch/x86"), at(&app, "arch/sh/boards/b10.c"), at(&app, "arch/sh/boards/b59.c"));
    app.off[0] = b10;
    screen(&mut app, &mut t);
    // The default, one row: the full path of the folder the first visible row is in.
    assert_eq!((app.settings.sticky_max, app.ui.sticky.clone()), (1, vec![boards]));
    let (x, y) = (app.ui.side_list.x, app.ui.side_list.y);
    let (_, hy) = cell_of(&t, &app.repo.to_uppercase()[..10], 0); // a long name is cut with …
    assert_eq!(y, hy + 1);
    // Nothing of the row it covers shows through after the path.
    assert!(side_line(&t, &app, y).trim_end().ends_with("arch › sh › boards"), "{}", side_line(&t, &app, y));
    // The separator shares the path row: the next file immediately follows it.
    assert_eq!(app.ui.sticky_h, 1, "the separator must not reserve an empty row");
    assert!(side_line(&t, &app, y + 1).contains("b11.c"));
    let buf = t.backend().buffer();
    for cx in x..app.ui.side_list.right() {
        let c = &buf[(cx, y)];
        assert!(c.modifier.contains(Modifier::UNDERLINED), "separator at the path row's bottom: col {cx}");
        assert_eq!(c.underline_color, crate::theme::P.sticky_line);
    }
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 3, y + 1);
    assert_eq!(app.sel[0], b10 + 1, "first visible file is clickable, not a sticky row");
    assert!(![crate::theme::P.sidebar_fg, crate::theme::P.editor_fg, Color::White].contains(&crate::theme::P.sticky_line));
    // It follows the folder ends: boards' last file visible, then sh's next row.
    app.off[0] = b59 - 2;
    screen(&mut app, &mut t);
    assert_eq!(app.ui.sticky, [boards]);
    app.off[0] = b59;
    screen(&mut app, &mut t);
    assert_eq!(app.ui.sticky, [sh], "kernel.c, sh's, is the first visible row");
    assert!(side_line(&t, &app, y).contains("arch › sh") && !side_line(&t, &app, y).contains("boards"));
    app.off[0] = x86 - 1;
    screen(&mut app, &mut t);
    assert!(app.ui.sticky.len() == 1 && app.tree[app.ui.sticky[0]].path == "arch", "x86, arch's, is the first visible row");
    app.off[0] = x86;
    screen(&mut app, &mut t);
    assert!(app.ui.sticky.is_empty(), "block, top level, is the first visible row");
    // Each name is its own target: sh scrolls back so it is visible again.
    app.off[0] = b10;
    screen(&mut app, &mut t);
    let seg = |app: &App, name: &str| app.ui.sticky_segs.iter().find(|(_, i)| app.tree[*i].name == name).map(|s| s.0);
    let r = seg(&app, "sh").unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x, r.y);
    assert_eq!(app.sel[0], sh);
    screen(&mut app, &mut t);
    assert!(side_line(&t, &app, y + (sh - app.off[0]) as u16).contains("sh"), "sh on screen");
    // Too narrow: cut from the left, the deepest names kept.
    (app.side_w, app.off[0]) = (Some(19), b10);
    screen(&mut app, &mut t);
    assert!(side_line(&t, &app, y).contains("… › sh › boards") && !side_line(&t, &app, y).contains("arch"), "{}", side_line(&t, &app, y));
    assert!(seg(&app, "arch").is_none() && seg(&app, "boards").is_some());
    wait_idle(&mut app);
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
    let rows: Vec<String> = (0..5).map(|k| side_line(&t, &app, y + k)).collect();
    // Each pinned row is drawn as in the tree: its own indent, chevron and icon.
    let col = |r: &str, name: &str| r.find(name).map(|i| r[..i].chars().count()).unwrap_or_else(|| panic!("{name} in {r:?}"));
    let ind = app.settings.spacing.tree_indent as usize;
    assert_eq!(col(&rows[1], "sh"), col(&rows[0], "arch") + ind, "{rows:?}");
    assert_eq!(col(&rows[2], "boards"), col(&rows[1], "sh") + ind, "{rows:?}");
    assert!(rows[..3].iter().all(|r| r.contains(crate::theme::icons().chev_down)), "{rows:?}");
    // Each pinned row is only itself: nothing of the longer row under it shows.
    for (r, name) in rows.iter().zip(["arch", "sh", "boards"]) {
        assert!(r.trim_end().ends_with(name), "{rows:?}");
    }
    // No separator-only row hides b13.
    assert!(rows[3].contains("b13.c") && rows[4].contains("b14.c"), "{rows:?}");
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
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 4, y + 4);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 4, y + 4);
    assert_eq!(app.groups[0].tabs[0].key, "file:arch/sh/boards/b14.c");
    wait_idle(&mut app);
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
    assert_eq!(app.ui.sticky, [boards], "1: one row, the path of the folder the first visible row is in");
    app.settings.sticky_max = 7;
    for h in [10, 12, 16] {
        let mut t = Terminal::new(TestBackend::new(100, h)).unwrap();
        screen(&mut app, &mut t);
        let lh = app.ui.side_list.height as usize;
        // With their separator row: never over 40% of the list.
        assert_eq!(app.ui.sticky.len(), 3.min(lh * 2 / 5), "{lh} rows");
        assert!(app.ui.sticky_h as usize <= lh * 2 / 5, "{lh} rows");
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
        // The separator does not add a row, including with a configured gap.
        let sep = 0;
        assert_eq!(app.ui.sticky_h, 3 * step + sep, "{tag}");
        assert_eq!(app.ui.sticky_at(y + 3 * step + sep), None, "{tag}: the tree below");
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
    wait_idle(&mut app);
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
        assert!(t.backend().buffer()[(x, y)].modifier.contains(Modifier::UNDERLINED), "{name}: separator shares the header row");
        assert!(side_line(&t, &app, y + 1).contains("05.c"), "{name}: files follow the header immediately");
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn arrows_in_search_results_show_each_match_as_they_go() {
    let root = repo("search-arrows");
    fs::write(root.join("a.txt"), "x\narrow_token here\n").unwrap();
    fs::write(root.join("b.txt"), "arrow_token there\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.show_side(Side::Search, false);
    app.query = "arrow_token".into();
    app.start_search();
    wait(&mut app);
    (app.sel[1], app.focus) = (0, Focus::Side);
    let files: Vec<String> = app.search_rows().iter().filter_map(|r| if let HRow::File(p, _) = r { Some(format!("file:{p}")) } else { None }).collect();
    assert_eq!(files.len(), 2);
    // Down from a file's row onto its match: shown, the keys still in the list.
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let tab = |app: &App| app.groups[0].tabs.get(app.groups[0].active).map(|t| (t.key.clone(), t.preview));
    assert_eq!((tab(&app), app.focus), (Some((files[0].clone(), true)), Focus::Side));
    let line = if files[0] == "file:a.txt" { 1 } else { 0 };
    assert!(matches!(app.ed(), Some((_, t)) if t.view.cursor.line == line), "at the match");
    // On over the next file's row (nothing opens) to its match: the preview moves there.
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(tab(&app).map(|t| t.0), Some(files[0].clone()), "a file's row shows nothing new");
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!((tab(&app), app.groups[0].tabs.len()), (Some((files[1].clone(), true)), 1), "one preview tab, replaced");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn search_file_groups_fold_without_opening_files_or_losing_results() {
    let root = repo("search-folds");
    fs::write(root.join("aaa.txt"), "fold_token\n".repeat(30)).unwrap();
    fs::write(root.join("zzz.txt"), "fold_token\n".repeat(2)).unwrap();
    let mut app = App::new(root.clone(), None);
    app.show_side(Side::Search, false);
    app.query = "fold_token".into();
    app.start_search();
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    let header = |app: &mut App, t: &mut Terminal<TestBackend>, path: &str| {
        let i = app.search_rows().iter().position(|r| matches!(r, HRow::File(p, _) if p == path)).unwrap();
        app.sel[1] = i;
        app.off[1] = i;
        app.focus = Focus::Side;
        screen(app, t);
        (app.ui.side_list.x + 1, app.ui.side_list.y)
    };
    let (x, y) = header(&mut app, &mut t, "aaa.txt");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
    assert_eq!(app.search_rows().len(), 4, "the arrow hides only that file's matches");
    assert!(app.groups[0].tabs.is_empty(), "folding must not open the file");
    assert_eq!(app.hits.len(), 32, "folding keeps all search results");
    assert!(screen(&mut app, &mut t).contains("32 results in 2 files"));
    header(&mut app, &mut t, "aaa.txt");
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(app.search_rows().len(), 34);
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(app.search_rows().len(), 4);
    key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert_eq!(app.search_rows().len(), 34);
    let g = app.search_rows().iter().position(|r| matches!(r, HRow::File(p, _) if p == "aaa.txt")).unwrap();
    app.off[1] = g + 5;
    screen(&mut app, &mut t);
    assert_eq!(app.ui.sticky, [g]);
    let (x, y) = (app.ui.side_list.x + 1, app.ui.side_list.y);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
    assert_eq!(app.search_rows().len(), 4, "a pinned header's arrow also folds");
    assert_eq!(app.sel[1], g);
    assert!(app.off[1] <= g, "the folded header stays visible");
    assert!(app.groups[0].tabs.is_empty());
    header(&mut app, &mut t, "zzz.txt");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(app.search_rows().len(), 2, "both headers remain when all matches are folded");
    assert!(screen(&mut app, &mut t).contains("32 results in 2 files"));
    app.start_search();
    wait(&mut app);
    assert_eq!(app.search_rows().len(), 34, "a new search starts expanded");
    screen(&mut app, &mut t);
    let (x, y) = cell_of(&t, "aaa.txt", 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(app.groups[0].tabs[0].key, "file:aaa.txt", "the file name still opens a preview");
    assert_eq!(app.search_rows().len(), 34);
    header(&mut app, &mut t, "zzz.txt");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    app.sel[1] = app.search_rows().iter().position(|r| matches!(r, HRow::Hit(h) if app.hits[*h].path == "aaa.txt" && app.hits[*h].line == 12)).unwrap();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.groups[0].tabs[0].view.cursor.line, 11, "result indices still target the correct line after folding another file");
    wait_idle(&mut app);
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
    let y = app.ui.side_list.y;
    assert!(side_line(&t, &app, y).contains("many.txt"));
    let (name_x, _) = cell_of(&t, "many.txt", 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), name_x, y);
    assert_eq!((app.off[1], app.sel[1]), (g, g), "the group's header back on top");
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// The sticky rows at `off` must be the folders, outermost first, holding the
/// first row visible below them (`off + pinned`), and no more could be pinned.
fn assert_sticky_holds(depth: &dyn Fn(usize) -> Option<usize>, pinned: &[usize], off: usize, cap: usize, sep: usize, tag: &str) {
    let c = pinned.len();
    let first = off + c + sep;
    for (k, &a) in pinned.iter().enumerate() {
        assert_eq!(depth(a), Some(k), "{tag} off {off}: pin {k} is at depth {k}");
        assert_eq!(decor::walk_back(depth, first, k), Some(a), "{tag} off {off}: pin {k} ({a}) holds the first visible row {first}");
    }
    // One more pin (the folder holding the row it would cover) would have to hold
    // the first row visible below it too.
    if c < cap {
        let more = decor::walk_back(depth, off + c, c);
        let holds_next = more.is_some_and(|a| decor::walk_back(depth, off + c + 1 + sep, c) == Some(a));
        assert!(!holds_next, "{tag} off {off}: {pinned:?} could pin {more:?} too");
    }
}

/// The one-row sticky Explorer at `off`: the folder holding the first row visible
/// below the row and its separator, while its top-level folder is above the list.
fn assert_breadcrumb_holds(app: &App, off: usize, tag: &str) {
    let first = off + 1 + app.sticky_sep();
    let depth = |j: usize| app.tree.get(j).map(|n| n.depth);
    let folder = depth(first).and_then(|d| d.checked_sub(1)).and_then(|d| decor::walk_back(depth, first, d));
    let want: Vec<usize> = folder.filter(|&f| decor::walk_back(depth, f, 0).unwrap_or(f) < off).into_iter().collect();
    assert_eq!(app.ui.sticky, want, "{tag} off {off}: the folder of row {first}");
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
            if cap == 1 {
                assert_breadcrumb_holds(&app, off, &tag);
            } else {
                assert_sticky_holds(&depth, &app.ui.sticky, off, cap, app.sticky_sep(), &tag);
            }
            // What the screen says: the sticky row's name, then a row inside it.
            if let Some(&a) = app.ui.sticky.first() {
                let y = app.ui.side_list.y;
                assert!(side_line(&t, &app, y).contains(&app.tree[a].name), "{tag}");
            }
        }
    }
    wait_idle(&mut app);
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
            assert_sticky_holds(&depth, &app.pinned(off, cap, &rows, &[]), off, cap, app.sticky_sep(), "search");
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
        assert_sticky_holds(&depth, &app.pinned(off, 1, &[], &rows), off, 1, app.sticky_sep(), "changes");
    }
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
            // Nested rows from arch, or (room for one) the breadcrumb of boards.
            assert!(app.ui.sticky[0] == arch || app.ui.sticky == [at(&app, "arch/sh/boards")], "{w}x{h}");
            assert!(app.ui.sticky_h as usize <= l.height as usize * 2 / 5 && l.bottom() <= h, "{w}x{h}: {l:?}");
            mouse(&mut app, MouseEventKind::ScrollDown, l.x, l.y);
            screen(&mut app, &mut t);
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), l.x, l.y);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), l.x, l.y);
            screen(&mut app, &mut t);
        }
    }
    assert!(stuck > 0, "some tiny size is in a sticky state");
    wait_idle(&mut app);
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
    let doc = DiffDoc { title: "big.c".into(), rows: rows.clone(), note: None, file, id: Default::default(), patch: None };
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
fn diff_syntax_colours_match_both_original_code_views() {
    let root = repo("diff-syntax");
    let old = "/* original\n * note\n */\nstatic int value(void) {\n    return 1;\n}\n";
    let new = "static int value(void) {\n    /* inserted\n     * note\n     */\n    return 2;\n}\n";
    let mut app = App::new(root.clone(), None);
    app.settings.render_whitespace = editor::Whitespace::None; // dots are not syntax colours
    // ponytail: the diff editor does not colour bracket pairs yet.
    app.settings.bracket_colors = false;
    app.minimap_on = false;
    app.guides_on = false;
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(160, 30)).unwrap();
    let mut colours = Vec::new();
    for (path, source) in [("original.c", old), ("modified.c", new)] {
        fs::write(root.join(path), source).unwrap();
        app.open_file(0, path, false);
        wait_idle(&mut app);
        screen(&mut app, &mut t);
        let text = app.ui.groups[0].text;
        colours.push(source.lines().enumerate().map(|(n, line)| {
            (0..line.len()).map(|col| t.backend().buffer()[(text.x + col as u16, text.y + n as u16)].fg).collect::<Vec<_>>()
        }).collect::<Vec<_>>());
    }
    assert_ne!(colours[0][3][0], crate::theme::P.editor_fg, "the regular editor colours C keywords");
    let rows = diff::align(old, new);
    let key = "cdiff:abc:syntax.c".to_string();
    let file = ScmFile { section: Section::Changes, path: "syntax.c".into(), orig: None, letter: 'M' };
    app.docs.insert(key.clone(), Doc::Diff(DiffDoc { title: "syntax.c".into(), rows: rows.clone(), note: None, file, id: Default::default(), patch: None }));
    app.show_tab(0, key, false);
    crate::editor::set_frame_budget(Some(Duration::ZERO));
    t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
    assert!(app.hl_pending(), "diff text keeps the event loop awake while colours catch up");
    screen(&mut app, &mut t);
    assert!(!app.hl_pending(), "the completed diff stops requesting frames");
    crate::editor::set_frame_budget(Some(crate::editor::FRAME_BUDGET));
    let texts = app.ui.groups[0].diff_text.unwrap();
    for (i, row) in rows.iter().enumerate() {
        for (side, cell) in [&row.left, &row.right].into_iter().enumerate() {
            if let Some((n, line)) = cell {
                for (col, want) in colours[side][*n].iter().enumerate().take(line.len()) {
                    assert_eq!(t.backend().buffer()[(texts[side].x + col as u16, texts[side].y + i as u16)].fg, *want, "side {side}, line {n}, column {col}");
                }
            }
        }
    }
    assert!(app.ui.minimaps.is_empty(), "syntax colours do not depend on the minimap");
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn diff_word_emphasis_is_stable_and_keeps_unchanged_prefix_muted() {
    let root = repo("diff-word-stable");
    let mut app = App::new(root.clone(), None);
    open_big_diff(&mut app);
    app.groups[0].tabs[0].view.scroll = 20;
    let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
    screen(&mut app, &mut t);
    let (x, y) = cell_of(&t, "line 020 changed", 0);
    assert_eq!(t.backend().buffer()[(x, y)].bg, crate::theme::P.diff_ins_line);
    assert_eq!(t.backend().buffer()[(x + 9, y)].bg, crate::theme::P.diff_ins_text.over(crate::theme::P.diff_ins_line));
    let before = t.backend().buffer().clone();
    for _ in 0..4 {
        screen(&mut app, &mut t);
        assert_eq!(t.backend().buffer(), &before, "unchanged viewport retains word emphasis");
    }
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn code_horizontal_wheel_and_drag_reveal_long_lines_without_moving_the_cursor() {
    let root = repo("horizontal-code");
    fs::write(root.join("wide.txt"), format!("start {} END\nshort\n", "x".repeat(240))).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "wide.txt", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    screen(&mut app, &mut t);
    let text = app.ui.groups[0].text;
    let cursor = app.groups[0].tabs[0].view.cursor;
    mouse(&mut app, MouseEventKind::ScrollRight, text.x + 2, text.y);
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, 6);
    assert_eq!(app.groups[0].tabs[0].view.cursor, cursor);
    let sb = *app.ui.scrollbars.iter().find(|s| s.track.width > 1).expect("horizontal scrollbar");
    assert_eq!(sb.track.y, text.bottom(), "bar stays below the text");
    assert_eq!(sb.track.y, app.ui.groups[0].area.bottom(), "horizontal slider shares the bottom frame row");
    assert_eq!(t.backend().buffer()[(app.ui.groups[0].area.x - 1, sb.track.y)].symbol(), crate::theme::icons().border[2]);
    assert_eq!(t.backend().buffer()[(app.ui.groups[0].area.right(), sb.track.y)].symbol(), crate::theme::icons().border[3]);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x + sb.slider.0, sb.track.y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sb.track.right() + 10, sb.track.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sb.track.right() + 10, sb.track.y);
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, sb.max_off);
    assert!(line_of(&t, text.y).contains("END"));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x, sb.track.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sb.track.x, sb.track.y);
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, sb.max_off.saturating_sub(sb.page));
    let before = app.groups[0].tabs[0].view.hscroll;
    mouse(&mut app, MouseEventKind::ScrollLeft, sb.track.x, sb.track.y);
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, before.saturating_sub(6), "the border row also receives horizontal wheels");
    t.backend_mut().resize(400, 26);
    t.resize(Rect::new(0, 0, 400, 26)).unwrap();
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, 0, "widening the editor clamps the offset");
    assert!(!app.ui.scrollbars.iter().any(|s| s.track.width > 1));
    t.backend_mut().resize(110, 26);
    t.resize(Rect::new(0, 0, 110, 26)).unwrap();
    screen(&mut app, &mut t);
    app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: text.x, row: text.y, modifiers: KeyModifiers::SHIFT });
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.scroll, 0, "horizontal limit must not turn into vertical scroll");
    for _ in 0..100 { mouse(&mut app, MouseEventKind::ScrollLeft, text.x, text.y); }
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, 0);
    assert!(line_of(&t, text.y).contains("start"));
    let (b, tab) = app.ed().unwrap();
    b.replace_lines(&mut tab.view, 0, 1, &["tiny".into()]);
    screen(&mut app, &mut t);
    assert!(!app.ui.scrollbars.iter().any(|s| s.track.width > 1), "shortened lines remove the horizontal bar");
    let (b, tab) = app.ed().unwrap();
    b.undo(&mut tab.view);
    screen(&mut app, &mut t);
    assert!(app.ui.scrollbars.iter().any(|s| s.track.width > 1), "undo restores the horizontal range");
    let width = app.ui.groups[0].text.width as usize;
    let (b, tab) = app.ed().unwrap();
    b.replace_lines(&mut tab.view, 0, 1, &["x".repeat(width)]);
    b.end_of_line(&mut tab.view, false);
    app.reveal();
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, 1, "the caret after an exactly full line still needs a visible cell");
    let (b, tab) = app.ed().unwrap();
    b.replace_lines(&mut tab.view, 0, 1, &["x".repeat(240)]);
    for style in [crate::blocks::BlockStyle::Lines, crate::blocks::BlockStyle::None] {
        app.settings.blocks = style;
        screen(&mut app, &mut t);
        let sb = app.ui.scrollbars.iter().find(|s| s.track.width > 1).unwrap();
        assert_eq!(sb.track.bottom(), app.ui.groups[0].area.bottom(), "unframed bars stay inside the pane");
    }
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn diff_horizontal_scrollbar_drags_both_sides_to_the_longest_line_end() {
    let root = repo("horizontal-diff");
    let mut app = App::new(root.clone(), None);
    open_big_diff(&mut app);
    app.groups[0].tabs[0].view.scroll = 98;
    let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
    screen(&mut app, &mut t);
    let texts = app.ui.groups[0].diff_text.unwrap();
    let sb = *app.ui.scrollbars.iter().find(|s| s.track.width > 1).expect("diff horizontal scrollbar");
    assert_eq!(sb.track.y, app.ui.groups[0].area.bottom(), "diff slider shares the bottom frame row");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sb.track.x + sb.slider.0, sb.track.y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sb.track.right() + 20, sb.track.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sb.track.right() + 20, sb.track.y);
    screen(&mut app, &mut t);
    assert_eq!(app.groups[0].tabs[0].view.hscroll, sb.max_off);
    for r in texts {
        assert!(line_of(&t, r.y + 2).chars().skip(r.x as usize).take(r.width as usize).collect::<String>().contains("xxxxxxxx"));
    }
    assert_eq!(app.groups[0].tabs[0].view.scroll, 98);
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn diff_tabs_scroll_like_the_editor() {
    let d = std::env::temp_dir().join(format!("termcode-dscroll-{}", std::process::id()));
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
    // The overview ruler: removed in the left half, inserted in the right, both split;
    // each where the slider is when its row shows (scrolled to the top here).
    let row_of = |i: usize| sb.track.y + decor::mark_row(th, n + th as usize - 1, th as usize, i, 0, n - 1);
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
    assert_eq!(view(&app).hscroll, 6, "the range includes long lines outside the viewport");
    app.groups[0].tabs[0].view.hscroll = 0;
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
    wait_idle(&mut app);
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
    assert!(app.diff_minis.is_empty() && app.diff_syntax.is_empty() && app.docs.is_empty(), "closed: its syntax and minimap buffers go with it");
    // A text file and a diff, the diff in a second group too; then every tab closed.
    app.open_file(0, "README.md", false);
    screen(&mut app, &mut t);
    open_big_diff(&mut app);
    app.split(0, false);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// The frame as truecolor ANSI, for `scratchpad/ans2png.py`.
fn ansi(t: &Terminal<TestBackend>) -> String {
    let b = t.backend().buffer();
    let rgb = |c: Color, fallback: (u8, u8, u8)| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => fallback,
    };
    (0..b.area.height)
        .map(|y| {
            let mut row = String::new();
            for x in 0..b.area.width {
                let c = &b[(x, y)];
                let ((fr, fg, fb), (br, bg, bb)) = (rgb(c.fg, (204, 204, 204)), rgb(c.bg, (30, 30, 30)));
                let bold = if c.modifier.contains(Modifier::BOLD) { "1" } else { "22" };
                row.push_str(&format!("\x1b[{bold};38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}m{}", c.symbol()));
            }
            row + "\x1b[0m"
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Selecting in a diff, as in VS Code's diff editor: on one side only, copying that
/// side's text without line numbers, signs or filler rows.
#[test]
fn a_reloaded_diff_with_new_rows_drops_the_selection() {
    let d = std::env::temp_dir().join(format!("termcode-dreload-{}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    let mut app = App::new(d.canonicalize().unwrap(), None);
    let rows = open_big_diff(&mut app);
    let key = "cdiff:abc:big.c".to_string();
    let land = |app: &mut App, rows: Vec<diff::Row>| {
        let file = ScmFile { section: Section::Changes, path: "big.c".into(), orig: None, letter: 'M' };
        app.loading.insert(key.clone(), 7);
        app.load_tx.send((key.clone(), 7, Load::Diff(DiffDoc { title: "big.c".into(), rows, note: None, file, id: Default::default(), patch: None }))).unwrap();
        app.drain();
    };
    app.groups[0].tabs[0].view.anchor = Some(Pos::new(3, 0));
    land(&mut app, rows.clone());
    assert!(app.groups[0].tabs[0].view.anchor.is_some(), "the same rows: the selection stays");
    land(&mut app, rows[1..].to_vec());
    assert!(app.groups[0].tabs[0].view.anchor.is_none(), "new rows: it would mark other text");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn diff_find_follows_the_pointer_and_keeps_the_widget_in_place() {
    let root = repo("diff-find");
    let mut app = App::new(root.clone(), None);
    let rows = open_big_diff(&mut app);
    let mut t = Terminal::new(TestBackend::new(200, 40)).unwrap();
    screen(&mut app, &mut t);
    let [left, right] = app.ui.groups[0].diff_text.unwrap();
    mouse(&mut app, MouseEventKind::Moved, left.x + 2, left.y + 6);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    typ(&mut app, "changed");
    screen(&mut app, &mut t);
    assert!(app.find.as_ref().unwrap().matches.is_empty(), "the original has no 'changed'");
    let widget = app.ui.find_inputs[0].0;
    mouse(&mut app, MouseEventKind::Moved, right.x + 2, right.y + 6);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    screen(&mut app, &mut t);
    assert_eq!(app.find.as_ref().unwrap().matches, vec![(Pos::new(20, 9), Pos::new(20, 16))], "the modified side has one match");
    assert_eq!(app.ui.find_inputs[0].0, widget, "changing sides never moves the search box");
    assert_eq!(t.backend().buffer()[(right.x + 9, right.y + 20)].bg, crate::theme::P.find_other, "the chosen side highlights its matches");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let view = &app.groups[0].tabs[0].view;
    assert_eq!((view.side, view.anchor, view.cursor), (1, Some(Pos::new(20, 9)), Pos::new(20, 16)));
    app.find.as_mut().unwrap().query = "LINE".into();
    app.find_refresh();
    assert_eq!(app.find.as_ref().unwrap().matches.len(), 297);
    app.find_button(FindBtn::Case);
    assert!(app.find.as_ref().unwrap().matches.is_empty());
    app.find_button(FindBtn::Case);
    app.find_button(FindBtn::Word);
    assert_eq!(app.find.as_ref().unwrap().matches.len(), 297);
    app.find.as_mut().unwrap().query = "inserted".into();
    app.find_refresh();
    assert_eq!(app.find.as_ref().unwrap().matches.len(), 5);
    mouse(&mut app, MouseEventKind::Moved, left.x + 2, left.y + 6);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(app.find.as_ref().unwrap().matches.is_empty(), "left-side filler rows do not match right-side insertions");
    screen(&mut app, &mut t);
    let input = app.ui.find_inputs[0].0;
    mouse(&mut app, MouseEventKind::Moved, input.x + 1, input.y);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(app.find.as_ref().unwrap().matches.is_empty(), "hovering the search box keeps the original target");
    key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
    assert!(app.find.as_ref().unwrap().replace.is_none(), "diff search stays read-only");
    let Doc::Diff(doc) = app.docs.get("cdiff:abc:big.c").unwrap() else { panic!() };
    assert_eq!(doc.rows, rows, "find never edits either diff side");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.find.is_none());
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn diff_find_reveals_matches_beyond_the_visible_rows_and_columns() {
    let root = repo("diff-find-scroll");
    let mut app = App::new(root.clone(), None);
    let rows = open_big_diff(&mut app);
    let mut t = Terminal::new(TestBackend::new(200, 40)).unwrap();
    screen(&mut app, &mut t);
    let left = app.ui.groups[0].diff_text.unwrap()[0];
    mouse(&mut app, MouseEventKind::Moved, left.x, left.y + 5);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    typ(&mut app, "line 299");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let v = &app.groups[0].tabs[0].view;
    assert_eq!((v.side, v.cursor.line), (0, rows.len() - 1));
    assert!(v.scroll <= v.cursor.line && v.cursor.line < v.scroll + left.height as usize);
    app.find.as_mut().unwrap().query = "x".into();
    app.find_refresh();
    key(&mut app, KeyCode::Enter, KeyModifiers::SHIFT);
    screen(&mut app, &mut t);
    let v = &app.groups[0].tabs[0].view;
    assert_eq!((v.cursor.line, v.cursor.col), (100, 309));
    let start = v.anchor.unwrap().col;
    assert!(v.hscroll > 0 && start >= v.hscroll && v.cursor.col <= v.hscroll + left.width as usize, "the whole match is visible horizontally");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    fs::write(root.join("wide.txt"), "x".repeat(150)).unwrap();
    app.open_file(0, "wide.txt", false);
    app.active_tab().unwrap().view.cursor = Pos::new(0, 100);
    app.reveal();
    assert_eq!(app.groups[0].tabs[app.groups[0].active].view.hscroll, 0, "a regular file never inherits the diff's half-width geometry");
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn diff_selection_copies_one_side() {
    let d = std::env::temp_dir().join(format!("termcode-dsel-{}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    let mut app = App::new(d.canonicalize().unwrap(), None);
    let rows = open_big_diff(&mut app);
    let mut t = Terminal::new(TestBackend::new(200, 40)).unwrap();
    screen(&mut app, &mut t);
    let [l, r] = app.ui.groups[0].diff_text.expect("a diff's two text areas");
    let y = |row: usize, app: &App| l.y + (row - app.groups[0].tabs[0].view.scroll) as u16;
    let copy = |app: &mut App| {
        app.clip.clear();
        key(app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.clip.clone()
    };
    let drag = |app: &mut App, from: (u16, u16), to: (u16, u16)| {
        mouse(app, MouseEventKind::Down(MouseButton::Left), from.0, from.1);
        mouse(app, MouseEventKind::Drag(MouseButton::Left), to.0, to.1);
        mouse(app, MouseEventKind::Up(MouseButton::Left), to.0, to.1);
    };
    // A drag on the right side copies exactly that text.
    {
        let (from, to) = ((r.x + 5, y(3, &app)), (r.x + 4, y(5, &app)));
        drag(&mut app, from, to);
    }
    assert_eq!(copy(&mut app), "003\nline 004\nline");
    // One that starts on the left and crosses to the right stays on the left.
    {
        let (from, to) = ((l.x, y(1, &app)), (r.x + 3, y(2, &app)));
        drag(&mut app, from, to);
    }
    assert_eq!(app.groups[0].tabs[0].view.side, 0);
    assert_eq!(copy(&mut app), "line 001\nline 002");
    // Filler rows are skipped: the left side across the five inserted lines.
    let ins = rows.iter().position(|r| r.kind == diff::Kind::Ins).unwrap();
    app.groups[0].tabs[0].view.scroll = ins - 2;
    screen(&mut app, &mut t);
    let (before, after) = (rows[ins - 1].left.clone().unwrap().1, rows[ins + 5].left.clone().unwrap().1);
    assert!((ins..ins + 5).all(|i| rows[i].left.is_none()), "five fillers on the left");
    {
        let (from, to) = ((l.x, y(ins - 1, &app)), (l.x + 3, y(ins + 5, &app)));
        drag(&mut app, from, to);
    }
    assert_eq!(copy(&mut app), format!("{before}\n{}", &after[..3]));
    // Double-click: a word; triple-click: the line; Shift+click extends; Ctrl+A: the side.
    app.groups[0].tabs[0].view.scroll = 0;
    screen(&mut app, &mut t);
    let (wx, wy) = (r.x + 6, y(3, &app));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), wx, wy);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), wx, wy);
    assert_eq!(copy(&mut app), "003");
    // F4 seeds Search with it, as VS Code seeds Find in Files.
    key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
    assert_eq!(app.query, "003");
    app.focus = Focus::Editor;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), wx, wy);
    assert_eq!(copy(&mut app), "line 003", "the third click: the line");
    {
        let at = (r.x, y(7, &app));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), at.0, at.1);
    }
    app.on_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 4, row: y(8, &app), modifiers: KeyModifiers::SHIFT });
    {
        let at = (r.x + 4, y(8, &app));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), at.0, at.1);
    }
    assert_eq!(copy(&mut app), "line 007\nline");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    let right: Vec<&str> = rows.iter().filter_map(|r| r.right.as_ref().map(|(_, t)| t.as_str())).collect();
    assert_eq!(copy(&mut app), right.join("\n"), "Ctrl+A: all of the modified side");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(copy(&mut app).is_empty(), "Esc clears it");
    // Drawn in editor.selectionBackground, over the diff's word colours.
    let chg = rows.iter().position(|r| r.kind == diff::Kind::Change).unwrap();
    let cx = r.x + "line 020 ".len() as u16;
    {
        let (from, to) = ((cx, y(chg, &app)), (cx + 7, y(chg, &app)));
        drag(&mut app, from, to);
    }
    screen(&mut app, &mut t);
    assert_eq!(copy(&mut app), "changed");
    let buf = t.backend().buffer();
    assert_eq!((buf[(cx, y(chg, &app))].symbol(), buf[(cx, y(chg, &app))].bg), ("c", crate::theme::P.selection));
    assert_ne!(buf[(cx - 2, y(chg, &app))].bg, crate::theme::P.selection, "outside it, the diff's colours");
    if let Some(out) = std::env::var_os("TERMCODE_ANSI") {
        {
        let (from, to) = ((r.x + 5, y(17, &app)), (r.x + 9, y(chg, &app)));
        drag(&mut app, from, to);
    }
        screen(&mut app, &mut t);
        fs::write(out, ansi(&t)).unwrap();
    }
    // F7 goes on from the selection's row.
    key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
    assert_eq!(app.groups[0].tabs[0].view.cursor.line, ins, "the next change after line 20's");
    wait_idle(&mut app);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn tiny_terminals_with_a_diff_tab_never_panic() {
    let d = std::env::temp_dir().join(format!("termcode-dtiny-{}", std::process::id()));
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
    wait_idle(&mut app);
    fs::remove_dir_all(&d).unwrap();
}

/// The sweep on a real tree, read only:
/// `TERMCODE_BIG_TREE=<kernel tree> cargo test -- --ignored --nocapture sticky_sweep`
/// expands arch, arch/sh, arch/sh/boards and scrolls one row at a time to past arch.
#[test]
#[ignore]
fn sticky_sweep_on_a_big_tree() {
    let Some(root) = std::env::var_os("TERMCODE_BIG_TREE").map(PathBuf::from) else { return };
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
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if cap == 1 {
                    assert_breadcrumb_holds(&app, off, "")
                } else {
                    assert_sticky_holds(&depth, &app.ui.sticky, off, cap, app.sticky_sep(), "")
                }
            }));
            if r.is_err() {
                bad.push(format!("cap {cap}: top {} | pinned {pinned:?} | first visible {}", app.tree[off].path, app.tree.get(off + pinned.len()).map_or("-", |n| n.path.as_str())));
            }
        }
    }
    eprintln!("{} rows swept twice, {} wrong:\n{}", end + 3 - arch, bad.len(), bad.join("\n"));
    assert!(bad.is_empty());
    // The default row deep in boards reads its full path.
    app.settings.sticky_max = 1;
    app.off[0] = at(&app, "arch/sh/boards") + 5;
    screen(&mut app, &mut t);
    let row = side_line(&t, &app, app.ui.side_list.y);
    eprintln!("sticky row: {}", row.trim_end());
    assert!(row.contains("arch › sh › boards"), "{row}");
}

/// Frame time over big lists, sticky scroll on and off. Read only:
/// `TERMCODE_BIG_TREE=<kernel tree> cargo test --release -- --ignored --nocapture frame_time`
/// expands its drivers/ in full; Search gets 5000 synthetic hits.
#[test]
#[ignore]
fn frame_time_with_big_lists() {
    let Some(root) = std::env::var_os("TERMCODE_BIG_TREE").map(PathBuf::from) else { return };
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// Tab drags through the real loop: pointer motion mixed in batches, drags never
/// coalesced, and a press that ends a batch still drops a drag whose release was lost.
#[test]
fn tab_drags_survive_the_batching_loop_and_a_held_press_drops_a_stale_one() {
    let root = repo("tabstep");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    app.open_file(0, "README.md", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut dirty = true;
    let run = |app: &mut App, t: &mut Terminal<TestBackend>, dirty: &mut bool, evs: Vec<Event>| {
        let mut q: std::collections::VecDeque<Event> = evs.into();
        while !q.is_empty() || app.pending_ev.is_some() {
            app.step(t, dirty, |_| Ok(q.pop_front())).unwrap();
        }
        app.step(t, dirty, |_| Ok(None)).unwrap();
    };
    run(&mut app, &mut t, &mut dirty, vec![]);
    let (tab, a) = (app.ui.groups[0].tabs[1].0, app.ui.groups[0].area);
    let (rx, ry) = (a.right() - 2, a.y + 5);
    let moved = |x, y| mev(MouseEventKind::Moved, x, y);
    let lmb = |k: fn(MouseButton) -> MouseEventKind, x, y| mev(k(MouseButton::Left), x, y);
    // Motion, press, drags with motion between them, release: a split right.
    let evs = vec![moved(tab.x, tab.y), moved(tab.x + 1, tab.y), lmb(MouseEventKind::Down, tab.x + 1, tab.y), lmb(MouseEventKind::Drag, tab.x + 9, ry), moved(rx, ry), lmb(MouseEventKind::Drag, rx, ry), lmb(MouseEventKind::Up, rx, ry)];
    run(&mut app, &mut t, &mut dirty, evs);
    assert_eq!(tab_names(&app), vec![vec!["main.rs".to_string()], vec!["README.md".to_string()]]);
    // A drag whose release was lost, motion, then a press on the title bar inside that
    // motion batch (held for the frame): the next drag splits nothing.
    // README.md (group 1) towards group 0's left side would split there.
    let tab = app.ui.groups[1].tabs[0].0;
    let a = app.ui.groups[0].area;
    let evs = vec![lmb(MouseEventKind::Down, tab.x + 1, tab.y), lmb(MouseEventKind::Drag, a.x + 2, a.y + 5), moved(a.x + 3, a.y + 5), moved(a.x + 4, a.y + 5), lmb(MouseEventKind::Down, a.x + 5, 0), lmb(MouseEventKind::Drag, a.x + 2, a.y + 6), lmb(MouseEventKind::Up, a.x + 2, a.y + 6)];
    run(&mut app, &mut t, &mut dirty, evs);
    assert_eq!(tab_names(&app), vec![vec!["main.rs".to_string()], vec!["README.md".to_string()]], "no split left from the stale drag");
    assert!(app.drop.is_none() && matches!(app.drag, Drag::None));
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    // Each slowed git call takes ≥ 400 ms: a step that waited on one would show it.
    assert!(worst < Duration::from_millis(30), "a UI step took {worst:?} while git ran");
    assert!(app.scm_files.iter().any(|f| f.path == "n00.txt" && f.section == Section::Changes));
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

fn slow_write(app: &mut App, ms: u64) {
    app.gitui.write("slow test write", move || {
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

/// A Ctrl+C in run()'s git wait ends the process there: the token file (what herdr
/// requests go through) is already gone when the wait starts.
#[test]
fn leaving_removes_the_token_before_the_git_wait() {
    let root = repo("leave");
    let mut app = App::new(root.clone(), None);
    wait(&mut app);
    let token = root.join(".git/test-token");
    fs::write(&token, "t").unwrap();
    app.token = Some("t".into());
    // The queued write watches for the token during the wait (3 s at most).
    let (gone, tok) = (Arc::new(AtomicBool::new(false)), token.clone());
    let g = gone.clone();
    app.gitui.write("watch the token", move || {
        let t0 = Instant::now();
        while tok.exists() && t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        g.store(!tok.exists(), Ordering::SeqCst);
        crate::scm::Msg::Wrote(Ok(String::new()), false)
    });
    leave(&mut app, Some(token.clone()), false);
    assert!(gone.load(Ordering::SeqCst), "the token was still there while git ran");
    assert_eq!(app.gitui.writes(), 0, "and the wait still let git finish");
    fs::remove_dir_all(&root).unwrap();
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
    // slow_git's fsmonitor is a program the repository runs: trusted, as the user would.
    crate::trust::trust(&root, &crate::trust::risky(&root)).unwrap();
    fs::write(root.join("new.txt"), "n\n").unwrap();
    let mut app = App::new(root.clone(), None);
    let _ = fs::remove_file(crate::trust::store()); // read at start only
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
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
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

/// The quit wait names what git is doing: "waiting for 2 git operations: … (N s)".
#[test]
fn the_quit_wait_names_the_running_git_operation() {
    let root = repo("waitline");
    let mut app = App::new(root.clone(), None);
    wait(&mut app);
    slow_write(&mut app, 400);
    slow_write(&mut app, 10);
    std::thread::sleep(Duration::from_millis(100));
    let line = app.gitui.waiting_line();
    assert!(line.starts_with("waiting for 2 git operations: slow test write… (0 s)") && line.ends_with("Ctrl+C to stop waiting"), "{line}");
    crate::scm::finish_writes(&mut app, None);
    assert_eq!(app.gitui.writes(), 0);
    fs::remove_dir_all(&root).unwrap();
}
/// The activity bar's SCM count hangs off the icon's top-right corner, Dock style: a
/// pill on the row above the icon (at most "99+"), nothing under the icon; a compact
/// item (no row above) has it beside the icon, a dot where no number fits.
#[test]
fn activity_badge_hangs_off_the_icons_top_right() {
    use crate::settings::Spacing;
    let root = repo("dock");
    let file = |i: usize| ScmFile { section: Section::Changes, path: format!("f{i}"), orig: None, letter: 'M' };
    let cases = [(false, 7, "\u{e0b6}7\u{e0b4}"), (false, 42, "\u{e0b6}42\u{e0b4}"), (false, 150, "99+"), (true, 3, "3"), (true, 42, "●")];
    for (compact, n, want) in cases {
        let mut app = App::new(root.clone(), None);
        app.settings.compact = compact;
        app.settings.spacing = if compact { Spacing::COMPACT } else { Spacing::COMFORTABLE };
        wait(&mut app);
        app.scm_files = (0..n).map(file).collect();
        let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
        screen(&mut app, &mut t);
        let b = t.backend().buffer();
        let (item, _) = app.ui.activity[2];
        assert_eq!(item.height, 2, "{n}: the hit rect is the item's");
        let iy = item.y + u16::from(!compact);
        let ix = (item.x..item.right()).find(|&x| b[(x, iy)].symbol() == crate::theme::icons().scm).expect("the icon");
        let row = |y: u16, x0: u16| (x0..item.right()).map(|x| b[(x, y)].symbol()).collect::<String>();
        if compact {
            assert_eq!(row(iy, ix + 1).trim(), want, "{n}: beside the icon, no row above");
        } else {
            assert_eq!(row(iy - 1, item.x + 1).trim(), want, "{n}: on the row above");
            // Off the top-right corner: a single digit just right of the icon column, a
            // longer badge shifted left only as far as the bar's edge needs.
            let end = (item.x..item.right()).rev().find(|&x| b[(x, iy - 1)].symbol() != " ").unwrap();
            assert_eq!(end, item.right() - 1, "{n}: against the bar's right edge");
            if n < 10 {
                assert_eq!(b[(ix + 1, iy - 1)].symbol(), "7", "a digit right of the icon column");
            }
        }
        assert_eq!(row(iy + 1, item.x + 1).trim(), "", "{n}: nothing under the icon");
    }
    fs::remove_dir_all(&root).unwrap();
}

/// Pills: the Add commit button, the count badges (a Changes header's, the activity
/// bar's), the Search field and the commit dialog's buttons end in round caps (U+E0B6,
/// U+E0B4) in their fill over the surface behind, inside their hit rects.
#[test]
fn buttons_badges_and_the_search_field_are_pills() {
    use crate::theme::P;
    let root = repo("pills");
    fs::write(root.join("README.md"), "changed\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.refresh();
    wait(&mut app);
    app.show_side(Side::Scm, false);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    let ends = |t: &Terminal<TestBackend>, r: Rect| {
        let b = t.backend().buffer();
        (b[(r.x, r.y)].clone(), b[(r.right() - 1, r.y)].clone())
    };
    let (l, r) = ends(&t, app.ui.commit_btn);
    assert_eq!((l.symbol(), r.symbol()), ("\u{e0b6}", "\u{e0b4}"), "Add commit");
    assert_eq!((l.fg, l.bg, r.fg, r.bg), (P.button_bg, P.sidebar_bg, P.button_bg, P.sidebar_bg));
    // A pill ◖1◗ at the end of a row: on the row's background.
    let pill_on_row = |t: &Terminal<TestBackend>, x0: u16, x1: u16, y: u16, under: Color| {
        let b = t.backend().buffer();
        let x = (x0..x1).find(|&x| b[(x, y)].symbol() == "\u{e0b6}").expect("a left cap");
        assert_eq!((b[(x + 1, y)].symbol(), b[(x + 2, y)].symbol()), ("1", "\u{e0b4}"), "row {y}");
        assert_eq!((b[(x, y)].bg, b[(x + 2, y)].bg), (under, under), "row {y}: caps over the surface");
        assert_eq!(b[(x, y)].fg, b[(x + 1, y)].bg, "row {y}: caps in the fill");
    };
    let (hx, hy) = cell_of(&t, "Changes", 0);
    let row_bg = t.backend().buffer()[(hx, hy)].bg;
    pill_on_row(&t, hx, app.ui.divider.x, hy, row_bg);
    let (scm, _) = app.ui.activity[2];
    let by = (scm.y..scm.bottom()).find(|&y| (scm.x..scm.right()).any(|x| t.backend().buffer()[(x, y)].symbol() == "1")).expect("the badge");
    pill_on_row(&t, scm.x, scm.right(), by, crate::blocks::active_bg(&serde_json::Map::new()));
    assert_ne!(t.backend().buffer()[(scm.x, by)].symbol(), "\u{e0b6}", "the indicator column stays");
    // The Search field: caps at its ends, a click on one focuses it.
    app.show_side(Side::Search, false);
    app.focus = Focus::Editor;
    screen(&mut app, &mut t);
    let inp = app.ui.side_input;
    let (l, r) = ends(&t, inp);
    assert_eq!((l.symbol(), r.symbol(), l.fg, l.bg), ("\u{e0b6}", "\u{e0b4}", P.input_bg, P.sidebar_bg), "Search");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inp.x, inp.y);
    assert_eq!(app.focus, Focus::SearchInput, "the cap is the field's");
    // One sectionGap between the field and the results the summary heads.
    typ(&mut app, "needle");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app);
    screen(&mut app, &mut t);
    assert_eq!(cell_of(&t, "1 results in 1 files", 0).1, inp.y + 1 + app.settings.spacing.section_gap);
    // The commit dialog's buttons, on the dialog's background.
    crate::commitdlg::open(&mut app);
    screen(&mut app, &mut t);
    let btns = app.gitui.dlg.as_ref().unwrap().hits.btns.clone();
    let (commit, _) = *btns.iter().find(|(_, b)| *b == crate::commitdlg::Btn::Commit).unwrap();
    let (l, r) = ends(&t, commit);
    assert_eq!((l.symbol(), r.symbol(), l.fg, l.bg), ("\u{e0b6}", "\u{e0b4}", P.button_bg, P.widget_bg), "Commit");
    app.gitui.dlg = None;
    wait_idle(&mut app);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn bottom_activity_opens_categorized_settings_without_editing_a_file() {
    let root = std::env::temp_dir();
    let mut app = App::new(root, None);
    let mut t = Terminal::new(TestBackend::new(110, 36)).unwrap();
    screen(&mut app, &mut t);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 2, 32);
    let shown = screen(&mut app, &mut t);
    assert!(shown.contains("Settings"), "the bottom activity button must open Settings");
    assert!(shown.contains("Appearance") && shown.contains("Apply") && shown.contains("Save"));
    assert!(app.groups.iter().all(|g| g.tabs.is_empty()), "settings is a modal, not a file tab");
}

#[test]
fn the_settings_item_looks_like_a_view_item_and_is_lifted_while_open() {
    let root = repo("gear");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(160, 30)).unwrap();
    screen(&mut app, &mut t);
    let (gear, (ex, _)) = (app.ui.settings_btn, app.ui.activity[0]);
    let (sy, ey) = (gear.bottom() - 2, ex.y + 1);
    let icon_x = |b: &ratatui::buffer::Buffer, y: u16, icon: &str| (ex.x..ex.right()).find(|&x| b[(x, y)].symbol() == icon);
    let b = t.backend().buffer().clone();
    let ic = crate::theme::icons();
    assert_eq!(icon_x(&b, sy, ic.settings), icon_x(&b, ey, ic.files), "the same icon column");
    assert_eq!((gear.y, gear.height), (sy - 1, 3), "a blank row above the icon, one below");
    assert_eq!(b[(ex.x, ey)].symbol(), "▎", "the view's item is lifted");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), gear.x + 2, sy);
    screen(&mut app, &mut t);
    assert!(app.settings_dlg.is_some());
    let b = t.backend().buffer();
    assert_eq!((b[(gear.x, sy)].symbol(), b[(gear.x, sy)].fg), ("▎", crate::theme::P.activity_border));
    assert_eq!((b[(gear.x + 1, sy - 1)].symbol(), b[(gear.x + 1, sy + 1)].symbol()), ("▄", "▀"), "half a row each side, as a view's");
    assert_ne!(b[(ex.x, ey)].symbol(), "▎", "the view's item is not, meanwhile");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn double_clicking_a_tab_toggles_keep_open_and_italics() {
    let root = repo("keep-open");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "README.md", true);
    app.open_file(0, "src/main.rs", false);
    app.groups[0].active = 0;
    let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
    screen(&mut app, &mut t);
    let (tr, _) = app.ui.groups[0].tabs[0];
    let italic = |t: &Terminal<TestBackend>| t.backend().buffer()[(tr.x + 5, tr.y)].modifier.contains(Modifier::ITALIC);
    assert!(app.groups[0].tabs[0].preview && italic(&t), "a preview tab is italic");
    let double = |app: &mut App| {
        for _ in 0..2 {
            mouse(app, MouseEventKind::Down(MouseButton::Left), tr.x + 5, tr.y);
            mouse(app, MouseEventKind::Up(MouseButton::Left), tr.x + 5, tr.y);
        }
    };
    double(&mut app);
    screen(&mut app, &mut t);
    assert!(!app.groups[0].tabs[0].preview && !italic(&t), "kept open: upright");
    double(&mut app);
    screen(&mut app, &mut t);
    assert!(app.groups[0].tabs[0].preview && italic(&t), "the second double-click: preview again");
    assert!(!app.groups[0].tabs[1].preview, "one preview a group");
    // A tab with unsaved changes stays open.
    app.open_file(0, "src/main.rs", false);
    app.focus = Focus::Editor;
    typ(&mut app, "x");
    screen(&mut app, &mut t);
    let (tr, _) = app.ui.groups[0].tabs[1];
    for _ in 0..2 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), tr.x + 5, tr.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), tr.x + 5, tr.y);
    }
    assert!(!app.groups[0].tabs[1].preview && app.msg.contains("unsaved"));
    wait_idle(&mut app);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_cut_explorer_name_shows_in_full_once_the_pointer_rests_on_it() {
    let root = repo("tip");
    let long = format!("{}.txt", "a_really_long_file_name_".repeat(2));
    fs::write(root.join(&long), "x\n").unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    assert!(!screen(&mut app, &mut t).contains(&long), "cut in the Explorer");
    let (r, _, name_x, ..) = app.ui.tips.iter().find(|(_, s, ..)| s.as_deref() == Some(long.as_str())).cloned().expect("its row offers the full name");
    assert!(app.ui.tips.iter().all(|(_, s, ..)| s.is_none() || s.as_deref() == Some(long.as_str())), "only cut names offer one");
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: r.x + 3, row: r.y, modifiers: KeyModifiers::NONE });
    assert!(app.tip.as_ref().is_some_and(|t| !t.shown), "not at once");
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: r.x + 6, row: r.y, modifiers: KeyModifiers::NONE });
    let tip = app.tip.as_mut().unwrap();
    tip.since -= TIP_DELAY; // rested long enough; moving along the row kept the timer
    let mut dirty = false;
    app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
    assert!(app.tip.as_ref().is_some_and(|t| t.shown), "the loop woke to draw it");
    let (x, y) = cell_of(&t, &long, 0);
    assert_eq!((x, y), (name_x, r.y), "the row itself grows to the whole name");
    let b = t.backend().buffer();
    assert_eq!(b[(x + long.len() as u16, y)].bg, b[(x, y)].bg, "in the row's own colours, a blank after it");
    let end = x + long.len() as u16 - 1;
    assert!(end >= app.ui.sidebar.right(), "past the Explorer");
    // Along it, it stays; a double-click there renames that row, in a box as wide.
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: end, row: y, modifiers: KeyModifiers::NONE });
    assert!(app.tip.as_ref().is_some_and(|t| t.shown));
    app.last_click = None;
    for _ in 0..2 {
        assert!(app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: end, row: y, modifiers: KeyModifiers::NONE })));
        app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: end, row: y, modifiers: KeyModifiers::NONE }));
    }
    assert_eq!(app.rename.as_ref().map(|r| r.text.as_str()), Some(long.as_str()), "{}", app.msg);
    screen(&mut app, &mut t);
    let field = app.ui.rename_field.0;
    assert!(field.right() > end, "the rename box shows all of it: {field:?}");
    // A click in its grown part places the caret, not the editor's.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), end, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), end, y);
    assert_eq!(app.rename.as_ref().unwrap().cur, long.len() - 1);
    assert!(app.tip.is_none(), "renaming closed it");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.rename.is_none());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn double_clicking_a_file_name_renames_it_and_its_open_buffer_follows() {
    let root = repo("rename");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
    screen(&mut app, &mut t);
    let (x, y) = cell_of(&t, "README.md", 0);
    let click = |app: &mut App| {
        mouse(app, MouseEventKind::Down(MouseButton::Left), x + 1, y);
        mouse(app, MouseEventKind::Up(MouseButton::Left), x + 1, y);
    };
    click(&mut app);
    app.focus = Focus::Editor;
    typ(&mut app, "edit ");
    app.last_click = None;
    click(&mut app);
    click(&mut app);
    let rn = app.rename.as_ref().expect("a double-click edits the name");
    assert_eq!((rn.text.as_str(), rn.selection()), ("README.md", Some((0, 6))), "the stem selected");
    screen(&mut app, &mut t);
    assert_eq!(t.get_cursor_position().unwrap(), Position::new(x + 6, y), "the field over the name, the caret after the stem");
    let name_dx = x - app.ui.rename.x; // where the field starts in its row
    // Shift+arrows select, Ctrl+C copies the selection (the whole name without one).
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "md");
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "README.md");
    // A click places the caret, a drag selects, a double-click takes the word.
    let field = app.ui.rename_field.0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), field.x + 2, field.y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), field.x + 5, field.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), field.x + 5, field.y);
    assert_eq!(app.rename.as_ref().unwrap().selection(), Some((2, 5)));
    assert!(app.rename.is_some(), "still editing");
    app.last_click = None;
    for _ in 0..2 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), field.x + 7, field.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), field.x + 7, field.y);
    }
    assert_eq!(app.rename.as_ref().unwrap().selection(), Some((7, 9)), "md");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), field.x + 7, field.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), field.x + 7, field.y);
    assert_eq!(app.rename.as_ref().unwrap().selection(), Some((0, 9)), "a third click: all of it");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    key(&mut app, KeyCode::End, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(app.rename.as_ref().unwrap().selection(), Some((0, 6)), "README again");
    app.on_paste("N\nignored second line");
    assert_eq!(app.rename.as_ref().unwrap().text, "N.md", "a paste replaces the selection, first line only");
    screen(&mut app, &mut t);
    let field = app.ui.rename;
    let shown: String = (x..field.right()).map(|cx| t.backend().buffer()[(cx, y)].symbol()).collect();
    assert_eq!(shown.trim_end(), "N.md", "nothing of the old name is left behind");
    typ(&mut app, "OTES");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.rename.is_none(), "{}", app.msg);
    assert!(root.join("NOTES.md").exists() && !root.join("README.md").exists());
    assert_eq!(app.groups[0].tabs[0].key, "file:NOTES.md");
    let Some(Doc::Text(b)) = app.docs.get("file:NOTES.md") else { panic!("the buffer moved") };
    assert!(b.dirty() && b.lines[0] == "edit hello", "unsaved edits kept");
    assert!(!app.docs.contains_key("file:README.md"));
    app.focus = Focus::Editor;
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(fs::read_to_string(root.join("NOTES.md")).unwrap(), "edit hello\n", "saved to the new name");
    // A taken name is refused and stays editable; Esc gives up.
    let i = app.tree.iter().position(|n| n.path == "NOTES.md").unwrap();
    app.start_rename(i);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    typ(&mut app, "sr");
    screen(&mut app, &mut t);
    let at = (app.ui.rename.x + name_dx + 1, app.ui.rename.y);
    let fg = |t: &Terminal<TestBackend>| t.backend().buffer()[at].fg;
    assert_eq!(fg(&t), crate::theme::P.input_fg, "a free name: the input colour");
    typ(&mut app, "c");
    screen(&mut app, &mut t);
    assert_eq!(fg(&t), crate::theme::P.error_fg, "src is taken: red as it is typed");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.msg.contains("already exists") && app.rename.is_some(), "{}", app.msg);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.rename.is_none() && root.join("NOTES.md").exists());
    wait_idle(&mut app);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_new_install_reloads_once_it_holds_still_and_nothing_is_unsaved() {
    assert_eq!(replaced(Path::new("/x/termcode (deleted)")), Some(PathBuf::from("/x/termcode")));
    assert_eq!(replaced(Path::new("/x/termcode")), None, "still the installed one");
    let root = repo("upgrade");
    let bin = root.join("termcode-new");
    fs::write(&bin, "v2").unwrap();
    let mut app = App::new(root.clone(), None);
    wait_idle(&mut app); // its first git job done
    app.upgrade_tick(Some(bin.clone()));
    assert!(!app.quit, "just written: maybe still being copied");
    app.upgrade_tick(Some(bin.clone()));
    assert!(app.quit && app.reload, "held still, nothing unsaved: reload");
    // Unsaved edits: told once, no reload; the manual command refuses too.
    let mut app = App::new(root.clone(), None);
    wait_idle(&mut app);
    app.open_file(0, "README.md", false);
    app.focus = Focus::Editor;
    typ(&mut app, "x");
    app.upgrade_tick(Some(bin.clone()));
    app.upgrade_tick(Some(bin.clone()));
    assert!(!app.quit && app.msg.contains("reloads once every file is saved"));
    app.msg.clear();
    app.upgrade_tick(Some(bin.clone()));
    assert!(app.msg.is_empty(), "said once");
    app.run_command(Cmd::ReloadWindow);
    assert!(!app.quit && app.msg.contains("save the unsaved files first"));
    key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
    app.upgrade_tick(Some(bin.clone()));
    assert!(app.quit && app.reload, "saved: it goes");
    wait_idle(&mut app);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn double_clicking_a_folder_renames_it_and_what_is_open_in_it_follows() {
    let root = repo("rename-dir");
    fs::create_dir_all(root.join("docs/deep")).unwrap();
    fs::write(root.join("docs/deep/x.md"), "x\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "docs/deep/x.md", false);
    let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
    screen(&mut app, &mut t);
    let (x, y) = cell_of(&t, "docs", 0);
    app.last_click = None;
    for _ in 0..2 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 1, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 1, y);
    }
    let rn = app.rename.as_ref().expect("a folder's name is edited too");
    assert_eq!((rn.text.as_str(), rn.selection()), ("docs", Some((0, 4))), "a folder's whole name selected");
    let i = app.tree.iter().position(|n| n.path == "docs").unwrap();
    if !app.tree[i].open {
        app.toggle(i);
    }
    let deep = app.tree.iter().position(|n| n.path == "docs/deep").unwrap();
    app.toggle(deep);
    typ(&mut app, "notes.d");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.rename.is_none(), "{}", app.msg);
    assert!(root.join("notes.d/deep/x.md").exists() && !root.join("docs").exists());
    assert_eq!(app.groups[0].tabs[0].key, "file:notes.d/deep/x.md", "the tab inside follows");
    assert!(app.docs.contains_key("file:notes.d/deep/x.md"));
    let open: Vec<&str> = app.tree.iter().filter(|n| n.open).map(|n| n.path.as_str()).collect();
    assert_eq!(open, ["notes.d", "notes.d/deep"], "still expanded");
    wait_idle(&mut app);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn search_runs_once_typing_pauses_without_enter() {
    let root = repo("search-on-type");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
    key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::SearchInput);
    typ(&mut app, "need");
    assert!(app.search.is_none() && app.hits.is_empty(), "not on every key");
    typ(&mut app, "le");
    let due = app.search_due.expect("a search is due");
    assert!(due > Instant::now() + SEARCH_DEBOUNCE / 2, "each key pushes it back");
    app.search_due = Some(Instant::now()); // the pause
    let mut dirty = false;
    app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
    wait(&mut app);
    assert!(app.hits.iter().any(|h| h.path == "src/main.rs"), "found without Enter");
    assert_eq!(app.focus, Focus::SearchInput, "still typing");
    // Emptied: the results go.
    for _ in 0..6 {
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    app.search_due = Some(Instant::now());
    app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
    assert!(app.hits.is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_find_box_is_the_query_and_its_count_and_toggles_show_while_on() {
    let root = repo("find-box");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "README.md", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    typ(&mut app, "l");
    screen(&mut app, &mut t);
    let (q, _) = app.ui.find_inputs[0];
    let row: String = (q.x..app.ui.find_area.right()).map(|x| t.backend().buffer()[(x, q.y)].symbol()).collect();
    let ic = crate::theme::icons();
    assert!(row.trim_end_matches([' ', '│']).ends_with("1 of 2"), "found as typed, the count against the right edge: {row:?}");
    assert_eq!(app.groups[0].tabs[0].view.selection(), Some((Pos::new(0, 2), Pos::new(0, 3))), "the first match selected without Enter");
    let mark = &t.backend().buffer()[(q.x, q.y)];
    assert_eq!((mark.symbol(), mark.fg), ("▎", crate::theme::P.focus_border), "a thin focus mark, as a tab's");
    let top: String = (app.ui.find_area.x..app.ui.find_area.right()).map(|x| t.backend().buffer()[(x, app.ui.find_area.y)].symbol()).collect();
    assert!(top.contains(" SEARCH "), "named in its border: {top:?}");
    typ(&mut app, "lo");
    assert_eq!(app.groups[0].tabs[0].view.selection(), Some((Pos::new(0, 2), Pos::new(0, 5))), "llo: a longer query keeps the match it extends");
    typ(&mut app, "x");
    screen(&mut app, &mut t);
    assert_eq!(t.backend().buffer()[(q.x, q.y)].fg, crate::theme::P.error_fg, "red while nothing is found");
    for _ in 0..3 {
        key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    }
    assert!(![ic.case, ic.word, ic.regex, ic.up, ic.down, ic.close, ic.chev_right].iter().any(|g| row.contains(g)), "no buttons: {row:?}");
    assert!(app.ui.find_btns.is_empty());
    key(&mut app, KeyCode::Char('c'), KeyModifiers::ALT);
    assert!(app.find.as_ref().unwrap().case);
    screen(&mut app, &mut t);
    let row: String = (q.x..app.ui.find_area.right()).map(|x| t.backend().buffer()[(x, q.y)].symbol()).collect();
    assert!(row.contains(ic.case), "Match Case on: shown in the box");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.find.is_none());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn changes_follow_the_disk_without_a_focus_or_view_switch() {
    let root = repo("status-poll");
    let mut app = App::new(root.clone(), None);
    wait(&mut app);
    app.poll_status = true;
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    fs::write(root.join("new.txt"), "n\n").unwrap();
    assert!(!app.scm_files.iter().any(|f| f.path == "new.txt"));
    let mut dirty = false;
    app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
    wait(&mut app);
    assert!(app.scm_files.iter().any(|f| f.path == "new.txt"), "picked up by the poll");
    let logged = app.gitui.log.gen();
    assert!(app.status_poll_due.is_some_and(|d| d > Instant::now()), "the next one is scheduled");
    // Nothing changed since: the next poll asks for no frame.
    app.status_poll_due = Some(Instant::now());
    app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
    let mut changed = true;
    for _ in 0..300 {
        changed = app.drain();
        if !app.loading.contains_key(STATUS_KEY) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!changed && !app.loading.contains_key(STATUS_KEY), "an unchanged status draws nothing");
    assert_eq!(app.gitui.log.gen(), logged, "polls stay out of Git Output");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn source_control_groups_fold_from_their_header() {
    let root = repo("scm-fold");
    fs::write(root.join("README.md"), "changed\n").unwrap();
    fs::write(root.join("new.txt"), "n\n").unwrap();
    let mut app = App::new(root.clone(), None);
    wait(&mut app);
    app.show_side(Side::Scm, false);
    let mut t = Terminal::new(TestBackend::new(110, 24)).unwrap();
    screen(&mut app, &mut t);
    let files = |app: &App| app.scm_rows().iter().filter(|r| matches!(r, SRow::File(_))).count();
    assert_eq!(files(&app), 2);
    let (x, y) = cell_of(&t, "Changes", 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 2, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 2, y);
    assert_eq!(files(&app), 0, "a click on the header folds it");
    let shown = screen(&mut app, &mut t);
    assert!(!shown.contains("new.txt"));
    assert_eq!(t.backend().buffer()[(x - 2, y)].symbol(), crate::theme::icons().chev_right, "the chevron says so");
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(files(&app), 2, "Right unfolds the selected header");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(files(&app), 0, "Left folds it");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(files(&app), 2, "Enter toggles");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn breadcrumbs_show_the_path_and_symbols_and_open_go_to_symbol() {
    let root = repo("crumbs");
    fs::write(root.join("src/lib.rs"), "struct A;\nimpl A {\n    fn run(&self) {\n        let x = 1;\n    }\n}\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/lib.rs", false);
    app.groups[0].tabs[0].view.cursor = Pos::new(3, 8);
    let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
    let shown = screen(&mut app, &mut t);
    let gh = app.ui.groups[0].clone();
    assert_eq!((gh.crumbs.y, gh.text.y), (gh.tabbar.y + 1, gh.tabbar.y + 2), "the code starts under the breadcrumbs");
    let row = shown.lines().nth(gh.crumbs.y as usize).unwrap();
    assert!(row.contains(" src › ") && row.contains("lib.rs › impl A › run"), "{row}");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), gh.crumbs.x + 3, gh.crumbs.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), gh.crumbs.x + 3, gh.crumbs.y);
    let q = app.quick.as_ref().expect("Go to Symbol");
    assert_eq!((q.query.as_str(), q.items.clone()), ("@", vec!["A".to_string(), "impl A".into(), "run".into()]));
    typ(&mut app, "ru");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.groups[0].tabs[0].view.cursor, Pos::new(2, 7), "on the name");
    app.breadcrumbs_on = false;
    screen(&mut app, &mut t);
    assert_eq!(app.ui.groups[0].text.y, gh.tabbar.y + 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn quick_open_goes_to_a_line_and_the_palette_lists_it_first_after() {
    let root = repo("goto-line");
    fs::write(root.join("a.txt"), (1..=50).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.txt", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    screen(&mut app, &mut t);
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    typ(&mut app, ":");
    assert!(app.quick.as_ref().unwrap().items[0].starts_with("Current Line: 1, Character: 1. Type a line number between 1 and 50"));
    typ(&mut app, "30:4");
    assert_eq!(app.quick.as_ref().unwrap().items, ["Go to line 30, character 4."]);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.quick.is_none());
    assert_eq!(app.groups[0].tabs[0].view.cursor, Pos::new(29, 3));
    let scroll = app.groups[0].tabs[0].view.scroll;
    assert!((15..=29).contains(&scroll), "near the middle: {scroll}");
    key(&mut app, KeyCode::F(1), KeyModifiers::NONE);
    typ(&mut app, "go to line");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.quick.as_ref().map(|q| q.query.as_str()), Some(":"), "the command opens Quick Open at :");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::F(1), KeyModifiers::NONE);
    assert_eq!(app.quick.as_ref().unwrap().items[0], "Go to Line/Column...", "recently used first");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn markdown_wraps_long_lines_and_clicks_land_on_the_wrapped_text() {
    let root = repo("wrap");
    let long = (0..40).map(|i| format!("w{i:02}")).collect::<Vec<_>>().join(" ");
    fs::write(root.join("README.md"), format!("{long}\nnext\n")).unwrap();
    let mut app = App::new(root.clone(), None);
    app.minimap_on = false;
    app.open_file(0, "README.md", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    screen(&mut app, &mut t);
    assert!(app.wraps("file:README.md") && !app.wraps("file:src/main.rs"), "Markdown wraps by default");
    let gh = app.ui.groups[0].clone();
    let rows: Vec<usize> = gh.rows.iter().flatten().filter(|r| r.0 == 0).map(|r| r.1).collect();
    assert!(rows.len() >= 2, "{rows:?}");
    assert!(gh.rows.iter().flatten().any(|r| r.0 == 1), "the next line follows the wrapped rows");
    let (x, y) = cell_of(&t, &format!("w{:02} ", rows[1] / 4), 0);
    assert_eq!((x, y), (gh.text.x, gh.text.y + 1), "the second row starts with a whole word");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 1, y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 1, y);
    assert_eq!(app.groups[0].tabs[0].view.cursor, Pos::new(0, rows[1] + 1));
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let c = app.groups[0].tabs[0].view.cursor;
    assert_eq!(c.line, usize::from(rows.len() == 2), "Down goes one screen row: {c:?}");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::ALT);
    screen(&mut app, &mut t);
    assert!(app.ui.groups[0].rows.is_empty(), "Alt+Z turns it off");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn markdown_preview_follows_the_unsaved_buffer_and_scrolls() {
    let root = repo("md-preview");
    let body: String = (1..=60).map(|i| format!("Paragraph {i}.\n\n")).collect();
    fs::write(root.join("README.md"), format!("# Title\n\n{body}")).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "README.md", false);
    app.focus = Focus::Editor;
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    typ(&mut app, " Now");
    key(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
    assert_eq!((app.groups.len(), app.groups[1].tabs[0].key.as_str(), app.ag), (2, "preview:README.md", 0), "to the side, source focused");
    let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
    let shown = screen(&mut app, &mut t);
    assert!(shown.contains("Preview README.md"), "the tab title");
    let gh = app.ui.groups[1].clone();
    let row = |t: &Terminal<TestBackend>, y: u16| (gh.text.x..gh.text.right()).map(|x| t.backend().buffer()[(x, y)].symbol()).collect::<String>();
    assert_eq!(row(&t, gh.text.y).trim_end(), "Title Now", "the heading without its marks, unsaved text included");
    assert!(gh.preview_rows > gh.text.height as usize);
    app.ag = 1;
    key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
    screen(&mut app, &mut t);
    assert!(app.groups[1].tabs[0].view.scroll > 0);
    assert!((gh.text.y..gh.text.y + 2).any(|y| row(&t, y).starts_with("Paragraph")), "scrolled");
    key(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert_eq!(app.groups[1].tabs[app.groups[1].active].key, "file:README.md", "again: back to the source");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn markdown_turns_to_its_preview_and_back_in_place() {
    let root = repo("md-toggle");
    fs::write(root.join("README.md"), "# Title\n\nSome *text*.\n").unwrap();
    fs::write(root.join("notes.md"), "# Notes\n").unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    app.open_file(0, "README.md", false);
    app.focus = Focus::Editor;
    let s = screen(&mut app, &mut t);
    assert!(s.contains(" preview ") && s.contains(" source ") && s.contains("# Title"), "{s}");
    let press = |app: &mut App, rendered: bool| {
        let r = app.ui.md_btns.iter().find(|b| b.2 == rendered).unwrap().0;
        mouse(app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        mouse(app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
    };
    // Rendered in the same tab; then its source again, edited: the preview follows.
    press(&mut app, true);
    assert_eq!(app.groups[0].tabs.iter().map(|t| t.key.as_str()).collect::<Vec<_>>(), ["preview:README.md"]);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("Title") && !s.contains("# Title"), "{s}");
    // Opened again from the Explorer: this tab, not a second one.
    app.open_file(0, "README.md", false);
    assert_eq!(app.groups[0].tabs.len(), 1);
    press(&mut app, false);
    assert_eq!(app.groups[0].tabs.iter().map(|t| t.key.as_str()).collect::<Vec<_>>(), ["file:README.md"]);
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    typ(&mut app, " Now");
    press(&mut app, true);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("Title Now") && !s.contains("# Title"), "{s}");
    // Back: the cursor where it was.
    press(&mut app, false);
    assert_eq!(app.groups[0].tabs[0].view.cursor, Pos::new(0, 11));
    // Shown rendered, then closed: its clean source goes with it.
    app.open_file(0, "notes.md", false);
    screen(&mut app, &mut t);
    press(&mut app, true);
    let i = app.groups[0].active;
    assert_eq!(app.groups[0].tabs[i].key, "preview:notes.md");
    app.close_tab(0, i, false);
    assert_eq!((app.docs.contains_key("file:notes.md"), app.docs.contains_key("file:README.md")), (false, true), "the dirty README stays");
    // The unsaved README closed from its preview: asked about, as its source.
    let r = app.groups[0].tabs.iter().position(|t| t.key == "file:README.md").unwrap();
    app.groups[0].active = r;
    screen(&mut app, &mut t);
    press(&mut app, true);
    app.close_tab(0, r, false);
    assert!(app.prompt.is_some() && app.groups[0].tabs[r].key == "file:README.md", "Save?");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_patch_turns_into_a_diff_of_its_files_and_back() {
    let root = repo("patch-view");
    fs::write(root.join("fix.patch"), "Subject: [PATCH] Fix\n\ndiff --git a/src/a.c b/src/a.c\n--- a/src/a.c\n+++ b/src/a.c\n@@ -40,2 +40,2 @@ int main()\n keep();\n-old();\n+new();\ndiff --git a/b.c b/b.c\n--- a/b.c\n+++ b/b.c\n@@ -1 +1,2 @@\n x();\n+y();\n").unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
    app.open_file(0, "fix.patch", false);
    app.focus = Focus::Editor;
    let s = screen(&mut app, &mut t);
    assert!(s.contains(" diff ") && s.contains(" source ") && s.contains("+new();"), "its text first\n{s}");
    let r = app.ui.md_btns.iter().find(|b| b.2).unwrap().0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(app.groups[0].tabs[0].key, "patch:fix.patch");
    let s = screen(&mut app, &mut t);
    // Both files under their bars, the hunk's @@ line, the files' own line numbers.
    assert!(s.contains(" src/a.c") && s.contains(" b.c") && s.contains("@@ -40,2 +40,2 @@ int main()"), "{s}");
    assert!(s.contains("41-old();") && s.contains("41+new();") && s.contains(" 2+y();"), "{s}");
    assert!(s.contains("Diff fix.patch"), "the tab's title\n{s}");
    // F7: its changes, as any diff's.
    key(&mut app, KeyCode::F(7), KeyModifiers::NONE);
    assert!(app.groups[0].tabs[0].view.want_x.is_some(), "jumped to a change");
    // Edited as text, the diff follows; back to the source.
    let r = app.ui.md_btns.iter().find(|b| !b.2).unwrap().0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(app.groups[0].tabs[0].key, "file:fix.patch");
    key(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert_eq!(app.groups[0].tabs[0].key, "patch:fix.patch", "Ctrl+Shift+V too");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_map_file_turns_into_its_memory_use_and_an_elf_shows_its_size() {
    let root = repo("size-view");
    fs::write(root.join("fw.map"), "Memory Configuration\n\nName             Origin             Length             Attributes\nFLASH            0x0000000008000000 0x0000000000010000 xr\n*default*        0x0000000000000000 0xffffffffffffffff\n\nLinker script and memory map\n\n.text           0x0000000008000000     0x800\n .text.main     0x0000000008000000     0x800 Core/Src/main.o\n").unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.open_file(0, "fw.map", false);
    (app.focus, app.side_visible, app.minimap_on) = (Focus::Editor, false, false);
    let s = screen(&mut app, &mut t);
    assert!(s.contains(" usage ") && s.contains("Memory Configuration"), "its text first\n{s}");
    let r = app.ui.md_btns.iter().find(|b| b.2).unwrap().0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(app.groups[0].tabs[0].key, "usage:fw.map");
    let s = screen(&mut app, &mut t);
    assert!(s.contains("Usage fw.map") && s.contains("3.1%") && s.contains("2.0 KB of 64.0 KB") && s.contains("Core/Src/main.o"), "{s}");
    // An ELF: size beside readelf and hexdump (with readelf here).
    if std::process::Command::new("readelf").arg("--version").output().is_ok() {
        fs::copy("/bin/true", root.join("tool")).unwrap();
        app.open_file(0, "tool", false);
        screen(&mut app, &mut t);
        let r = app.ui.view_btns.iter().find(|(_, h)| *h == crate::media::As::Size).unwrap().0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("tool: size") && s.contains("text ") && s.contains(" · bss "), "{s}");
        let Some(Doc::Text(b)) = app.docs.get_mut("file:tool") else { panic!("a text view") };
        assert!(b.size_view && !b.symbols().is_empty(), "its parts in the Outline");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_log_reads_by_level_filters_jumps_to_problems_and_follows_its_end() {
    let root = repo("log-view");
    let mut text = String::new();
    for i in 0..100 {
        let lv = if i == 40 { "ERROR" } else if i == 70 { "WARN" } else { "INFO" };
        text.push_str(&format!("2026-11-17 12:00:{:02}.{:03} {lv} step {i}\n", i / 10, (i % 10) * 100));
        if i == 40 {
            text.push_str("    at trace.line()\n");
        }
    }
    fs::write(root.join("app.log"), &text).unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
    app.open_file(0, "app.log", false);
    (app.focus, app.side_visible) = (Focus::Editor, false);
    screen(&mut app, &mut t);
    let r = app.ui.md_btns.iter().find(|b| b.2).unwrap().0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(app.groups[0].tabs[0].key, "log:app.log");
    let settle = |app: &mut App, t: &mut Terminal<TestBackend>, want: &str| {
        for _ in 0..200 {
            crate::logview::tick(app);
            let s = screen(app, t);
            if s.contains(want) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no {want:?}\n{}", screen(app, t));
    };
    let s = settle(&mut app, &mut t, "101 lines");
    assert!(s.contains("● 1 errors") && s.contains("● 1 warnings") && s.contains("● 98 info"), "{s}");
    assert!(s.contains("+100 ms") && s.contains("INFO step 1"), "the time since the line before\n{s}");
    // F8: the error, selected; again: the warning.
    key(&mut app, KeyCode::F(8), KeyModifiers::NONE);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("ERROR step 40"), "{s}");
    key(&mut app, KeyCode::F(8), KeyModifiers::NONE);
    assert!(screen(&mut app, &mut t).contains("WARN step 70"));
    // A filter: the error alone, with its trace when only errors show.
    key(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
    typ(&mut app, "step 4");
    let s = settle(&mut app, &mut t, "11 of 101 lines");
    assert!(s.contains("INFO step 41") && !s.contains("step 50"), "{s}");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    let info = app.logs_hit_for_test("info");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), info.x + 2, info.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), info.x + 2, info.y);
    let warn = app.logs_hit_for_test("warnings");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), warn.x + 2, warn.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), warn.x + 2, warn.y);
    let s = settle(&mut app, &mut t, "2 of 101 lines");
    assert!(s.contains("ERROR step 40") && s.contains("at trace.line()"), "{s}");
    // All levels again; at the end, lines written later show as they come.
    for l in ["info", "warnings"] {
        let r = app.logs_hit_for_test(l);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 2, r.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), r.x + 2, r.y);
    }
    settle(&mut app, &mut t, "101 lines");
    key(&mut app, KeyCode::End, KeyModifiers::NONE);
    screen(&mut app, &mut t);
    let mut f = fs::OpenOptions::new().append(true).open(root.join("app.log")).unwrap();
    std::io::Write::write_all(&mut f, b"2026-11-17 12:01:00.000 INFO later\n").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let s = settle(&mut app, &mut t, "INFO later");
    assert!(s.contains("102 lines"), "{s}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_minimap_slider_held_at_its_edge_scrolls_on_faster_the_farther() {
    let root = repo("mini-auto");
    let body: String = (0..2000).map(|i| format!("let v{i} = {i};\n")).collect();
    fs::write(root.join("long.rs"), body).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "long.rs", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    app.set_scroll(ScrollTarget::Editor(0), 1000);
    screen(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    let (top, _) = m.mini.slider(m.scroll);
    let x = m.area.x + 1;
    let row = |app: &App| { let g = &app.groups[0]; g.tabs[g.active].view.scroll };
    // The slider by its top row, to the minimap's top edge: touching, a slow step.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, m.area.y + top);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, m.area.y);
    screen(&mut app, &mut t);
    let at = row(&app);
    std::thread::sleep(crate::panel::mouse::MINI_STEP + Duration::from_millis(5));
    assert!(app.minimap_auto());
    screen(&mut app, &mut t);
    let slow = at - row(&app);
    assert_eq!(slow, 4, "touching the edge: 4 lines a step");
    // Two rows past it: 4 × 3² a step; held still, it goes on.
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, m.area.y - 2);
    screen(&mut app, &mut t);
    for _ in 0..2 {
        let at = row(&app);
        std::thread::sleep(crate::panel::mouse::MINI_STEP + Duration::from_millis(5));
        assert!(app.minimap_auto());
        screen(&mut app, &mut t);
        assert_eq!(at - row(&app), 36, "farther, faster; held still, on");
    }
    // Let go: it stops, and the slider goes to the middle.
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, m.area.y - 2);
    std::thread::sleep(crate::panel::mouse::MINI_STEP + Duration::from_millis(5));
    assert!(!app.minimap_auto() && app.mini_auto.is_none());
    screen(&mut app, &mut t);
    let h = app.ui.minimaps[0];
    let (top, len) = h.mini.slider(h.scroll);
    assert!((top + len / 2).abs_diff(h.area.height / 2) <= 1, "middle: {top}+{len} of {}", h.area.height);
    // A click held at the bottom edge (off the slider): after its jump, on down.
    screen(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    let bottom = m.area.y + m.area.height - 1;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, bottom);
    screen(&mut app, &mut t);
    let at = row(&app);
    std::thread::sleep(crate::panel::mouse::MINI_STEP + Duration::from_millis(5));
    assert!(app.minimap_auto());
    assert!(row(&app) > at, "held: on down");
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, bottom);
    let _ = fs::remove_dir_all(&root);
}

/// The pictures a frame sends (Kitty), decoded: their PNGs' pixels.
fn sent_pictures(t: &Terminal<TestBackend>) -> Vec<image::RgbaImage> {
    let b64 = |s: &str| -> Vec<u8> {
        let v = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        let s: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
        s.chunks(4).flat_map(|c| {
            let n = c.iter().enumerate().fold(0u32, |n, (i, &c)| n | u32::from(v(c)) << (18 - 6 * i));
            (0..c.len() - 1).map(move |i| (n >> (16 - 8 * i)) as u8)
        }).collect()
    };
    let mut out = Vec::new();
    for cell in t.backend().buffer().content.iter().filter(|c| c.symbol().contains("a=T")) {
        let payload: String = cell.symbol().split("\x1b_G").filter_map(|c| Some(c.split_once(';')?.1.split('\x1b').next()?.to_string())).collect();
        out.push(image::load_from_memory(&b64(&payload)).unwrap().to_rgba8());
    }
    out
}

/// Frames drawn until highlighting settles, and the pictures they sent (each frame's).
fn frames(app: &mut App, t: &mut Terminal<TestBackend>) -> Vec<image::RgbaImage> {
    let mut pics = Vec::new();
    for _ in 0..10_000 {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        pics.extend(sent_pictures(t));
        if !app.hl_pending() {
            return pics;
        }
    }
    panic!("highlighting never settled");
}

#[test]
fn a_diffs_minimap_in_kitty_is_pixels_with_its_changes_tinted() {
    let root = repo("diff-mini-pixels");
    let mut app = App::new(root.clone(), None);
    app.media.kitty_for_tests();
    let old: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let new = old.replace("line 5\n", "line 5 changed\n").replace("line 9\n", "");
    let rows = diff::align(&old, &new);
    let changed = rows.iter().position(|r| r.kind != crate::diff::Kind::Same).unwrap();
    let key = "cdiff:abc:p.txt".to_string();
    let file = ScmFile { section: Section::Changes, path: "p.txt".into(), orig: None, letter: 'M' };
    app.docs.insert(key.clone(), Doc::Diff(DiffDoc { title: "p.txt".into(), rows, note: None, file, id: Default::default(), patch: None }));
    app.show_tab(0, key, false);
    (app.focus, app.side_visible) = (Focus::Editor, false); // room for the minimap
    let mut t = Terminal::new(TestBackend::new(140, 40)).unwrap();
    let pics = frames(&mut app, &mut t);
    let img = pics.last().expect("the diff's minimap sent as a picture, as the editor's");
    let m = app.ui.minimaps[0];
    let buf = t.backend().buffer();
    assert!(buf[(m.area.x + 3, m.area.y + 1)].symbol().starts_with('\u{10EEEE}'), "placeholder cells");
    // 8×16 px cells at scale 3: a line 6 px tall. The changed line's row is tinted
    // across the text (opaque), a line the same is clear between its glyphs.
    let opaque = |line: usize| (8..img.width()).all(|x| img.get_pixel(x, line as u32 * 6 + 2)[3] == 255);
    assert!(opaque(changed), "the change's row tinted");
    assert!(!opaque(0), "an unchanged row clear");
}

#[test]
fn github_sign_in_offers_a_qr_code_for_a_phone() {
    use crate::github::{device_code, QrSignIn, State};
    assert_eq!(device_code("! First copy your one-time code: 1A2B-3C4D\nPress Enter"), Some("1A2B-3C4D".into()));
    assert_eq!(device_code("one-time code: 1A2B"), None, "not whole yet");
    let root = repo("gh-qr");
    let mut app = App::new(root.clone(), None);
    app.side_w = Some(40);
    app.show_side(Side::GitHub, false);
    app.gh.state = State::SignedOut;
    let mut t = Terminal::new(TestBackend::new(120, 50)).unwrap();
    let s = screen(&mut app, &mut t);
    assert!(s.contains("Sign in with GitHub") && s.contains("Sign in with a QR code"), "both ways: {s}");
    assert_eq!(app.gh.hits.btns.iter().map(|b| b.1).collect::<Vec<_>>(), [crate::github::Btn::SignIn, crate::github::Btn::SignInQr]);
    // Signing in by QR: gh's code, and the device page as a QR code, black on white.
    app.gh.signing_in = true;
    app.gh.qr = Some(QrSignIn { term: 0, code: Some("1A2B-3C4D".into()), entered: true });
    let s = screen(&mut app, &mut t);
    assert!(s.contains("1A2B-3C4D") && s.contains("Scan this with your phone"), "{s}");
    let buf = t.backend().buffer();
    let (x, y) = (0..buf.area.height).flat_map(|y| (0..buf.area.width).map(move |x| (x, y))).find(|&(x, y)| buf[(x, y)].symbol() == "▀").expect("the QR code");
    let (black, white) = (ratatui::style::Color::Rgb(0, 0, 0), ratatui::style::Color::Rgb(255, 255, 255));
    // The finder pattern's corner, after the two modules of quiet zone: its first column
    // dark in both halves, its first row dark above the light ring.
    assert_eq!((buf[(x, y)].fg, buf[(x, y)].bg), (white, white), "quiet zone");
    assert_eq!((buf[(x + 2, y + 1)].fg, buf[(x + 2, y + 1)].bg), (black, black));
    assert_eq!((buf[(x + 3, y + 1)].fg, buf[(x + 3, y + 1)].bg), (black, white));
}

#[test]
fn code_serial_and_github_keep_their_own_editor_areas() {
    use crate::panel::Space;
    let root = repo("spaces");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.txt", false);
    app.focus = Focus::Editor;
    app.toggle_terminal();
    let mut t = Terminal::new(TestBackend::new(140, 40)).unwrap();
    let s = screen(&mut app, &mut t);
    assert!(s.contains("alpha") && app.term_open);
    // A serial tab: the Serial view, its own group; the code and the terminal out of sight.
    app.show_tab(app.ag, "serial:ttyX9".into(), false);
    assert_eq!((app.side, app.space(), app.groups.len()), (Side::Serial, Space::Serial, 2));
    let s = screen(&mut app, &mut t);
    assert!(!s.contains("alpha") && s.contains("ttyX9"), "{s}");
    assert!(app.ui.groups[0].area.is_empty() && app.ui.term.is_empty(), "the code group and the terminal hidden");
    // Back to the Explorer: the code tab, its group active again, the terminal back.
    app.show_side(Side::Explorer, false);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("alpha") && !s.contains("ttyX9"));
    assert_eq!((app.ag, app.groups[app.ag].space), (0, Space::Code));
    assert!(!app.ui.term.is_empty());
    // Opening a file from anywhere goes back to the code space.
    app.show_side(Side::Serial, false);
    app.open_file(app.ag, "a.txt", false);
    assert_eq!(app.space(), Space::Code);
    assert_eq!(app.groups.iter().filter(|g| g.space == Space::Code).count(), 1, "into the code group, not a new one");
}

#[test]
fn a_serial_tab_dragged_to_an_edge_splits_in_its_own_space() {
    use crate::panel::Space;
    let root = repo("drag-split");
    let mut app = App::new(root.clone(), None);
    app.show_tab(app.ag, "serial:ttyA1".into(), false);
    app.show_tab(app.ag, "serial:ttyB2".into(), false);
    let mut t = Terminal::new(TestBackend::new(140, 40)).unwrap();
    screen(&mut app, &mut t);
    let g = app.ag;
    let (tab_b, area) = (app.ui.groups[g].tabs[1].0, app.ui.groups[g].area);
    let ev = |kind, x: u16, y: u16| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    // ttyB2's tab dragged to the group's bottom edge: a group under it, in the serial space.
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), tab_b.x + 1, tab_b.y));
    app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), area.x + area.width / 2, area.bottom() - 2));
    app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), area.x + area.width / 2, area.bottom() - 2));
    let serial: Vec<usize> = (0..app.groups.len()).filter(|&i| app.groups[i].space == Space::Serial).collect();
    assert_eq!(serial.len(), 2, "split in the serial space");
    let lower = serial[1];
    assert!(app.groups[lower].below && app.groups[lower].tabs[0].key == "serial:ttyB2");
    screen(&mut app, &mut t);
    let (a, b) = (app.ui.groups[serial[0]].area, app.ui.groups[lower].area);
    assert!(a.x == b.x && b.y > a.y, "{a:?} {b:?}");
    // The split-down button beside the split one does the same.
    let (r, g2, down) = *app.ui.split_btns.iter().find(|(_, g2, down)| *g2 == lower && *down).expect("a split-down button");
    let _ = (g2, down);
    app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), r.x, r.y));
    assert_eq!((0..app.groups.len()).filter(|&i| app.groups[i].space == Space::Serial).count(), 3);
}

#[test]
fn an_editor_splits_down_as_well_as_right_into_columns() {
    let root = repo("split-down");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "a.txt", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(140, 40)).unwrap();
    // Ctrl+T -: the tab again under it, in the same column.
    key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
    screen(&mut app, &mut t);
    assert_eq!((app.groups.len(), app.ag, app.groups[1].below), (2, 1, true));
    let (top, under) = (app.ui.groups[0].area, app.ui.groups[1].area);
    assert!(top.x == under.x && top.width == under.width && under.y >= top.bottom(), "{top:?} {under:?}");
    // Ctrl+T \ from the top: a new column right of that whole column.
    app.ag = 0;
    key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('\\'), KeyModifiers::NONE);
    screen(&mut app, &mut t);
    assert_eq!((app.groups.len(), app.ag, app.groups[2].below), (3, 2, false));
    let (left, right) = (app.ui.groups[0].area, app.ui.groups[2].area);
    assert!(right.x >= left.right() && right.height > left.height, "a column of its own, full height: {left:?} {right:?}");
    // Ctrl+T ↓ from the top one: the one under it.
    app.ag = 0;
    key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.ag, 1);
    // The column's head closed: the one under it heads the column, full height.
    app.close_tab(0, 0, true);
    screen(&mut app, &mut t);
    assert!(!app.groups[0].below && app.ui.groups[0].area.height > top.height);
}

#[test]
fn the_minimap_in_kitty_is_pixels_at_vscodes_scales() {
    let root = repo("mini-pixels");
    let body: String = (0..300).map(|i| format!("{}\n", "x".repeat(i % 40 + 1))).collect();
    fs::write(root.join("p.rs"), body).unwrap();
    let mut app = App::new(root.clone(), None);
    app.media.kitty_for_tests();
    app.open_file(0, "p.rs", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let pics = frames(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    // The last picture sent (one a frame at most, as its colours came) is what shows.
    let img = pics.last().expect("the minimap sent as a picture");
    // 8×16 px cells: a line 2 px tall, a character 1 px wide, after the lane's cell.
    assert_eq!(img.dimensions(), (u32::from(m.area.width) * 8, u32::from(m.area.height) * 16));
    // Clear where there is no code; at scale 3 (the default) each character its glyph
    // shrunk to 3×6 px (some of it lit), a line 6 px tall.
    let lit = |x: u32, y: u32| img.get_pixel(x, y)[3] > 0;
    let char_lit = |c: u32, line: u32| (0..3).any(|dx| (0..6).any(|dy| lit(8 + c * 3 + dx, line * 6 + dy)));
    assert!((0..6).all(|c| char_lit(c, 5)) && !char_lit(6, 5), "line 5: 6 characters");
    assert!((0..11).all(|c| char_lit(c, 50)) && !char_lit(11, 50), "line 50: 11 characters");
    assert!((0..6).any(|dy| (8..26).any(|x| !lit(x, 30 + dy))), "clear between strokes: the slider shows through");
    // The slider: cell backgrounds under the picture.
    let p = &crate::theme::P;
    let buf = t.backend().buffer();
    assert!(buf[(m.area.x + 3, m.area.y + 2)].symbol().starts_with('\u{10EEEE}'), "placeholder cells");
    assert_ne!(buf[(m.area.x + 3, m.area.y)].bg, p.mini_bg, "the slider's rows");
    assert_eq!(buf[(m.area.x + 3, m.area.y + m.area.height - 1)].bg, p.mini_bg);
    // Scrolled inside the stretch: cells change, the picture is not sent again.
    app.set_scroll(ScrollTarget::Editor(0), 3);
    assert!(frames(&mut app, &mut t).is_empty(), "scrolling sends no pixels");
    // An edit: the picture again, the one before deleted a frame later.
    app.set_scroll(ScrollTarget::Editor(0), 0);
    key(&mut app, KeyCode::Char('Z'), KeyModifiers::NONE);
    assert!(!frames(&mut app, &mut t).is_empty());
    t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
    assert!(t.backend().buffer().content.iter().any(|c| c.symbol().contains("a=d")), "the one before deleted");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ghosttys_22_px_cells_keep_every_minimap_line_a_glyph_6_px_apart() {
    let root = repo("mini-ghostty");
    // 'l' over and over: a glyph with clear columns, never a full 3×6 block.
    let body: String = (0..200).map(|_| "llll llll\n".to_string()).collect();
    fs::write(root.join("g.rs"), body).unwrap();
    let mut app = App::new(root.clone(), None);
    app.media.kitty_cells_for_tests((8, 22));
    app.open_file(0, "g.rs", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(140, 40)).unwrap();
    let img = frames(&mut app, &mut t).pop().expect("the minimap's picture");
    if crate::minimap::glyphs(3).is_none() {
        return; // no font here: blocks by design
    }
    // Every line the same: 6 px apart, its glyph's pixels not all lit (no block).
    let row = |line: u32| (0..6).map(|dy| (8..20).map(|x| img.get_pixel(x, line * 6 + dy)[3]).collect::<Vec<_>>()).collect::<Vec<_>>();
    let first = row(0);
    for line in 1..60 {
        assert_eq!(row(line), first, "line {line}: the same as line 0, 6 px on");
    }
    assert!(first.iter().flatten().any(|&a| a == 0) && first.iter().flatten().any(|&a| a > 0), "a glyph, not a block");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_prefix_then_a_quick_key_runs_its_command_and_shows_the_keys_meanwhile() {
    let root = repo("prefix");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let ctrl_t = || KeyCode::Char('t');
    // Ctrl+T: waiting, its quick keys listed.
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    assert!(app.prefix_armed);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("Ctrl+T then") && s.contains("View: Toggle Minimap") && s.contains("Ctrl+T again: Ctrl+T itself"), "{s}");
    // m: the minimap off, the list gone.
    let on = app.minimap_on;
    key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
    assert!(!app.prefix_armed && app.minimap_on != on);
    assert!(!screen(&mut app, &mut t).contains("Ctrl+T then"));
    // A key with none: said so; Esc: nothing.
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
    assert!(app.msg.contains("Ctrl+T Q: no quick key"), "{}", app.msg);
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.prefix_armed);
    // A click cancels it.
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 60, 10);
    assert!(!app.prefix_armed);
    // Twice: the key itself goes on (here, to the editor: no command, the prefix not waiting).
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    assert!(!app.prefix_armed);
    // Ctrl+T →: the pane on the right; ←: back. Down to the terminal and up again.
    app.split(0, false);
    app.focus = Focus::Editor;
    screen(&mut app, &mut t);
    let start = app.ag;
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    let left = app.ag;
    assert_ne!(left, start, "the group on the left");
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert_eq!((app.ag, app.focus), (start, Focus::Editor));
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(app.focus, Focus::Side, "past the first group: the side bar");
    // x with one terminal: hidden as t hides it, its shell kept.
    app.toggle_terminal();
    assert!(app.term_open && app.terms.len() == 1);
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(!app.term_open && app.terms.len() == 1, "the last one hidden, not closed");
    // Off ("" in settings): Ctrl+T is the editor's again.
    app.settings.prefix = None;
    key(&mut app, ctrl_t(), KeyModifiers::CONTROL);
    assert!(!app.prefix_armed);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn brackets_are_coloured_by_depth_and_a_stray_closer_stands_out() {
    let root = repo("brackets");
    fs::write(root.join("b.c"), "f(a[1]);\n)\n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.minimap_on = false;
    app.open_file(0, "b.c", false);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    screen(&mut app, &mut t);
    let (x, y) = cell_of(&t, "f(a[1]);", 0);
    let fg = |dx: u16, dy: u16| t.backend().buffer()[(x + dx, y + dy)].fg;
    let p = &crate::theme::P;
    assert_eq!([fg(1, 0), fg(3, 0), fg(5, 0), fg(6, 0)], [p.bracket_fg[0], p.bracket_fg[1], p.bracket_fg[1], p.bracket_fg[0]]);
    assert_eq!(fg(0, 1), p.bracket_bad);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_title_names_a_folder_called_termcode_once() {
    let root = repo("title");
    let mut app = App::new(root.clone(), None);
    app.repo = "termcode".into();
    app.open_file(0, "src/main.rs", false);
    let mut t = Terminal::new(TestBackend::new(100, 20)).unwrap();
    let shown = screen(&mut app, &mut t);
    let title = shown.lines().next().unwrap();
    assert!(title.contains("main.rs — termcode") && !title.contains("termcode — termcode"), "{title}");
    let close = app.ui.close;
    let cross = (close.x..close.right()).map(|x| &t.backend().buffer()[(x, close.y)]).find(|c| c.symbol() == crate::theme::icons().tab_close).expect("the tab's cross on Close");
    assert!(cross.modifier.contains(Modifier::BOLD), "bold, as on a tab");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_large_files_minimap_never_highlights_the_whole_file() {
    let root = repo("largefile");
    let n = crate::editor::LARGE_LINES + 10_000;
    fs::write(root.join("big.rs"), "fn f() { let x = 1; }\n".repeat(n)).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "big.rs", false);
    let mut t = Terminal::new(TestBackend::new(140, 40)).unwrap();
    for _ in 0..50 {
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
    }
    assert!(!app.hl_pending(), "nothing more to do once the view is coloured");
    let (b, _) = app.ed().unwrap();
    assert!(b.readonly.is_none() && b.lines.len() == n, "opened whole and editable");
    assert!(b.segs(10).is_some() && b.segs(n - 10).is_none(), "the view coloured, the far end left alone");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_status_bar_keeps_to_its_items_lights_them_and_errors_go_to_a_notice() {
    let root = repo("statusclean");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    let mut t = Terminal::new(TestBackend::new(140, 30)).unwrap();
    screen(&mut app, &mut t);
    let items = app.ui.status_items.clone();
    assert_eq!(items.len(), 5, "Ln/Col, indent, encoding, line ends, language");
    // Each item lights under the pointer, and moving between them redraws.
    for r in &items {
        let p = Position::new(r.x + 1, r.y);
        assert!(app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: p.x, row: p.y, modifiers: KeyModifiers::NONE }), "moving onto {r:?} redraws");
        screen(&mut app, &mut t);
        assert_eq!(t.backend().buffer()[(p.x, p.y)].bg, crate::theme::P.status_hover, "{r:?} lit");
    }
    // Ln/Col: Go to Line.
    let l = app.ui.status_line;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), l.x, l.y);
    assert!(app.quick.is_some(), "Go to Line opened");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    // A settings error: a notification, never the status bar; Close hides it until it changes.
    app.settings_err = Some("line 3: expected `,`".into());
    let s = screen(&mut app, &mut t);
    assert!(!s.lines().last().unwrap().contains("settings.json") && s.contains("settings.json has an error") && s.contains("expected"), "{s}");
    let c = app.ui.notice_close;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), c.x, c.y);
    assert!(!screen(&mut app, &mut t).contains("settings.json has an error"), "closed");
    app.settings_err = Some("line 4: trailing garbage".into());
    assert!(screen(&mut app, &mut t).contains("trailing garbage"), "a new error shows again");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_terminal_button_opens_a_shell_under_the_editors_and_its_top_edge_resizes_it() {
    let root = repo("terminal");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    assert!(!screen(&mut app, &mut t).contains(" Terminal "), "hidden until asked for");
    let full = app.ui.editor_area;
    let btn = app.ui.term_btns[0];
    let split_down = app.ui.split_btns.iter().find(|b| b.2).unwrap().0;
    assert_eq!(btn.right(), split_down.x, "just before Split Editor Down (then Right)");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), btn.x, btn.y);
    assert!(app.term_open && app.focus == Focus::Term);
    let s = screen(&mut app, &mut t);
    assert!(s.contains(" Terminal "), "{s}");
    let (ed, term) = (app.ui.editor_area, app.ui.term);
    assert_eq!((ed.x, ed.width, ed.bottom()), (full.x, full.width, app.ui.term_sash.y), "the editors above it");
    assert_eq!((term.x - 1, term.width + 2, term.bottom() + 1), (full.x, full.width, full.bottom()), "as wide as the editors");

    // Keys go to the shell, and its output is drawn.
    typ(&mut app, "echo hi-$((40+2))");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !screen(&mut app, &mut t).contains("hi-42") {
        assert!(Instant::now() < deadline, "no output");
        app.term_pump();
        std::thread::sleep(Duration::from_millis(10));
    }

    // Its top edge, dragged up 4 rows, makes it 4 rows taller.
    let (sash, h) = (app.ui.term_sash, app.ui.term.height + 2);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sash.x + 20, sash.y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sash.x + 20, sash.y - 4);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sash.x + 20, sash.y - 4);
    screen(&mut app, &mut t);
    assert_eq!((app.term_h, app.ui.term.height + 2), (Some(h + 4), h + 4));
    // And down 6 rows, the pointer over the shell's text on the way: 6 shorter.
    let sash = app.ui.term_sash;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sash.x + 20, sash.y);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sash.x + 20, sash.y + 6);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sash.x + 20, sash.y + 6);
    screen(&mut app, &mut t);
    assert_eq!((app.term_h, app.ui.term.height + 2), (Some(h - 2), h - 2), "dragged down");

    // Ctrl+` hides it with the shell still running; the button shows it again.
    key(&mut app, KeyCode::Char('`'), KeyModifiers::CONTROL);
    assert!(!app.term_open && !app.terms.is_empty() && app.focus == Focus::Editor);
    assert!(!screen(&mut app, &mut t).contains(" Terminal "));
    assert_eq!(app.ui.editor_area, full);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), btn.x, btn.y);
    assert!(screen(&mut app, &mut t).contains("hi-42"), "the same shell");

    // The shell exiting closes the panel.
    typ(&mut app, "exit");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    while !app.terms.is_empty() {
        assert!(Instant::now() < deadline, "the shell exited");
        app.term_pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.term_open && app.focus == Focus::Editor);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn minimap_git_lane_lights_only_the_dot_rows_of_changed_lines() {
    let root = repo("mini-lane");
    let body: String = (1..=200).map(|i| format!("let line_{i} = {i};\n")).collect();
    fs::write(root.join("f.rs"), &body).unwrap();
    let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&root).args(args).status().unwrap().success());
    git(&["add", "f.rs"]);
    git(&["commit", "-qm", "f"]);
    let mut lines: Vec<String> = body.lines().map(String::from).collect();
    lines[49] = "let changed = 50;".into();
    lines.insert(120, "let added = 0;".into());
    lines.remove(171); // marked on the line above: index 170
    fs::write(root.join("f.rs"), lines.join("\n") + "\n").unwrap();
    let mut app = App::new(root.clone(), None);
    // The whole file in the minimap ("fill"): all three changes in view.
    app.settings.minimap.size = crate::minimap::Size::Fill;
    app.open_file(0, "f.rs", false);
    wait(&mut app);
    app.update_qdiff();
    // 200 lines in 34 rows: about 6 lines a cell, 1.5 a dot row.
    let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
    screen(&mut app, &mut t);
    let m = app.ui.minimaps[0];
    let p = &crate::theme::P;
    let changed = [(49, p.mini_modified), (120, p.mini_added), (170, p.mini_deleted)];
    let mut seen = 0;
    for row in 0..m.area.height {
        let mut dots = 0u32;
        let mut color = None;
        for (k, d) in [0x09, 0x12, 0x24, 0xc0].into_iter().enumerate() {
            let slot = m.mini.slot_rows(m.scroll, row, k);
            if let Some(&(_, c)) = changed.iter().find(|(l, _)| slot.contains(l)) {
                (dots, color) = (dots | d, Some(c));
            }
        }
        let lane = &t.backend().buffer()[(m.area.x, m.area.y + row)];
        match color {
            Some(c) => {
                seen += 1;
                assert_eq!((lane.symbol(), lane.fg), (char::from_u32(0x2800 + dots).unwrap().to_string().as_str(), c), "row {row}");
            }
            None => assert_eq!(lane.symbol(), " ", "row {row}"),
        }
    }
    assert_eq!(seen, 3);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_outline_button_lists_the_symbols_right_of_the_editors_and_a_click_jumps() {
    let root = repo("outline");
    let src = "struct S;\nimpl S {\n    fn one(&self) {\n        let x = 1;\n    }\n    fn two(&self) {}\n}\nfn free() {}\n";
    fs::write(root.join("src/lib.rs"), src).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/lib.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    assert!(!screen(&mut app, &mut t).contains(" Outline "), "hidden until asked for");
    let full = app.ui.editor_area;
    let btn = app.ui.outline_btns[0];
    assert_eq!(btn.right(), app.ui.term_btns[0].x, "just before Toggle Terminal");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), btn.x, btn.y);
    let s = screen(&mut app, &mut t);
    assert!(app.outline_open && s.contains(" Outline "), "{s}");
    let (ed, out) = (app.ui.editor_area, app.ui.outline);
    assert_eq!((ed.x, ed.right() + 1, out.right() + 1), (full.x, out.x, full.right()), "right of the editors, to the edge");
    // Nested as the breadcrumbs nest them.
    let rows: Vec<String> = (0..5).map(|i| (0..out.width).map(|x| t.backend().buffer()[(out.x + x, out.y + i)].symbol()).collect::<String>().trim_end().to_string()).collect();
    let ic = crate::theme::icons().symbols; // function, struct, class (impl), …
    let down = crate::theme::icons().chev_down;
    assert_eq!(rows, [format!("   {} S", ic[1]), format!(" {down} {} impl S", ic[2]), format!("     {} one", ic[0]), format!("     {} two", ic[0]), format!("   {} free", ic[0])]);
    assert_eq!(t.backend().buffer()[(out.x + 5, out.y + 2)].fg, crate::theme::P.symbol[0], "a function's icon in symbolIcon.functionForeground");
    // The symbol holding the cursor is lit; a click goes to another.
    let lit = |app: &App, t: &Terminal<TestBackend>, i: u16| t.backend().buffer()[(out.x + 2, out.y + i)].bg == crate::theme::P.list_inactive_bg && app.outline_cur == Some(i as usize);
    assert!(lit(&app, &t, 0), "the cursor is on line 1: struct S");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), out.x + 8, out.y + 3);
    assert_eq!((app.ed().unwrap().1.view.cursor, app.focus), (editor::Pos::new(5, 7), Focus::Outline), "fn two: its name; the Outline keeps the keys");
    screen(&mut app, &mut t);
    assert_eq!((t.backend().buffer()[(out.x + 2, out.y + 3)].bg, app.outline_cur), (crate::theme::P.list_active_bg, Some(3)), "selected, focused");
    // Its left edge, dragged 6 columns left, makes it 6 wider.
    let (sash, w) = (app.ui.outline_sash, app.ui.outline.width + 2);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sash.x, sash.y + 5);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sash.x - 6, sash.y + 5);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sash.x - 6, sash.y + 5);
    screen(&mut app, &mut t);
    assert_eq!((app.outline_w, app.ui.outline.width + 2), (Some(w + 6), w + 6));
    // The button (moved left with the narrower editors) hides it again.
    let btn = app.ui.outline_btns[0];
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), btn.x, btn.y);
    assert!(!screen(&mut app, &mut t).contains(" Outline "));
    assert_eq!(app.ui.editor_area, full);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_search_query_selects_whole_by_ctrl_a_or_double_click_to_copy_cut_or_replace() {
    let root = repo("search-sel");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    app.show_side(Side::Search, true);
    typ(&mut app, "needle");
    // Ctrl+A, Ctrl+C: the whole query copied, shown selected, still there.
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!((app.clip.as_str(), app.query.as_str()), ("needle", "needle"));
    screen(&mut app, &mut t);
    let inp = app.ui.side_input;
    assert_eq!(t.backend().buffer()[(inp.x + 2, inp.y)].bg, crate::theme::P.selection);
    // Typing replaces it; then it is unselected and typing adds.
    typ(&mut app, "pin");
    typ(&mut app, "e");
    assert_eq!(app.query, "pine");
    // A double-click selects it all; Ctrl+X cuts it.
    screen(&mut app, &mut t);
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)].repeat(2) {
        mouse(&mut app, kind, inp.x + 2, inp.y);
    }
    key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!((app.clip.as_str(), app.query.as_str()), ("pine", ""));
    // A paste replaces a selected query; Backspace clears one.
    typ(&mut app, "abc");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    app.on_paste("xyz");
    assert_eq!(app.query, "xyz");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(app.query, "");
    // A single click leaves it unselected.
    typ(&mut app, "q");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    screen(&mut app, &mut t);
    std::thread::sleep(DOUBLE_CLICK);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inp.x + 3, inp.y);
    typ(&mut app, "r");
    assert_eq!(app.query, "qr");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_drag_in_the_terminal_selects_and_copies_its_text_and_a_double_click_a_word() {
    let root = repo("term-select");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    app.toggle_terminal();
    screen(&mut app, &mut t);
    typ(&mut app, "clear; echo hello wide-world");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let deadline = Instant::now() + Duration::from_secs(10);
    let row = loop {
        app.term_pump();
        let s = screen(&mut app, &mut t);
        let tx = app.ui.term.x as usize;
        if let Some(y) = s.lines().position(|l| l.chars().skip(tx).collect::<String>().starts_with("hello wide-world")) {
            break y as u16;
        }
        assert!(Instant::now() < deadline, "no output: {s}");
        std::thread::sleep(Duration::from_millis(10));
    };
    let x = app.ui.term.x;
    // Drag over "hello": copied on release, drawn selected.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 4, row);
    screen(&mut app, &mut t);
    assert_eq!(t.backend().buffer()[(x + 2, row)].bg, crate::theme::P.selection);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 4, row);
    assert_eq!((app.clip.as_str(), app.focus), ("hello", Focus::Term));
    // A click (no drag) selects nothing; a double-click takes the word under it.
    std::thread::sleep(DOUBLE_CLICK);
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)].repeat(2) {
        mouse(&mut app, kind, x + 9, row);
    }
    assert_eq!(app.clip, "wide-world");
    // Ctrl+C with a selection copies it; without one it goes to the shell as ^C.
    app.clip.clear();
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "wide-world");
    assert!(app.term().unwrap().selected().is_none(), "and the selection goes");
    app.hide_terminal();
    app.terms.clear();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_program_that_asks_for_the_mouse_gets_the_terminals_clicks() {
    let root = repo("term-mouse");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    app.toggle_terminal();
    screen(&mut app, &mut t);
    // SGR mouse mode on, then cat: the terminal echoes what the click sends as ^[[<…
    typ(&mut app, "printf '\\033[?1000h\\033[?1006h'; cat");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.term().unwrap().wants_mouse() {
        assert!(Instant::now() < deadline, "mouse mode never came on");
        app.term_pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    let a = app.ui.term;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), a.x + 9, a.y + 4);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), a.x + 9, a.y + 4);
    while !screen(&mut app, &mut t).contains("^[[<0;10;5M^[[<0;10;5m") {
        assert!(Instant::now() < deadline, "the press and release reach the program");
        app.term_pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.term().unwrap().selected().is_none(), "the program's click, not a selection");
    app.hide_terminal();
    app.terms.clear();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn quitting_waits_on_a_program_in_the_terminal_and_a_reload_keeps_it_running() {
    let root = repo("term-busy");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    app.toggle_terminal();
    screen(&mut app, &mut t);
    let deadline = Instant::now() + Duration::from_secs(10);
    // Busy: until the forked shell has become sleep.
    let settle = |app: &mut App, busy: bool| {
        while app.term().unwrap().running().is_some_and(|p| p == "sleep") != busy || !busy && app.term().unwrap().running().is_some() {
            assert!(Instant::now() < deadline, "running: {busy}");
            app.term_pump();
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    typ(&mut app, "sleep 30");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    settle(&mut app, true);
    while app.gitui.writes() > 0 {
        assert!(Instant::now() < deadline, "startup's git writes finish");
        app.drain();
        std::thread::sleep(Duration::from_millis(10));
    }
    // Quit asks; Cancel keeps everything.
    app.request_quit();
    let text = app.prompt.as_ref().map(|p| p.text.join(" ")).unwrap_or_default();
    assert!(text.contains("sleep is still running"), "{text}");
    app.run_prompt(PAct::Cancel);
    assert!(!app.quit && !app.terms.is_empty());
    // A new termcode installed meanwhile does not wait: the program goes on across the
    // reload (its terminal is handed over).
    let exe = root.join("exe");
    fs::write(&exe, "x").unwrap();
    app.upgrade_tick(Some(exe.clone()));
    app.upgrade_tick(Some(exe.clone()));
    assert!(app.reload && app.quit, "{}", app.msg);
    (app.reload, app.quit) = (false, false);
    // Interrupted: the shell idles.
    app.focus = Focus::Term;
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    settle(&mut app, false);
    app.request_quit();
    assert!(app.quit && app.prompt.is_none(), "an idle shell: no question");
    app.terms.clear();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_find_fields_select_whole_by_ctrl_a_or_double_click_to_copy_cut_or_replace() {
    let root = repo("find-sel");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    let mut t = Terminal::new(TestBackend::new(110, 26)).unwrap();
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    typ(&mut app, "needle");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(app.find.as_ref().unwrap().all, "focus {:?}", app.focus);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "needle");
    assert!(app.find.as_ref().unwrap().all, "still selected after copying");
    screen(&mut app, &mut t);
    let q = app.ui.find_inputs[0].0;
    assert_eq!(t.backend().buffer()[(q.x + 2, q.y)].bg, crate::theme::P.selection, "shown selected");
    typ(&mut app, "pin");
    assert_eq!(app.find.as_ref().unwrap().query, "pin", "typing replaced it");
    // The Replace field: a double-click selects it, Ctrl+X cuts it.
    key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
    typ(&mut app, "thread");
    screen(&mut app, &mut t);
    let r = app.ui.find_inputs[1].0;
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)].repeat(2) {
        mouse(&mut app, kind, r.x + 2, r.y);
    }
    key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!((app.clip.as_str(), app.find.as_ref().unwrap().replace.as_deref()), ("thread", Some("")));
    // A paste replaces a selected field.
    typ(&mut app, "abc");
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    app.on_paste("xyz");
    assert_eq!(app.find.as_ref().unwrap().replace.as_deref(), Some("xyz"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_outline_folds_and_walks_by_keys_and_its_chevrons() {
    let root = repo("outline-keys");
    let src = "struct S;\nimpl S {\n    fn one(&self) {\n        let x = 1;\n    }\n    fn two(&self) {}\n}\nfn free() {}\n";
    fs::write(root.join("src/lib.rs"), src).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/lib.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    app.run_command(Cmd::FocusOutline);
    screen(&mut app, &mut t);
    let names = |app: &App| app.ui.outline_items.iter().map(|it| it.path.clone()).collect::<Vec<_>>();
    let sel = |app: &App| app.outline_sel.clone().unwrap_or_default();
    assert_eq!((app.focus, sel(&app)), (Focus::Outline, "S".into()), "starts on the cursor's symbol");
    let k = |app: &mut App, t: &mut Terminal<TestBackend>, code| {
        key(app, code, KeyModifiers::NONE);
        screen(app, t);
    };
    k(&mut app, &mut t, KeyCode::Down);
    assert_eq!(sel(&app), "impl S");
    // ← folds it, → unfolds it, → again steps in, ← steps out.
    k(&mut app, &mut t, KeyCode::Left);
    assert_eq!(names(&app), ["S", "impl S", "free"]);
    assert!(!app.ui.outline_items[1].open);
    k(&mut app, &mut t, KeyCode::Right);
    assert_eq!(names(&app).len(), 5);
    k(&mut app, &mut t, KeyCode::Right);
    assert_eq!(sel(&app), "impl S/one");
    k(&mut app, &mut t, KeyCode::Left);
    assert_eq!(sel(&app), "impl S");
    // Space shows the symbol and keeps the keys; Enter goes there with them.
    k(&mut app, &mut t, KeyCode::End);
    k(&mut app, &mut t, KeyCode::Char(' '));
    assert_eq!((app.ed().unwrap().1.view.cursor.line, app.focus), (7, Focus::Outline));
    k(&mut app, &mut t, KeyCode::Up);
    k(&mut app, &mut t, KeyCode::Enter);
    assert_eq!((app.ed().unwrap().1.view.cursor.line, app.focus), (5, Focus::Editor));
    // A click on a chevron folds; the cursor inside lights the row it is folded into.
    let chev = app.ui.outline_chevs[0].0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), chev.x, chev.y);
    screen(&mut app, &mut t);
    assert_eq!(names(&app), ["S", "impl S", "free"]);
    app.focus = Focus::Editor;
    screen(&mut app, &mut t);
    assert_eq!((app.outline_cur, sel(&app)), (Some(1), "impl S".into()), "fn two is inside the folded impl");
    // A double-click on a row gives the editor the keys.
    let r = app.ui.outline_rows[2].0;
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)].repeat(2) {
        mouse(&mut app, kind, r.x + 8, r.y);
    }
    assert_eq!((app.ed().unwrap().1.view.cursor.line, app.focus), (7, Focus::Editor));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_long_query_scrolls_and_leaves_the_caret_a_blank_cell_before_the_toggles() {
    let root = repo("long-query");
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "src/main.rs", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(110, 20)).unwrap();
    let long = "a_very_long_search_query_that_runs_past_the_box_end_XYZ";
    // Search: the end of the query, then the caret on a blank cell, then the toggles.
    app.show_side(Side::Search, true);
    typ(&mut app, long);
    screen(&mut app, &mut t);
    let caret = t.get_cursor_position().unwrap();
    let b = t.backend().buffer();
    assert_eq!((b[(caret.x - 3, caret.y)].symbol(), b[(caret.x - 1, caret.y)].symbol(), b[(caret.x, caret.y)].symbol()), ("X", "Z", " "));
    let toggle = app.ui.find_btns.iter().map(|(r, _)| r.x).min().unwrap();
    assert_eq!(toggle, caret.x + 1, "the toggles follow the caret's cell");
    // Find, with all three toggles on: the same, its text never under them.
    app.focus = Focus::Editor;
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    typ(&mut app, long);
    for c in ['c', 'w', 'r'] {
        key(&mut app, KeyCode::Char(c), KeyModifiers::ALT);
    }
    screen(&mut app, &mut t);
    let caret = t.get_cursor_position().unwrap();
    let b = t.backend().buffer();
    assert_eq!((b[(caret.x - 1, caret.y)].symbol(), b[(caret.x, caret.y)].symbol()), ("Z", " "));
    let q = app.ui.find_inputs[0].0;
    let rest: String = (caret.x + 1..q.right()).map(|x| b[(x, caret.y)].symbol()).collect();
    assert!(!rest.contains(|c: char| c.is_ascii_alphanumeric() || c == '_'), "only toggles after the caret: {rest:?}");
    let _ = fs::remove_dir_all(&root);
}

/// A fake GitHub CLI in `dir`: signed out until `auth login` runs, then an account with
/// two repositories (tester/proj and o/lib), one of each list, and for a repository (named
/// by --repo) two pull requests, an issue and two runs; it logs every command to `dir/gh.log`.
fn fake_gh(dir: &Path) -> PathBuf {
    let bin = dir.join("gh");
    let script = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/gh.log"
case "$1 $2" in
  "auth login") rm -f "$d/signed-out"; echo "Logged in as tester"; exit 0 ;;
esac
if [ -e "$d/signed-out" ]; then echo "To get started with GitHub CLI, please run:  gh auth login" >&2; exit 4; fi
repo=""; prev=""
for a in "$@"; do [ "$prev" = "--repo" ] && repo="$a"; prev="$a"; done
# A repository page: a folder (its entries, then their last commits and the README), a file.
case "$*" in
  *isBinary*) printf '%s\n' '{"data":{"repository":{"object":{"text":"fn main() {}\n","isBinary":false,"byteSize":13}}}}'; exit 0 ;;
  *'history('*) printf '%s\n' '{"data":{"repository":{"object":{"e0":{"nodes":[{"messageHeadline":"Add the sources","committedDate":"2026-11-10T00:00:00Z"}]},"e1":{"nodes":[{"messageHeadline":"Say hello","committedDate":"2026-11-12T00:00:00Z"}]}},"readme":{"text":"Proj\n====\n\nHello **world** from the README.\n"}}}}'; exit 0 ;;
  *stargazerCount*) printf '%s\n' '{"data":{"repository":{"stargazerCount":7,"forkCount":2,"watchers":{"totalCount":3},"open":{"totalCount":2},"merged":{"totalCount":30},"closed":{"totalCount":5},"openIssues":{"totalCount":1},"closedIssues":{"totalCount":9},"languages":{"edges":[{"size":900,"node":{"name":"Rust","color":"\u0023dea584"}},{"size":100,"node":{"name":"Shell","color":"\u002389e051"}}]}}}}'; exit 0 ;;
  *'refs('*) echo '{"data":{"repository":{"refs":{"nodes":[{"name":"main"},{"name":"dev"}]}}}}'; exit 0 ;;
  *'"dev:"'*) echo '{"data":{"repository":{"defaultBranchRef":{"name":"main"},"head":{"oid":"abcdef1234567","messageHeadline":"Say hello","committedDate":"2026-11-12T00:00:00Z","author":{"name":"Ann","user":{"login":"ann"}}},"object":{"entries":[{"name":"src","type":"tree","path":"src"},{"name":"DEV.md","type":"blob","path":"DEV.md"}]}}}}'; exit 0 ;;
  *'"HEAD:src"'*) echo '{"data":{"repository":{"defaultBranchRef":{"name":"main"},"head":{"oid":"abcdef1234567","messageHeadline":"Say hello","committedDate":"2026-11-12T00:00:00Z","author":{"name":"Ann","user":{"login":"ann"}}},"object":{"entries":[{"name":"main.rs","type":"blob","path":"src/main.rs"}]}}}}'; exit 0 ;;
  *'"HEAD:"'*) echo '{"data":{"repository":{"defaultBranchRef":{"name":"main"},"head":{"oid":"abcdef1234567","messageHeadline":"Say hello","committedDate":"2026-11-12T00:00:00Z","author":{"name":"Ann","user":{"login":"ann"}}},"object":{"entries":[{"name":"README.md","type":"blob","path":"README.md"},{"name":"src","type":"tree","path":"src"}]}}}}'; exit 0 ;;
esac
case "$1 $2" in
  "api user") echo '{"login":"tester","avatar_url":"https://avatars.example/u/1?v=4"}' ;;
  "api graphql") echo '{"data":{"viewer":{"followers":{"totalCount":4},"following":{"totalCount":2},"contributionsCollection":{"contributionCalendar":{"totalContributions":13,"weeks":[
      {"contributionDays":[{"contributionCount":0,"contributionLevel":"NONE","date":"2026-11-01","weekday":0},{"contributionCount":3,"contributionLevel":"SECOND_QUARTILE","date":"2026-11-02","weekday":1}]},
      {"contributionDays":[{"contributionCount":10,"contributionLevel":"FOURTH_QUARTILE","date":"2026-11-08","weekday":0}]}]}}}}}' ;;
  "api https://avatars.example/u/1?v=4&s=64") echo 'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGM4YWPzH4QZYAwAT6QI/d3jAgwAAAAASUVORK5CYII=' | base64 -d ;;
  "api user/repos"*"page=2"*) echo '[]' ;;
  "api user/repos"*) echo '[{"full_name":"tester/proj","description":"The project","private":true,"fork":false,"archived":false,"pushed_at":"2026-11-13T00:00:00Z","stargazers_count":7,"forks_count":2,"language":"Rust","default_branch":"main","open_issues_count":3,"html_url":"https://github.com/tester/proj"},
                            {"full_name":"o/lib","description":"","private":false,"fork":true,"archived":false,"pushed_at":"2026-11-01T00:00:00Z","stargazers_count":0,"forks_count":0,"language":"C","default_branch":"master","open_issues_count":0,"html_url":"https://github.com/o/lib"}]' ;;
  "search prs") case "$3" in
      --review-requested) echo '[{"number":3,"title":"Review me","author":{"login":"dee"},"repository":{"name":"lib","nameWithOwner":"o/lib"},"isDraft":false,"updatedAt":"2026-11-13T00:00:00Z","url":"https://github.com/o/lib/pull/3"}]' ;;
      *) echo '[]' ;;
    esac ;;
  "search issues") echo '[]' ;;
  "api notifications") echo '[{"reason":"mention","updated_at":"2026-11-13T00:00:00Z","subject":{"title":"You were mentioned","type":"Issue","url":"https://api.github.com/repos/o/lib/issues/4"},"repository":{"full_name":"o/lib","html_url":"https://github.com/o/lib"}}]' ;;
  "pr list") echo '[{"number":12,"title":"Fix the thing in '"$repo"'","author":{"login":"ann"},"isDraft":false,"headRefName":"fix","updatedAt":"2026-11-13T00:00:00Z","url":"https://github.com/'"$repo"'/pull/12","statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"}]},
                    {"number":13,"title":"Draft work","author":{"login":"bob"},"isDraft":true,"headRefName":"wip","updatedAt":"2026-11-12T00:00:00Z","url":"https://github.com/'"$repo"'/pull/13","statusCheckRollup":[]}]' ;;
  "issue list") echo '[{"number":8,"title":"It breaks","author":{"login":"cy"},"updatedAt":"2026-11-10T00:00:00Z","url":"https://github.com/'"$repo"'/issues/8"}]' ;;
  "run list") echo '[{"databaseId":71,"displayTitle":"Fix the thing","workflowName":"CI","headBranch":"fix","status":"completed","conclusion":"failure","updatedAt":"2026-11-13T00:00:00Z","url":"https://github.com/'"$repo"'/actions/runs/71"},
                     {"databaseId":72,"displayTitle":"nightly","workflowName":"Nightly","headBranch":"main","status":"in_progress","conclusion":"","updatedAt":"2026-11-13T00:00:00Z","url":"https://github.com/'"$repo"'/actions/runs/72"}]' ;;
  "api repos/"*"/stats/commit_activity") echo '[{"days":[0,2,0,0,0,0,0],"total":2,"week":1793491200},{"days":[1,0,0,0,0,0,0],"total":1,"week":1794096000}]' ;;
  "api repos/"*"/stats/punch_card") echo '[[1,9,4],[1,10,2],[3,14,7]]' ;;
  "api repos/"*"/stats/code_frequency") echo '[[1793491200,120,-30],[1794096000,40,-80]]' ;;
  "api repos/"*"/languages") echo '{"Rust":900,"Shell":100}' ;;
  "api repos/"*"/contributors"*) echo '[{"login":"ann","contributions":12}]' ;;
  *) exit 0 ;;
esac
"#;
    fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Runs the GitHub view's background work until `done`.
fn gh_settle(app: &mut App, done: &dyn Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        assert!(Instant::now() < deadline, "github work never finished");
        crate::github::tick(app);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_github_view_signs_in_first_then_lists_and_edits_a_title_only_once_confirmed() {
    use crate::github::{Row, Sec, BOTTOM, TOP};
    let root = repo("github");
    Command::new("git").arg("-C").arg(&root).args(["remote", "add", "origin", "https://github.com/tester/proj.git"]).status().unwrap();
    let fake = std::env::temp_dir().join(format!("termcode-fake-gh-{}", std::process::id()));
    fs::create_dir_all(&fake).unwrap();
    fs::write(fake.join("signed-out"), "").unwrap();
    let bin = fake_gh(&fake);
    let log = || fs::read_to_string(fake.join("gh.log")).unwrap_or_default();
    let mut app = App::new(root.clone(), None);
    assert_eq!((app.gh.here.as_deref(), app.gh.chosen.as_deref()), (Some("tester/proj"), Some("tester/proj")), "the folder's repository, first");
    app.gh.bin = bin.clone();
    app.side_w = Some(70); // whole titles in the rows
    app.refresh();
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(160, 76)).unwrap();
    // Signed out: only the sign-in shows.
    app.show_side(Side::GitHub, false);
    crate::github::tick(&mut app);
    gh_settle(&mut app, &|a| !a.gh.loading);
    let s = screen(&mut app, &mut t);
    assert_eq!(app.gh.state, crate::github::State::SignedOut);
    assert!(s.contains("Sign in to GitHub") && s.contains("Sign in with GitHub") && !s.contains("REPOSITORIES"), "{s}");
    // Signing in runs gh auth login in the terminal; the view then fills by itself.
    let b = app.gh.hits.btns[0].0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), b.x + 1, b.y);
    assert!(app.gh.signing_in && app.term_open && app.focus == Focus::Term);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !log().contains("auth login --web") {
        assert!(Instant::now() < deadline, "the terminal ran gh auth login: {}", log());
        app.term_pump();
        std::thread::sleep(Duration::from_millis(20));
    }
    crate::github::fetch(&mut app);
    gh_settle(&mut app, &|a| a.gh.state == crate::github::State::Ready && a.gh.avatar_ready() && !a.gh.repo_loading);
    app.hide_terminal();
    app.terms.clear();
    app.focus = Focus::Side;
    let s = screen(&mut app, &mut t);
    for want in ["tester", "2 repositories", "ACTIVITY · 13 in the past year", "‹  past year  ›", "2 repos · ", " 7 · 4 followers · 2 following", "REPOSITORIES", "proj", "o/lib", "REVIEW REQUESTS", "#3 Review me", "NOTIFICATIONS", "You were mentioned",
                 "tester/proj", "The project", "private · main · Rust", "ACTIONS", "nightly", "PULL REQUESTS", "#12 Fix the thing in tester/proj", "ISSUES", "#8 It breaks"] {
        assert!(s.contains(want), "{want}: {s}");
    }
    assert!(app.msg.contains("signed in to GitHub as tester"), "{}", app.msg);
    // The repository's stats above its lists: its commit graph, languages, contributors.
    for want in ["STATS · 3 commits in the past year", "Rust 90.0%", "Shell 10.0%", "Contributors: ann 12"] {
        assert!(s.contains(want), "{want}: {s}");
    }
    let repo_day = app.gh.hits.act_cells.iter().find(|(_, d, _, w)| d == "2026-11-02" && *w == 1).map(|c| c.0).expect("the repository's graph");
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: repo_day.x, row: repo_day.y, modifiers: KeyModifiers::NONE });
    assert!(screen(&mut app, &mut t).contains("STATS · Nov 2: 2 commits"));
    // GitHub's graph: a green block a day, a row a weekday, the newest week at the right.
    let cell = |app: &App, date: &str| app.gh.hits.act_cells.iter().find(|(_, d, _, w)| d == date && *w == 0).unwrap().0;
    let (sun, mon, last) = (cell(&app, "2026-11-01"), cell(&app, "2026-11-02"), cell(&app, "2026-11-08"));
    assert_eq!((mon.x, mon.y, last.x, last.y), (sun.x, sun.y + 1, sun.x + 2, sun.y), "Monday under Sunday; the next week two cells right");
    // Centred: the two weeks (and the weekday names before them) in the middle of the pane.
    let pane = app.gh.hits.lists[crate::github::TOP];
    let (left, right) = (sun.x - 2 - pane.x, pane.right() - (last.x + 2));
    assert!(left.abs_diff(right) <= 1, "{left} | {right}");
    let b = t.backend().buffer();
    assert_eq!((b[(last.x, last.y)].symbol(), b[(last.x, last.y)].fg), ("■", crate::theme::P.gh_green), "the busiest day: the full green");
    assert_ne!(b[(sun.x, sun.y)].fg, b[(mon.x, mon.y)].fg, "none and some differ");
    // The day under the pointer, in the header; a click on it opens its month.
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: last.x, row: last.y, modifiers: KeyModifiers::NONE });
    assert!(screen(&mut app, &mut t).contains("Nov 8: 10 contributions"));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), last.x, last.y);
    assert_eq!((app.gh.act_month, app.gh.act_ym), (true, (2026, 11)));
    screen(&mut app, &mut t);
    gh_settle(&mut app, &|a| !a.gh.busy());
    let s = screen(&mut app, &mut t);
    assert!(s.contains("ACTIVITY · 13 in Nov 2026") && s.contains("Less") && s.contains("More"), "{s}");
    let (sun, mon) = (cell(&app, "2026-11-01"), cell(&app, "2026-11-02"));
    assert_eq!((mon.x, mon.y), (sun.x + 2, sun.y), "a month: a row a week, a column a weekday, blocks as the year's");
    // Centred in the pane: as much room left of Sunday as right of Saturday.
    let pane = app.gh.hits.lists[crate::github::TOP];
    let (left, right) = (sun.x - pane.x, pane.right() - (sun.x + 7 * 2));
    assert!(left.abs_diff(right) <= 1, "{left} | {right}");
    // Year again, then ‹ goes to this calendar year; the header folds it all.
    let btn = |app: &App, b: crate::github::Btn| app.gh.hits.btns.iter().find(|(_, x)| *x == b).unwrap().0;
    let year = btn(&app, crate::github::Btn::ActMode(false));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), year.x + 1, year.y);
    screen(&mut app, &mut t);
    let back = btn(&app, crate::github::Btn::ActStep(-1));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), back.x, back.y);
    assert_eq!((app.gh.act_month, app.gh.act_year), (false, Some(crate::github::ymd(crate::scm::now()).0)));
    let head = btn(&app, crate::github::Btn::Activity);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), head.x + 2, head.y);
    let s = screen(&mut app, &mut t);
    assert!(app.gh.activity_folded && !s.contains("Less") && s.contains("ACTIVITY"), "{s}");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), head.x + 2, head.y);
    // Edit a title in the bottom pane: e, type, Enter asks; Cancel goes back, Enter, confirm.
    app.gh.pane = BOTTOM;
    app.gh.sel[BOTTOM] = crate::github::rows(&app.gh, BOTTOM).iter().position(|r| *r == Row::Pr(Sec::RepoPrs, 0)).unwrap();
    key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
    typ(&mut app, "Fix it properly");
    screen(&mut app, &mut t);
    assert!(app.gh.hits.field.0.width > 0, "the title field shows in the row");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let text = app.prompt.as_ref().map(|p| p.text.join(" ")).unwrap_or_default();
    assert!(text.contains("Change the title of pull request #12 in tester/proj?") && text.contains("becomes \"Fix it properly\""), "{text}");
    assert!(!log().contains("pr edit"), "nothing changes before it is confirmed");
    app.run_prompt(PAct::Cancel);
    assert!(app.gh.edit.is_some(), "Cancel goes back to the field");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE); // the prompt's first button: Change Title
    gh_settle(&mut app, &|a| a.gh.acting.is_none() && log().contains("pr edit 12 --repo tester/proj --title Fix it properly"));
    assert!(app.gh.edit.is_none() && app.msg.contains("#12 is now \"Fix it properly\""), "{}", app.msg);
    // The folder's repository: its pull requests check out (asked first; Esc leaves it).
    key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
    assert!(app.prompt.as_ref().is_some_and(|p| p.text[0] == "Check out pull request #12?"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.prompt.is_none() && !log().contains("pr checkout"));
    // The failed run offers Re-run of its failed jobs.
    app.gh.sel[BOTTOM] = crate::github::rows(&app.gh, BOTTOM).iter().position(|r| *r == Row::Run(0)).unwrap();
    key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
    assert_eq!(app.prompt.as_ref().map(|p| p.text[1].as_str()), Some("Only its failed jobs run again."));
    app.run_prompt(app.prompt.as_ref().unwrap().buttons[0].1.clone());
    gh_settle(&mut app, &|a| a.gh.acting.is_none() && log().contains("run rerun 71 --repo tester/proj --failed"));
    // Tab to the top pane; Enter on another repository shows it below (no checkout there).
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.gh.pane, TOP);
    app.gh.sel[TOP] = crate::github::rows(&app.gh, TOP).iter().position(|r| *r == Row::Repo(1)).unwrap();
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    gh_settle(&mut app, &|a| !a.gh.repo_loading);
    let s = screen(&mut app, &mut t);
    assert_eq!(app.gh.chosen.as_deref(), Some("o/lib"));
    assert!(log().contains("pr list --repo o/lib") && s.contains("#12 Fix the thing in o/lib") && s.contains("fork · master · C"), "{s}");
    let pr = Row::Pr(Sec::RepoPrs, 0);
    assert!(!crate::github::keys_of(&app.gh, pr).contains(&crate::github::Key::Checkout), "not the folder's repository");
    fs::remove_dir_all(&root).unwrap();
    fs::remove_dir_all(&fake).unwrap();
}

#[test]
fn the_github_item_sits_above_settings_and_shows_the_account_in_any_repository() {
    let root = repo("not-github");
    let mut app = App::new(root.clone(), None);
    assert_eq!((app.gh.here.as_deref(), app.gh.chosen.as_deref()), (None, None), "no GitHub remote, nothing chosen");
    app.side_w = Some(70);
    let mut t = Terminal::new(TestBackend::new(160, 56)).unwrap();
    screen(&mut app, &mut t);
    let gh = app.ui.activity.iter().find(|(_, s)| *s == Side::GitHub).map(|(r, _)| *r).expect("always there");
    assert_eq!(gh.bottom(), app.ui.settings_btn.y, "right above Settings");
    assert!(app.ui.activity.iter().filter(|(_, s)| *s != Side::GitHub).all(|(r, _)| r.bottom() <= gh.y), "the views above it");
    // Lit exactly as a view is: the same rows around its icon, in both spacings.
    for compact in [false, true] {
        app.settings.compact = compact;
        let shape = |app: &mut App, t: &mut Terminal<TestBackend>, side: Side| {
            app.show_side(side, false);
            screen(app, t);
            let r = app.ui.activity.iter().find(|(_, s)| *s == side).unwrap().0;
            let icon = r.y + u16::from(!compact);
            let b = t.backend().buffer();
            (icon.saturating_sub(2)..icon + 3).map(|y| (b[(r.x + 2, y)].symbol().to_string(), b[(r.x + 2, y)].bg)).collect::<Vec<_>>()
        };
        let (explorer, github) = (shape(&mut app, &mut t, Side::Explorer), shape(&mut app, &mut t, Side::GitHub));
        let lit = |v: &[(String, ratatui::style::Color)]| v.iter().map(|(s, bg)| (s.as_str() == "▄" || s.as_str() == "▀" || *bg != crate::theme::P.activity_bg) as u8).collect::<Vec<_>>();
        // Comfortable: half a row above, the icon's, half below; compact: the icon's and below.
        let rows = if compact { 2..4 } else { 1..4 };
        assert_eq!(lit(&github)[rows.clone()], lit(&explorer)[rows], "compact {compact}: {github:?} vs {explorer:?}");
    }
    app.settings.compact = false;
    // The account and all its repositories in a folder not on GitHub; the bottom pane waits
    // for a choice, then shows the chosen one (by --repo, from here).
    let fake = std::env::temp_dir().join(format!("termcode-fake-gh2-{}", std::process::id()));
    fs::create_dir_all(&fake).unwrap();
    app.gh.bin = fake_gh(&fake);
    app.show_side(Side::GitHub, false);
    crate::github::tick(&mut app);
    gh_settle(&mut app, &|a| !a.gh.loading);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("REPOSITORIES") && s.contains("o/lib") && s.contains("Choose a repository above"), "{s}");
    let log = || fs::read_to_string(fake.join("gh.log")).unwrap();
    assert!(log().contains("api user/repos") && !log().contains("pr list"), "{}", log());
    let row = app.gh.hits.rows.iter().find(|(_, pane, i)| *pane == crate::github::TOP && crate::github::rows(&app.gh, crate::github::TOP)[*i] == crate::github::Row::Repo(0)).unwrap().0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), row.x + 4, row.y);
    gh_settle(&mut app, &|a| !a.gh.repo_loading);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("The project") && s.contains("#12 Fix the thing in tester/proj") && log().contains("pr list --repo tester/proj"), "{s}");
    // The choice and the panes' split are kept with the layout.
    let file = std::env::temp_dir().join(format!("termcode-gh-layout-{}.json", std::process::id()));
    app.layout_file = Some(file.clone());
    app.save_layout();
    let again = App::new(root.clone(), Some(file.clone()));
    assert_eq!(again.gh.chosen.as_deref(), Some("tester/proj"));
    let _ = fs::remove_file(file);
    fs::remove_dir_all(&fake).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn every_page_of_repositories_is_read_until_one_comes_short() {
    let dir = std::env::temp_dir().join(format!("termcode-fake-gh-pages-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("gh");
    // Page 1 and 2 full (100 each), page 3 with 7: 207 repositories, three calls.
    let script = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/gh.log"
case "$*" in
  "api user") echo '{"login":"t","avatar_url":""}' ;;
  *"page=3") n=7 ;;
  *"page="*) n=100 ;;
  "api notifications"|"search "*) echo '[]' ;;
esac
if [ -n "$n" ]; then printf '['; i=0; while [ $i -lt $n ]; do [ $i -gt 0 ] && printf ','; printf '{"full_name":"t/r%s"}' "$i"; i=$((i+1)); done; echo ']'; fi
"#;
    fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let root = repo("gh-pages");
    let mut app = App::new(root.clone(), None);
    app.gh.bin = bin;
    crate::github::fetch(&mut app);
    gh_settle(&mut app, &|a| !a.gh.loading);
    assert_eq!(app.gh.data.as_ref().map(|d| d.repos.len()), Some(207), "{:?}", app.gh.state);
    let log = fs::read_to_string(dir.join("gh.log")).unwrap();
    assert_eq!(log.matches("user/repos").count(), 3, "{log}");
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_wheel_scrolls_the_github_panes_and_keys_bring_the_selection_back() {
    let dir = std::env::temp_dir().join(format!("termcode-fake-gh-wheel-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("gh");
    // 60 repositories: more than the top pane shows.
    let script = r#"#!/bin/sh
case "$*" in
  "api user") echo '{"login":"t","avatar_url":""}' ;;
  *"page=1") printf '['; i=0; while [ $i -lt 60 ]; do [ $i -gt 0 ] && printf ','; printf '{"full_name":"t/repo%s"}' "$i"; i=$((i+1)); done; echo ']' ;;
  *) echo '[]' ;;
esac
"#;
    fs::write(&bin, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let root = repo("gh-wheel");
    let mut app = App::new(root.clone(), None);
    app.gh.bin = bin;
    app.show_side(Side::GitHub, false);
    crate::github::fetch(&mut app);
    gh_settle(&mut app, &|a| !a.gh.loading);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    let list = app.gh.hits.lists[crate::github::TOP];
    let first = |t: &Terminal<TestBackend>| (list.x..list.right()).map(|x| t.backend().buffer()[(x, list.y)].symbol()).collect::<String>();
    assert!(first(&t).contains("REPOSITORIES"));
    for _ in 0..3 {
        mouse(&mut app, MouseEventKind::ScrollDown, list.x + 3, list.y + 2);
    }
    screen(&mut app, &mut t);
    assert!(first(&t).contains("repo8"), "scrolled 9 rows: {:?}", first(&t));
    screen(&mut app, &mut t);
    assert!(first(&t).contains("repo8"), "and stays there: {:?}", first(&t));
    // A key brings the selection (moved from the header to repo0) back into view.
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    screen(&mut app, &mut t);
    assert!(first(&t).contains("repo0 "), "{:?}", first(&t));
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn dragging_the_github_divider_follows_the_pointer_and_keeps_the_stats_header() {
    let root = repo("gh-short");
    Command::new("git").arg("-C").arg(&root).args(["remote", "add", "origin", "https://github.com/tester/proj.git"]).status().unwrap();
    let fake = std::env::temp_dir().join(format!("termcode-fake-gh-short-{}", std::process::id()));
    fs::create_dir_all(&fake).unwrap();
    let mut app = App::new(root.clone(), None);
    app.gh.bin = fake_gh(&fake);
    app.side_w = Some(70);
    app.show_side(Side::GitHub, false);
    crate::github::fetch(&mut app);
    gh_settle(&mut app, &|a| !a.gh.loading && !a.gh.repo_loading && !a.gh.busy());
    let mut t = Terminal::new(TestBackend::new(160, 46)).unwrap();
    screen(&mut app, &mut t);
    // The line between the panes dragged down, row by row: wherever the bottom pane still
    // has room for its facts and a header, the stats header shows.
    // The line follows the pointer row for row, whatever comes and goes above it.
    let sash = app.gh.hits.sash;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), sash.x + 5, sash.y);
    let mut s = String::new();
    for y in sash.y + 1..app.gh.hits.area.bottom() - 6 {
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), sash.x + 5, y);
        s = screen(&mut app, &mut t);
        assert_eq!(app.gh.hits.sash.y, y, "the line is where the pointer is");
        // Under the facts (3 rows): the header, and the lists' rows.
        let room = app.gh.hits.panes[crate::github::BOTTOM].height.saturating_sub(3);
        if room > crate::github::LIST_ROWS {
            assert!(s.contains("STATS · 3 commits in the past year"), "the header shows while it fits (line at {y}): {s}");
        }
    }
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), sash.x + 5, sash.y);
    assert!(s.contains("Rust") || s.contains("Contributors") || s.contains("█") || s.contains("▪"), "and what fits under it: {s}");
    fs::remove_dir_all(&fake).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

/// The screen row of Explorer item `path` (as drawn last).
fn tree_row(app: &App, path: &str) -> u16 {
    let i = app.tree.iter().position(|n| n.path == path).unwrap_or_else(|| panic!("{path} in the tree"));
    app.ui.side_list.y + (i - app.off[0]) as u16 * (1 + app.ui.side_gap)
}

#[test]
fn dragging_an_explorer_item_onto_a_folder_moves_it_once_confirmed() {
    let root = repo("tree-move");
    fs::create_dir_all(root.join("docs/old")).unwrap();
    fs::write(root.join("docs/old/x.md"), "x").unwrap();
    fs::write(root.join("notes.md"), "n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "notes.md", false);
    wait(&mut app);
    app.show_side(Side::Explorer, false);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    let x = app.ui.side_list.x + 4;
    // notes.md over docs: docs lights up; let go: asked first.
    let (from, to) = (tree_row(&app, "notes.md"), tree_row(&app, "docs"));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, from);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, from - 1);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, to);
    assert_eq!(app.tree_drop.as_deref(), Some("docs"));
    screen(&mut app, &mut t);
    assert_eq!(t.backend().buffer()[(x, to)].bg, crate::theme::P.drop_bg, "the target folder lights up");
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, to);
    let text = app.prompt.as_ref().map(|p| p.text.join(" ")).unwrap_or_default();
    assert!(text.contains("move 'notes.md' into 'docs'"), "{text}");
    assert!(root.join("notes.md").exists(), "nothing moves before it is confirmed");
    app.run_prompt(PAct::Move(vec!["notes.md".into()], "docs".into()));
    assert!(root.join("docs/notes.md").exists() && !root.join("notes.md").exists());
    assert!(app.groups[0].tabs.iter().any(|tb| tb.key == "file:docs/notes.md"), "the open editor follows");
    assert_eq!(app.tree.get(app.sel[0]).map(|n| n.path.as_str()), Some("docs/notes.md"), "shown and selected");
    // No target over its own folder, nor (a folder) inside itself.
    screen(&mut app, &mut t);
    let (from, own) = (tree_row(&app, "docs"), tree_row(&app, "docs/old"));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, from);
    screen(&mut app, &mut t); // the folder folds on the press: its rows move
    let own = if app.tree.iter().any(|n| n.path == "docs/old") { tree_row(&app, "docs/old") } else { own };
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, from + 1);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, own);
    assert_eq!(app.tree_drop, None, "{:?}", app.tree.iter().map(|n| &n.path).collect::<Vec<_>>());
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, own);
    assert!(app.prompt.is_none());
    // A name already there: said, not asked.
    fs::write(root.join("README2.md"), "r").unwrap();
    fs::write(root.join("docs/README2.md"), "taken").unwrap();
    app.ask_move(vec!["README2.md".into()], "docs".into());
    assert!(app.prompt.is_none() && app.msg.contains("already exists in docs"), "{}", app.msg);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn explorer_copy_cut_and_paste_number_the_copies() {
    let root = repo("tree-copy");
    fs::create_dir_all(root.join("lib/deep")).unwrap();
    fs::write(root.join("lib/deep/a.txt"), "a").unwrap();
    fs::write(root.join("a.tar.gz"), "z").unwrap();
    fs::write(root.join(".env"), "e").unwrap();
    let mut app = App::new(root.clone(), None);
    app.show_side(Side::Explorer, false);
    let ctrl = |app: &mut App, c: char| key(app, KeyCode::Char(c), KeyModifiers::CONTROL);
    let select = |app: &mut App, path: &str| app.sel[0] = app.tree.iter().position(|n| n.path == path).unwrap();
    // A file copied next to itself: (1), then (2), the extension kept last.
    select(&mut app, "a.tar.gz");
    ctrl(&mut app, 'c');
    ctrl(&mut app, 'v');
    file_settle(&mut app);
    ctrl(&mut app, 'v');
    file_settle(&mut app);
    assert!(root.join("a.tar (1).gz").exists() && root.join("a.tar (2).gz").exists());
    assert_eq!(app.tree.get(app.sel[0]).map(|n| n.path.as_str()), Some("a.tar (2).gz"), "the copy selected");
    assert_eq!(app.free_name("", ".env", &[]), ".env (1)", "a dotfile's dot is no extension");
    // A folder with all in it, into a folder (a file selected: its folder).
    select(&mut app, "lib");
    ctrl(&mut app, 'c');
    app.reveal_path("src/main.rs");
    ctrl(&mut app, 'v');
    file_settle(&mut app);
    assert_eq!(fs::read_to_string(root.join("src/lib/deep/a.txt")).unwrap(), "a");
    // Not into itself.
    select(&mut app, "lib");
    ctrl(&mut app, 'v');
    assert!(app.msg.contains("cannot go inside itself") && !root.join("lib/lib").exists(), "{}", app.msg);
    // Cut and paste moves it, once.
    select(&mut app, ".env");
    ctrl(&mut app, 'x');
    select(&mut app, "src");
    ctrl(&mut app, 'v');
    assert!(root.join("src/.env").exists() && !root.join(".env").exists() && app.tree_clip.is_none());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn delete_moves_to_the_trash_once_asked_and_shift_delete_removes_for_good() {
    let root = repo("tree-delete");
    let trash = std::env::temp_dir().join(format!("termcode-trash-{}", std::process::id()));
    let _ = fs::remove_dir_all(&trash);
    fs::write(root.join("a b.txt"), "one").unwrap();
    fs::write(root.join("kept.txt"), "two").unwrap();
    fs::create_dir_all(root.join("old/inner")).unwrap();
    fs::write(root.join("old/inner/x"), "x").unwrap();
    let mut app = App::new(root.clone(), None);
    app.trash = trash.clone();
    app.open_file(0, "a b.txt", false);
    app.open_file(0, "kept.txt", false);
    wait(&mut app);
    app.focus = Focus::Editor;
    typ(&mut app, "edited "); // kept.txt has unsaved edits
    app.show_side(Side::Explorer, false);
    let select = |app: &mut App, path: &str| app.sel[0] = app.tree.iter().position(|n| n.path == path).unwrap();
    // Delete: asked; Move to Trash: in the Trash with what restores it.
    select(&mut app, "a b.txt");
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    let p = app.prompt.as_ref().unwrap();
    assert_eq!((p.text[0].as_str(), p.buttons[0].0.as_str()), ("Are you sure you want to delete 'a b.txt'?", "Move to Trash"));
    assert!(root.join("a b.txt").exists(), "nothing before it is confirmed");
    app.run_prompt(p.buttons[0].1.clone());
    assert!(!root.join("a b.txt").exists() && trash.join("files/a b.txt").exists());
    let info = fs::read_to_string(trash.join("info/a b.txt.trashinfo")).unwrap();
    let abs = root.canonicalize().unwrap().join("a%20b.txt");
    // A symlink trashed is recorded where the link was, not where it pointed: restoring
    // never lands on the file it points to.
    fs::write(root.join("target.txt"), "t").unwrap();
    std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
    app.reload_all();
    select(&mut app, "link.txt");
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    app.run_prompt(app.prompt.as_ref().unwrap().buttons[0].1.clone());
    let link_info = fs::read_to_string(trash.join("info/link.txt.trashinfo")).unwrap();
    assert!(link_info.contains(&format!("Path={}\n", root.canonicalize().unwrap().join("link.txt").display())) && root.join("target.txt").exists(), "{link_info}");
    assert!(info.starts_with("[Trash Info]\nPath=") && info.contains(&format!("Path={}", abs.display())) && info.contains("DeletionDate=20"), "{info}");
    assert!(!app.groups[0].tabs.iter().any(|t| t.key == "file:a b.txt"), "its editor closed");
    // A second one of the name: NAME.2.
    fs::write(root.join("a b.txt"), "again").unwrap();
    app.reload_all();
    select(&mut app, "a b.txt");
    app.delete_paths(&["a b.txt".into()], false);
    assert_eq!(fs::read_to_string(trash.join("files/a b.txt.2")).unwrap(), "again");
    // An editor with unsaved edits stays open.
    select(&mut app, "kept.txt");
    app.delete_paths(&["kept.txt".into()], false);
    assert!(app.groups[0].tabs.iter().any(|t| t.key == "file:kept.txt"), "unsaved edits are kept");
    // Shift+Delete: a folder and its contents, for good.
    select(&mut app, "old");
    key(&mut app, KeyCode::Delete, KeyModifiers::SHIFT);
    let p = app.prompt.as_ref().unwrap();
    assert_eq!(p.text[0], "Are you sure you want to permanently delete 'old' and its contents?");
    app.run_prompt(p.buttons[0].1.clone());
    assert!(!root.join("old").exists() && !trash.join("files/old").exists());
    fs::remove_dir_all(&trash).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn ctrl_z_takes_back_explorer_moves_copies_and_deletes_and_ctrl_y_redoes_them() {
    let root = repo("tree-undo");
    let trash = std::env::temp_dir().join(format!("termcode-trash-undo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&trash);
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("notes.md"), "n").unwrap();
    let mut app = App::new(root.clone(), None);
    app.trash = trash.clone();
    app.open_file(0, "notes.md", false);
    wait(&mut app);
    app.show_side(Side::Explorer, false);
    app.focus = Focus::Side;
    let ctrl = |app: &mut App, c: char, shift: bool| {
        let m = if shift { KeyModifiers::CONTROL | KeyModifiers::SHIFT } else { KeyModifiers::CONTROL };
        key(app, KeyCode::Char(c), m);
    };
    let at = |p: &str| root.join(p).exists();
    // A move: back, then again; the open editor follows both ways.
    app.move_into(&["notes.md".into()], "docs");
    ctrl(&mut app, 'z', false);
    assert!(at("notes.md") && !at("docs/notes.md") && app.groups[0].tabs.iter().any(|t| t.key == "file:notes.md"), "{}", app.msg);
    ctrl(&mut app, 'y', false);
    assert!(at("docs/notes.md") && !at("notes.md") && app.groups[0].tabs.iter().any(|t| t.key == "file:docs/notes.md"), "{}", app.msg);
    // A paste: the copy goes to the Trash; Ctrl+Shift+Z pastes it again.
    app.sel[0] = app.tree.iter().position(|n| n.path == "docs").unwrap();
    app.tree_clip = Some((vec!["README.md".into()], false));
    app.tree_paste();
    file_settle(&mut app);
    assert!(at("docs/README.md"));
    ctrl(&mut app, 'z', false);
    assert!(!at("docs/README.md") && trash.join("files/README.md").exists(), "{}", app.msg);
    ctrl(&mut app, 'z', true);
    file_settle(&mut app);
    assert!(at("docs/README.md"), "{}", app.msg);
    // A delete: restored from the Trash, with its info file gone.
    app.delete_paths(&["docs/README.md".into()], false);
    let name = match app.file_undo.last().map(Vec::as_slice) { Some([crate::panel::rename::FileOp::Trashed { name, .. }]) => name.clone(), op => panic!("{op:?}") };
    ctrl(&mut app, 'z', false);
    assert!(at("docs/README.md") && !trash.join("info").join(format!("{name}.trashinfo")).exists(), "{}", app.msg);
    // Changed meanwhile (the moved file deleted elsewhere): said, nothing touched.
    app.move_into(&["docs/notes.md".into()], "");
    fs::remove_file(root.join("notes.md")).unwrap();
    ctrl(&mut app, 'z', false);
    assert!(app.msg.contains("couldn't undo: it is no longer where it was"), "{}", app.msg);
    fs::remove_dir_all(&trash).unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn delete_on_another_disk_goes_to_that_disks_trash_and_ctrl_z_brings_it_back() {
    use std::os::unix::fs::MetadataExt;
    let shm = Path::new("/dev/shm");
    let dev = |p: &Path| fs::metadata(p).map(|m| m.dev()).ok();
    if dev(shm).is_none() || dev(shm) == dev(&std::env::temp_dir()) {
        return; // no second disk to try it on
    }
    let root = shm.join(format!("termcode-test-disk-{}", std::process::id()));
    let disk_trash = shm.join(format!(".Trash-test-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.txt"), "a").unwrap();
    let mut app = App::new(root.clone(), None);
    app.trash = std::env::temp_dir().join(format!("termcode-trash-disk-{}", std::process::id()));
    app.delete_paths(&["a.txt".into()], false);
    assert!(app.prompt.is_none(), "not asked to delete it for good");
    assert!(!root.join("a.txt").exists() && disk_trash.join("files/a.txt").exists(), "{}", app.msg);
    assert!(fs::read_to_string(disk_trash.join("info/a.txt.trashinfo")).unwrap().contains("/a.txt"));
    assert_eq!(fs::metadata(&disk_trash).unwrap().mode() & 0o777, 0o700);
    app.focus = Focus::Side;
    app.show_side(Side::Explorer, false);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(root.join("a.txt").exists() && !disk_trash.join("files/a.txt").exists(), "{}", app.msg);
    let _ = fs::remove_dir_all(&disk_trash);
    let _ = fs::remove_dir_all(&app.trash);
    fs::remove_dir_all(&root).unwrap();
}

/// Waits for an Explorer paste on its thread to finish, and folds it in.
fn file_settle(app: &mut App) {
    let t = Instant::now();
    while app.file_job.is_some() {
        app.file_job_tick();
        assert!(t.elapsed() < Duration::from_secs(10), "the paste never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn explorer_multi_select_moves_copies_and_deletes_together_and_ctrl_z_takes_all_back() {
    let root = repo("tree-multi");
    let trash = std::env::temp_dir().join(format!("termcode-trash-multi-{}", std::process::id()));
    let _ = fs::remove_dir_all(&trash);
    fs::create_dir_all(root.join("docs/deep")).unwrap();
    fs::create_dir_all(root.join("box")).unwrap();
    for f in ["a.txt", "b.txt", "c.txt", "docs/deep/x.md"] {
        fs::write(root.join(f), f).unwrap();
    }
    let mut app = App::new(root.clone(), None);
    app.trash = trash.clone();
    app.show_side(Side::Explorer, false);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    let x = app.ui.side_list.x + 4;
    let press = |app: &mut App, kind, y, m| app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: m });
    let click = |app: &mut App, path: &str, m| {
        let y = tree_row(app, path);
        press(app, MouseEventKind::Down(MouseButton::Left), y, m);
        press(app, MouseEventKind::Up(MouseButton::Left), y, m);
    };
    let at = |p: &str| root.join(p).exists();
    // Ctrl+click: a.txt, then c.txt; both lit, nothing opened.
    click(&mut app, "a.txt", KeyModifiers::NONE);
    click(&mut app, "c.txt", KeyModifiers::CONTROL);
    assert_eq!(app.tree_selected(), ["a.txt", "c.txt"]);
    screen(&mut app, &mut t);
    let lit = |app: &App, t: &Terminal<TestBackend>, p: &str| t.backend().buffer()[(x, tree_row(app, p))].bg == crate::theme::P.list_active_bg;
    assert!(lit(&app, &t, "a.txt") && lit(&app, &t, "c.txt") && !lit(&app, &t, "b.txt"));
    // Dragged together onto box: asked once, both move; Ctrl+Z brings both back.
    let to = tree_row(&app, "box");
    let y = tree_row(&app, "a.txt");
    press(&mut app, MouseEventKind::Down(MouseButton::Left), y, KeyModifiers::NONE);
    press(&mut app, MouseEventKind::Drag(MouseButton::Left), to - 1, KeyModifiers::NONE);
    press(&mut app, MouseEventKind::Drag(MouseButton::Left), to, KeyModifiers::NONE);
    press(&mut app, MouseEventKind::Up(MouseButton::Left), to, KeyModifiers::NONE);
    let p = app.prompt.as_ref().unwrap();
    assert!(p.text[0].contains("move the following 2 items into 'box'") && p.text[1] == "a.txt, c.txt", "{:?}", p.text);
    app.run_prompt(p.buttons[0].1.clone());
    assert!(at("box/a.txt") && at("box/c.txt") && !at("a.txt") && !at("c.txt"), "{}", app.msg);
    app.focus = Focus::Side;
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(at("a.txt") && at("c.txt") && !at("box/a.txt"), "{}", app.msg);
    // Shift+↓ from b.txt: b.txt and c.txt; Ctrl+drag onto box copies them, unasked.
    screen(&mut app, &mut t);
    app.sel[0] = app.tree.iter().position(|n| n.path == "b.txt").unwrap();
    app.tree_unmark(app.sel[0]);
    key(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(app.tree_selected(), ["b.txt", "c.txt"]);
    let to = tree_row(&app, "box");
    let y = tree_row(&app, "b.txt");
    press(&mut app, MouseEventKind::Down(MouseButton::Left), y, KeyModifiers::NONE);
    press(&mut app, MouseEventKind::Drag(MouseButton::Left), to + 1, KeyModifiers::CONTROL);
    press(&mut app, MouseEventKind::Drag(MouseButton::Left), to, KeyModifiers::CONTROL);
    press(&mut app, MouseEventKind::Up(MouseButton::Left), to, KeyModifiers::CONTROL);
    assert!(app.prompt.is_none());
    file_settle(&mut app);
    assert!(at("box/b.txt") && at("box/c.txt") && at("b.txt") && at("c.txt"), "{}", app.msg);
    assert_eq!(app.msg, "pasted 2 items");
    // Both copies selected again with Ctrl+click, then deleted at once: one question.
    screen(&mut app, &mut t);
    click(&mut app, "box/b.txt", KeyModifiers::NONE);
    click(&mut app, "box/c.txt", KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    let p = app.prompt.as_ref().unwrap();
    assert_eq!(p.text[0], "Are you sure you want to delete the following 2 items?");
    app.run_prompt(p.buttons[0].1.clone());
    assert!(!at("box/b.txt") && !at("box/c.txt") && trash.join("files/b.txt").exists(), "{}", app.msg);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(at("box/b.txt") && at("box/c.txt"), "{}", app.msg);
    assert_eq!(app.msg, "took back 2 file operations");
    // A plain click leaves one item selected; Esc drops a selection.
    click(&mut app, "box/b.txt", KeyModifiers::CONTROL);
    click(&mut app, "a.txt", KeyModifiers::NONE);
    assert_eq!(app.tree_selected(), ["a.txt"]);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(app.tree_selected().len() > 3);
    app.focus = Focus::Side;
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.tree_marks.is_empty());
    // A dragged item resting on a closed folder opens it.
    screen(&mut app, &mut t);
    let docs = tree_row(&app, "docs");
    app.last_click = None; // not a double-click on a.txt
    let y = tree_row(&app, "a.txt");
    press(&mut app, MouseEventKind::Down(MouseButton::Left), y, KeyModifiers::NONE);
    press(&mut app, MouseEventKind::Drag(MouseButton::Left), docs + 1, KeyModifiers::NONE);
    press(&mut app, MouseEventKind::Drag(MouseButton::Left), docs, KeyModifiers::NONE);
    assert_eq!(app.tree_spring.as_ref().map(|s| s.0.as_str()), Some("docs"), "{} {:?}", matches!(app.drag, Drag::File { active: true, .. }), app.tree.iter().map(|n| (&n.path, n.open)).collect::<Vec<_>>());
    app.tree_spring.as_mut().unwrap().1 = Instant::now();
    let mut dirty = false;
    let _ = app.step(&mut t, &mut dirty, |_| Ok(None));
    assert!(app.tree.iter().any(|n| n.path == "docs/deep"), "docs opened");
    press(&mut app, MouseEventKind::Up(MouseButton::Left), docs, KeyModifiers::NONE);
    let _ = fs::remove_dir_all(&trash);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn terminals_open_as_tabs_switch_close_and_find_in_the_scrollback() {
    let root = repo("term-tabs");
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(110, 30)).unwrap();
    screen(&mut app, &mut t);
    let deadline = Instant::now() + Duration::from_secs(20);
    let until = |app: &mut App, t: &mut Terminal<TestBackend>, what: &str| {
        while !screen(app, t).contains(what) {
            assert!(Instant::now() < deadline, "no {what}");
            app.term_pump();
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    app.toggle_terminal();
    screen(&mut app, &mut t);
    // Many lines: most scroll into the scrollback, with a marker far up.
    // (printf: the command line itself never holds the word sought.)
    typ(&mut app, "printf 'ne%sle-1\\n' ed; seq 1 300; printf 'ne%sle-2\\n' ed; echo done-$((6*7))");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    until(&mut app, &mut t, "done-42");
    // The + on the border: a second shell, shown; both get a tab.
    let plus = app.ui.term_new;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), plus.x + 1, plus.y);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), plus.x + 1, plus.y);
    assert_eq!((app.terms.len(), app.term_at), (2, 1));
    let s = screen(&mut app, &mut t);
    assert!(s.contains(" 1: ") && s.contains(" 2: ") && !s.contains("done-42"), "{s}");
    // Ctrl+PgUp: back to the first; a click on tab 2 and back again.
    key(&mut app, KeyCode::PageUp, KeyModifiers::CONTROL);
    assert_eq!(app.term_at, 0);
    screen(&mut app, &mut t);
    let tab2 = app.ui.term_tabs[1].0;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), tab2.x + 1, tab2.y);
    assert_eq!(app.term_at, 1);
    key(&mut app, KeyCode::PageDown, KeyModifiers::CONTROL);
    assert_eq!(app.term_at, 0);
    // Ctrl+F: find, from the newest match up (Enter), wrapping round.
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(app.term_find.is_some());
    typ(&mut app, "needle");
    let f = app.term_find.as_ref().unwrap();
    assert_eq!(f.count, (2, 2), "the newest of the two");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.term_find.as_ref().unwrap().count.0, 1);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("needle-1") && s.contains("1 of 2") && !s.contains("done-42"), "scrolled back to it: {s}");
    assert_eq!(app.term().unwrap().selected().as_deref(), Some("needle"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.term_find.as_ref().unwrap().count.0, 2, "wraps round");
    typ(&mut app, "zzz");
    assert!(screen(&mut app, &mut t).contains("No results"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.term_find.is_none() && screen(&mut app, &mut t).contains("done-42"), "back at the bottom");
    // × closes the shown one; the other stays.
    let x = app.ui.term_kill;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x.x + 1, x.y);
    assert_eq!((app.terms.len(), app.term_at, app.term_open), (1, 0, true));
    app.terms.clear();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn resting_on_an_explorer_file_shows_its_md5_with_a_copy_button() {
    let root = repo("tip-md5");
    fs::write(root.join("fw.bin"), "x\n").unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(170, 20)).unwrap();
    screen(&mut app, &mut t);
    let (r, _, _, file, _) = app.ui.tips.iter().find(|(_, _, _, f, _)| f.as_deref() == Some("fw.bin")).cloned().expect("a file's row has a tooltip");
    assert_eq!(file.as_deref(), Some("fw.bin"));
    assert!(app.ui.tips.iter().all(|(_, _, _, f, _)| f.as_deref() != Some("src")), "not a folder's");
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: r.x + 3, row: r.y, modifiers: KeyModifiers::NONE });
    app.tip.as_mut().unwrap().since -= TIP_DELAY;
    let mut dirty = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.tip.as_ref().is_some_and(|t| t.md5.is_some()) {
        assert!(Instant::now() < deadline, "no MD5");
        app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    let s = screen(&mut app, &mut t);
    assert!(s.contains("MD5  401b30e3b8b5d629635a5c613cdb7919") && s.contains("Copy"), "{s}");
    assert_eq!(app.ui.tip_md5.y, r.y + 1, "on the line under the row");
    // Onto the line it stays; Copy copies the sum.
    let c = app.ui.tip_copy;
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: c.x + 2, row: c.y, modifiers: KeyModifiers::NONE });
    assert!(app.tip.as_ref().is_some_and(|t| t.shown));
    app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: c.x + 2, row: c.y, modifiers: KeyModifiers::NONE }));
    app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: c.x + 2, row: c.y, modifiers: KeyModifiers::NONE }));
    assert_eq!(app.clip, "401b30e3b8b5d629635a5c613cdb7919");
    assert!(app.msg.starts_with("copied the MD5 of fw.bin"), "{}", app.msg);
    // Elsewhere on the line, a click is the row under it's: the tooltip closes.
    let m = app.ui.tip_md5;
    let below = app.tree.iter().position(|n| n.path == "README.md").unwrap();
    assert_eq!(app.ui.side_item(m.y, app.off[0]), Some(below), "the line lies over README.md's row");
    app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: m.x + 2, row: m.y, modifiers: KeyModifiers::NONE }));
    app.handle_event(Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), column: m.x + 2, row: m.y, modifiers: KeyModifiers::NONE }));
    assert!(app.tip.is_none() && app.sel[0] == below, "{}", app.sel[0]);
    // Off it, it goes; opened, a .bin is its hexdump.
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: 0, row: 0, modifiers: KeyModifiers::NONE });
    assert!(app.tip.is_none());
    app.open_file(0, "fw.bin", false);
    wait(&mut app);
    let s = screen(&mut app, &mut t);
    assert!(s.contains("00000000  78 0a") && s.contains("|x.|") && s.contains("Hex"), "{s}");
    let gh = app.ui.groups[0].text;
    let first = s.lines().nth(gh.y as usize).unwrap();
    let at = first.find("00000000").unwrap();
    assert!(!first[..at].trim_end().ends_with('1'), "no line numbers before the offsets: {first}");
    // F1 View: Toggle Hexdump: the same file as text, and back.
    app.run_command(Cmd::ToggleHex);
    let s = screen(&mut app, &mut t);
    assert!(!s.contains("00000000") && !s.contains("Hex") && app.msg == "shown as text", "{s}");
    app.run_command(Cmd::ToggleHex);
    assert!(screen(&mut app, &mut t).contains("00000000  78 0a"));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn esc_stops_a_paste_and_takes_out_what_it_half_copied() {
    let root = repo("paste-stop");
    let big = root.join("big");
    fs::create_dir_all(&big).unwrap();
    for i in 0..2000 {
        fs::write(big.join(format!("f{i}")), "data").unwrap();
    }
    let mut app = App::new(root.clone(), None);
    app.show_side(Side::Explorer, false);
    app.focus = Focus::Side;
    app.sel[0] = app.tree.iter().position(|n| n.path == "src").unwrap();
    app.tree_clip = Some((vec!["big".into()], false));
    app.tree_paste();
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE); // 2000 files take longer than this
    file_settle(&mut app);
    assert_eq!(app.msg, "paste stopped");
    assert!(!root.join("src/big").exists(), "the half copy is gone");
    assert!(app.file_undo.is_empty(), "nothing to take back");
    // Unstopped, it reports its files as it goes, then the item.
    app.tree_paste();
    file_settle(&mut app);
    assert_eq!((app.msg.as_str(), fs::read_dir(root.join("src/big")).unwrap().count()), ("pasted big", 2000));
    fs::remove_dir_all(&root).unwrap();
}


#[test]
fn a_repository_page_has_a_main_page_a_tree_a_files_code_and_a_branch_to_switch() {
    use crate::github::page::Target;
    use crate::github::{Row, TOP};
    let root = repo("gh-page");
    let fake = std::env::temp_dir().join(format!("termcode-fake-gh-page-{}", std::process::id()));
    fs::create_dir_all(&fake).unwrap();
    let mut app = App::new(root.clone(), None);
    app.gh.bin = fake_gh(&fake);
    app.side_w = Some(50);
    app.outline_open = true;
    let mut t = Terminal::new(TestBackend::new(170, 90)).unwrap();
    app.show_side(Side::GitHub, false);
    crate::github::tick(&mut app);
    gh_settle(&mut app, &|a| a.gh.data.is_some());
    // Enter on a repository: its page opens beside, as a preview tab, on its main page.
    app.gh.sel[TOP] = crate::github::rows(&app.gh, TOP).iter().position(|r| matches!(r, Row::Repo(_))).unwrap();
    app.focus = Focus::Side;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    let tab = &app.groups[app.ag].tabs[app.groups[app.ag].active];
    assert!(tab.key == "repo:tester/proj" && tab.preview, "{}", tab.key);
    gh_settle(&mut app, &|a| a.gh.pages.get("tester/proj").is_some_and(|p| p.dirs.contains_key("")));
    let s = screen(&mut app, &mut t);
    for want in ["tester/proj", " main ", "ann", "Say hello", "abcdef1", "FILES", "src", "Add the sources", "README.md", "Hello world from the README."] {
        assert!(s.contains(want), "{want}: {s}");
    }
    assert!(app.ui.outline.is_empty() && app.ui.outline_btns.is_empty(), "the page has its own outline");
    let hit = |app: &App, want: &Target| app.gh.hits.page.iter().rev().find(|(_, t)| t == want).map(|(r, _)| *r).unwrap_or_else(|| panic!("{want:?}"));
    let press = |app: &mut App, r: Rect| mouse(app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    let text = app.ui.groups[app.ag].text;
    let (list_src, tree_src) = (hit(&app, &Target::Into("src".into())), hit(&app, &Target::Folder("src".into())));
    assert!(list_src.x < text.right() && tree_src.x > text.right(), "the list in the middle, the tree on the right");
    // Under the files, the insights: counts, the year's commits, when they land, churn.
    gh_settle(&mut app, &|a| a.gh.pages["tester/proj"].stats.is_some());
    let s = screen(&mut app, &mut t);
    for want in ["INSIGHTS", "7 stars", "2 open · 30 merged · 5 closed", "1 open · 9 closed", "Rust 90%", "Commits", "When", "Code changes", "+160 / -110", "Contributors"] {
        assert!(s.contains(want), "{want}: {s}");
    }
    // The list's src: listed in the middle (with ..), and opened in the tree.
    press(&mut app, list_src);
    gh_settle(&mut app, &|a| a.gh.pages["tester/proj"].dirs.contains_key("src"));
    let s = screen(&mut app, &mut t);
    assert!(s.contains("tester/proj / src") && s.contains(" ..") && app.gh.pages["tester/proj"].open.contains("src"), "{s}");
    // main.rs: its code in the middle, an editor's (it copies), its outline on the right.
    let r = hit(&app, &Target::File("src/main.rs".into()));
    press(&mut app, r);
    gh_settle(&mut app, &|a| a.docs.contains_key("repo:tester/proj"));
    let s = screen(&mut app, &mut t);
    assert!(s.contains("fn main() {}") && s.contains("tester/proj / src / main.rs") && s.contains("OUTLINE"), "{s}");
    app.focus = Focus::Editor;
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.clip, "fn main() {}");
    let r = hit(&app, &Target::Symbol(0));
    press(&mut app, r);
    assert_eq!(app.groups[app.ag].tabs[app.groups[app.ag].active].view.cursor, Pos::new(0, 0));
    // The button over FILES: the main page again.
    let button = app.gh.hits.page.iter().find(|(r, t)| *t == Target::Main && r.x > text.right()).map(|(r, _)| *r).expect("the main page button");
    press(&mut app, button);
    assert!(!app.docs.contains_key("repo:tester/proj") && screen(&mut app, &mut t).contains("Hello world from the README."));
    // The branch: a pick of the branches; dev shows its own files.
    let r = hit(&app, &Target::Branches);
    press(&mut app, r);
    gh_settle(&mut app, &|a| a.gitui.cmds.picker.is_some());
    let labels: Vec<String> = app.gitui.cmds.picker.as_ref().unwrap().items.iter().map(|i| i.label.clone()).collect();
    assert_eq!(labels, ["main", "dev"], "the shown one first");
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    gh_settle(&mut app, &|a| a.gh.pages["tester/proj"].dirs.contains_key(""));
    let s = screen(&mut app, &mut t);
    assert!(s.contains("DEV.md") && s.contains(" dev ") && app.gh.pages["tester/proj"].branch.as_deref() == Some("dev"), "{s}");
    let _ = fs::remove_dir_all(&fake);
    fs::remove_dir_all(&root).unwrap();
}


#[test]
fn symlinks_carry_an_arrow_and_their_tooltip_says_where_they_point() {
    let root = repo("tree-links");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("libz.so.1.3"), "x\n").unwrap();
    std::os::unix::fs::symlink("libz.so.1.3", root.join("libz.so.1")).unwrap();
    std::os::unix::fs::symlink("docs", root.join("docs-link")).unwrap();
    std::os::unix::fs::symlink("nowhere", root.join("broken")).unwrap();
    let mut app = App::new(root.clone(), None);
    let mut t = Terminal::new(TestBackend::new(170, 24)).unwrap();
    let s = screen(&mut app, &mut t);
    let node = |p: &str| app.tree.iter().find(|n| n.path == p).unwrap();
    assert_eq!(node("libz.so.1").link, Some(("libz.so.1.3".to_string(), true)));
    assert!(node("docs-link").dir && node("docs-link").link.is_some(), "a link to a folder opens as one");
    assert_eq!(node("broken").link, Some(("nowhere".to_string(), false)));
    assert!(node("libz.so.1.3").link.is_none());
    let row = |s: &str, w: &str| s.lines().find(|l| l.contains(w)).unwrap().to_string();
    assert!(row(&s, "libz.so.1 ").contains("↪") && !row(&s, "libz.so.1.3").contains("↪"), "{s}");
    let (r, ..) = app.ui.tips.iter().find(|(_, _, _, _, l)| l.as_deref() == Some("broken")).cloned().unwrap();
    let ax = s.lines().nth(r.y as usize).unwrap().chars().position(|c| c == '↪').unwrap() as u16;
    assert_eq!(t.backend().buffer()[(ax, r.y)].fg, crate::theme::P.error_fg, "a broken link's arrow is red");
    // Resting on the link: where it points, then its target's MD5.
    let (r, ..) = app.ui.tips.iter().find(|(_, _, _, _, l)| l.as_deref() == Some("libz.so.1")).cloned().unwrap();
    app.on_mouse(MouseEvent { kind: MouseEventKind::Moved, column: r.x + 3, row: r.y, modifiers: KeyModifiers::NONE });
    app.tip.as_mut().unwrap().since -= TIP_DELAY;
    let mut dirty = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !app.tip.as_ref().is_some_and(|t| t.md5.is_some()) {
        assert!(Instant::now() < deadline, "no MD5");
        app.step(&mut t, &mut dirty, |_| Ok(None)).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    let s = screen(&mut app, &mut t);
    let link = s.lines().nth(r.y as usize + 1).unwrap();
    assert!(link.contains("Link → libz.so.1.3 ") && !link.contains(&*root.to_string_lossy()), "as written, no absolute path: {s}");
    assert!(s.lines().nth(r.y as usize + 2).unwrap().contains("MD5  401b30e3b8b5d629635a5c613cdb7919"), "{s}");
    fs::remove_dir_all(&root).unwrap();
}


#[test]
fn a_binarys_banner_chooses_between_readelf_and_hexdump() {
    if Command::new("readelf").arg("--version").output().is_err() || !Path::new("/bin/true").exists() {
        return; // no binutils here
    }
    let root = repo("elf-view");
    fs::copy("/bin/true", root.join("t.so")).unwrap();
    let mut app = App::new(root.clone(), None);
    app.open_file(0, "t.so", false);
    wait(&mut app);
    let mut t = Terminal::new(TestBackend::new(170, 30)).unwrap();
    let s = screen(&mut app, &mut t);
    assert!(s.contains("# t.so: ELF") && s.contains(" readelf ") && s.contains(" hexdump ") && s.contains("ELF"), "{s}");
    let btn = |app: &App, hex: bool| {
        let how = if hex { crate::media::As::Hex } else { crate::media::As::Main };
        app.ui.view_btns.iter().find(|(_, h)| *h == how).map(|(r, _)| *r).unwrap()
    };
    let elf = |app: &App| match app.docs.get("file:t.so") { Some(Doc::Text(b)) => (b.elf, b.binary), _ => panic!() };
    let r = btn(&app, true);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(elf(&app), (false, true), "its bytes");
    assert!(screen(&mut app, &mut t).contains("00000000  7f 45 4c 46"));
    let r = btn(&app, false);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
    assert_eq!(elf(&app), (true, false), "readelf's report again");
    fs::remove_dir_all(&root).unwrap();
}

/// explorer.autoReveal: the active editor's file shows in the Explorer, as the active
/// editor changes, and only then.
#[test]
fn the_explorer_shows_the_active_editors_file() {
    let root = repo("reveal");
    fs::create_dir_all(root.join("drivers/net/eth")).unwrap();
    fs::write(root.join("drivers/net/eth/phy.c"), "int phy;\n").unwrap();
    fs::write(root.join("top.c"), "int top;\n").unwrap();
    let mut app = App::new(root.clone(), None);
    wait(&mut app);
    app.show_side(Side::Explorer, false);
    app.auto_reveal();
    let at = |app: &App, p: &str| app.tree.iter().position(|n| n.path == p);
    // A file deep in folders: they open, it is selected.
    app.open_file(0, "drivers/net/eth/phy.c", false);
    assert!(app.auto_reveal());
    assert!(["drivers", "drivers/net", "drivers/net/eth"].iter().all(|d| app.tree[at(&app, d).unwrap()].open));
    assert_eq!(app.sel[0], at(&app, "drivers/net/eth/phy.c").unwrap());
    // A folder closed by hand stays closed while the same editor is active.
    let d = at(&app, "drivers").unwrap();
    app.toggle(d);
    assert!(!app.auto_reveal() && at(&app, "drivers/net").is_none());
    // Another editor: shown in its turn; back to the first: open again.
    app.open_file(0, "top.c", false);
    assert!(app.auto_reveal());
    assert_eq!(app.sel[0], at(&app, "top.c").unwrap());
    app.cycle_tab(-1);
    assert!(app.auto_reveal() && at(&app, "drivers/net/eth/phy.c").is_some());
    // Turned off: the Explorer stays as it is.
    app.settings.auto_reveal = false;
    let d = at(&app, "drivers").unwrap();
    app.toggle(d);
    app.cycle_tab(1);
    assert!(!app.auto_reveal() && at(&app, "drivers/net").is_none());
    fs::remove_dir_all(&root).unwrap();
}

