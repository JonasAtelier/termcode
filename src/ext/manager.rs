//! The Extensions view (+, right above Settings): every view, built in or installed, each
//! turned on or off; an extension installed from a git URL or a folder (what it runs and
//! may do shown first, installed only on the user's yes) or removed. It is an in-tree
//! driver of the subsystem it manages.

use super::install;
use super::process::{draw_rows, root, Cell, Ext, Manifest, Row, Shared, Slot, MANAGER};
use super::{Ops, DRIVERS, EXTENSIONS};
use crate::panel::{App, Focus, PAct, Prompt, Side};
use crate::theme::P;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::Frame;
use std::sync::mpsc::TryRecvError;

pub(super) const OPS: Ops = Ops {
    id: "extensions",
    name: "Extensions",
    title: "EXTENSIONS",
    about: "Every view: turn one off, install or remove an extension",
    icon: || crate::theme::icons().plus,
    side: draw,
    owns: |k| k.starts_with(KEY),
    tab: |app, f, _, r, key, _| {
        page(app, f, r, key);
        super::Drawn::Whole
    },
    tab_title: |app, key| {
        let id = key.strip_prefix(KEY)?;
        let name = app.exts.list.iter().find(|e| e.m.id == id).map(|e| e.m.name.clone()).or_else(|| DRIVERS.iter().find(|o| o.id == id).map(|o| o.name.to_string()))?;
        Some(format!("Extension: {name}"))
    },
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
    Row { cells, action: None, hint: None, head: None, pic: None }
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

/// What is typed: a source to install (a URL or a folder), else a search.
fn is_source(q: &str) -> bool {
    ["https://", "ssh://", "git@", "/", "~", "./", "../"].iter().any(|p| q.starts_with(p))
}

/// `s` in `w` columns at most, "…" where cut.
fn cut(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    s.chars().take(w.saturating_sub(1)).collect::<String>() + "…"
}

/// One extension in the list, VS Code's way: its icon, name and version; its description
/// on one line; what it is (built in, may do, disabled). The whole of it opens its page.
struct Item {
    id: String,
    icon: String,
    name: String,
    version: String,
    about: String,
    meta: (String, Option<&'static str>),
    install: bool,
}

/// A detail tab's key: `extensions:<id>`.
const KEY: &str = "extensions:";

fn item_rows(it: &Item, w: usize, picked: bool) -> Vec<Row> {
    let open = Some(format!("open:{}", it.id));
    let bg = picked.then(|| "active".to_string());
    let cell = |t: String, fg: Option<&str>, bold: bool| Cell { text: t, fg: fg.map(String::from), bg: bg.clone(), bold, ..Cell::default() };
    let pad = |used: usize| " ".repeat(w.saturating_sub(used));
    let head = format!("{} {}", it.icon, cut(&it.name, w.saturating_sub(4 + it.version.chars().count())));
    let mut first = vec![cell(head.clone(), None, true), cell(format!(" {}", it.version), Some("dim"), false)];
    let used = head.chars().count() + 1 + it.version.chars().count();
    first.push(cell(pad(used), None, false));
    let about = format!("  {}", cut(&it.about, w.saturating_sub(2)));
    let mut rows = vec![Row { cells: first, action: open.clone(), hint: None, head: None, pic: None }, Row { cells: vec![cell(about.clone(), Some("dim"), false), cell(pad(about.chars().count()), None, false)], action: open.clone(), hint: None, head: None, pic: None }];
    let meta = format!("  {}", cut(&it.meta.0, w.saturating_sub(if it.install { 12 } else { 2 })));
    let mut third = vec![cell(meta.clone(), it.meta.1, false)];
    if it.install {
        third.push(cell(pad(meta.chars().count() + 9), None, false));
        third.push(Cell { text: "Install".into(), button: true, action: Some(format!("get:{}", it.id)), ..Cell::default() });
    } else {
        third.push(cell(pad(meta.chars().count()), None, false));
    }
    rows.push(Row { cells: third, action: open, hint: None, head: None, pic: None });
    rows.push(Row::default());
    rows
}

fn draw(app: &mut App, f: &mut Frame, r: Rect) {
    let focused = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let r = crate::ui::inset(card, (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1));
    // The search box, as Search's: it filters the list; a URL or a folder installs.
    let inp = Rect::new(r.x, r.y, r.width, 1.min(r.height));
    crate::ui::fill(f, inp, P.input_bg);
    let typing = app.exts.typing && focused;
    crate::ui::input(f, inp, &app.exts.input, "Search, or a git URL / folder", typing && crate::theme::ascii());
    crate::ui::round_ends(f, inp, P.input_bg, P.sidebar_bg);
    if typing && inp.width > 2 {
        f.set_cursor_position((inp.x + 1 + (Span::raw(app.exts.input.as_str()).width() as u16).min(inp.width - 2), inp.y));
    }
    app.exts.hits.push(super::process::Hit { r: inp, ext: MANAGER, slot: Slot::Left, sec: 0, action: "type".into() });
    let gap = 1 + app.settings.spacing.section_gap;
    let r = Rect::new(r.x, r.y + gap, r.width, r.height.saturating_sub(gap));
    let w = r.width as usize;
    let x = &app.exts;
    let mut rows = Vec::new();
    let q = x.input.trim().to_lowercase();
    let source = is_source(&q);
    if let Some((title, st)) = &x.progress {
        let (stage, p) = st.lock().map_or(("", 0), |s| *s);
        rows.push(line(vec![text(cut(title, w), None)]));
        rows.push(line(vec![text(format!("{stage} "), Some("dim")), text(bar(p), Some("accent")), text(format!(" {p}%"), None)]));
    } else if source {
        rows.push(line(vec![button("Install", "install".into()), text(format!(" from {}", cut(x.input.trim(), w.saturating_sub(15))), Some("dim"))]));
    }
    if !rows.is_empty() {
        rows.push(Row::default());
    }
    let hit = |it: &Item| source || q.is_empty() || [&it.name, &it.id, &it.about].iter().any(|t| t.to_lowercase().contains(&q));
    let installed: Vec<Item> = x
        .list
        .iter()
        .map(|e| {
            let m = &e.m;
            let off = x.off.contains(&m.id);
            let meta = if off { ("Disabled".to_string(), Some("warning")) } else { (format!("{} · may {}", e.state(), install::perms(&m.permissions)), Some("dim")) };
            Item { id: m.id.clone(), icon: m.icon.get().to_string(), name: m.name.clone(), version: format!("v{}", m.version), about: m.description.clone(), meta, install: false }
        })
        .filter(|it| hit(it))
        .collect();
    // The code views first (Explorer, Search, Source Control), then the others.
    let builtin: Vec<Item> = DRIVERS
        .iter()
        .filter(|o| o.code)
        .chain(DRIVERS.iter().filter(|o| !o.code))
        .filter(|o| o.id != OPS.id)
        .map(|o| {
            let meta = if x.off.contains(o.id) { ("Disabled".to_string(), Some("warning")) } else { ("Built in".to_string(), Some("dim")) };
            Item { id: o.id.to_string(), icon: (o.icon)().to_string(), name: o.name.into(), version: String::new(), about: o.about.into(), meta, install: false }
        })
        .filter(|it| hit(it))
        .collect();
    // What termcode had built in, one click away while it is not installed.
    let serial = x.list.iter().any(|e| e.m.id == "xcomm" || e.m.api.iter().any(|(m, _)| m == "serial.ports"));
    let recommended: Vec<Item> = (!serial)
        .then(|| Item { id: "xcomm".into(), icon: crate::theme::icons().plus.into(), name: "Serial (xcomm)".into(), version: String::new(), about: "Serial ports and their streams, on xcomm".into(), meta: ("JonasAtelier/termcode".into(), Some("dim")), install: true })
        .into_iter()
        .filter(|it| hit(it))
        .collect();
    let picked = x.picked.clone();
    for (name, items, empty) in [("INSTALLED", installed, "None yet: search above, or termcode ext new"), ("BUILT-IN", builtin, ""), ("RECOMMENDED", recommended, "")] {
        if items.is_empty() && empty.is_empty() {
            continue;
        }
        let folded = x.folded.contains(name);
        let cells = vec![text(name, None), text(items.len().to_string(), None)];
        rows.push(Row { cells, action: Some(format!("fold:{name}")), hint: None, head: Some(!folded), pic: None });
        if folded {
            continue;
        }
        if items.is_empty() {
            rows.push(line(vec![text(format!("  {}", cut(empty, w.saturating_sub(2))), Some("dim"))]));
            rows.push(Row::default());
        }
        for it in &items {
            rows.extend(item_rows(it, w, picked.as_deref() == Some(&it.id)));
        }
    }
    let hover = app.hover;
    let x = &mut app.exts;
    x.view.rows = rows;
    x.panes.push((r, MANAGER, Slot::Left, 0));
    let _ = draw_rows(f, r, &mut x.view, MANAGER, Slot::Left, 0, hover, &mut x.hits, P.sidebar_bg, P.sidebar_fg);
}

/// An extension's page (VS Code's extension editor): what it is, its buttons, what it
/// brings (commands, API methods), how it runs, its README.
fn page(app: &mut App, f: &mut Frame, r: Rect, key: &str) {
    crate::ui::fill(f, r, P.editor_bg);
    let r = crate::ui::inset(r, 1);
    let id = key.strip_prefix(KEY).unwrap_or_default();
    let w = r.width as usize;
    let x = &app.exts;
    let off = x.off.contains(id);
    let toggle = button(if off { "Enable" } else { "Disable" }, format!("toggle:{id}"));
    let field = |k: &str, v: String| line(vec![text(format!("{k:<12}"), Some("dim")), text(v, None)]);
    let heading = |t: &str| line(vec![Cell { text: t.into(), bold: true, ..Cell::default() }]);
    let mut rows = Vec::new();
    if let Some(e) = x.list.iter().find(|e| e.m.id == id) {
        let m = &e.m;
        rows.push(line(vec![text(format!("{}  ", m.icon.get()), Some("accent")), Cell { text: m.name.clone(), bold: true, ..Cell::default() }, text(format!("   v{}", m.version), Some("dim"))]));
        rows.push(line(vec![text(format!("   {}", m.id), Some("dim")), text(if off { "  · Disabled".into() } else { format!("  · {}", e.state()) }, Some(if off { "warning" } else { "dim" }))]));
        for l in textwrap(&m.description, w.saturating_sub(3)) {
            rows.push(line(vec![text(format!("   {l}"), None)]));
        }
        rows.push(Row::default());
        rows.push(line(vec![text("   ", None), toggle, text(" ", None), button("Uninstall", format!("remove:{id}")), text(" ", None), button("Restart", format!("restart:{id}"))]));
        rows.push(Row::default());
        rows.push(line(vec![text("─".repeat(w), Some("dim"))]));
        rows.push(heading("DETAILS"));
        let folder = std::fs::read_link(&m.dir).map_or_else(|_| m.dir.display().to_string(), |t| format!("{} → {}", m.dir.display(), t.display()));
        rows.push(field("Runs", m.run.join(" ")));
        if !m.build.is_empty() {
            rows.push(field("Builds", m.build.join(" ")));
        }
        rows.push(field("May", install::perms(&m.permissions)));
        rows.push(field("Folder", folder));
        rows.push(field("Log", e.log().display().to_string()));
        if !m.commands.is_empty() {
            rows.push(Row::default());
            rows.push(heading("COMMANDS (F1)"));
            for (_, t) in &m.commands {
                rows.push(line(vec![text(format!("  {}: {t}", m.name), None)]));
            }
        }
        if !m.api.is_empty() {
            rows.push(Row::default());
            rows.push(heading("API METHODS"));
            for (name, tier) in &m.api {
                let t = match tier {
                    crate::api::Tier::Read => "read",
                    crate::api::Tier::Edit => "edit",
                    crate::api::Tier::Run => "run",
                };
                rows.push(line(vec![text(format!("  {name:<20}"), None), text(t.to_string(), Some("dim"))]));
            }
        }
        if let Ok(readme) = std::fs::read_to_string(m.dir.join("README.md")) {
            rows.push(Row::default());
            rows.push(line(vec![text("─".repeat(w), Some("dim"))]));
            for l in readme.lines().take(400) {
                let h = l.trim_start_matches('#');
                rows.push(if h.len() < l.len() { heading(h.trim()) } else { line(vec![text(l.to_string(), None)]) });
            }
        }
    } else if let Some(o) = DRIVERS.iter().find(|o| o.id == id) {
        rows.push(line(vec![text(format!("{}  ", (o.icon)()), Some("accent")), Cell { text: o.name.into(), bold: true, ..Cell::default() }]));
        rows.push(line(vec![text("   Built in", Some("dim")), text(if off { "  · Disabled" } else { "" }.to_string(), Some("warning"))]));
        rows.push(line(vec![text(format!("   {}", o.about), None)]));
        rows.push(Row::default());
        // The Explorer stays: something must show the workspace.
        rows.push(line(vec![text("   ", None), if o.id == DRIVERS[super::EXPLORER as usize].id { text("Always on", Some("dim")) } else { toggle }]));
    } else if id == "xcomm" {
        // Recommended, not installed: what it is, and Install.
        rows.push(line(vec![text(format!("{}  ", crate::theme::icons().plus), Some("accent")), Cell { text: "Serial (xcomm)".into(), bold: true, ..Cell::default() }]));
        rows.push(line(vec![text("   JonasAtelier/termcode · not installed", Some("dim"))]));
        rows.push(line(vec![text("   Serial ports and their streams, on xcomm: monitor, attach, line settings, notes.", None)]));
        rows.push(Row::default());
        rows.push(line(vec![text("   ", None), button("Install", "get:xcomm".into())]));
        rows.push(Row::default());
        rows.push(field("From", crate::rpc::XCOMM.to_string()));
    } else {
        rows.push(line(vec![text(format!("{id}: not installed"), Some("dim"))]));
    }
    let hover = app.hover;
    let x = &mut app.exts;
    x.detail.rows = rows;
    x.panes.push((r, MANAGER, Slot::Main, 0));
    let _ = draw_rows(f, r, &mut x.detail, MANAGER, Slot::Main, 0, hover, &mut x.hits, P.editor_bg, P.editor_fg);
}

fn textwrap(t: &str, w: usize) -> Vec<String> {
    wrapped(t, w).into_iter().filter_map(|r| r.cells.into_iter().next().map(|c| c.text)).collect()
}

/// A click in the view, or on an extension's page.
pub(super) fn action(app: &mut App, action: &str) {
    app.exts.typing = action == "type";
    let (verb, id) = action.split_once(':').unwrap_or((action, ""));
    let id = id.to_string();
    match verb {
        "type" => app.focus = Focus::Side,
        "install" => install(app),
        "get" if id == "xcomm" => {
            app.exts.input = crate::rpc::XCOMM.to_string();
            install(app);
        }
        "fold" => {
            if !app.exts.folded.remove(&id) {
                app.exts.folded.insert(id);
            }
        }
        // Its page, in the Extensions view's own editor area.
        "open" => {
            app.exts.picked = Some(id.clone());
            let (side, visible) = (app.side, app.side_visible);
            app.show_tab(app.ag, format!("{KEY}{id}"), true);
            (app.side, app.side_visible, app.focus) = (side, visible, Focus::Side);
        }
        "toggle" if id == DRIVERS[super::EXPLORER as usize].id => {}
        "toggle" => {
            let off = !app.exts.off.contains(&id);
            app.exts.set_off(&id, off);
            // The view shown turned off: the Explorer instead.
            let Side::View(n) = app.side;
            if off && super::id_of(app, n) == id {
                app.show_side(Side::EXPLORER, false);
            }
            if off {
                if let Some(e) = app.exts.list.iter_mut().find(|e| e.m.id == id) {
                    e.stop();
                }
            }
        }
        "restart" => {
            if let Some(i) = app.exts.list.iter().position(|e| e.m.id == id) {
                app.exts.list[i].stop();
                if !app.exts.off.contains(&id) {
                    super::process::start(app, i);
                }
                app.msg = format!("{} restarted", app.exts.list[i].m.name);
            }
        }
        "remove" => {
            let Some(e) = app.exts.list.iter().find(|e| e.m.id == id) else { return };
            app.prompt = Some(Prompt {
                text: vec![format!("Uninstall {} {}?", e.m.name, e.m.version), install::what(&root(), &id)],
                buttons: vec![("Uninstall".into(), PAct::ExtRemove(id.clone())), ("Cancel".into(), PAct::Cancel)],
                sel: 1,
            });
        }
        _ => {}
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
        KeyCode::Enter if is_source(app.exts.input.trim()) => install(app),
        KeyCode::Enter => app.exts.typing = false,
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
    app.msg = format!("Fetching {}…", src.trim_start_matches("https://"));
    let (tx, rx) = std::sync::mpsc::channel();
    let root = root();
    let st = progress(app, format!("Fetching {}", src.trim_start_matches("https://")), "fetching");
    std::thread::spawn(move || {
        let _ = tx.send(install::fetch(&root, &src, &|p| set(&st, "fetching", p)));
    });
    app.exts.job = Some(rx);
    app.exts.typing = false;
}

fn tick(app: &mut App) -> bool {
    // Its question closed without an answer (Esc): the fetched copy goes.
    if app.prompt.is_none() {
        if let Some(f) = app.exts.pending.take() {
            f.discard();
        }
    }
    let done = built(app) | fetched(app) | uninstalled(app);
    // On the status bar while it goes.
    match progress_line(app) {
        Some(l) => {
            app.msg = l;
            true
        }
        None => done,
    }
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
    app.exts.progress = None;
    match got {
        Ok(m) => {
            app.msg = format!("Installed {} · {} 100% · its icon is in the activity bar", m.name, bar(100));
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
    app.exts.progress = None;
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
    // Placed (to 5%), built (to 95%), read (100%).
    let st = progress(app, format!("Installing {name}"), "placing");
    std::thread::spawn(move || {
        let done = install::place(&root, f, false).and_then(|dest| {
            set(&st, "building", 5);
            install::build(&dest, true, &|p| set(&st, "building", 5 + (u32::from(p) * 90 / 100) as u8)).inspect_err(|_| {
                let _ = install::remove(&root, &id, &|_| {});
            })?;
            set(&st, "loading", 98);
            Manifest::read(&dest)
        });
        let _ = tx.send(done);
    });
    app.exts.building = Some((name, rx));
}

/// The user's yes to removing `id`: its program ended, its folder removed in the
/// background (its progress shown), then its tabs closed (`uninstalled`).
pub fn removed(app: &mut App, id: &str) {
    let Some(e) = app.exts.list.iter_mut().find(|e| e.m.id == id) else { return };
    e.stop();
    let name = e.m.name.clone();
    let st = progress(app, format!("Uninstalling {name}"), "removing");
    let (tx, rx) = std::sync::mpsc::channel();
    let (root, ident) = (root(), id.to_string());
    std::thread::spawn(move || {
        let _ = tx.send(install::remove(&root, &ident, &|p| set(&st, "removing", p)));
    });
    app.exts.removing = Some((id.to_string(), rx));
}

/// The folder removed: its tabs closed, it gone from the list.
fn uninstalled(app: &mut App) -> bool {
    let Some((id, rx)) = &app.exts.removing else { return false };
    let got = match rx.try_recv() {
        Err(TryRecvError::Empty) => return false,
        Err(TryRecvError::Disconnected) => Err("stopped".to_string()),
        Ok(r) => r,
    };
    let id = id.clone();
    app.exts.removing = None;
    app.exts.progress = None;
    match got {
        Ok(()) => {
            for g in app.groups.iter_mut() {
                g.tabs.retain(|t| !matches!(super::process::split_key(&t.key), Some((i, _)) if i == id));
                g.active = g.active.min(g.tabs.len().saturating_sub(1));
            }
            let name = app.exts.list.iter().find(|e| e.m.id == id).map_or(id.clone(), |e| e.m.name.clone());
            app.exts.list.retain(|e| e.m.id != id);
            app.exts.set_off(&id, false);
            app.msg = format!("Uninstalled {name} · {} 100%", bar(100));
        }
        Err(e) => app.msg = format!("Not uninstalled: {e}"),
    }
    true
}

fn progress(app: &mut App, title: String, stage: &'static str) -> Shared {
    let st: Shared = std::sync::Arc::new(std::sync::Mutex::new((stage, 0)));
    app.exts.progress = Some((title, st.clone()));
    st
}

fn set(st: &Shared, stage: &'static str, p: u8) {
    if let Ok(mut s) = st.lock() {
        *s = (stage, p.min(100));
    }
}

/// Ten cells, filled as far as `p` percent.
fn bar(p: u8) -> String {
    crate::ui::kit::progress_bar(p, 10)
}

/// The step going on, as the status bar and the view show it.
fn progress_line(app: &App) -> Option<String> {
    let (title, st) = app.exts.progress.as_ref()?;
    let (stage, p) = *st.lock().ok()?;
    Some(format!("{title} · {stage} {} {p}%", bar(p)))
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
    fn every_view_is_a_driver_a_code_view_turns_off_and_an_extension_shows_a_badge() {
        let d = std::env::temp_dir().join(format!("termcode-ext-views-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.clone(), None);
        frame(&mut app);
        let icons = |app: &App| app.ui.activity.iter().map(|(_, s)| *s).collect::<Vec<_>>();
        assert_eq!(icons(&app)[..3], [Side::EXPLORER, Side::SEARCH, Side::SCM], "the code views on top, from their drivers");
        // Source Control off: its icon gone, the Explorer shown instead, F2 refused.
        app.show_side(Side::SCM, false);
        action(&mut app, "toggle:scm");
        frame(&mut app);
        assert!(!icons(&app).contains(&Side::SCM) && app.side == Side::EXPLORER);
        app.show_side(Side::SCM, false);
        assert!(app.side == Side::EXPLORER && app.msg.contains("turned off"), "{}", app.msg);
        // The Explorer stays, whatever asks.
        action(&mut app, "toggle:explorer");
        frame(&mut app);
        assert!(icons(&app).contains(&Side::EXPLORER));
        action(&mut app, "toggle:scm");
        frame(&mut app);
        assert!(icons(&app).contains(&Side::SCM));
        // An extension's count on its icon, as Source Control's.
        app.exts.list.push(crate::ext::test_ext("badged", &["t"]));
        let n = crate::ext::num("badged");
        let c = crate::api::Client { ext: Some("badged".into()), ..crate::api::Client::default() };
        assert!(crate::ext::call(&mut app, &c, "view.badge", &serde_json::json!({"count": "x"})).is_err());
        crate::ext::call(&mut app, &c, "view.badge", &serde_json::json!({"count": 7})).unwrap();
        assert_eq!(crate::ext::badge(&app, n), 7);
        let mut t = ratatui::Terminal::new(TestBackend::new(140, 50)).unwrap();
        let buf = t.draw(|f| crate::ui::draw(&mut app, f)).unwrap().buffer.clone();
        let q = app.ui.activity.iter().find(|(_, s)| *s == Side::View(n)).expect("its icon").0;
        let seen: String = (q.y.saturating_sub(1)..q.bottom()).flat_map(|y| (q.x..q.right()).map(move |x| (x, y))).map(|p| buf[p].symbol().to_string()).collect();
        assert!(seen.contains('7'), "the badge drawn: {seen:?}");
        let _ = std::fs::remove_dir_all(&d);
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
            tick(&mut app);
            if app.prompt.is_some() {
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
        // An extension's page: a press on its text (not a button) is nothing to select.
        action(&mut app, "open:mgr-demo");
        frame(&mut app);
        let page = app.exts.panes.iter().find(|(_, i, slot, _)| *i == MANAGER && *slot == Slot::Main).map(|p| p.0).expect("its page");
        let press = ratatui::crossterm::event::MouseEvent { kind: ratatui::crossterm::event::MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Left), column: page.x + 1, row: page.bottom() - 1, modifiers: KeyModifiers::NONE };
        app.on_mouse(press);
        // A built-in view turned off: its icon gone; on again: back.
        action(&mut app, "toggle:programmer");
        frame(&mut app);
        assert!(!app.ui.activity.iter().any(|(_, s)| *s == Side::PROGRAMMER));
        action(&mut app, "toggle:programmer");
        frame(&mut app);
        assert!(app.ui.activity.iter().any(|(_, s)| *s == Side::PROGRAMMER));
        // Removed after its own yes.
        action(&mut app, "remove:mgr-demo");
        assert!(app.prompt.take().is_some_and(|p| p.text[0] == "Uninstall Demo 2?"));
        removed(&mut app, "mgr-demo");
        // In the background, its progress on the status bar, then gone.
        for _ in 0..500 {
            if uninstalled(&mut app) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.exts.list.is_empty() && !d.join("root/mgr-demo").exists());
        assert!(app.msg.starts_with("Uninstalled Demo · ██████████ 100%"), "{}", app.msg);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
