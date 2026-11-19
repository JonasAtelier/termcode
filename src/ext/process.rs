//! Out-of-tree drivers: extensions, programs termcode starts, each filling the slots of
//! its own view with rows:
//!
//!   slot     where                                      `view.set` / `view.append`
//!   left     the side bar, in sections split by lines   `section` names one
//!   main     tabs in the editor area (its own space), `tab` names one
//!   right    the panel right of the editors (the Outline's place)
//!   bottom   the panel under the editors (the terminal's place)
//!
//! Each talks JSON-RPC on its stdin and stdout, a message a line: termcode's whole API
//! (doc/api.md) as a client granted what its manifest asked at install, `view.*` for its
//! panes; termcode tells it `view.shown`, `view.action` (a click on a row or a button),
//! `view.key`, `view.paste`, and of its tabs `view.tab` (the one in front), `view.resize`
//! and `view.closed`. Started when its view is first shown; it ends when its stdin closes.
//! One table of ops (`OPS`) serves them all.

use crate::api::{Client, Tier};
use crate::panel::{App, Focus, Side, Space};
use crate::theme::P;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use super::{num, Drawn, Ops};

/// An extension's manifest, in its folder's root.
pub const MANIFEST: &str = "termcode-extension.json";
/// A main tab's key: `ext:<id>`.
pub const PREFIX: &str = "ext:";
/// Rows a pane keeps (`view.append` drops the oldest past it), and a row's text.
const MAX_ROWS: usize = 10_000;
const MAX_TEXT: usize = 2_000;
/// The click that restarts an extension that ended (its side bar's button).
const RESTART: &str = "\u{0}restart";

/// What an extension is: `termcode-extension.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub icon: Icon,
    /// The command: a program on PATH or a path inside the extension's folder, then its arguments.
    pub run: Vec<String>,
    /// Asked at install: changing files and showing things (`edit`), running (`run`).
    pub permissions: Vec<Tier>,
    /// Its commands in F1 (id, title), there before it starts; and what builds it at install.
    pub commands: Vec<(String, String)>,
    pub build: Vec<String>,
    /// API methods it serves for other clients (`serial.ports`), each with what calling
    /// it may do (asked of the caller as termcode's own are).
    pub api: Vec<(String, Tier)>,
    pub dir: PathBuf,
}

impl Manifest {
    pub fn read(dir: &Path) -> Result<Manifest, String> {
        let path = dir.join(MANIFEST);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let s = |k: &str| v[k].as_str().unwrap_or_default().trim().to_string();
        let id = s("id");
        let good = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if id.is_empty() || id.len() > 40 || id.starts_with('-') || !id.chars().all(good) {
            return Err(format!("{MANIFEST}: \"id\" is 1 to 40 of a-z, 0-9 and -, got {id:?}"));
        }
        let run: Vec<String> = v["run"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        if run.first().is_none_or(|r| r.is_empty()) {
            return Err(format!("{MANIFEST}: \"run\" is its command, e.g. [\"python3\", \"main.py\"]"));
        }
        let mut permissions = Vec::new();
        for p in v["permissions"].as_array().into_iter().flatten() {
            permissions.push(match p.as_str() {
                Some("edit") => Tier::Edit,
                Some("run") => Tier::Run,
                _ => return Err(format!("{MANIFEST}: permission {p}: \"edit\" or \"run\"")),
            });
        }
        let name = Some(s("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
        let mut commands = Vec::new();
        for c in v["commands"].as_array().into_iter().flatten().take(50) {
            match (c["id"].as_str(), c["title"].as_str()) {
                (Some(i), Some(t)) if !i.is_empty() && !t.trim().is_empty() => commands.push((i.to_string(), clean(t.trim()))),
                _ => return Err(format!("{MANIFEST}: a command is {{\"id\": …, \"title\": …}}, got {c}")),
            }
        }
        let build: Vec<String> = v["build"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        let mut api = Vec::new();
        let name_ok = |n: &str| !n.is_empty() && n.len() <= 32 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        for (ns, methods) in v["api"].as_object().into_iter().flatten() {
            if !name_ok(ns) || CORE.contains(&ns.as_str()) {
                return Err(format!("{MANIFEST}: api namespace {ns:?}: a-z, 0-9, _, not one of termcode's ({})", CORE.join(", ")));
            }
            for (m, t) in methods.as_object().into_iter().flatten() {
                let tier = match t.as_str() {
                    Some("read") => Tier::Read,
                    Some("edit") => Tier::Edit,
                    Some("run") => Tier::Run,
                    _ => return Err(format!("{MANIFEST}: api {ns}.{m}: \"read\", \"edit\" or \"run\"")),
                };
                if !name_ok(m) {
                    return Err(format!("{MANIFEST}: api method {m:?}: a-z, 0-9, _"));
                }
                api.push((format!("{ns}.{m}"), tier));
            }
        }
        let icon = Icon::read(&v["icon"], &name);
        Ok(Manifest { version: s("version"), description: s("description"), icon, name, id, run, permissions, commands, build, api, dir: dir.to_path_buf() })
    }
}

/// An extension's activity bar icon in each of termcode's icon sets (`termcode.icons`):
/// a Nerd Font glyph, a Unicode symbol, an ASCII letter.
#[derive(Clone, Debug, PartialEq)]
pub struct Icon {
    nerd: String,
    unicode: String,
    ascii: String,
}

impl Icon {
    /// `"⇄"` for every set (ASCII: its name's first letter unless it is ASCII), or
    /// `{"nerd", "unicode", "ascii"}`, each falling back to the plainer one.
    fn read(v: &Value, name: &str) -> Icon {
        let one = |k: &str| v[k].as_str().or(v.as_str()).and_then(|s| s.chars().next()).map(String::from);
        let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        let ascii = one("ascii").filter(|c| c.is_ascii() && !c.trim().is_empty()).unwrap_or(letter);
        let unicode = one("unicode").filter(|c| !is_private(c)).unwrap_or_else(|| ascii.clone());
        let nerd = one("nerd").unwrap_or_else(|| unicode.clone());
        Icon { nerd, unicode, ascii }
    }

    #[cfg(test)]
    pub fn plain(c: &str) -> Icon {
        Icon { nerd: c.into(), unicode: c.into(), ascii: c.into() }
    }

    /// The one for the icon set in use.
    pub fn get(&self) -> &str {
        match (crate::theme::nerd(), crate::theme::ascii()) {
            (true, _) => &self.nerd,
            (_, true) => &self.ascii,
            _ => &self.unicode,
        }
    }
}

/// A Nerd Font glyph (Unicode's private use area): drawn only with the Nerd Font set.
fn is_private(c: &str) -> bool {
    c.chars().any(|c| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0x10FFFD))
}

/// termcode's own API namespaces: no extension serves methods in them.
const CORE: &[&str] = &["hello", "command", "editor", "state", "buffer", "terminal", "events", "ui", "view", "api"];

/// Where extensions are installed ($TERMCODE_EXTENSIONS, else termcode's data folder).
pub fn root() -> PathBuf {
    std::env::var_os("TERMCODE_EXTENSIONS").map(PathBuf::from).unwrap_or_else(|| crate::data_dir(false).join("extensions"))
}

/// The installed extensions, by id; and the folders that are not one, why.
pub fn installed(root: &Path) -> (Vec<Manifest>, Vec<String>) {
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        if d.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        match Manifest::read(&d) {
            Ok(m) => ok.push(m),
            Err(e) => bad.push(format!("{}: {e}", d.display())),
        }
    }
    (ok, bad)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Left,
    Main,
    Right,
    Bottom,
}

impl Slot {
    pub(super) fn parse(s: &str) -> Result<Slot, String> {
        match s {
            "left" => Ok(Slot::Left),
            "main" => Ok(Slot::Main),
            "right" => Ok(Slot::Right),
            "bottom" => Ok(Slot::Bottom),
            _ => Err(format!("slot {s:?}: left, main, right or bottom")),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Slot::Left => "left",
            Slot::Main => "main",
            Slot::Right => "right",
            Slot::Bottom => "bottom",
        }
    }
}

/// A piece of a row: its text, colors (theme names or #rrggbb), and what a click on it does.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Cell {
    pub text: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub button: bool,
    pub action: Option<String>,
    /// Shown in a card while the pointer is on it.
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Row {
    pub cells: Vec<Cell>,
    /// The whole row clicks (a list's item).
    pub action: Option<String>,
    /// The whole row's card on hover.
    pub hint: Option<String>,
    /// A section header (open, or folded), drawn as the side bar's: its first cell the
    /// title, its second a dim count.
    pub head: Option<bool>,
}

/// What a slot shows: a title, rows, where it is scrolled to.
#[derive(Debug, Default)]
pub struct Pane {
    id: String,
    title: String,
    pub(super) rows: Vec<Row>,
    scroll: usize,
    /// At the bottom: appended rows keep it there (a log).
    follow: bool,
    /// A side bar section's height, dragged.
    height: Option<u16>,
    /// A tab's size as last drawn (told to the extension when it changes).
    size: (u16, u16),
    /// The extension scrolls it (`scroll: "self"`, a terminal's scrollback): the wheel is told.
    own_scroll: bool,
    /// A side bar section folded to its header.
    folded: bool,
}

/// One extension: what it is, its process, its panes.
pub struct Ext {
    pub m: Manifest,
    pub n: u16,
    out: Option<Sender<String>>,
    done: Arc<AtomicBool>,
    pub(super) started: bool,
    /// Why it is not running (it failed to start, or ended).
    error: Option<String>,
    left: Vec<Pane>,
    /// Its tabs, by name (`tab`; "main" unless named).
    main: Vec<Pane>,
    right: Option<Pane>,
    bottom: Option<Pane>,
    /// The last `view.shown` sent; the tab last said to be in front.
    shown: bool,
    front: Option<String>,
}

impl Ext {
    pub fn new(m: Manifest) -> Ext {
        Ext { n: num(&m.id), m, out: None, done: Arc::new(AtomicBool::new(false)), started: false, error: None, left: Vec::new(), main: Vec::new(), right: None, bottom: None, shown: false, front: None }
    }

    /// Right or bottom (the side bar has sections, the editor area tabs).
    fn pane(&mut self, slot: Slot) -> &mut Option<Pane> {
        if slot == Slot::Right { &mut self.right } else { &mut self.bottom }
    }

    /// Turned off: its program told to end (its stdin closed), its panes gone.
    pub(super) fn stop(&mut self) {
        self.close();
        self.started = false;
        (self.left, self.main, self.right, self.bottom) = (Vec::new(), Vec::new(), None, None);
    }

    /// Its stdin closed: it is to end.
    fn close(&mut self) -> bool {
        self.out.take().is_some_and(|out| out.send(crate::api::CLOSE.into()).is_ok())
    }

    fn notify(&self, method: &str, params: Value) {
        if let Some(out) = &self.out {
            let _ = out.send(json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string());
        }
    }

    /// Running, not started, or ended (its log says why).
    pub(super) fn state(&self) -> &'static str {
        match (&self.out, self.started) {
            (Some(_), _) => "Running",
            (None, false) => "Not started",
            (None, true) => "Ended",
        }
    }

    pub(super) fn log(&self) -> PathBuf {
        // (Tests keep theirs out of the user's state folder.)
        let dir = if cfg!(test) { std::env::temp_dir().join(format!("termcode-ext-logs-{}", std::process::id())) } else { crate::data_dir(true).join("extensions") };
        dir.join(format!("{}.log", self.m.id))
    }
}

/// Where the panes were drawn this frame: clicks, wheels and drags find them there.
#[derive(Default)]
pub struct Exts {
    pub list: Vec<Ext>,
    /// Views turned off in the Extensions view, by id (kept in extensions.json).
    pub off: std::collections::HashSet<String>,
    /// The Extensions view: what is typed to install, typing, a fetch running, one fetched
    /// waiting for the user's yes.
    pub(super) input: String,
    pub(super) typing: bool,
    pub(super) job: Option<std::sync::mpsc::Receiver<Result<super::install::Fetched, String>>>,
    pub(super) pending: Option<super::install::Fetched>,
    /// One placed and building (its build step), in the background; one being removed;
    /// the step going on (what, its stage and how far, shared with its thread).
    pub(super) building: Option<(String, std::sync::mpsc::Receiver<Result<Manifest, String>>)>,
    pub(super) removing: Option<(String, std::sync::mpsc::Receiver<Result<(), String>>)>,
    pub(super) progress: Option<(String, Shared)>,
    pub(super) hits: Vec<Hit>,
    pub(super) panes: Vec<(Rect, usize, Slot, usize)>,
    /// Text selected in an extension's tab; the card of the row or cell under the pointer.
    sel: Option<Sel>,
    hint: Option<(Rect, String)>,
    /// The Extensions view's rows (built each frame), its scroll; the sections folded; the
    /// extension whose page shows, and that page.
    pub(super) view: Pane,
    pub(super) folded: std::collections::HashSet<String>,
    pub(super) picked: Option<String>,
    pub(super) detail: Pane,
    sashes: Vec<(Rect, usize, usize)>,
    /// Side bar sections' chevrons (fold, unfold): extension, section.
    folds: Vec<(Rect, usize, usize)>,
    /// A side bar line dragged: the extension, the section above it, that section's top.
    drag: Option<(usize, usize, u16)>,
}

pub(super) struct Hit {
    pub r: Rect,
    /// The extension (MANAGER: the Extensions view's own).
    pub ext: usize,
    pub slot: Slot,
    pub sec: usize,
    pub action: String,
}

/// A selection in tab `tab` (its index) of extension `ext`: (row of its rows, column)
/// where the drag started, where it is.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Sel {
    ext: usize,
    tab: usize,
    a: (usize, u16),
    b: (usize, u16),
    dragging: bool,
}

/// A step's progress, shared with the thread doing it: its stage, how far (0 to 100).
pub(super) type Shared = Arc<std::sync::Mutex<(&'static str, u8)>>;

/// A click place of the Extensions view's.
pub(super) const MANAGER: usize = usize::MAX;

impl Exts {
    /// The installed ones (none in tests: they never run the user's programs).
    pub fn load() -> Exts {
        if cfg!(test) {
            return Exts::default();
        }
        let list = installed(&root()).0.into_iter().map(Ext::new).collect();
        let off = std::fs::read(off_file()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let off = off.as_ref().and_then(|v| v["off"].as_array()).into_iter().flatten().filter_map(Value::as_str).map(String::from).collect();
        Exts { list, off, ..Exts::default() }
    }

    /// An install fetching or building in the background: the loop looks for its end.
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.building.is_some() || self.removing.is_some()
    }

    /// `id` on or off, kept.
    pub(super) fn set_off(&mut self, id: &str, off: bool) {
        if off {
            self.off.insert(id.to_string());
        } else {
            self.off.remove(id);
        }
        if !cfg!(test) {
            let mut ids: Vec<&String> = self.off.iter().collect();
            ids.sort();
            let _ = std::fs::write(off_file(), json!({"off": ids}).to_string());
        }
    }

    pub fn by_num(&self, n: u16) -> Option<usize> {
        self.list.iter().position(|e| e.n == n)
    }

    /// A new frame: last frame's places go.
    pub fn begin(&mut self) {
        self.hint = None;
        self.hits.clear();
        self.panes.clear();
        self.sashes.clear();
        self.folds.clear();
    }

}

fn off_file() -> PathBuf {
    crate::data_dir(false).join("extensions.json")
}

/// A tab's key: `ext:<id>` for its "main" tab, `ext:<id>/<tab>` for another.
pub(super) fn tab_key(id: &str, tab: &str) -> String {
    if tab == "main" { format!("{PREFIX}{id}") } else { format!("{PREFIX}{id}/{tab}") }
}

/// A tab key's extension and tab.
pub(super) fn split_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix(PREFIX)?;
    Some(rest.split_once('/').unwrap_or((rest, "main")))
}

/// The extension serving API method `method` (turned on), and what calling it may do.
pub fn api_owner(app: &App, method: &str) -> Option<(usize, Tier)> {
    app.exts.list.iter().enumerate().filter(|(_, e)| !app.exts.off.contains(&e.m.id)).find_map(|(i, e)| e.m.api.iter().find(|(m, _)| m == method).map(|(_, t)| (i, *t)))
}

/// The API methods extensions serve.
pub fn api_methods(app: &App) -> Vec<String> {
    app.exts.list.iter().filter(|e| !app.exts.off.contains(&e.m.id)).flat_map(|e| e.m.api.iter().map(|(m, _)| m.clone())).collect()
}

/// API call `method` passed to extension `i` (started if it is not) as `api.call`; the
/// key its reply comes back by.
pub fn forward(app: &mut App, i: usize, method: &str, params: &Value, caller: &str) -> Result<String, String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if !app.exts.list[i].started {
        start(app, i);
    }
    let e = &app.exts.list[i];
    let out = e.out.as_ref().ok_or_else(|| format!("{method}: the {} extension is not running", e.m.name))?;
    let key = format!("t{}", N.fetch_add(1, Ordering::SeqCst));
    let msg = json!({"jsonrpc": "2.0", "id": key, "method": "api.call", "params": {"method": method, "params": params, "caller": caller}});
    out.send(msg.to_string()).map_err(|_| format!("{method}: the {} extension ended", e.m.name))?;
    Ok(key)
}

/// May extension `client` publish `topic` (in a namespace it serves)?
pub fn may_emit(app: &App, client: &Client, topic: &str) -> bool {
    let Some(e) = client.ext.as_ref().and_then(|id| app.exts.list.iter().find(|e| &e.m.id == id)) else { return false };
    let ns = topic.split('.').next().unwrap_or("");
    e.m.api.iter().any(|(m, _)| m.split('.').next() == Some(ns))
}

/// The extensions' commands as F1 lists them: "Name: Title".
pub fn command_titles(app: &App) -> Vec<String> {
    app.exts.list.iter().filter(|e| !app.exts.off.contains(&e.m.id)).flat_map(|e| e.m.commands.iter().map(move |(_, t)| format!("{}: {t}", e.m.name))).collect()
}

/// F1 ran `title`: its extension started if it was not, then told (`view.command`).
pub fn run_command(app: &mut App, title: &str) -> bool {
    let found = app.exts.list.iter().enumerate().find_map(|(i, e)| e.m.commands.iter().find(|(_, t)| format!("{}: {t}", e.m.name) == title).map(|(id, _)| (i, id.clone())));
    let Some((i, id)) = found else { return false };
    if !app.exts.list[i].started {
        start(app, i);
    }
    app.exts.list[i].notify("view.command", json!({"id": id}));
    true
}

/// Its view was shown: started the first time; its main tab opened where it has one.
pub fn shown(app: &mut App, n: u16) {
    let Some(i) = app.exts.by_num(n) else { return };
    if !app.exts.list[i].started {
        start(app, i);
    }
    open_main(app, i, false);
}

pub(super) fn start(app: &mut App, i: usize) {
    let e = &mut app.exts.list[i];
    e.started = true;
    e.error = None;
    e.done = Arc::new(AtomicBool::new(false));
    let m = e.m.clone();
    let prog = if m.run[0].contains('/') { m.dir.join(&m.run[0]).display().to_string() } else { m.run[0].clone() };
    let mut cmd = std::process::Command::new(prog);
    // In its own folder (`python3 main.py` finds main.py), told the workspace.
    cmd.args(&m.run[1..]).current_dir(&m.dir).env("TERMCODE_EXTENSION", &m.id).env("TERMCODE_EXTENSION_DIR", &m.dir).env("TERMCODE_WORKSPACE", &app.root).env_remove("TERMCODE_API_ALLOW");
    // What it prints on stderr: its log, for its author.
    let log = e.log();
    if let Some(d) = log.parent() {
        let _ = crate::private_dirs(&[d.to_path_buf()]);
    }
    cmd.stderr(std::fs::File::create(&log).map(std::process::Stdio::from).unwrap_or_else(|_| std::process::Stdio::null()));
    let client = Client { label: format!("{} extension", m.name), ext: Some(m.id.clone()), granted: m.permissions.clone(), ..Client::default() };
    let done = e.done.clone();
    match crate::api::host(app, cmd, client, done) {
        Ok((out, _)) => app.exts.list[i].out = Some(out),
        Err(err) => app.exts.list[i].error = Some(format!("{} did not start: {err}", m.run[0])),
    }
}

/// Tab `tab` (`focus`: brought to the front, its view shown), or else its "main" one when
/// its space shows with no tab yet.
fn open_main(app: &mut App, i: usize, focus: bool) {
    open_tab(app, i, "main", focus);
}

fn open_tab(app: &mut App, i: usize, tab: &str, focus: bool) {
    let e = &app.exts.list[i];
    if !e.main.iter().any(|p| p.id == tab) || (!focus && app.space() != Space::View(e.n)) {
        return;
    }
    let key = tab_key(&e.m.id, tab);
    let there = app.groups.iter().any(|g| g.space == Space::View(e.n) && g.tabs.iter().any(|t| t.key == key));
    if focus || !there {
        let (side, visible, foc) = (app.side, app.side_visible, app.focus);
        app.show_tab(app.ag, key, false);
        if focus {
            app.focus = Focus::Editor;
        } else {
            (app.side, app.side_visible, app.focus) = (side, visible, foc);
        }
    }
}

/// termcode quitting or reloading: each extension's stdin closed (it ends), and a moment
/// given for them to go, so none is left behind unreaped (a reload replaces this
/// process, threads and all).
pub fn shutdown(app: &mut App) {
    let running: Vec<Arc<AtomicBool>> = app.exts.list.iter_mut().filter_map(|e| e.close().then(|| e.done.clone())).collect();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while running.iter().any(|d| !d.load(Ordering::SeqCst)) && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Ended ones noted; each told whether its view shows. True: redraw.
pub fn tick(app: &mut App) -> bool {
    let space = app.space();
    let mut changed = false;
    for e in app.exts.list.iter_mut() {
        if e.out.is_some() && e.done.load(Ordering::SeqCst) {
            e.close();
            e.error = Some(format!("{} ended", e.m.name));
            changed = true;
        }
        let visible = space == Space::View(e.n);
        if e.out.is_some() && visible != e.shown {
            e.shown = visible;
            e.notify("view.shown", json!({"visible": visible}));
        }
        // Its tab in front (what its right panel is about, say).
        let front = app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).and_then(|t| split_key(&t.key)).filter(|(id, _)| *id == e.m.id).map(|(_, t)| t.to_string());
        if visible && e.out.is_some() && front.is_some() && front != e.front {
            e.notify("view.tab", json!({"tab": front}));
            e.front = front;
        }
    }
    changed
}

/// An extension's `view.*` request.
pub fn call(app: &mut App, client: &Client, method: &str, p: &Value) -> Result<Value, String> {
    let id = client.ext.as_deref().ok_or_else(|| format!("{method}: only an extension has views (termcode ext)"))?;
    let i = app.exts.list.iter().position(|e| e.m.id == id).ok_or("no such extension")?;
    let slot = Slot::parse(p["slot"].as_str().unwrap_or("main"))?;
    let section = p["section"].as_str().unwrap_or("main").to_string();
    let tab = p["tab"].as_str().map(clean).unwrap_or_else(|| "main".into());
    if tab.is_empty() || tab.chars().count() > 80 {
        return Err("\"tab\": a name of 1 to 80 characters".into());
    }
    match method {
        "view.set" | "view.append" => {
            let rows = rows(&p["rows"])?;
            let title = p["title"].as_str().map(clean);
            let e = &mut app.exts.list[i];
            let pane = match slot {
                Slot::Left => {
                    let k = e.left.iter().position(|s| s.id == section).unwrap_or_else(|| {
                        e.left.push(Pane { id: section.clone(), ..Pane::default() });
                        e.left.len() - 1
                    });
                    &mut e.left[k]
                }
                Slot::Main => {
                    let k = e.main.iter().position(|t| t.id == tab).unwrap_or_else(|| {
                        e.main.push(Pane { id: tab.clone(), ..Pane::default() });
                        e.main.len() - 1
                    });
                    &mut e.main[k]
                }
                _ => e.pane(slot).get_or_insert_with(Pane::default),
            };
            if let Some(t) = title {
                pane.title = t;
            }
            if let Some(m) = p["scroll"].as_str() {
                pane.own_scroll = m == "self";
            }
            if method == "view.set" {
                pane.rows = rows;
            } else {
                pane.rows.extend(rows);
                let over = pane.rows.len().saturating_sub(MAX_ROWS);
                pane.rows.drain(..over);
                pane.scroll = pane.scroll.saturating_sub(over);
            }
            pane.rows.truncate(MAX_ROWS);
            let n = pane.rows.len();
            open_main(app, i, false);
            Ok(json!({"rows": n}))
        }
        "view.clear" => {
            let e = &mut app.exts.list[i];
            match slot {
                Slot::Left => e.left.retain(|s| p["section"].is_string() && s.id != section),
                // A tab cleared: closed too.
                Slot::Main => {
                    e.main.retain(|t| t.id != tab);
                    let key = tab_key(&e.m.id, &tab);
                    while let Some((g, k)) = app.groups.iter().enumerate().find_map(|(g, grp)| grp.tabs.iter().position(|t| t.key == key).map(|k| (g, k))) {
                        app.close_tab(g, k, true);
                    }
                }
                _ => *e.pane(slot) = None,
            }
            Ok(json!({"cleared": slot.name()}))
        }
        "view.open" => {
            let n = app.exts.list[i].n;
            app.show_side(Side::View(n), false);
            open_tab(app, i, &tab, true);
            Ok(json!({"shown": true}))
        }
        _ => Err(format!("unknown method {method:?} (view.set, view.append, view.clear, view.open)")),
    }
}

/// A card's text: its lines kept, each cleaned.
fn hint_text(s: &str) -> String {
    s.lines().take(20).map(clean).collect::<Vec<_>>().join("\n")
}

/// Text as termcode shows it: no control characters (no escape reaches the terminal), cut long.
fn clean(s: &str) -> String {
    s.chars().take(MAX_TEXT).map(|c| if c == '\t' { ' ' } else if c.is_control() { '\u{fffd}' } else { c }).collect()
}

pub(super) fn rows(v: &Value) -> Result<Vec<Row>, String> {
    v.as_array().ok_or("\"rows\": an array of rows")?.iter().take(MAX_ROWS).map(row).collect()
}

/// A row: "text", a cell {text, …}, [cells…], or {cells: […], action}.
fn row(v: &Value) -> Result<Row, String> {
    match v {
        Value::Array(a) => Ok(Row { cells: a.iter().map(cell).collect::<Result<_, _>>()?, action: None, hint: None, head: None }),
        // A section header: {"header": "Title", "open": true, "extra": "3", "action": …}.
        Value::Object(o) if o.contains_key("header") => {
            let t = |k: &str| o.get(k).and_then(Value::as_str).map(clean).unwrap_or_default();
            let cells = vec![Cell { text: t("header"), ..Cell::default() }, Cell { text: t("extra"), ..Cell::default() }];
            Ok(Row { cells, action: o.get("action").and_then(Value::as_str).map(clean), hint: None, head: Some(o.get("open").and_then(Value::as_bool).unwrap_or(true)) })
        }
        Value::Object(o) if o.contains_key("cells") => {
            let cells = o["cells"].as_array().ok_or("\"cells\": an array")?.iter().map(cell).collect::<Result<_, _>>()?;
            Ok(Row { cells, action: o.get("action").and_then(Value::as_str).map(clean), hint: o.get("hint").and_then(Value::as_str).map(hint_text), head: None })
        }
        _ => Ok(Row { cells: vec![cell(v)?], action: None, hint: None, head: None }),
    }
}

/// A cell: "text", or {text, fg, bg, bold, dim, italic, button, action}.
fn cell(v: &Value) -> Result<Cell, String> {
    match v {
        Value::String(s) => Ok(Cell { text: clean(s), ..Cell::default() }),
        Value::Object(o) => {
            let s = |k: &str| o.get(k).and_then(Value::as_str).map(clean);
            let b = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
            let hint = o.get("hint").and_then(Value::as_str).map(hint_text);
            Ok(Cell { text: s("text").unwrap_or_default(), fg: s("fg"), bg: s("bg"), bold: b("bold"), dim: b("dim"), italic: b("italic"), button: b("button"), action: s("action"), hint })
        }
        _ => Err(format!("a cell is \"text\" or {{\"text\": …}}, got {v}")),
    }
}

/// A color: a theme's name, so extensions follow the theme, or #rrggbb.
fn color(name: &str) -> Option<Color> {
    Some(match name {
        "fg" => P.editor_fg,
        "dim" => P.dim,
        "accent" => P.focus_border,
        "error" => P.error_fg,
        "warning" => P.git_modified,
        "ok" => P.git_added,
        "info" => P.git_renamed,
        "selection" => P.selection,
        "input" => P.input_bg,
        "active" => P.list_active_bg,
        "hover" => P.list_hover_bg,
        "bg" => P.editor_bg,
        _ => {
            let h = name.strip_prefix('#').filter(|h| h.len() == 6)?;
            let c = |k: usize| u8::from_str_radix(&h[k..k + 2], 16).ok();
            Color::Rgb(c(0)?, c(2)?, c(4)?)
        }
    })
}

/// `pane`'s rows in `r` from its scroll: each click place noted; the hovered one lit.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_rows(f: &mut Frame, r: Rect, pane: &mut Pane, ext: usize, slot: Slot, sec: usize, hover: Option<Position>, hits: &mut Vec<Hit>, bg: Color, fg: Color) -> Option<(Rect, String)> {
    let mut card = None;
    let max = pane.rows.len().saturating_sub(r.height as usize);
    if pane.follow {
        pane.scroll = max;
    }
    pane.scroll = pane.scroll.min(max);
    pane.follow = pane.scroll == max;
    let base = Style::new().bg(bg).fg(fg);
    for (k, row) in pane.rows.iter().skip(pane.scroll).take(r.height as usize).enumerate() {
        let y = r.y + k as u16;
        let line_r = Rect::new(r.x, y, r.width, 1);
        if let Some(open) = row.head {
            // As the side bar's section headers (Source Control's): chevron, bold title, dim count.
            let title = row.cells.first().map_or("", |c| c.text.as_str());
            let extra = row.cells.get(1).map_or("", |c| c.text.as_str());
            let lit = row.action.is_some() && hover.is_some_and(|p| line_r.contains(p));
            crate::ui::kit::head(f, line_r, title, open, extra, lit);
            if let Some(a) = &row.action {
                hits.push(Hit { r: line_r, ext, slot, sec, action: a.clone() });
            }
            continue;
        }
        let row_hot = row.action.is_some() && hover.is_some_and(|p| line_r.contains(p));
        let row_bg = if row_hot { P.list_hover_bg } else { bg };
        // The row's own click first: a button in it, noted after, is found first (hits are
        // searched from the last).
        if let Some(a) = &row.action {
            hits.push(Hit { r: line_r, ext, slot, sec, action: a.clone() });
        }
        let mut spans = Vec::new();
        let mut x = r.x;
        for c in &row.cells {
            if c.button {
                // The kit's button: a danger one when its colour says error.
                let w = crate::ui::kit::tool_w(&c.text).min(r.right().saturating_sub(x));
                let cr = Rect::new(x, y, w, 1);
                let danger = [&c.fg, &c.bg].iter().any(|k| k.as_deref() == Some("error"));
                if let Some(a) = &c.action {
                    hits.push(Hit { r: cr, ext, slot, sec, action: a.clone() });
                }
                if let Some(h) = c.hint.as_ref().filter(|_| hover.is_some_and(|p| cr.contains(p))) {
                    card = Some((cr, h.clone()));
                }
                let look = if danger { crate::ui::kit::Look::Danger } else { crate::ui::kit::Look::Primary };
                spans.extend(crate::ui::kit::button_spans(&c.text, look, cr, row_bg));
                x += w;
                continue;
            }
            let text = c.text.clone();
            let w = (Span::raw(text.as_str()).width() as u16).min(r.right().saturating_sub(x));
            let cr = Rect::new(x, y, w, 1);
            let hot = c.action.is_some() && hover.is_some_and(|p| cr.contains(p));
            let mut st = base.bg(row_bg);
            if hot {
                st = st.bg(P.list_hover_bg);
            }
            if let Some(c) = c.fg.as_deref().and_then(color) {
                st = st.fg(c);
            }
            if let Some(c) = c.bg.as_deref().and_then(color) {
                st = st.bg(c);
            }
            for (on, m) in [(c.bold, Modifier::BOLD), (c.dim, Modifier::DIM), (c.italic, Modifier::ITALIC)] {
                if on {
                    st = st.add_modifier(m);
                }
            }
            if let Some(a) = &c.action {
                hits.push(Hit { r: cr, ext, slot, sec, action: a.clone() });
            }
            if let Some(h) = c.hint.as_ref().filter(|_| hover.is_some_and(|p| cr.contains(p))) {
                card = Some((cr, h.clone()));
            }
            spans.push(Span::styled(text, st));
            x += w;
        }
        if let Some(h) = row.hint.as_ref().filter(|_| card.is_none() && hover.is_some_and(|p| line_r.contains(p))) {
            card = Some((line_r, h.clone()));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(base.bg(row_bg)), line_r);
    }
    card
}

pub fn title(app: &App, n: u16) -> String {
    app.exts.by_num(n).map_or_else(String::new, |i| app.exts.list[i].m.name.to_uppercase())
}

/// The side bar: its sections, one under another, a line (draggable) between them; or why
/// it is not running.
pub fn draw_side(app: &mut App, f: &mut Frame, r: Rect, n: u16) {
    let Some(i) = app.exts.by_num(n) else { return };
    let focused = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let r = crate::ui::inset(card, (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1));
    let (hover, drag) = (app.hover, app.exts.drag);
    let Exts { list, hits, panes, sashes, folds, hint, .. } = &mut app.exts;
    let e = &mut list[i];
    if let Some(err) = e.error.clone() {
        let log = e.log().display().to_string();
        let log = std::env::var("HOME").ok().and_then(|h| log.strip_prefix(&h).map(|r| format!("~{r}"))).unwrap_or(log);
        let text = |t: String, fg: &str| Row { cells: vec![Cell { text: t, fg: Some(fg.into()), ..Cell::default() }], action: None, hint: None, head: None };
        let mut p = Pane { rows: vec![text(err, "error"), text("Its log:".into(), "dim"), text(log, "dim"), Row::default(), Row { cells: vec![Cell { text: "Restart".into(), button: true, action: Some(RESTART.into()), ..Cell::default() }], action: None, hint: None, head: None }], ..Pane::default() };
        draw_rows(f, r, &mut p, i, Slot::Left, 0, hover, hits, P.sidebar_bg, P.sidebar_fg);
        return;
    }
    let k = e.left.len();
    if k == 0 {
        let msg = if e.out.is_some() { "Starting…" } else { "" };
        f.render_widget(Paragraph::new(msg).style(Style::new().fg(P.dim).bg(P.sidebar_bg)), r);
        return;
    }
    // Headings take a row each (a lone untitled section none); the open sections share the
    // rest: the dragged ones keep their height, the others split what is left, the last
    // open one taking what remains. A folded one is its header alone.
    let heads: Vec<u16> = e.left.iter().map(|s| u16::from(k > 1 || !s.title.is_empty())).collect();
    let last = e.left.iter().rposition(|s| !s.folded);
    let body = r.height.saturating_sub(heads.iter().sum());
    let others = |s: &(usize, &Pane)| !s.1.folded && Some(s.0) != last;
    let fixed: u16 = e.left.iter().enumerate().filter(others).filter_map(|s| s.1.height).sum();
    let free = e.left.iter().enumerate().filter(others).filter(|s| s.1.height.is_none()).count() as u16 + 1;
    let share = body.saturating_sub(fixed.min(body)) / free;
    let open_after: Vec<u16> = (0..k).map(|s| e.left[s + 1..].iter().filter(|x| !x.folded).count() as u16).collect();
    let mut y = r.y;
    let mut headers = Vec::new();
    for (s, sec) in e.left.iter_mut().enumerate() {
        let left = r.bottom().saturating_sub(y + heads[s]);
        let h = match () {
            _ if sec.folded => 0,
            _ if Some(s) == last => left,
            _ => sec.height.unwrap_or(share).min(left.saturating_sub(2 * open_after[s])),
        };
        if heads[s] == 1 && y < r.bottom() {
            let hr = Rect::new(r.x, y, r.width, 1);
            let hot = s > 0 && (hover.is_some_and(|p| hr.contains(p)) || drag.is_some_and(|d| d.0 == i && d.1 + 1 == s));
            headers.push((hr, sec.title.to_uppercase(), !sec.folded, hot));
            // Its chevron folds it; below a section, the rest of it drags the line.
            folds.push((Rect::new(hr.x, hr.y, hr.width.min(4), 1), i, s));
            if s > 0 {
                sashes.push((hr, i, s - 1));
            }
            y += 1;
        }
        let pr = Rect::new(r.x, y, r.width, h.min(r.bottom().saturating_sub(y)));
        if pr.height > 0 {
            panes.push((pr, i, Slot::Left, s));
            if let Some(c) = draw_rows(f, pr, sec, i, Slot::Left, s, hover, hits, P.sidebar_bg, P.sidebar_fg) {
                *hint = Some(c);
            }
        }
        y += pr.height;
    }
    // Headers as every side bar draws them (Source Control's sections).
    for (hr, title, open, hot) in headers {
        crate::ui::section_head(app, f, hr, &title, open, "", hot);
    }
}

/// A main tab (`ext:<id>`): its rows on the editor's background.
pub fn draw_main(app: &mut App, f: &mut Frame, r: Rect, key: &str) {
    let Some((id, tab)) = split_key(key) else { return };
    let Some(i) = app.exts.list.iter().position(|e| e.m.id == id) else { return };
    crate::ui::fill(f, r, P.editor_bg);
    let r = crate::ui::inset(r, 1);
    let hover = app.hover;
    let Exts { list, hits, panes, hint, sel, .. } = &mut app.exts;
    let e = &mut list[i];
    let Some(k) = e.main.iter().position(|t| t.id == tab) else { return };
    if e.main[k].size != (r.width, r.height) {
        e.main[k].size = (r.width, r.height);
        e.notify("view.resize", json!({"slot": "main", "tab": tab, "width": r.width, "height": r.height}));
    }
    panes.push((r, i, Slot::Main, k));
    if let Some(c) = draw_rows(f, r, &mut e.main[k], i, Slot::Main, k, hover, hits, P.editor_bg, P.editor_fg) {
        *hint = Some(c);
    }
    // The selection, lit.
    if let Some(s) = sel.filter(|s| s.ext == i && s.tab == k) {
        let (a, b) = if s.a <= s.b { (s.a, s.b) } else { (s.b, s.a) };
        let top = e.main[k].scroll;
        let buf = f.buffer_mut();
        for y in 0..r.height {
            let row = top + y as usize;
            if row < a.0 || row > b.0 {
                continue;
            }
            let from = if row == a.0 { a.1 } else { 0 };
            let to = if row == b.0 { b.1 } else { r.width.saturating_sub(1) };
            for x in from..=to.min(r.width.saturating_sub(1)) {
                buf[(r.x + x, r.y + y)].set_bg(P.selection);
            }
        }
    }
}

/// Text selected in a tab, as shown (a row a line).
fn selected(app: &App) -> Option<String> {
    let s = app.exts.sel?;
    let pane = app.exts.list.get(s.ext)?.main.get(s.tab)?;
    let (a, b) = if s.a <= s.b { (s.a, s.b) } else { (s.b, s.a) };
    let mut out = Vec::new();
    for row in a.0..=b.0.min(pane.rows.len().saturating_sub(1)) {
        let text: String = pane.rows[row].cells.iter().map(|c| if c.button { format!(" {} ", c.text) } else { c.text.clone() }).collect();
        let (from, to) = (if row == a.0 { a.1 as usize } else { 0 }, if row == b.0 { b.1 as usize } else { usize::MAX });
        let mut col = 0;
        let mut line = String::new();
        for ch in text.chars() {
            if col >= from && col <= to {
                line.push(ch);
            }
            col += Span::raw(ch.to_string()).width();
        }
        out.push(line.trim_end().to_string());
    }
    Some(out.join("\n")).filter(|t| !t.trim().is_empty())
}

/// The pointer in an extension's tab: a press (not on a button) starts a selection, a
/// drag extends it.
fn mouse(app: &mut App, m: ratatui::crossterm::event::MouseEvent) -> Option<bool> {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    let p = Position::new(m.column, m.row);
    let at = |app: &App, r: Rect, i: usize, k: usize, p: Position| {
        let y = p.y.clamp(r.y, r.bottom().saturating_sub(1)) - r.y;
        let x = p.x.clamp(r.x, r.right().saturating_sub(1)) - r.x;
        (app.exts.list.get(i).and_then(|e| e.main.get(k)).map_or(0, |t| t.scroll) + y as usize, x)
    };
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // (An extension's tab; not the Extensions view's own page.)
            let &(r, i, _, k) = app.exts.panes.iter().find(|(r, i, slot, _)| *slot == Slot::Main && *i != MANAGER && r.contains(p))?;
            if app.exts.hits.iter().any(|h| h.r.contains(p)) {
                app.exts.sel = None;
                return None;
            }
            let a = at(app, r, i, k, p);
            app.exts.sel = Some(Sel { ext: i, tab: k, a, b: a, dragging: true });
            if let Some(g) = app.ui.groups.iter().position(|gh| gh.text.contains(p)) {
                app.ag = g;
            }
            app.focus = Focus::Editor;
            Some(true)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let s = app.exts.sel.filter(|s| s.dragging)?;
            let &(r, ..) = app.exts.panes.iter().find(|(_, i, slot, k)| *slot == Slot::Main && *i == s.ext && *k == s.tab)?;
            let b = at(app, r, s.ext, s.tab, p);
            app.exts.sel = Some(Sel { b, ..s });
            Some(true)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let s = app.exts.sel.filter(|s| s.dragging)?;
            app.exts.sel = (s.a != s.b).then_some(Sel { dragging: false, ..s });
            Some(true)
        }
        _ => None,
    }
}

/// A row's or cell's card, by what it is about.
fn draw_hint(app: &mut App, f: &mut Frame) {
    use ratatui::widgets::{Block, Clear};
    let Some((at, text)) = app.exts.hint.clone() else { return };
    let lines: Vec<&str> = text.lines().collect();
    let area = f.area();
    let w = (lines.iter().map(|l| Span::raw(*l).width()).max().unwrap_or(0) as u16 + 2).min(60).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let x = if at.right() + w < area.right() { at.right() + 1 } else { at.x.saturating_sub(w) };
    let y = at.y.min(area.bottom().saturating_sub(h));
    let r = Rect::new(x, y, w, h);
    f.render_widget(Clear, r);
    let block = Block::bordered().border_set(crate::blocks::border_set()).border_style(Style::new().fg(P.widget_border).bg(P.widget_bg)).style(Style::new().bg(P.widget_bg).fg(P.widget_fg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    f.render_widget(Paragraph::new(lines.iter().map(|l| Line::raw(*l)).collect::<Vec<_>>()), inner);
}

pub fn main_title(app: &App, key: &str) -> Option<String> {
    let (id, tab) = split_key(key)?;
    let e = app.exts.list.iter().find(|e| e.m.id == id)?;
    let title = e.main.iter().find(|t| t.id == tab).map(|t| t.title.clone()).filter(|t| !t.is_empty());
    Some(title.unwrap_or_else(|| if tab == "main" { e.m.name.clone() } else { tab.to_string() }))
}

/// Its tab closed by the user: told (the extension decides what that ends).
fn closed(app: &mut App, key: &str) {
    let Some((id, tab)) = split_key(key) else { return };
    if let Some(e) = app.exts.list.iter().find(|e| e.m.id == id) {
        e.notify("view.closed", json!({"slot": "main", "tab": tab}));
    }
}

/// A paste into its tab: the extension's.
fn paste(app: &mut App, text: &str) -> bool {
    let Some(key) = app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).map(|t| t.key.clone()).filter(|_| app.focus == Focus::Editor) else { return false };
    let Some((id, tab)) = split_key(&key) else { return false };
    let Some(e) = app.exts.list.iter().find(|e| e.m.id == id) else { return false };
    e.notify("view.paste", json!({"slot": "main", "tab": tab, "text": text}));
    true
}

/// The extension whose view shows, when it has `slot`.
fn here(app: &App, slot: Slot) -> Option<usize> {
    let Space::View(n) = app.space() else { return None };
    let i = app.exts.by_num(n)?;
    let e = &app.exts.list[i];
    let has = match slot {
        Slot::Right => e.right.is_some(),
        Slot::Bottom => e.bottom.is_some(),
        _ => true,
    };
    has.then_some(i)
}

pub fn right_here(app: &App) -> bool {
    here(app, Slot::Right).is_some()
}

pub fn bottom_here(app: &App) -> bool {
    here(app, Slot::Bottom).is_some()
}

/// The right or bottom panel, framed as the Outline and the terminal are (the bottom
/// one's top border resizes it as the terminal's does).
pub fn draw_panel(app: &mut App, f: &mut Frame, r: Rect, slot: Slot) -> bool {
    use ratatui::widgets::Block;
    let Some(i) = here(app, slot) else { return false };
    let hover = app.hover;
    let e = &mut app.exts.list[i];
    let Some(p) = (if slot == Slot::Right { e.right.as_mut() } else { e.bottom.as_mut() }) else { return false };
    let hot = slot == Slot::Bottom && (matches!(app.drag, crate::panel::Drag::TermSash) || hover.is_some_and(|q| q.y == r.y && (r.x..r.right()).contains(&q.x)));
    let edge = if hot { P.sash_hover } else { crate::blocks::border(false) };
    let block = Block::bordered()
        .border_set(crate::blocks::border_set())
        .border_style(Style::new().fg(edge).bg(P.editor_bg))
        .style(Style::new().bg(P.editor_bg).fg(P.editor_fg))
        .title(Span::styled(format!(" {} ", if p.title.is_empty() { e.m.name.as_str() } else { p.title.as_str() }), Style::new().fg(P.editor_fg).bold()));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if slot == Slot::Bottom {
        app.ui.term_sash = Rect::new(r.x, r.y, r.width, 1);
        app.ui.term = inner;
    }
    let Exts { list, hits, panes, .. } = &mut app.exts;
    let p = if slot == Slot::Right { list[i].right.as_mut() } else { list[i].bottom.as_mut() };
    if let Some(p) = p {
        panes.push((inner, i, slot, 0));
        draw_rows(f, inner, p, i, slot, 0, hover, hits, P.editor_bg, P.editor_fg);
    }
    true
}

/// A click: a row's or a button's action goes to its extension; a click anywhere on its
/// panes is theirs; a side bar line starts its drag.
pub fn click(app: &mut App, p: Position) -> bool {
    if let Some(&(_, i, s)) = app.exts.folds.iter().find(|(r, ..)| r.contains(p)) {
        if let Some(sec) = app.exts.list.get_mut(i).and_then(|e| e.left.get_mut(s)) {
            sec.folded = !sec.folded;
        }
        return true;
    }
    if let Some(&(_, i, s)) = app.exts.sashes.iter().find(|(r, ..)| r.contains(p)) {
        let top = app.exts.panes.iter().find(|(_, e, slot, k)| *e == i && *slot == Slot::Left && *k == s).map_or(0, |q| q.0.y);
        app.exts.drag = Some((i, s, top));
        app.drag = crate::panel::Drag::View(app.exts.list[i].n);
        return true;
    }
    if let Some(h) = app.exts.hits.iter().rev().find(|h| h.r.contains(p)) {
        let (i, slot, sec, action) = (h.ext, h.slot, h.sec, h.action.clone());
        if i == MANAGER {
            super::manager::action(app, &action);
            return true;
        }
        if action == RESTART {
            start(app, i);
            return true;
        }
        let e = &app.exts.list[i];
        let section = if slot == Slot::Left { e.left.get(sec).map(|s| s.id.clone()) } else { None };
        let tab = if slot == Slot::Main { e.main.get(sec).map(|t| t.id.clone()) } else { None };
        e.notify("view.action", json!({"slot": slot.name(), "section": section, "tab": tab, "action": action}));
        focus_slot(app, slot);
        return true;
    }
    if let Some(&(_, _, slot, _)) = app.exts.panes.iter().find(|(r, ..)| r.contains(p)) {
        focus_slot(app, slot);
        return true;
    }
    false
}

/// Keys then go where the click was: the side bar's to its extension, the main tab's too.
fn focus_slot(app: &mut App, slot: Slot) {
    match slot {
        Slot::Left => app.focus = Focus::Side,
        Slot::Main => app.focus = Focus::Editor,
        _ => {}
    }
}

pub fn drag_to(app: &mut App, y: u16) {
    if let Some((i, s, top)) = app.exts.drag {
        if let Some(sec) = app.exts.list.get_mut(i).and_then(|e| e.left.get_mut(s)) {
            sec.height = Some(y.saturating_sub(top).max(1));
        }
    }
}

pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    let Some(&(_, i, slot, sec)) = app.exts.panes.iter().find(|(r, ..)| r.contains(p)) else { return false };
    if i == MANAGER {
        let v = if slot == Slot::Main { &mut app.exts.detail } else { &mut app.exts.view };
        v.scroll = if d < 0 { v.scroll.saturating_sub(3) } else { v.scroll + 3 };
        v.follow = false;
        return true;
    }
    let e = &mut app.exts.list[i];
    let pane = match slot {
        Slot::Left => e.left.get_mut(sec),
        Slot::Main => e.main.get_mut(sec),
        Slot::Right => e.right.as_mut(),
        Slot::Bottom => e.bottom.as_mut(),
    };
    if let Some(pane) = pane {
        if pane.own_scroll {
            let tab = (slot == Slot::Main).then(|| pane.id.clone());
            let section = (slot == Slot::Left).then(|| pane.id.clone());
            e.notify("view.wheel", json!({"slot": slot.name(), "section": section, "tab": tab, "lines": d.signum() * 3}));
            return true;
        }
        pane.scroll = if d < 0 { pane.scroll.saturating_sub(3) } else { pane.scroll + 3 };
        pane.follow = false;
    }
    true
}

/// A key while its side bar or main tab has the focus: the extension's (`view.key`), but
/// termcode's own (F1, Ctrl+Q, the terminal's, the tab keys) and Esc in the side bar.
pub fn takes(app: &mut App, k: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let slot = match (app.focus, app.side) {
        (Focus::Side, Side::View(n)) if !app.activity_lit && app.exts.by_num(n).is_some() => Slot::Left,
        (Focus::Editor, _) if app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).is_some_and(|t| t.key.starts_with(PREFIX)) => Slot::Main,
        _ => return false,
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    // Ctrl+C with text selected in its tab copies it (as in the terminal).
    if slot == Slot::Main && ctrl && matches!(k.code, KeyCode::Char('c' | 'C')) {
        if let Some(text) = selected(app) {
            app.msg = format!("copied via {}", crate::panel::clipboard_set(&text));
            app.clip = text;
            app.exts.sel = None;
            return true;
        }
    }
    let termcodes = matches!(k.code, KeyCode::F(1)) || (ctrl && matches!(k.code, KeyCode::Char('q' | 'Q') | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Tab | KeyCode::BackTab)) || crate::term::toggle_key(&k) || (slot == Slot::Left && k.code == KeyCode::Esc);
    if termcodes {
        return false;
    }
    let n = match slot {
        Slot::Left => match app.side {
            Side::View(n) => n,
            _ => return false,
        },
        _ => match app.space() {
            Space::View(n) => n,
            _ => return false,
        },
    };
    let Some(i) = app.exts.by_num(n) else { return false };
    let name = match k.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::BackTab => "Tab".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        c => format!("{c:?}"),
    };
    let m = |x| k.modifiers.contains(x);
    let shift = m(KeyModifiers::SHIFT) || k.code == KeyCode::BackTab;
    let tab = (slot == Slot::Main).then(|| app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).and_then(|t| split_key(&t.key)).map(|(_, t)| t.to_string())).flatten();
    app.exts.list[i].notify("view.key", json!({"slot": slot.name(), "tab": tab, "key": name, "ctrl": ctrl, "alt": m(KeyModifiers::ALT), "shift": shift}));
    true
}

/// An extension for tests: a tab for each of `tabs`, a row in each; no program (it is
/// never started).
#[cfg(test)]
pub fn test_ext(id: &str, tabs: &[&str]) -> Ext {
    let m = Manifest { id: id.into(), name: id.into(), version: String::new(), description: String::new(), icon: Icon::plain("T"), run: vec!["/nonexistent".into()], permissions: Vec::new(), commands: Vec::new(), build: Vec::new(), api: Vec::new(), dir: std::env::temp_dir() };
    let mut e = Ext::new(m);
    e.started = true;
    e.main = tabs.iter().map(|t| Pane { id: t.to_string(), title: t.to_string(), rows: vec![Row { cells: vec![Cell { text: format!("stream of {t}"), ..Cell::default() }], action: None, hint: None, head: None }], ..Pane::default() }).collect();
    e
}

/// The ops serving every extension (the view shown, or the panes clicked, says which).
pub(super) const OPS: Ops = Ops {
    id: "extensions-host",
    side: |app, f, r| {
        if let Side::View(n) = app.side {
            draw_side(app, f, r, n);
        }
    },
    shown: |app| {
        if let Side::View(n) = app.side {
            shown(app, n);
        }
    },
    owns: |key| key.starts_with(PREFIX),
    tab: |app, f, _, r, key, _| {
        draw_main(app, f, r, key);
        Drawn::Whole
    },
    tab_title: main_title,
    closed,
    paste,
    mouse,
    overlay: draw_hint,
    right: |app, f, r| draw_panel(app, f, r, Slot::Right),
    click: |app, p, _| click(app, p),
    wheel,
    drag_to,
    takes,
    tick,
    ..Ops::NONE
};

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("termcode-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_manifest_is_checked_before_anything_runs() {
        let d = tmp("manifest");
        let put = |v: Value| std::fs::write(d.join(MANIFEST), v.to_string()).unwrap();
        put(json!({"id": "hello", "run": ["python3", "main.py"], "permissions": ["edit"], "icon": "◆x"}));
        let m = Manifest::read(&d).unwrap();
        assert_eq!((m.name.as_str(), &m.icon.unicode, &m.icon.ascii, m.permissions.as_slice()), ("hello", &"◆".to_string(), &"H".to_string(), &[Tier::Edit][..]));
        // One per icon set: a Nerd Font plug, ⇄, S; a private-use glyph never stands for Unicode.
        let i = Icon::read(&json!({"nerd": "\u{f1e6}", "unicode": "⇄", "ascii": "S"}), "Serial");
        assert_eq!((i.nerd.as_str(), i.unicode.as_str(), i.ascii.as_str()), ("\u{f1e6}", "⇄", "S"));
        let i = Icon::read(&json!({"nerd": "\u{f1e6}"}), "serial");
        assert_eq!((i.nerd.as_str(), i.unicode.as_str(), i.ascii.as_str()), ("\u{f1e6}", "S", "S"));
        put(json!({"id": "x", "run": ["./x"], "build": ["make"], "commands": [{"id": "open", "title": "Open…"}]}));
        let m = Manifest::read(&d).unwrap();
        assert_eq!((m.build, m.commands), (vec!["make".to_string()], vec![("open".to_string(), "Open…".to_string())]));
        // Tabs: "main" is the extension's own key, others carry their name (a slash or not).
        assert_eq!((tab_key("x", "main"), tab_key("x", "pts/3")), ("ext:x".to_string(), "ext:x/pts/3".to_string()));
        assert_eq!((split_key("ext:x"), split_key("ext:x/pts/3"), split_key("file:a")), (Some(("x", "main")), Some(("x", "pts/3")), None));
        for bad in [json!({"id": "Hello", "run": ["x"]}), json!({"id": "a", "run": ["x"], "commands": [{"id": "c"}]}), json!({"id": "../x", "run": ["x"]}), json!({"id": "a"}), json!({"id": "a", "run": ["x"], "permissions": ["root"]})] {
            put(bad.clone());
            assert!(Manifest::read(&d).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rows_take_text_cells_and_actions_and_never_an_escape() {
        let r = rows(&json!(["plain\u{1b}[2J", [{"text": "ok", "fg": "ok", "bold": true}, {"text": "Go", "button": true, "action": "go"}], {"cells": ["item"], "action": "pick:1"}])).unwrap();
        assert_eq!(r[0].cells[0].text, "plain\u{fffd}[2J");
        assert_eq!((r[1].cells[0].bold, r[1].cells[1].action.as_deref(), r[1].cells[1].button), (true, Some("go"), true));
        assert_eq!(r[2].action.as_deref(), Some("pick:1"));
        assert!(rows(&json!("x")).is_err() && rows(&json!([42])).is_err());
        assert_eq!(color("#102030"), Some(Color::Rgb(16, 32, 48)));
        assert_eq!(color("nope"), None);
    }

    #[test]
    fn an_extension_fills_its_slots_and_hears_clicks_and_keys() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("e2e");
        std::fs::create_dir_all(d.join("run")).unwrap();
        std::fs::set_permissions(d.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir_all(d.join("ext")).unwrap();
        std::fs::write(d.join("work.txt"), "w\n").unwrap();
        // A driver in sh: two views and a notice (edit, granted), then it logs what it hears.
        let script = r#"#!/bin/sh
echo '{"jsonrpc":"2.0","id":1,"method":"view.set","params":{"slot":"left","section":"a","title":"Things","rows":[{"cells":["first"],"action":"pick"}]}}'
read r; echo "$r" >> "$TERMCODE_EXTENSION_DIR/got"
echo '{"jsonrpc":"2.0","id":2,"method":"view.set","params":{"slot":"main","title":"Board","rows":["hello",[{"text":"Go","button":true,"action":"go"}]]}}'
read r; echo "$r" >> "$TERMCODE_EXTENSION_DIR/got"
echo '{"jsonrpc":"2.0","id":3,"method":"ui.notify","params":{"text":"ready"}}'
read r; echo "$r" >> "$TERMCODE_EXTENSION_DIR/got"
while read line; do echo "$line" >> "$TERMCODE_EXTENSION_DIR/got"; done
"#;
        std::fs::write(d.join("ext/run.sh"), script).unwrap();
        std::fs::set_permissions(d.join("ext/run.sh"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let m = Manifest { id: "e2e-test".into(), name: "Probe".into(), version: "1".into(), description: String::new(), icon: Icon::plain("P"), run: vec!["./run.sh".into()], permissions: vec![Tier::Edit], commands: Vec::new(), build: Vec::new(), api: Vec::new(), dir: d.join("ext") };
        let mut app = App::new(std::fs::canonicalize(&d).unwrap(), None);
        app.api = crate::api::start(&d.join("run"));
        app.exts.list.push(Ext::new(m));
        let n = num("e2e-test");
        app.show_side(Side::View(n), false);
        let got = || std::fs::read_to_string(d.join("ext/got")).unwrap_or_default();
        let settle = |app: &mut App, want: &dyn Fn(&App) -> bool| {
            for _ in 0..500 {
                crate::api::tick(app);
                tick(app);
                if want(app) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("never settled: {}", got());
        };
        settle(&mut app, &|a| a.msg.contains("ready"));
        assert_eq!(app.msg, "Probe extension: ready", "ui.notify: granted edit, shown with its name");
        let mut t = ratatui::Terminal::new(TestBackend::new(120, 30)).unwrap();
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let b = t.backend().buffer().clone();
        let text: String = (0..30).map(|y| (0..120).map(|x| b[(x, y)].symbol()).collect::<String>() + "\n").collect();
        assert!(text.contains("PROBE") && text.contains("THINGS") && text.contains("first"), "its side bar:\n{text}");
        assert!(text.contains("Board") && text.contains("hello") && text.contains(&crate::ui::kit::btn_text("Go")), "its tab, opened in its own space:\n{text}");
        assert_eq!(app.space(), Space::View(n));
        // A click on the row: `view.action` with its section; a key in the tab: `view.key`.
        let row = app.exts.hits.iter().find(|h| h.action == "pick").map(|h| Position::new(h.r.x, h.r.y)).unwrap();
        assert!(click(&mut app, row));
        let go = app.exts.hits.iter().find(|h| h.action == "go").map(|h| Position::new(h.r.x, h.r.y)).unwrap();
        assert!(click(&mut app, go));
        assert_eq!(app.focus, Focus::Editor);
        assert!(takes(&mut app, KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)));
        assert!(!takes(&mut app, KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)), "F1 stays termcode's");
        for _ in 0..300 {
            if got().contains("view.key") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let log = got();
        assert!(log.contains(r#""action":"pick","section":"a","slot":"left""#), "{log}");
        assert!(log.contains(r#""action":"go","section":null,"slot":"main""#), "{log}");
        assert!(log.contains(r#""alt":false,"ctrl":true,"key":"x""#), "{log}");
        assert!(log.contains(r#""visible":true"#), "view.shown: {log}");
        // view.* is an extension's only.
        assert!(call(&mut app, &Client::default(), "view.set", &json!({"rows": []})).is_err());
        // Turned off (Disable, Uninstall, a reload): its stdin closed, it ends, it is reaped.
        let done = app.exts.list[0].done.clone();
        app.exts.list[0].stop();
        for _ in 0..300 {
            if done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(done.load(Ordering::SeqCst), "it ended when told");
        drop(app);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_tab_selects_text_and_a_hint_shows_on_hover() {
        let d = tmp("sel");
        let m = Manifest { id: "sel-test".into(), name: "S".into(), version: String::new(), description: String::new(), icon: Icon::plain("S"), run: vec!["x".into()], permissions: Vec::new(), commands: Vec::new(), build: Vec::new(), api: Vec::new(), dir: d.clone() };
        let mut app = App::new(d.clone(), None);
        let mut e = Ext::new(m);
        e.main.push(Pane { id: "t".into(), rows: rows(&json!(["hello world", ["ab", {"text": "cd", "hint": "a card\nof two lines"}]])).unwrap(), ..Pane::default() });
        app.exts.list.push(e);
        // From "world" on the first row to "ab" on the second, as shown.
        app.exts.sel = Some(Sel { ext: 0, tab: 0, a: (0, 6), b: (1, 1), dragging: false });
        assert_eq!(selected(&app).as_deref(), Some("world\nab"));
        // The pointer on "cd": its card.
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 4)).unwrap();
        let mut card = None;
        t.draw(|f| {
            let pane = &mut app.exts.list[0].main[0];
            card = draw_rows(f, Rect::new(0, 0, 30, 4), pane, 0, Slot::Main, 0, Some(Position::new(3, 1)), &mut Vec::new(), P.editor_bg, P.editor_fg);
        })
        .unwrap();
        assert_eq!(card.map(|(r, t)| (r.x, r.y, t)), Some((2, 1, "a card\nof two lines".to_string())));
        // A button in a row that clicks: the button's click, the rest of the row the row's.
        let mut pane = Pane { rows: rows(&json!([{"cells": ["item ", {"text": "Go", "button": true, "action": "go"}], "action": "open"}])).unwrap(), ..Pane::default() };
        let mut hits = Vec::new();
        t.draw(|f| {
            let _ = draw_rows(f, Rect::new(0, 0, 30, 4), &mut pane, 0, Slot::Left, 0, None, &mut hits, P.editor_bg, P.editor_fg);
        })
        .unwrap();
        let at = |x: u16| hits.iter().rev().find(|h| h.r.contains(Position::new(x, 0))).map(|h| h.action.clone());
        assert_eq!((at(6), at(1)), (Some("go".to_string()), Some("open".to_string())));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_extension_may_what_its_install_granted_and_asks_for_more() {
        let gate = crate::api::Gate::default();
        let c = Client { ext: Some("x".into()), granted: vec![Tier::Edit], ..Client::default() };
        assert_eq!(gate.check("off", &c, Tier::Read), Some(true), "its views work with the API off");
        assert_eq!(gate.check("off", &c, Tier::Edit), Some(true));
        assert_eq!(gate.check("ask", &c, Tier::Run), None, "not granted: asked");
        assert_eq!(gate.check("off", &c, Tier::Run), Some(false));
    }
}
