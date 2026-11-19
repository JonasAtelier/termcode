//! The view subsystem, as Linux's DSA: termcode owns the frame (the activity bar, the side
//! bar, the editor area's tabs, the right and bottom panels, mouse, keys, layout); each view
//! is a driver filling it through one table of ops, `Ops` (DSA's `dsa_switch_ops`).
//!
//! In-tree drivers are Rust, built in: the Programmer (programmer.rs), GitHub (github/),
//! and the Extensions view (manager.rs). Out-of-tree drivers are
//! programs (process.rs), installed with `termcode ext` (install.rs) or the Extensions
//! view, filling slots with rows over JSON-RPC; one table of ops serves them all.
//! A view is numbered (`Side::View`, `Space::View`): in-tree first, then extensions.
//! Guides: doc/extensions.md (people), skills/termcode-extension/SKILL.md (agents).

mod install;
mod manager;
mod process;

pub use install::cli;
pub use manager::{decided, removed};
#[cfg(test)]
pub use process::test_ext;
pub use process::{api_methods, api_owner, bottom_here, call, command_titles, draw_panel, forward, may_emit, right_here, run_command, shutdown, Exts, Slot};

use crate::panel::{App, Side, Space};
use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::{Position, Rect};
use ratatui::Frame;
use std::sync::Mutex;

/// The in-tree drivers' numbers (their place in DRIVERS). (Serial is an extension:
/// extensions/xcomm.)
pub const PROGRAMMER: u16 = 0;
pub const GITHUB: u16 = 1;
pub const EXTENSIONS: u16 = 2;

/// The in-tree drivers, by number.
pub static DRIVERS: [&Ops; 3] = [&crate::programmer::OPS, &crate::github::OPS, &manager::OPS];

/// A view's number: the in-tree ones fixed, extensions' given as their ids are first met.
pub fn num(id: &str) -> u16 {
    static IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut ids = IDS.lock().unwrap_or_else(|e| e.into_inner());
    if ids.is_empty() {
        ids.extend(DRIVERS.iter().map(|o| o.id.to_string()));
    }
    let n = ids.iter().position(|i| i == id).unwrap_or_else(|| {
        ids.push(id.to_string());
        ids.len() - 1
    });
    n as u16
}

/// How a driver drew its tab: all of it, a page (its text area, preview rows), or only its
/// frame (the rest: the tab's document, drawn as an editor).
pub enum Drawn {
    Whole,
    Page(Rect, usize),
    Rest(Rect),
}

/// A driver's ops: what the subsystem calls. Each has a default doing nothing (`Ops::NONE`).
pub struct Ops {
    /// Its id (settings, the session), its name (the Extensions view), its side bar title,
    /// one line on it, and the hint in its editor area while no tab is open.
    pub id: &'static str,
    pub name: &'static str,
    pub title: &'static str,
    pub about: &'static str,
    pub hint: &'static str,
    /// Its activity bar icon (the theme's).
    pub icon: fn() -> &'static str,
    /// The side bar, while its view shows; its view just shown.
    pub side: fn(&mut App, &mut Frame, Rect),
    pub shown: fn(&mut App),
    /// The tab keys that are its (they open in its space); drawing one (group, its body,
    /// the key, focused); its last copy closed.
    pub owns: fn(&str) -> bool,
    pub tab: fn(&mut App, &mut Frame, usize, Rect, &str, bool) -> Drawn,
    /// A tab's title (None: termcode's, from its key).
    pub tab_title: fn(&App, &str) -> Option<String>,
    pub closed: fn(&mut App, &str),
    /// A tab of its showing its own right side (no Outline beside it).
    pub page: fn(&str) -> bool,
    /// The right panel in the Outline's place (true: drawn).
    pub right: fn(&mut App, &mut Frame, Rect) -> bool,
    /// The terminal panel shows in its space.
    pub terminal: bool,
    /// Pointer and keys: true (or Some) when it took them. `mouse` sees every event first;
    /// `key` the side bar's; `tab_key` its tab's; `takes` any, before termcode's own.
    pub click: fn(&mut App, Position, bool) -> bool,
    pub wheel: fn(&mut App, Position, isize) -> bool,
    pub mouse: fn(&mut App, MouseEvent) -> Option<bool>,
    pub drag_to: fn(&mut App, u16),
    pub key: fn(&mut App, KeyEvent) -> bool,
    pub tab_key: fn(&mut App, KeyEvent) -> bool,
    pub takes: fn(&mut App, KeyEvent) -> bool,
    pub paste: fn(&mut App, &str) -> bool,
    /// Each loop: its work done (true: redraw); the files the loop waits on for it.
    pub tick: fn(&mut App) -> bool,
    pub fds: fn() -> Vec<i32>,
    /// Drawn over everything (a peek card).
    pub overlay: fn(&mut App, &mut Frame),
}

fn no_draw(_: &mut App, _: &mut Frame, _: Rect) {}
fn no_app(_: &mut App) {}
fn no_str(_: &str) -> bool {
    false
}
fn no_tab(_: &mut App, _: &mut Frame, _: usize, r: Rect, _: &str, _: bool) -> Drawn {
    Drawn::Rest(r)
}
fn no_closed(_: &mut App, _: &str) {}
fn no_title(_: &App, _: &str) -> Option<String> {
    None
}
fn no_right(_: &mut App, _: &mut Frame, _: Rect) -> bool {
    false
}
fn no_click(_: &mut App, _: Position, _: bool) -> bool {
    false
}
fn no_wheel(_: &mut App, _: Position, _: isize) -> bool {
    false
}
fn no_mouse(_: &mut App, _: MouseEvent) -> Option<bool> {
    None
}
fn no_drag(_: &mut App, _: u16) {}
fn no_key(_: &mut App, _: KeyEvent) -> bool {
    false
}
fn no_paste(_: &mut App, _: &str) -> bool {
    false
}
fn no_tick(_: &mut App) -> bool {
    false
}
fn no_fds() -> Vec<i32> {
    Vec::new()
}
fn no_overlay(_: &mut App, _: &mut Frame) {}
fn no_icon() -> &'static str {
    ""
}

impl Ops {
    pub const NONE: Ops = Ops {
        id: "",
        name: "",
        title: "",
        about: "",
        hint: "",
        icon: no_icon,
        side: no_draw,
        shown: no_app,
        owns: no_str,
        tab: no_tab,
        tab_title: no_title,
        closed: no_closed,
        page: no_str,
        right: no_right,
        terminal: false,
        click: no_click,
        wheel: no_wheel,
        mouse: no_mouse,
        drag_to: no_drag,
        key: no_key,
        tab_key: no_key,
        takes: no_key,
        paste: no_paste,
        tick: no_tick,
        fds: no_fds,
        overlay: no_overlay,
    };
}

/// View `n`'s ops: an in-tree driver's, else the extensions' host's.
pub fn ops(n: u16) -> &'static Ops {
    DRIVERS.get(n as usize).copied().unwrap_or(&process::OPS)
}

fn all() -> impl Iterator<Item = &'static Ops> {
    DRIVERS.iter().copied().chain(std::iter::once(&process::OPS))
}

/// The view a tab key belongs to.
pub fn owner(key: &str) -> Option<u16> {
    DRIVERS.iter().position(|o| (o.owns)(key)).map(|i| i as u16).or_else(|| process::split_key(key).map(|(id, _)| num(id)))
}

/// A view's side bar title.
pub fn title(app: &App, n: u16) -> String {
    match DRIVERS.get(n as usize) {
        Some(o) => o.title.to_string(),
        None => process::title(app, n),
    }
}

/// The activity bar's views below the code ones, upward from Settings: the Extensions
/// view (+), the installed extensions right above it, then GitHub and the Programmer.
/// Each: number, icon (the icon set's).
pub fn bottom_items(app: &App) -> Vec<(u16, String)> {
    let on = |id: &str| !app.exts.off.contains(id);
    let builtin = |n: u16| (n, (DRIVERS[n as usize].icon)().to_string());
    let installed = app.exts.list.iter().filter(|e| on(&e.m.id)).map(|e| (e.n, e.m.icon.get().to_string()));
    std::iter::once(builtin(EXTENSIONS)).chain(installed).chain([GITHUB, PROGRAMMER].into_iter().filter(|&n| on(DRIVERS[n as usize].id)).map(builtin)).collect()
}

/// The editor area's hint while view `space` has no tab.
pub fn hint(space: Space) -> &'static str {
    match space {
        Space::View(n) => ops(n).hint,
        Space::Code => "",
    }
}

/// The active tab draws its own right side.
pub fn page(app: &App) -> bool {
    app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).is_some_and(|t| page_key(&t.key))
}

pub fn page_key(key: &str) -> bool {
    DRIVERS.iter().any(|o| (o.page)(key))
}

/// The terminal panel in the view shown.
pub fn terminal_here(app: &App) -> bool {
    match app.space() {
        Space::Code => true,
        Space::View(n) => ops(n).terminal,
    }
}

pub fn shown(app: &mut App, s: Side) {
    if let Side::View(n) = s {
        (ops(n).shown)(app);
    }
}

pub fn side(app: &mut App, f: &mut Frame, r: Rect, n: u16) {
    (ops(n).side)(app, f, r);
}

pub fn tab(app: &mut App, f: &mut Frame, g: usize, r: Rect, key: &str, focused: bool) -> Option<Drawn> {
    owner(key).map(|n| (ops(n).tab)(app, f, g, r, key, focused))
}

pub fn tab_title(app: &App, key: &str) -> Option<String> {
    owner(key).and_then(|n| (ops(n).tab_title)(app, key))
}

pub fn closed(app: &mut App, key: &str) {
    if let Some(n) = owner(key) {
        (ops(n).closed)(app, key);
    }
}

pub fn right(app: &mut App, f: &mut Frame, r: Rect) -> bool {
    all().any(|o| (o.right)(app, f, r))
}

pub fn click(app: &mut App, p: Position, double: bool) -> bool {
    all().any(|o| (o.click)(app, p, double))
}

pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    all().any(|o| (o.wheel)(app, p, d))
}

pub fn mouse(app: &mut App, m: MouseEvent) -> Option<bool> {
    all().find_map(|o| (o.mouse)(app, m))
}

pub fn drag_to(app: &mut App, n: u16, y: u16) {
    (ops(n).drag_to)(app, y);
}

/// A key in view `n`'s side bar.
pub fn key(app: &mut App, n: u16, k: KeyEvent) -> bool {
    (ops(n).key)(app, k)
}

pub fn tab_key(app: &mut App, k: KeyEvent) -> bool {
    all().any(|o| (o.tab_key)(app, k))
}

pub fn takes(app: &mut App, k: KeyEvent) -> bool {
    all().any(|o| (o.takes)(app, k))
}

pub fn paste(app: &mut App, text: &str) -> bool {
    all().any(|o| (o.paste)(app, text))
}

pub fn tick(app: &mut App) -> bool {
    all().fold(false, |changed, o| (o.tick)(app) | changed)
}

pub fn fds() -> Vec<i32> {
    all().flat_map(|o| (o.fds)()).collect()
}

pub fn overlay(app: &mut App, f: &mut Frame) {
    for o in all() {
        (o.overlay)(app, f);
    }
}
