//! The Extensions view (+, right above Settings): every view, built in or installed, each
//! turned on or off; an extension installed from a git URL or a folder (what it runs and
//! may do shown first, installed only on the user's yes) or removed. It is an in-tree
//! driver of the subsystem it manages.

use super::install;
use super::process::{draw_rows, root, Cell, Ext, Manifest, Row, Slot, MANAGER};
use super::{Ops, DRIVERS, EXTENSIONS};
use crate::panel::{App, Focus, PAct, Prompt, Side};
use crate::theme::P;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;
use std::sync::mpsc::TryRecvError;

pub(super) const OPS: Ops = Ops {
    id: "extensions",
    name: "Extensions",
    title: "EXTENSIONS",
    about: "Every view: turn one off, install or remove an extension",
    icon: || crate::theme::icons().plus,
    side: draw,
    takes,
    paste,
    tick,
    ..Ops::NONE
};

fn text(t: impl Into<String>, fg: Option<&str>) -> Cell {
    Cell { text: t.into(), fg: fg.map(String::from), ..Cell::default() }
}

fn button(t: &str, action: String) -> Cell {
    Cell { text: t.into(), button: true, action: Some(action), ..Cell::default() }
}

fn line(cells: Vec<Cell>) -> Row {
    Row { cells, action: None }
}

/// `t` in dim rows of at most `w` characters, broken between words.
fn wrapped(t: &str, w: usize) -> Vec<Row> {
    let mut rows: Vec<String> = Vec::new();
    for word in t.split_whitespace() {
        match rows.last_mut() {
            Some(l) if l.chars().count() + 1 + word.chars().count() <= w.max(8) => {
                l.push(' ');
                l.push_str(word);
            }
            _ => rows.push(word.to_string()),
        }
    }
    rows.into_iter().map(|l| line(vec![text(l, Some("dim"))])).collect()
}

fn draw(app: &mut App, f: &mut Frame, r: Rect) {
    let focused = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let r = crate::ui::inset(card, (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1));
    let w = r.width as usize;
    let x = &app.exts;
    let mut rows = Vec::new();
    // What to install: a field (click to type), its button.
    let shown = match (x.input.is_empty(), x.typing) {
        (true, false) => text(format!(" {:<w$}", "Git URL or folder", w = w.saturating_sub(1)), Some("dim")),
        _ => text(format!(" {}{:<w$}", x.input, if x.typing { "▏" } else { "" }, w = w.saturating_sub(1 + x.input.chars().count())), None),
    };
    rows.push(Row { cells: vec![Cell { bg: Some("input".into()), ..shown }], action: Some("type".into()) });
    rows.push(line(match (&x.job, &x.building) {
        (Some(_), _) => vec![text("Fetching…", Some("dim"))],
        (_, Some((name, _))) => vec![text(format!("Building {name}…"), Some("dim"))],
        _ => vec![button("Install", "install".into())],
    }));
    rows.push(Row::default());
    let state = |id: &str| if x.off.contains(id) { ("Turn on", Some("dim")) } else { ("Turn off", None) };
    rows.push(line(vec![Cell { text: "BUILT IN".into(), bold: true, fg: Some("dim".into()), ..Cell::default() }]));
    for o in DRIVERS.iter().filter(|o| o.id != OPS.id) {
        let (verb, fg) = state(o.id);
        rows.push(line(vec![text(format!("{} ", (o.icon)()), fg), Cell { text: o.name.into(), bold: true, fg: fg.map(String::from), ..Cell::default() }]));
        rows.extend(wrapped(o.about, w));
        rows.push(line(vec![button(verb, format!("toggle:{}", o.id))]));
        rows.push(Row::default());
    }
    rows.push(line(vec![Cell { text: "INSTALLED".into(), bold: true, fg: Some("dim".into()), ..Cell::default() }]));
    if x.list.is_empty() {
        rows.push(line(vec![text("None yet. Install one above,", Some("dim"))]));
        rows.push(line(vec![text("or write your own: termcode ext new", Some("dim"))]));
    }
    for e in &x.list {
        let m = &e.m;
        let (verb, fg) = state(&m.id);
        rows.push(line(vec![text(format!("{} ", m.icon), fg), Cell { text: m.name.clone(), bold: true, fg: fg.map(String::from), ..Cell::default() }, text(format!(" {}", m.version), Some("dim"))]));
        rows.extend(wrapped(&m.description, w));
        rows.push(line(vec![text(format!("May: {}", install::perms(&m.permissions)), Some("dim"))]));
        rows.push(line(vec![button(verb, format!("toggle:{}", m.id)), text(" ", None), button("Remove", format!("remove:{}", m.id))]));
        rows.push(Row::default());
    }
    let hover = app.hover;
    let x = &mut app.exts;
    x.view.rows = rows;
    x.panes.push((r, MANAGER, Slot::Left, 0));
    draw_rows(f, r, &mut x.view, MANAGER, Slot::Left, 0, hover, &mut x.hits, P.sidebar_bg, P.sidebar_fg);
}

/// A click in the view.
pub(super) fn action(app: &mut App, action: &str) {
    app.focus = Focus::Side;
    app.exts.typing = action == "type";
    if action == "install" {
        return install(app);
    }
    if let Some(id) = action.strip_prefix("toggle:") {
        let off = !app.exts.off.contains(id);
        app.exts.set_off(id, off);
        if off {
            if let Some(e) = app.exts.list.iter_mut().find(|e| e.m.id == id) {
                e.stop();
            }
            if app.side == Side::View(super::num(id)) {
                app.show_side(Side::Explorer, false);
                app.show_side(Side::View(EXTENSIONS), false);
            }
        }
    } else if let Some(id) = action.strip_prefix("remove:") {
        let Some(e) = app.exts.list.iter().find(|e| e.m.id == id) else { return };
        app.prompt = Some(Prompt {
            text: vec![format!("Remove {} {}?", e.m.name, e.m.version), install::what(&root(), id)],
            buttons: vec![("Remove".into(), PAct::ExtRemove(id.to_string())), ("Cancel".into(), PAct::Cancel)],
            sel: 1,
        });
    }
}

/// Keys while typing in the field: its own (before termcode's).
fn takes(app: &mut App, k: KeyEvent) -> bool {
    if !app.exts.typing || app.focus != Focus::Side || app.side != Side::View(EXTENSIONS) {
        return false;
    }
    match k.code {
        KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => app.exts.input.push(c),
        KeyCode::Backspace => {
            app.exts.input.pop();
        }
        KeyCode::Enter => install(app),
        KeyCode::Esc => app.exts.typing = false,
        _ => return false,
    }
    true
}

fn paste(app: &mut App, t: &str) -> bool {
    if !app.exts.typing || app.focus != Focus::Side || app.side != Side::View(EXTENSIONS) {
        return false;
    }
    app.exts.input.push_str(t.lines().next().unwrap_or("").trim());
    true
}

/// Fetched in the background (a clone takes a while), then shown for the user's yes.
fn install(app: &mut App) {
    let src = app.exts.input.trim().to_string();
    if src.is_empty() {
        app.exts.typing = true;
        app.msg = "Type a git URL (https://…) or a folder, then Install".into();
        return;
    }
    if app.exts.job.is_some() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let root = root();
    std::thread::spawn(move || {
        let _ = tx.send(install::fetch(&root, &src));
    });
    app.exts.job = Some(rx);
    app.exts.typing = false;
}

fn tick(app: &mut App) -> bool {
    built(app) | fetched(app)
}

/// Placed and built: shown at once.
fn built(app: &mut App) -> bool {
    let Some((_, rx)) = &app.exts.building else { return false };
    let got = match rx.try_recv() {
        Err(TryRecvError::Empty) => return false,
        Err(TryRecvError::Disconnected) => Err("the build stopped".to_string()),
        Ok(r) => r,
    };
    app.exts.building = None;
    match got {
        Ok(m) => {
            app.msg = format!("Installed {}: its icon is in the activity bar", m.name);
            app.exts.list.retain(|e| e.m.id != m.id);
            app.exts.list.push(Ext::new(m));
            app.exts.input.clear();
        }
        Err(e) => app.msg = format!("Not installed: {e}"),
    }
    true
}

fn fetched(app: &mut App) -> bool {
    let Some(rx) = &app.exts.job else { return false };
    let got = match rx.try_recv() {
        Err(TryRecvError::Empty) => return false,
        Err(TryRecvError::Disconnected) => None,
        Ok(r) => Some(r),
    };
    app.exts.job = None;
    match got {
        Some(Ok(f)) => {
            let mut text = install::describe(&f.m);
            if let Some(old) = app.exts.list.iter().find(|e| e.m.id == f.m.id) {
                text.push(format!("It replaces {} {}.", old.m.name, old.m.version));
            }
            app.prompt = Some(Prompt { text, buttons: vec![("Install".into(), PAct::ExtInstall(true)), ("Cancel".into(), PAct::ExtInstall(false))], sel: 1 });
            app.exts.pending = Some(f);
        }
        Some(Err(e)) => app.msg = e,
        None => {}
    }
    true
}

/// The user's answer to installing the one fetched: placed and built in the background
/// (a build takes a while), then shown (`built`).
pub fn decided(app: &mut App, yes: bool) {
    let Some(f) = app.exts.pending.take() else { return };
    if !yes {
        f.discard();
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (root, id, name) = (root(), f.m.id.clone(), f.m.name.clone());
    std::thread::spawn(move || {
        let done = install::place(&root, f, false).and_then(|dest| {
            install::build(&dest, true).inspect_err(|_| {
                let _ = install::remove(&root, &id);
            })?;
            Manifest::read(&dest)
        });
        let _ = tx.send(done);
    });
    app.exts.building = Some((name, rx));
}

/// The user's yes to removing `id`: gone, its tabs closed, its program ended.
pub fn removed(app: &mut App, id: &str) {
    match install::remove(&root(), id) {
        Ok(()) => {
            let key = format!("{}{id}", super::PREFIX);
            for g in app.groups.iter_mut() {
                g.tabs.retain(|t| t.key != key);
                g.active = g.active.min(g.tabs.len().saturating_sub(1));
            }
            app.exts.list.retain(|e| e.m.id != id);
            app.exts.set_off(id, false);
            app.msg = format!("Removed {id}");
        }
        Err(e) => app.msg = e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn frame(app: &mut App) {
        let mut t = ratatui::Terminal::new(TestBackend::new(140, 50)).unwrap();
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
    }

    #[test]
    fn the_plus_view_installs_turns_views_off_and_removes() {
        let d = std::env::temp_dir().join(format!("termcode-ext-manager-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src").join(super::super::process::MANIFEST), r#"{"id": "mgr-demo", "name": "Demo", "version": "2", "run": ["./x.sh"], "permissions": ["edit"]}"#).unwrap();
        std::env::set_var("TERMCODE_EXTENSIONS", d.join("root"));
        let mut app = App::new(std::fs::canonicalize(&d).unwrap(), None);
        app.show_side(Side::View(EXTENSIONS), false);
        frame(&mut app);
        // Typed into its field, then Install: fetched, shown, installed on the yes.
        let field = app.exts.hits.iter().find(|h| h.action == "type").map(|h| ratatui::layout::Position::new(h.r.x, h.r.y)).unwrap();
        assert!(super::super::process::click(&mut app, field));
        for c in d.join("src").display().to_string().chars() {
            assert!(takes(&mut app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        assert!(takes(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        for _ in 0..500 {
            if tick(&mut app) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let asked = app.prompt.take().expect("asked first").text.join("\n");
        assert!(asked.contains("Demo 2 (mgr-demo)") && asked.contains("Runs: ./x.sh") && asked.contains("May: read, edit"), "{asked}");
        decided(&mut app, true);
        for _ in 0..500 {
            if built(&mut app) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(d.join("root/mgr-demo").is_dir());
        frame(&mut app);
        let n = crate::ext::num("mgr-demo");
        assert!(app.ui.activity.iter().any(|(_, s)| *s == Side::View(n)), "its icon at once, no reload");
        // A built-in view turned off: its icon gone; on again: back.
        action(&mut app, "toggle:serial");
        frame(&mut app);
        assert!(!app.ui.activity.iter().any(|(_, s)| *s == Side::SERIAL));
        action(&mut app, "toggle:serial");
        frame(&mut app);
        assert!(app.ui.activity.iter().any(|(_, s)| *s == Side::SERIAL));
        // Removed after its own yes.
        action(&mut app, "remove:mgr-demo");
        assert!(app.prompt.take().is_some_and(|p| p.text[0] == "Remove Demo 2?"));
        removed(&mut app, "mgr-demo");
        assert!(app.exts.list.is_empty() && !d.join("root/mgr-demo").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
