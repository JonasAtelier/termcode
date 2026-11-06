//! Go to Definition (F12, Ctrl+click), Peek Definition (Alt+F12), Find All References
//! (Shift+F12) and Go Back / Forward, VS Code style, on top of `nav`. The event loop
//! routes every key and mouse event through `on_key` / `on_mouse`, `App::drain` calls
//! `poll`, and `ui::draw` calls `draw` after the editors.

use crate::editor::{self, Buffer, Pos};
use crate::nav::{self, Kind, Location, Mark, NavHistory, Pending, Query};
use crate::panel::{App, Doc, Focus};
use crate::theme::{self, Rgba, P};
use crate::ui;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::Clear;
use serde_json::{Map, Value};
use std::ops::Range;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

/// `editor.gotoLocation.multipleDefinitions`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum GotoMode {
    #[default]
    Peek,
    GotoAndPeek,
    Goto,
}

pub fn goto_mode(v: Option<&Value>) -> GotoMode {
    match v.and_then(Value::as_str) {
        Some("gotoAndPeek") => GotoMode::GotoAndPeek,
        Some("goto") => GotoMode::Goto,
        _ => GotoMode::Peek,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Definition,
    Peek,
    References,
    Back,
    Forward,
}

struct Req {
    act: Act,
    sym: String,
    pending: Pending,
    /// A root below its repo (`herdr-code drivers/net`): the same lookup over the whole
    /// repo, run when the root has no definition, in repo-relative paths.
    repo: Option<Query>,
    /// Results in repo-relative paths (the repo-wide run), and the root's declarations
    /// kept for when the repo has no definition either.
    in_repo: bool,
    decls: Vec<Location>,
}

pub struct Peek {
    /// "2 definitions", "4 references".
    pub title: String,
    pub sym: String,
    pub items: Vec<Location>,
    pub sel: usize,
    pub off: usize,
    /// First preview line (0-based); None keeps the selected target in view.
    pub top: Option<usize>,
    /// The first preview line last drawn.
    shown_top: usize,
    /// Files not open in an editor, loaded for the preview.
    files: Vec<(String, Buffer)>,
}

/// Peek areas from the last draw, for hit-testing.
#[derive(Default, Clone, Copy, Debug)]
pub struct PeekHits {
    pub pop: Rect,
    pub list: Rect,
    pub preview: Rect,
}

#[derive(Default)]
pub struct Nav {
    req: Option<Req>,
    pub history: NavHistory,
    pub peek: Option<Peek>,
    /// Doc key and 0-based line highlighted for a moment after a jump.
    pub flash: Option<(String, usize, Instant)>,
    /// Ctrl+hover: group, 0-based line and byte range drawn as a link.
    pub link: Option<(usize, usize, Range<usize>)>,
    pub hits: PeekHits,
}

impl Nav {
    /// The event loop wakes often while a lookup runs or a flash fades.
    pub fn busy(&self) -> bool {
        self.req.is_some() || self.flash.is_some()
    }
}

/// The active group's text document: key, buffer, view.
fn active(app: &App) -> Option<(&str, &Buffer, &editor::View)> {
    let g = app.groups.get(app.ag)?;
    let t = g.tabs.get(g.active)?;
    match app.docs.get(&t.key)? {
        Doc::Text(b) => Some((t.key.as_str(), b, &t.view)),
        Doc::Diff(_) => None,
    }
}

/// The cursor as a history mark (`Location` convention: 1-based line, byte column).
fn here(app: &App) -> Option<Mark> {
    let (key, b, v) = active(app)?;
    let c = b.clamp(v.cursor);
    Some((key.strip_prefix("file:")?.to_string(), c.line + 1, c.col + 1))
}

/// Show the place a jump lands on for a moment, like VS Code's range highlight.
const FLASH: Duration = Duration::from_millis(800);
/// Preview buffers kept for files not open in an editor.
const MAX_PREVIEW_FILES: usize = 8;

/// Dark registry defaults of VS Code 1.106 for the keys drawn here (the theme wins via
/// `theme::lookup`); a value naming a key falls back to that key, as in VS Code.
const DEFAULTS: &[(&str, &str)] = &[
    ("peekView.border", "editorInfo.foreground"),
    ("editorInfo.foreground", "#59a4f9"),
    ("peekViewTitle.background", "#252526"),
    ("peekViewTitleLabel.foreground", "#FFFFFF"),
    ("peekViewTitleDescription.foreground", "#ccccccb3"),
    ("peekViewResult.background", "#252526"),
    ("peekViewResult.fileForeground", "#FFFFFF"),
    ("peekViewResult.selectionBackground", "#3399ff33"),
    ("peekViewResult.selectionForeground", "#FFFFFF"),
    ("peekViewEditor.background", "#001F33"),
    ("peekViewEditorGutter.background", "peekViewEditor.background"),
    ("peekViewEditor.matchHighlightBackground", "#ff8f0099"),
    ("editor.rangeHighlightBackground", "#ffffff0b"),
    ("editorLink.activeForeground", "#4E94CE"),
];

fn color(theme: &Map<String, Value>, key: &str, depth: u8) -> Option<Rgba> {
    theme::lookup(theme, key).or_else(|| match DEFAULTS.iter().find(|d| d.0 == key)?.1 {
        d if d.starts_with('#') => Rgba::parse(d),
        d if depth < 4 => color(theme, d, depth + 1),
        _ => None,
    })
}

/// The colours of one draw.
struct Cols {
    border: Color,
    title_bg: Color,
    title_fg: Color,
    dim: Rgba,
    result_bg: Color,
    file_fg: Color,
    sel_bg: Color,
    sel_fg: Color,
    peek_bg: Color,
    gutter_bg: Color,
    target: Color,
    matched: Color,
    range: Rgba,
    link: Color,
}

impl Cols {
    fn new(app: &App) -> Cols {
        let empty = Map::new();
        let t = app.settings.theme.json.get("colors").and_then(Value::as_object).unwrap_or(&empty);
        let c = |k: &str| color(t, k, 0).unwrap_or(Rgba(255, 0, 255, 255));
        let ed = P.editor_bg;
        let (title_bg, result_bg, peek_bg) = (c("peekViewTitle.background").over(ed), c("peekViewResult.background").over(ed), c("peekViewEditor.background").over(ed));
        let sel_bg = c("peekViewResult.selectionBackground").over(result_bg);
        let range = c("editor.rangeHighlightBackground");
        Cols {
            border: c("peekView.border").over(ed),
            title_bg,
            title_fg: c("peekViewTitleLabel.foreground").over(title_bg),
            dim: c("peekViewTitleDescription.foreground"),
            result_bg,
            file_fg: c("peekViewResult.fileForeground").over(result_bg),
            sel_bg,
            sel_fg: c("peekViewResult.selectionForeground").over(sel_bg),
            peek_bg,
            gutter_bg: c("peekViewEditorGutter.background").over(ed),
            target: range.over(peek_bg),
            matched: c("peekViewEditor.matchHighlightBackground").over(peek_bg),
            range,
            link: c("editorLink.activeForeground").over(ed),
        }
    }
}

/// Start a lookup for the identifier at the cursor; a running one is cancelled.
fn request(app: &mut App, act: Act) {
    app.nav.req = None; // dropping a Pending cancels it
    let Some((key, b, v)) = active(app) else { return };
    let Some(path) = key.strip_prefix("file:").map(String::from) else { return };
    let c = b.clamp(v.cursor);
    let Some((sym, _)) = nav::symbol_at(&b.lines[c.line], c.col) else {
        app.msg = "No symbol at the cursor".into();
        return;
    };
    let defs = |from_file: String| Query::Definitions { from_file, from_line: c.line + 1, buffer: b.lines.join("\n") };
    let (query, repo) = match act {
        Act::References => (Query::References, None),
        _ => {
            let below = app.git_root.as_ref().is_some_and(|g| *g != app.root);
            (defs(path.clone()), below.then(|| app.repo_path(&path)).flatten().map(defs))
        }
    };
    let pending = nav::spawn(app.root.clone(), sym.clone(), nav::lang_for(&path), query, app.live_rg.clone());
    app.msg = if act == Act::References { "Searching references…" } else { "Searching definitions…" }.into();
    app.nav.req = Some(Req { act, sym, pending, repo, in_repo: false, decls: Vec::new() });
}

/// Take a finished lookup and expire the flash; true when the screen changed.
pub fn poll(app: &mut App) -> bool {
    let mut changed = false;
    if app.nav.flash.as_ref().is_some_and(|f| f.2.elapsed() >= FLASH) {
        app.nav.flash = None;
        changed = true;
    }
    let res = match app.nav.req.as_ref().map(|r| r.pending.rx.try_recv()) {
        None | Some(Err(TryRecvError::Empty)) => return changed,
        Some(Ok(res)) => res,
        Some(Err(TryRecvError::Disconnected)) => Err("lookup failed".into()),
    };
    if let Some(r) = app.nav.req.take() {
        finish(app, r, res);
    }
    true
}

fn finish(app: &mut App, r: Req, res: Result<Vec<Location>, String>) {
    let Req { act, sym, repo, in_repo, decls, .. } = r;
    let mut v = match res {
        Ok(v) => v,
        Err(e) => {
            app.msg = e;
            return;
        }
    };
    app.msg.clear();
    // Focus moved on (Find, Search, the commit box, a dialog): no jump or peek over it.
    if app.focus != Focus::Editor || app.prompt.is_some() || app.quick.is_some() {
        return;
    }
    if in_repo {
        for l in &mut v {
            l.path = app.doc_path(&l.path);
        }
    }
    let defined = |v: &[Location]| v.iter().any(|l| l.kind != Kind::Decl);
    // Nothing defined under a subfolder root: look in the whole repo, the way include/
    // headers resolve for a driver directory. Same cap, same cancel.
    if let (false, Some(q)) = (defined(&v), repo) {
        let lang = nav::lang_for(&sym_path(&q));
        let root = app.git_root.clone().unwrap_or_else(|| app.root.clone());
        let pending = nav::spawn(root, sym.clone(), lang, q, app.live_rg.clone());
        app.msg = "Searching definitions in the repo…".into();
        app.nav.req = Some(Req { act, sym, pending, repo: None, in_repo: true, decls: v });
        return;
    }
    if !defined(&v) && v.is_empty() {
        v = decls;
    }
    // Like cpptools, Go to Definition skips the declarations when there is a definition.
    if act != Act::References && defined(&v) {
        v.retain(|l| l.kind != Kind::Decl);
    }
    let (n, what) = (v.len(), if act == Act::References { "reference" } else { "definition" });
    if n == 0 {
        app.msg = format!("No {what}s found for '{sym}'");
        return;
    }
    let title = format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    match (act, app.settings.goto_multiple) {
        (Act::Definition, _) if n == 1 => jump(app, &v[0]),
        (Act::Definition, GotoMode::Goto) => jump(app, &v[0]),
        (Act::Definition, GotoMode::GotoAndPeek) => {
            jump(app, &v[0]);
            open_peek(app, &sym, title, v);
        }
        _ => open_peek(app, &sym, title, v),
    }
}

/// The file a definitions query was asked from (its language picks the rg types).
fn sym_path(q: &Query) -> String {
    match q {
        Query::Definitions { from_file, .. } => from_file.clone(),
        Query::References => String::new(),
    }
}

/// Go to `l` in the active group; the place left is recorded for Go Back.
fn jump(app: &mut App, l: &Location) {
    if let Some(m) = here(app) {
        app.nav.history.push(m);
    }
    open_at(app, &l.path, l.line, l.col, true);
}

fn open_at(app: &mut App, path: &str, line: usize, col: usize, flash: bool) {
    app.open_file(app.ag, path, false);
    app.focus = Focus::Editor;
    let rows = app.ui.groups.get(app.ag).map_or(20, |g| g.text.height.max(1) as usize);
    let mut at = None;
    if let Some((b, tab)) = app.ed() {
        let p = b.clamp(Pos::new(line.saturating_sub(1), col.saturating_sub(1)));
        let v = &mut tab.view;
        (v.cursor, v.anchor, v.want_x) = (p, None, None);
        // VS Code's revealRangeInCenterIfOutsideViewport.
        if p.line < v.scroll || p.line >= v.scroll + rows {
            v.scroll = p.line.saturating_sub(rows / 2);
        }
        at = Some(p.line);
    }
    app.reveal();
    if let (true, Some(line)) = (flash, at) {
        app.nav.flash = Some((format!("file:{path}"), line, Instant::now()));
    }
}

fn go(app: &mut App, back: bool) {
    let Some(cur) = here(app) else { return };
    let h = &mut app.nav.history;
    if let Some((path, line, col)) = if back { h.back(cur) } else { h.forward(cur) } {
        open_at(app, &path, line, col, false);
    }
}

/// F1 palette entries and their keys.
pub fn command(app: &mut App, a: Act) {
    match a {
        Act::Back | Act::Forward => {
            app.nav.peek = None;
            go(app, a == Act::Back);
        }
        _ => request(app, a),
    }
}

pub fn open_peek(app: &mut App, sym: &str, title: String, items: Vec<Location>) {
    app.nav.peek = Some(Peek { title, sym: sym.into(), items, sel: 0, off: 0, top: None, shown_top: 0, files: Vec::new() });
}

fn open_item(app: &mut App, i: usize) {
    if let Some(l) = app.nav.peek.take().and_then(|mut pk| (i < pk.items.len()).then(|| pk.items.swap_remove(i))) {
        jump(app, &l);
    }
}

/// Every key goes through here: the nav keys, then the open peek, then the panel.
pub fn on_key(app: &mut App, k: KeyEvent) {
    if app.prompt.is_some() || app.quick.is_some() || !nav_key(app, k) {
        app.on_key(k);
    }
}

fn nav_key(app: &mut App, k: KeyEvent) -> bool {
    let m = k.modifiers;
    let (ctrl, alt, shift) = (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::ALT), m.contains(KeyModifiers::SHIFT));
    app.nav.link = None; // no key releases in a terminal: any key ends a Ctrl+hover
    let act = match k.code {
        KeyCode::F(12) if !ctrl && !alt => Some(if shift { Act::References } else { Act::Definition }),
        KeyCode::F(12) if alt && !ctrl && !shift => Some(Act::Peek),
        KeyCode::Left if alt && !ctrl && !shift => Some(Act::Back),
        KeyCode::Right if alt && !ctrl && !shift => Some(Act::Forward),
        // VS Code on Linux: Ctrl+Alt+- back, Ctrl+Shift+- forward. Legacy terminals send
        // Ctrl+- and Ctrl+_ as 0x1f, which crossterm reads as Ctrl+7.
        KeyCode::Char('-' | '7') if ctrl && alt => Some(Act::Back),
        KeyCode::Char('-' | '_') if ctrl && shift => Some(Act::Forward),
        KeyCode::Char('_' | '7') if ctrl => Some(Act::Forward),
        _ => None,
    };
    let typing = matches!(app.focus, Focus::Find | Focus::SearchInput | Focus::CommitInput);
    match act {
        Some(a @ (Act::Back | Act::Forward)) if !typing => return command_true(app, a),
        Some(a) if app.focus == Focus::Editor => return command_true(app, a),
        _ => {}
    }
    // The peek has the keyboard only while the editor does (a click into Search or the
    // commit box types there).
    if app.nav.peek.is_some() && app.focus == Focus::Editor {
        return peek_key(app, k, ctrl || alt);
    }
    if k.code == KeyCode::Esc && app.nav.req.take().is_some() {
        app.msg.clear();
        return true;
    }
    false
}

fn command_true(app: &mut App, a: Act) -> bool {
    command(app, a);
    true
}

/// The open peek has the keyboard; shortcuts with Ctrl or Alt still reach the panel.
fn peek_key(app: &mut App, k: KeyEvent, modded: bool) -> bool {
    let rows = app.nav.hits.list.height.max(1) as usize;
    let Some(pk) = app.nav.peek.as_mut() else { return false };
    let last = pk.items.len().saturating_sub(1);
    match k.code {
        KeyCode::Esc => app.nav.peek = None,
        KeyCode::Enter => {
            let i = pk.sel;
            open_item(app, i);
        }
        KeyCode::Up => pk.sel = pk.sel.saturating_sub(1),
        KeyCode::Down => pk.sel = (pk.sel + 1).min(last),
        KeyCode::PageUp => pk.sel = pk.sel.saturating_sub(rows),
        KeyCode::PageDown => pk.sel = (pk.sel + rows).min(last),
        KeyCode::Home => pk.sel = 0,
        KeyCode::End => pk.sel = last,
        _ => return !modded,
    }
    if let Some(pk) = app.nav.peek.as_mut() {
        pk.top = None;
    }
    true
}

/// Every mouse event goes through here: Ctrl+hover, the peek, Ctrl+click, then the panel.
pub fn on_mouse(app: &mut App, m: MouseEvent) -> bool {
    if app.prompt.is_some() || app.quick.is_some() {
        return app.on_mouse(m);
    }
    let p = Position::new(m.column, m.row);
    let ctrl = m.modifiers.contains(KeyModifiers::CONTROL);
    let link = (ctrl && m.kind == MouseEventKind::Moved).then(|| symbol_under(app, p)).flatten().map(|(g, pos, r, _)| (g, pos.line, r));
    let relink = link != app.nav.link;
    app.nav.link = link;
    if let Some(redraw) = peek_mouse(app, m, p) {
        return redraw || relink;
    }
    if ctrl && m.kind == MouseEventKind::Down(MouseButton::Left) {
        if let Some((g, pos, ..)) = symbol_under(app, p) {
            (app.ag, app.focus) = (g, Focus::Editor);
            if let Some((_, tab)) = app.ed() {
                (tab.view.cursor, tab.view.anchor) = (pos, None);
            }
            request(app, Act::Definition);
            return true;
        }
    }
    app.on_mouse(m) || relink
}

/// None: not the peek's event (a click outside closes it and goes on to what is under it).
fn peek_mouse(app: &mut App, m: MouseEvent, p: Position) -> Option<bool> {
    let h = app.nav.hits;
    let item = list_item(app, p);
    let pk = app.nav.peek.as_mut()?;
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(i) = item.or_else(|| h.preview.contains(p).then_some(pk.sel)) {
                open_item(app, i);
            } else if !h.pop.contains(p) {
                app.nav.peek = None;
                return None;
            }
            Some(true)
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if h.pop.contains(p) => {
            let d: isize = if m.kind == MouseEventKind::ScrollUp { -3 } else { 3 };
            if h.list.contains(p) {
                pk.sel = (pk.sel as isize + d).clamp(0, pk.items.len() as isize - 1) as usize;
                pk.top = None;
            } else {
                pk.top = Some((pk.shown_top as isize + d).max(0) as usize);
            }
            Some(true)
        }
        MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) if h.pop.contains(p) => Some(false),
        _ => None,
    }
}

/// The peek list item drawn at `p` (the same rows `draw` used).
pub fn list_item(app: &App, p: Position) -> Option<usize> {
    let (pk, l) = (app.nav.peek.as_ref()?, app.nav.hits.list);
    let i = pk.off + p.y.checked_sub(l.y)? as usize;
    (l.contains(p) && i < pk.items.len()).then_some(i)
}

/// Byte offset of the char drawn over display column `x`: a wide char's or a tab's
/// later cells are that char (`editor::byte_at_display` rounds to the nearer edge).
fn byte_under(line: &str, x: usize) -> usize {
    let mut cx = 0;
    for (i, c) in line.char_indices() {
        cx = editor::advance(cx, c);
        if x < cx {
            return i;
        }
    }
    line.len()
}

/// The identifier drawn at cell `p` of an editor: group, cursor position, byte range, name.
pub fn symbol_under(app: &App, p: Position) -> Option<(usize, Pos, Range<usize>, String)> {
    let g = app.ui.groups.iter().position(|g| g.text.contains(p))?;
    let gh = &app.ui.groups[g];
    let grp = app.groups.get(g)?;
    let tab = grp.tabs.get(grp.active)?;
    let Some(Doc::Text(b)) = app.docs.get(&tab.key) else { return None };
    // The editor's own row mapping (it skips an open quick-diff zone); a row past the end
    // of the file maps to no line here.
    let line = app.line_at(g, p.y).filter(|&l| app.line_y(g, l) == Some(p.y))?;
    let text = b.lines.get(line)?;
    let byte = byte_under(text, tab.view.hscroll + (p.x - gh.text.x) as usize);
    // The cell itself must be part of the name (the keyboard's symbol_at also takes the end).
    if !text.as_bytes().get(byte).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
        return None;
    }
    let (name, r) = nav::symbol_at(text, byte)?;
    Some((g, Pos::new(line, byte), r, name))
}

fn split(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((d, n)) => (n.to_string(), d.to_string()),
        None => (path.to_string(), String::new()),
    }
}

/// After the editors: the jump flash, the Ctrl+hover link, the peek.
pub fn draw(app: &mut App, f: &mut Frame) {
    app.nav.hits = PeekHits::default();
    if app.nav.flash.is_none() && app.nav.link.is_none() && app.nav.peek.is_none() {
        return;
    }
    let c = Cols::new(app);
    let area = f.area();
    // The flash: a range highlight over whatever the line already shows.
    if let Some((key, line, _)) = &app.nav.flash {
        for (g, gh) in app.ui.groups.iter().enumerate() {
            let Some(t) = app.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)) else { continue };
            let Some(y) = app.line_y(g, *line).filter(|_| t.key == *key) else { continue };
            let r = Rect::new(gh.text.x, y, gh.text.width, 1).intersection(area);
            for x in r.left()..r.right() {
                let cell = &mut f.buffer_mut()[(x, r.y)];
                let bg = c.range.over(cell.bg);
                cell.set_bg(bg);
            }
        }
    }
    if let Some((g, line, r)) = &app.nav.link {
        link(app, f, *g, *line, r.clone(), c.link);
    }
    peek(app, f, &c);
}

fn link(app: &App, f: &mut Frame, g: usize, line: usize, r: Range<usize>, fg: Color) -> Option<()> {
    let (gh, grp) = (app.ui.groups.get(g)?, app.groups.get(g)?);
    let t = grp.tabs.get(grp.active)?;
    let Some(Doc::Text(b)) = app.docs.get(&t.key) else { return None };
    let (text, y) = (b.lines.get(line)?, app.line_y(g, line)?);
    let area = gh.text.intersection(f.area());
    let st = Style::new().fg(fg).add_modifier(Modifier::UNDERLINED);
    for col in editor::display_col(text, r.start).max(t.view.hscroll)..editor::display_col(text, r.end) {
        let x = gh.text.x as usize + col - t.view.hscroll;
        if x >= area.right() as usize || y >= area.bottom() {
            break;
        }
        f.buffer_mut()[(x as u16, y)].set_style(st);
    }
    Some(())
}

/// VS Code's inline peek: under the cursor line (else at the editor's bottom), a title
/// row, the preview on the left and the result list on the right.
fn peek(app: &mut App, f: &mut Frame, c: &Cols) {
    let Some(gh) = app.ui.groups.get(app.ag).cloned() else { return };
    let (comfy, sp) = (!app.settings.compact, app.settings.spacing);
    // Under the cursor's row as the editor drew it (a quick-diff zone may sit above).
    let cur = active(app).and_then(|(_, b, v)| app.line_y(app.ag, b.clamp(v.cursor).line));
    let text = gh.text;
    let want = (text.height * 6 / 10).clamp(8, 20).min(text.height);
    let y = match cur.map(|r| r.saturating_add(1)) {
        Some(y) if y.saturating_add(want) <= text.bottom() => y,
        _ => text.bottom() - want,
    };
    let pop = Rect::new(gh.area.x, y, gh.area.width, want).intersection(f.area());
    let Some(pk) = app.nav.peek.as_mut() else { return };
    let Some(sel) = pk.items.get(pk.sel).cloned() else { return };
    if pop.height < 3 + 2 * u16::from(comfy) || pop.width < 12 {
        // Too small to show: close it rather than keep an invisible peek that takes keys.
        app.nav.peek = None;
        app.msg = "The editor is too small for the peek".into();
        return;
    }
    f.render_widget(Clear, pop);
    let mut inner = pop;
    if comfy {
        // VS Code draws the zone's top and bottom edges in peekView.border.
        for ry in [pop.y, pop.bottom() - 1] {
            ui::row(f, Rect::new(pop.x, ry, pop.width, 1), P.editor_bg, vec![Span::styled("─".repeat(pop.width as usize), Style::new().fg(c.border))]);
        }
        inner = Rect::new(pop.x, pop.y + 1, pop.width, pop.height - 2);
    }
    let pad = sp.sidebar_padding;
    let title = Rect::new(inner.x, inner.y, inner.width, 1);
    let (name, dir) = split(&sel.path);
    let dim = |bg: Color| Style::new().fg(c.dim.over(bg));
    ui::row(f, title, c.title_bg, vec![Span::styled(format!("{}{name}", ui::sp(pad)), Style::new().fg(c.title_fg)), Span::styled(format!("  {dir}"), dim(c.title_bg))]);
    ui::right(f, title, vec![Span::styled(format!("  {}{}", pk.title, ui::sp(pad)), dim(c.title_bg).bg(c.title_bg))]);
    let body = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    let lw = (body.width * 3 / 10).clamp(16.min(body.width / 2), 60);
    let list = Rect::new(body.right() - lw, body.y, lw, body.height);
    let preview = Rect::new(body.x, body.y, body.width - lw - 1, body.height);
    // The result list.
    ui::fill(f, list, c.result_bg);
    let rows = list.height as usize;
    if pk.sel < pk.off {
        pk.off = pk.sel;
    } else if pk.sel >= pk.off + rows {
        pk.off = pk.sel + 1 - rows;
    }
    for (k, l) in pk.items.iter().enumerate().skip(pk.off).take(rows) {
        let r = Rect::new(list.x, list.y + (k - pk.off) as u16, list.width, 1);
        let (bg, fg) = if k == pk.sel { (c.sel_bg, c.sel_fg) } else { (c.result_bg, c.file_fg) };
        let (name, dir) = split(&l.path);
        ui::row(f, r, bg, vec![Span::styled(format!("{}{name}:{}", ui::sp(pad), l.line), Style::new().fg(fg)), Span::styled(format!("  {dir}"), dim(bg))]);
    }
    for yy in body.top()..body.bottom() {
        f.buffer_mut()[(list.x - 1, yy)].set_symbol("│").set_fg(c.border).set_bg(c.result_bg);
    }
    // The preview: the open buffer (unsaved edits included), else the file from disk.
    let key = format!("file:{}", sel.path);
    let b: &mut Buffer = match app.docs.get_mut(&key) {
        Some(Doc::Text(b)) => b,
        _ => {
            let i = match pk.files.iter().position(|(p, _)| *p == sel.path) {
                Some(i) => i,
                None => {
                    if pk.files.len() >= MAX_PREVIEW_FILES {
                        pk.files.remove(0);
                    }
                    pk.files.push((sel.path.clone(), Buffer::open(&app.root.join(&sel.path))));
                    pk.files.len() - 1
                }
            };
            &mut pk.files[i].1
        }
    };
    let (target, rows) = (sel.line.saturating_sub(1), preview.height as usize);
    let top = pk.top.unwrap_or(target.saturating_sub(rows / 3)).min(b.lines.len().saturating_sub(1));
    pk.top = pk.top.map(|_| top);
    pk.shown_top = top;
    b.highlight_upto(top + rows);
    let gw = b.lines.len().max(1).to_string().len() as u16 + 1;
    let gutter = (gw + sp.editor_padding.max(1)).min(preview.width);
    let code = Rect::new(preview.x + gutter, preview.y, preview.width - gutter, preview.height);
    ui::fill(f, preview, c.peek_bg);
    ui::fill(f, Rect::new(preview.x, preview.y, gutter, preview.height), c.gutter_bg);
    for (k, li) in (top..b.lines.len()).take(rows).enumerate() {
        let y = preview.y + k as u16;
        let nr = if li == target { P.line_nr_active } else { P.line_nr };
        f.render_widget(Span::styled(format!("{:>w$}", li + 1, w = gw as usize), Style::new().fg(nr).bg(c.gutter_bg)), Rect::new(preview.x, y, gutter, 1));
        let r = Rect::new(code.x, y, code.width, 1);
        let mut base = Style::new().fg(P.editor_fg).bg(c.peek_bg);
        let mut ov = Vec::new();
        if li == target {
            ui::fill(f, r, c.target);
            base = base.bg(c.target);
            let s = sel.col.saturating_sub(1);
            ov.push((s, s + pk.sym.len(), c.matched));
        }
        f.render_widget(ui::render_line(&b.lines[li], b.segs(li), &ov, None, 0, code.width as usize, base), r);
    }
    app.nav.hits = PeekHits { pop, list, preview };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{base, Side};
    use ratatui::crossterm::event::Event;
    use crate::settings::Spacing;
    use ratatui::backend::TestBackend;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    const DSA_H: &str = "int dsa_port_change_master(struct dsa_port *dp,\n\t\t\t   int x);\nint twice(int);\nstruct dsa_port {\n\tint index;\n};\n";
    const PORT_C: &str = "int\ndsa_port_change_master(struct dsa_port *dp,\n\t\t       int x)\n{\n\treturn x;\n}\n";
    const A_C: &str = "int twice(int v)\n{\n\treturn 2 * v;\n}\n";
    const B_C: &str = "int twice(int v)\n{\n\treturn v + v;\n}\n";
    const USER_C: &str = "#include \"dsa.h\"\nint user(void)\n{\n\t/* 中文 */\tdsa_port_change_master(0, twice(1));\n\treturn dsa_port_change_master(1, 2);\n}\nstruct dsa_port *last;\n";
    /// 0-based line of the call in user.c.
    const CALL: usize = 3;

    fn app(tag: &str) -> (App, PathBuf) {
        let d = std::env::temp_dir().join(format!("herdr-code-navui-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for (p, t) in [("inc/dsa.h", DSA_H), ("net/port.c", PORT_C), ("net/a.c", A_C), ("net/b.c", B_C), ("net/user.c", USER_C)] {
            let f = d.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, t).unwrap();
        }
        assert!(Command::new("git").args(["init", "-q"]).arg(&d).status().unwrap().success());
        let root = d.canonicalize().unwrap();
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "net/user.c", false);
        app.focus = Focus::Editor;
        (app, root)
    }

    fn term(w: u16, h: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(w, h)).unwrap()
    }

    fn draw(app: &mut App, t: &mut Terminal<TestBackend>) {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
    }

    /// The symbols of one screen row inside `r`.
    fn text(t: &Terminal<TestBackend>, y: u16, r: Rect) -> String {
        (r.x..r.right()).map(|x| t.backend().buffer()[(x, y)].symbol()).collect()
    }

    fn press(app: &mut App, code: KeyCode, m: KeyModifiers) {
        on_key(app, KeyEvent::new(code, m));
    }

    fn mouse(app: &mut App, kind: MouseEventKind, p: Position, modifiers: KeyModifiers) {
        on_mouse(app, MouseEvent { kind, column: p.x, row: p.y, modifiers });
    }

    fn click(app: &mut App, p: Position, m: KeyModifiers) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), p, m);
        mouse(app, MouseEventKind::Up(MouseButton::Left), p, m);
    }

    /// Let the lookup finish (the panel's drain polls every 100 ms).
    fn finish(app: &mut App) {
        let t = Instant::now();
        while app.nav.req.is_some() && t.elapsed() < Duration::from_secs(5) {
            poll(app);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.nav.req.is_none(), "lookup did not finish");
    }

    fn file(app: &App) -> String {
        here(app).unwrap().0
    }

    fn cursor(app: &App) -> (usize, usize) {
        let m = here(app).unwrap();
        (m.1, m.2)
    }

    /// Put the cursor on `word` in line `line` (0-based) of the active editor.
    fn put(app: &mut App, line: usize, word: &str) {
        let (b, tab) = app.ed().unwrap();
        tab.view.cursor = Pos::new(line, b.lines[line].find(word).unwrap());
    }

    /// The screen cell of byte `byte` of line `line` in the active editor (after a draw).
    /// From the drawn layout: the text rect's origin and the editor's row mapping.
    fn cell(app: &App, line: usize, byte: usize) -> Position {
        let (_, b, v) = active(app).unwrap();
        let gh = &app.ui.groups[app.ag];
        let x = gh.text.x as usize + editor::display_col(&b.lines[line], byte) - v.hscroll;
        assert!(x < gh.text.right() as usize, "byte {byte} of line {line} is off screen ({:?})", gh.text);
        Position::new(x as u16, app.line_y(app.ag, line).expect("line on screen"))
    }

    /// A terminal wide enough to show the whole call line, measured on the drawn layout
    /// (gutter, minimap and scrollbar widths are the editor's business).
    fn fitting_term(app: &mut App) -> Terminal<TestBackend> {
        let need = editor::display_col(USER_C.lines().nth(CALL).unwrap(), usize::MAX) as u16 + 1;
        let mut w = 100;
        loop {
            let mut t = term(w, 30);
            draw(app, &mut t);
            let have = app.ui.groups[app.ag].text.width;
            if have >= need {
                return t;
            }
            w += need - have; // a wider editor may also widen the minimap: measure again
            assert!(w < 400, "the call line never fits");
        }
    }

    fn screen(t: &Terminal<TestBackend>) -> String {
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// A key through `App::step`, the event loop's own turn: the key as the first input,
    /// then none, until the loop would go idle (wait for input up to 5 s). Returns the
    /// screen it leaves then: whatever the key started must be drawn without more input.
    fn turn(app: &mut App, t: &mut Terminal<TestBackend>, code: KeyCode, m: KeyModifiers) -> String {
        let mut key = Some(Event::Key(KeyEvent::new(code, m)));
        let (start, mut dirty) = (Instant::now(), true);
        loop {
            let first = key.is_some();
            let wait = app
                .step(t, &mut dirty, |w| {
                    if key.is_none() {
                        std::thread::sleep(w.min(Duration::from_millis(20)));
                    }
                    Ok(key.take())
                })
                .unwrap();
            // Idle with no input: the real loop now sleeps up to 5 s, so nothing may be
            // left undrawn (a lookup's peek once waited out that timeout this way).
            if !first && wait >= Duration::from_secs(1) {
                assert!(!dirty, "the loop waits for input with a change not on screen yet");
                return screen(t);
            }
            assert!(start.elapsed() < Duration::from_secs(10), "the loop never went idle");
        }
    }

    #[test]
    fn e2e_live_sequence_peeks_after_jumps() {
        let (mut app, root) = app("e2e");
        let mut t = term(120, 40);
        draw(&mut app, &mut t);
        let none = KeyModifiers::NONE;
        put(&mut app, CALL, "dsa_port");
        let s = turn(&mut app, &mut t, KeyCode::F(12), none);
        assert!(s.lines().next().unwrap().contains("port.c —"), "F12 jumps:\n{s}");
        turn(&mut app, &mut t, KeyCode::Left, KeyModifiers::ALT);
        turn(&mut app, &mut t, KeyCode::Right, KeyModifiers::ALT);
        turn(&mut app, &mut t, KeyCode::Char('7'), KeyModifiers::CONTROL | KeyModifiers::ALT);
        turn(&mut app, &mut t, KeyCode::Char('7'), KeyModifiers::CONTROL);
        app.open_file(0, "net/user.c", false);
        put(&mut app, CALL + 1, "dsa_port");
        let s = turn(&mut app, &mut t, KeyCode::F(12), KeyModifiers::ALT);
        assert!(s.contains("1 definition"), "Alt+F12 peeks:\n{s}");
        turn(&mut app, &mut t, KeyCode::Esc, none);
        put(&mut app, CALL, "twice");
        let s = turn(&mut app, &mut t, KeyCode::F(12), none);
        assert!(s.contains("2 definitions") && s.contains("b.c:1"), "F12 peeks two:\n{s}");
        turn(&mut app, &mut t, KeyCode::Esc, none);
        let s = turn(&mut app, &mut t, KeyCode::F(12), KeyModifiers::SHIFT);
        assert!(s.contains("4 references"), "Shift+F12:\n{s}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_subfolder_root_finds_definitions_in_the_rest_of_its_repo() {
        let (_, repo) = app("sub");
        let mut app = App::new(repo.join("net"), None);
        app.open_file(0, "user.c", false);
        app.focus = Focus::Editor;
        let last = USER_C.lines().count() - 1;
        put(&mut app, last, "dsa_port");
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        finish(&mut app);
        // inc/dsa.h is outside the root: it opens by its absolute path.
        assert_eq!((file(&app), cursor(&app).0), (repo.join("inc/dsa.h").to_string_lossy().into_owned(), 4));
        // Under the root, definitions there win: no repo-wide search.
        put(&mut app, 0, "dsa");
        app.open_file(0, "user.c", false);
        put(&mut app, CALL, "dsa_port");
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        finish(&mut app);
        assert_eq!((file(&app).as_str(), cursor(&app).0), ("port.c", 2));
        fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn a_result_after_focus_left_the_editor_is_dropped_and_typing_reaches_the_input() {
        let (mut app, root) = app("focus");
        let mut t = term(120, 40);
        draw(&mut app, &mut t);
        put(&mut app, CALL, "twice");
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        app.show_side(Side::Search, true);
        finish(&mut app);
        assert!(app.nav.peek.is_none(), "no peek over the Search box");
        assert_eq!(file(&app), "net/user.c");
        // An open peek does not take what is typed into the Search box.
        app.focus = Focus::Editor;
        press(&mut app, KeyCode::F(12), KeyModifiers::ALT);
        finish(&mut app);
        assert!(app.nav.peek.is_some());
        app.show_side(Side::Search, true);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.query, "x");
        // A peek too small to draw closes instead of taking keys unseen.
        app.focus = Focus::Editor;
        let mut tiny = term(40, 7);
        draw(&mut app, &mut tiny);
        assert!(app.nav.peek.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ctrl_click_maps_cells_to_symbols_with_cjk_and_tabs() {
        let (mut app, root) = app("click");
        let _t = fitting_term(&mut app);
        let line = USER_C.lines().nth(CALL).unwrap();
        let at = |w: &str| line.find(w).unwrap();
        let name = |app: &App, p: Position| symbol_under(app, p).map(|s| s.3);
        let d = at("dsa_port");
        let last = d + "dsa_port_change_master".len() - 1;
        assert_eq!(name(&app, cell(&app, CALL, d)).as_deref(), Some("dsa_port_change_master"));
        assert_eq!(name(&app, cell(&app, CALL, last)).as_deref(), Some("dsa_port_change_master"));
        assert_eq!(name(&app, cell(&app, CALL, last + 1)), None, "the `(` after the name");
        let cjk = cell(&app, CALL, at("中"));
        assert_eq!(name(&app, cjk), None);
        assert_eq!(name(&app, Position::new(cjk.x + 1, cjk.y)), None, "right half of a wide char");
        // The tab between `*/` and the name spans several cells, none of them a symbol.
        let (tab, dcell) = (cell(&app, CALL, at("*/") + 2), cell(&app, CALL, d));
        assert!(dcell.x > tab.x + 1, "the tab is wider than one cell");
        for x in tab.x..dcell.x {
            assert_eq!(name(&app, Position::new(x, tab.y)), None, "tab cell {x}");
        }
        assert_eq!(name(&app, cell(&app, CALL, at("twice"))).as_deref(), Some("twice"));
        // Rows past the end of the file are no line (line_at would clamp them to the last one).
        let below = Position::new(dcell.x, app.line_y(0, USER_C.lines().count() - 1).unwrap() + 1);
        assert_eq!(name(&app, below), None);
        // An open quick-diff zone under line 1 pushes the call down; its own rows are no line.
        let zy = cell(&app, 1, 0).y + 1;
        app.ui.groups[0].zone = Some((zy, 2));
        assert_eq!(cell(&app, CALL, d).y, dcell.y + 2);
        assert_eq!(name(&app, cell(&app, CALL, d)).as_deref(), Some("dsa_port_change_master"));
        assert_eq!(name(&app, Position::new(dcell.x, zy)), None, "inside the zone");
        app.ui.groups[0].zone = None;
        // A plain click only moves the cursor; Ctrl+click goes to the definition.
        let tw = cell(&app, CALL, at("twice"));
        click(&mut app, tw, KeyModifiers::NONE);
        assert!(app.nav.req.is_none());
        click(&mut app, dcell, KeyModifiers::CONTROL);
        assert_eq!(app.msg, "Searching definitions…");
        finish(&mut app);
        assert_eq!((file(&app).as_str(), cursor(&app)), ("net/port.c", (2, 1)), "the definition, not the prototype");
        assert!(app.nav.flash.is_some() && app.nav.peek.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ctrl_hover_underlines_the_symbol_as_a_link() {
        let (mut app, root) = app("hover");
        let mut t = fitting_term(&mut app);
        let line = USER_C.lines().nth(CALL).unwrap();
        let p = cell(&app, CALL, line.find("twice").unwrap() + 2);
        mouse(&mut app, MouseEventKind::Moved, p, KeyModifiers::CONTROL);
        let b = line.find("twice").unwrap();
        assert_eq!(app.nav.link, Some((0, CALL, b..b + 5)));
        draw(&mut app, &mut t);
        let c = t.backend().buffer()[(cell(&app, CALL, b).x, p.y)].clone();
        assert!(c.modifier.contains(Modifier::UNDERLINED) && c.symbol() == "t", "{c:?}");
        mouse(&mut app, MouseEventKind::Moved, p, KeyModifiers::NONE);
        assert_eq!(app.nav.link, None, "Ctrl released");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn goto_location_modes_single_peek_and_references() {
        for mode in [GotoMode::Peek, GotoMode::GotoAndPeek, GotoMode::Goto] {
            let (mut app, root) = app(&format!("mode{mode:?}"));
            app.settings.goto_multiple = mode;
            put(&mut app, CALL, "twice");
            press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
            finish(&mut app);
            let peek = app.nav.peek.as_ref().map(|p| (p.title.clone(), p.items.iter().map(|l| l.path.clone()).collect::<Vec<_>>()));
            let both = Some(("2 definitions".to_string(), vec!["net/a.c".to_string(), "net/b.c".to_string()]));
            let want = match mode {
                GotoMode::Peek => ("net/user.c", both),
                GotoMode::GotoAndPeek => ("net/a.c", both),
                GotoMode::Goto => ("net/a.c", None),
            };
            assert_eq!((file(&app).as_str(), peek), want, "{mode:?}");
            fs::remove_dir_all(&root).unwrap();
        }
        assert_eq!(goto_mode(Some(&"gotoAndPeek".into())), GotoMode::GotoAndPeek);
        assert_eq!(goto_mode(Some(&"bogus".into())), GotoMode::Peek);

        let (mut app, root) = app("single");
        // Alt+F12: a peek even for one definition.
        put(&mut app, CALL, "dsa_port");
        press(&mut app, KeyCode::F(12), KeyModifiers::ALT);
        finish(&mut app);
        assert_eq!(app.nav.peek.as_ref().map(|p| (p.title.as_str(), p.items.len())), Some(("1 definition", 1)));
        assert_eq!(file(&app), "net/user.c");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.nav.peek.is_none());
        // Shift+F12: every whole-word use.
        put(&mut app, CALL, "twice");
        press(&mut app, KeyCode::F(12), KeyModifiers::SHIFT);
        assert_eq!(app.msg, "Searching references…");
        finish(&mut app);
        assert_eq!(app.nav.peek.as_ref().map(|p| p.title.as_str()), Some("4 references"));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        // Esc cancels a running lookup.
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        assert!(app.nav.req.is_some());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.nav.req.is_none());
        assert_eq!(file(&app), "net/user.c");
        // No identifier at the cursor: a message, no lookup.
        put(&mut app, CALL, " twice");
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        assert!(app.nav.req.is_none());
        assert_eq!(app.msg, "No symbol at the cursor");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn back_and_forward_follow_every_jump() {
        let (mut app, root) = app("hist");
        app.settings.goto_multiple = GotoMode::Goto;
        put(&mut app, CALL, "dsa_port");
        let start = cursor(&app);
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        finish(&mut app);
        assert_eq!(file(&app), "net/port.c");
        press(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!((file(&app), cursor(&app)), ("net/user.c".to_string(), start));
        press(&mut app, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(file(&app), "net/port.c");
        // VS Code Linux's Ctrl+Alt+- / Ctrl+Shift+-, as legacy terminals send them (0x1f = Ctrl+7).
        press(&mut app, KeyCode::Char('7'), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(file(&app), "net/user.c");
        press(&mut app, KeyCode::Char('7'), KeyModifiers::CONTROL);
        assert_eq!(file(&app), "net/port.c");
        // The F1 palette.
        press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
        for c in "Go Back".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(file(&app), "net/user.c");
        // A new jump drops Forward.
        put(&mut app, CALL, "twice");
        press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
        finish(&mut app);
        assert_eq!(file(&app), "net/a.c");
        press(&mut app, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(file(&app), "net/a.c", "Forward is empty after a new jump");
        press(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(file(&app), "net/user.c");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn peek_popup_draws_what_it_hit_tests() {
        for (compact, w, h) in [(false, 100, 30), (true, 100, 30), (false, 60, 16), (true, 44, 12)] {
            let (mut app, root) = app(&format!("peek{w}{compact}"));
            app.settings.compact = compact;
            app.settings.spacing = if compact { Spacing::COMPACT } else { Spacing::COMFORTABLE };
            let mut t = term(w, h);
            draw(&mut app, &mut t);
            put(&mut app, CALL, "twice");
            press(&mut app, KeyCode::F(12), KeyModifiers::NONE);
            finish(&mut app);
            draw(&mut app, &mut t);
            let hits = app.nav.hits;
            let items = app.nav.peek.as_ref().unwrap().items.clone();
            assert!(!hits.list.is_empty() && !hits.preview.is_empty(), "{w}x{h}: {hits:?}");
            assert!(text(&t, hits.pop.y + u16::from(!compact), hits.pop).contains("2 definitions"), "{w}x{h} title");
            // Every list row hit-tests as the item it shows.
            let mut seen = 0;
            for y in hits.list.y..hits.list.bottom() {
                let Some(i) = list_item(&app, Position::new(hits.list.x + 1, y)) else { continue };
                let row = text(&t, y, hits.list);
                assert!(row.contains(&format!("{}:{}", base(&items[i].path), items[i].line)), "{w}x{h} row {y}: {row:?} is item {i}");
                seen += 1;
            }
            assert_eq!(seen, items.len().min(hits.list.height as usize));
            // The preview shows the target with the symbol highlighted.
            if w == 100 {
                let y = (hits.preview.y..hits.preview.bottom()).find(|&y| text(&t, y, hits.preview).contains("int twice(int v)")).unwrap();
                let x = hits.preview.x + text(&t, y, hits.preview).find("twice").unwrap() as u16;
                assert_ne!(t.backend().buffer()[(x, y)].bg, t.backend().buffer()[(x - 2, y)].bg, "symbol highlighted");
            }
            // Wheel: the list moves the selection, the preview scrolls.
            let lp = Position::new(hits.list.x + 1, hits.list.y);
            mouse(&mut app, MouseEventKind::ScrollDown, lp, KeyModifiers::NONE);
            assert_eq!(app.nav.peek.as_ref().unwrap().sel, 1);
            mouse(&mut app, MouseEventKind::ScrollUp, lp, KeyModifiers::NONE);
            assert_eq!(app.nav.peek.as_ref().unwrap().sel, 0);
            mouse(&mut app, MouseEventKind::ScrollDown, Position::new(hits.preview.x + 1, hits.preview.y), KeyModifiers::NONE);
            assert!(app.nav.peek.as_ref().unwrap().top.is_some());
            // A click on a row opens that item and closes the peek.
            draw(&mut app, &mut t);
            let y = (app.nav.hits.list.y..app.nav.hits.list.bottom()).find(|&y| list_item(&app, Position::new(app.nav.hits.list.x, y)) == Some(1)).unwrap();
            let row = Position::new(app.nav.hits.list.x + 1, y);
            click(&mut app, row, KeyModifiers::NONE);
            assert!(app.nav.peek.is_none());
            assert_eq!((file(&app), cursor(&app).0), (items[1].path.clone(), items[1].line));
            // A click outside closes it.
            press(&mut app, KeyCode::Left, KeyModifiers::ALT);
            put(&mut app, CALL, "twice");
            press(&mut app, KeyCode::F(12), KeyModifiers::ALT);
            finish(&mut app);
            draw(&mut app, &mut t);
            click(&mut app, Position::new(0, 0), KeyModifiers::NONE);
            assert!(app.nav.peek.is_none());
            fs::remove_dir_all(&root).unwrap();
        }
    }
}
