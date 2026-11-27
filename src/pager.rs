//! git's pager in termcode's terminal, in a window: the terminal pane's assist key for git
//! (Settings, Assist Key: Git, and each command on its own). There git's pager is `termcode
//! pager` (term.rs sets pager.<command> for its shells, the commands turned on): `git log`,
//! `git diff`, `git show`, `git blame`… hand their output to this termcode a chunk at a time (`pager.feed`), as the
//! window wants more; the pager stays until the window closes, as less does, then git ends
//! and the prompt is back. Its title row: the command, the branch, the repository. It reads
//! as less: j/k, Space/b, d/u, g/G (a count before: 50g), / ? n N, & to keep only matching
//! lines, -S chop or fold long lines, -N line numbers, -i case, h help, q; a drag selects,
//! Ctrl+C copies it, a click on a commit hash copies the hash.

use crate::panel::App;
use crate::theme::P;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;
use ratatui::Frame;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_width::UnicodeWidthChar;

/// How many lines past the window's last row are read ahead (more as it scrolls on).
const AHEAD: usize = 2000;

/// less's keys, for `h`.
const HELP: &[&str] = &[
    "j k ↓ ↑ Enter     a line (a count before: that many)",
    "Space f b PgDn PgUp   a page",
    "d u               half a page",
    "g G Home End      the top, the end (50g: line 50)",
    "/text ?text       search on, back; n N the next, the one before",
    "&text             only the lines with it (& alone: all again)",
    "-S  -N  -i        chop or fold long lines; line numbers; search case",
    "← →               sideways (lines chopped)",
    "drag, then Ctrl+C   select and copy; a click on a commit hash copies it",
    "q Esc             close (git ends, the prompt is back)",
];

/// The window: git's lines as they came (colours and all), what it shows of them, its
/// search, filter, selection and options.
pub struct Pager {
    pub id: u64,
    title: String,
    /// Where git runs: its branch, its repository's name.
    branch: String,
    repo: String,
    lines: Vec<String>,
    /// The part of a line a chunk ended in.
    partial: String,
    /// git is done: no more lines will come.
    done: bool,
    /// The lines `&` keeps (indexes into `lines`); None: all of them.
    kept: Option<Vec<usize>>,
    filter: String,
    /// The first row shown (an index into the shown lines).
    top: usize,
    left: usize,
    wrap: bool,
    numbers: bool,
    icase: bool,
    /// Rows and columns the last frame had, its text's rect, and each row's line and the
    /// character it starts at.
    height: usize,
    width: usize,
    body: Rect,
    rows: Vec<(usize, usize)>,
    /// The last search: its text, forward.
    search: Option<(String, bool)>,
    /// What is being typed after / ? or &.
    input: Option<(char, String)>,
    /// A forward search past what has come: the shown line it goes on from.
    pending: Option<usize>,
    /// G: the end, as far as it goes.
    to_end: bool,
    /// A count typed before a key (50g), and `-` waiting for its option letter.
    count: String,
    opt: bool,
    help: bool,
    /// A selection: from, to (line, character); the pointer still down.
    sel: Option<((usize, usize), (usize, usize))>,
    dragging: bool,
    msg: String,
}

impl Pager {
    fn new(id: u64, title: &str, branch: &str, repo: &str) -> Pager {
        Pager {
            id,
            title: title.to_string(),
            branch: branch.to_string(),
            repo: repo.to_string(),
            lines: Vec::new(),
            partial: String::new(),
            done: false,
            kept: None,
            filter: String::new(),
            top: 0,
            left: 0,
            wrap: true,
            numbers: false,
            icase: false,
            height: 20,
            width: 80,
            body: Rect::default(),
            rows: Vec::new(),
            search: None,
            input: None,
            pending: None,
            to_end: false,
            count: String::new(),
            opt: false,
            help: false,
            sel: None,
            dragging: false,
            msg: String::new(),
        }
    }

    fn has(&self, i: usize, pat: &str) -> bool {
        let text = plain(&self.lines[i]);
        if self.icase { text.to_lowercase().contains(&pat.to_lowercase()) } else { text.contains(pat) }
    }

    fn push(&mut self, text: &str, done: bool) {
        let mut text = std::mem::take(&mut self.partial) + text;
        if !done && !text.ends_with('\n') {
            match text.rfind('\n') {
                Some(at) => self.partial = text.split_off(at + 1),
                None => {
                    self.partial = text;
                    return;
                }
            }
        }
        let from = self.lines.len();
        self.lines.extend(text.lines().map(str::to_string));
        if self.kept.is_some() {
            let filter = self.filter.clone();
            let more: Vec<usize> = (from..self.lines.len()).filter(|&i| self.has(i, &filter)).collect();
            if let Some(kept) = self.kept.as_mut() {
                kept.extend(more);
            }
        }
        self.done |= done;
        if let Some(from) = self.pending {
            self.find_from(from, true);
        }
    }

    /// The shown lines: all, or those `&` keeps.
    fn shown(&self) -> usize {
        self.kept.as_ref().map_or(self.lines.len(), Vec::len)
    }

    fn line(&self, row: usize) -> usize {
        self.kept.as_ref().map_or(row, |k| k[row])
    }

    /// The last top that still fills the window: folded lines take their rows.
    fn max_top(&self) -> usize {
        let n = self.shown();
        if !self.wrap {
            return n.saturating_sub(self.height);
        }
        let mut rows = 0;
        for r in (0..n).rev() {
            rows += self.rows_of(self.line(r));
            if rows > self.height {
                return r + 1;
            }
        }
        0
    }

    fn rows_of(&self, line: usize) -> usize {
        let w: usize = cells(&self.lines[line], Style::new()).iter().map(|c| c.0.width().unwrap_or(0)).sum();
        w.div_ceil(self.text_width().max(1)).max(1)
    }

    fn text_width(&self) -> usize {
        self.width.saturating_sub(self.gutter())
    }

    fn gutter(&self) -> usize {
        if self.numbers { self.lines.len().to_string().len() + 1 } else { 0 }
    }

    /// More lines wanted now: the window near their end, a search or G going on.
    fn wants(&self) -> bool {
        !self.done && (self.to_end || self.pending.is_some() || self.shown() < self.top + self.height + AHEAD)
    }

    fn scroll(&mut self, by: isize) {
        self.to_end = false;
        self.top = self.top.saturating_add_signed(by).min(self.max_top());
    }

    /// The next shown line from `from` (back: before it) with the search in it, at the top.
    fn find_from(&mut self, from: usize, forward: bool) {
        let Some((pat, _)) = self.search.clone() else { return };
        let n = self.shown();
        let found = if forward { (from..n).find(|&r| self.has(self.line(r), &pat)) } else { (0..from.min(n)).rev().find(|&r| self.has(self.line(r), &pat)) };
        self.pending = None;
        match found {
            Some(r) => {
                self.top = r;
                self.to_end = false;
                self.msg.clear();
            }
            None if forward && !self.done => {
                self.pending = Some(n);
                self.msg = "searching…".into();
            }
            None => self.msg = "Pattern not found".into(),
        }
    }

    fn search(&mut self, forward: bool) {
        let Some((_, first)) = self.search.clone() else {
            self.msg = "No previous search".into();
            return;
        };
        // n goes the search's own way, N the other.
        let fwd = forward == first;
        self.find_from(if fwd { self.top + 1 } else { self.top }, fwd);
    }

    /// `&text`: only the lines with it from now on (empty: all again).
    fn keep(&mut self, pat: String) {
        self.top = 0;
        self.to_end = false;
        if pat.is_empty() {
            self.kept = None;
            self.filter.clear();
            return;
        }
        self.filter = pat.clone();
        self.kept = Some((0..self.lines.len()).filter(|&i| self.has(i, &pat)).collect());
        if self.kept.as_ref().is_some_and(Vec::is_empty) && self.done {
            self.msg = "Pattern not found".into();
        }
    }

    /// The selection's text, lines joined.
    fn selected(&self) -> Option<String> {
        let (a, b) = self.sel?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if a == b {
            return None;
        }
        let text: Vec<String> = (a.0..=b.0)
            .filter(|&l| l < self.lines.len())
            .map(|l| {
                let chars: Vec<char> = plain(&self.lines[l]).chars().collect();
                let from = if l == a.0 { a.1.min(chars.len()) } else { 0 };
                let to = if l == b.0 { b.1.min(chars.len()) } else { chars.len() };
                chars[from..to.max(from)].iter().collect()
            })
            .collect();
        Some(text.join("\n"))
    }

    /// The line and character under `p`, when it is on the text.
    fn at(&self, p: Position) -> Option<(usize, usize)> {
        let b = self.body;
        if !b.contains(p) {
            return None;
        }
        let &(line, start) = self.rows.get(usize::from(p.y - b.y))?;
        let dx = usize::from(p.x - b.x).saturating_sub(self.gutter());
        let cells = cells(&self.lines[line], Style::new());
        // Chopped: the row starts `left` columns in.
        let skip = if self.wrap { 0 } else { self.left };
        let (mut col, mut i) = (0usize, start);
        while i < cells.len() {
            let w = cells[i].0.width().unwrap_or(0);
            if col >= skip + dx || (col + w > skip + dx && col >= skip) {
                break;
            }
            col += w;
            i += 1;
        }
        Some((line, i))
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// `pager.feed`: the first call (no `id`) opens a window titled `title` (`branch`, `repo`:
/// where git runs; one window at a time: a new one takes the old one's place); each then
/// adds `text` (whole lines; `done` with the last). The answer: whether the window is
/// still open, and whether it wants more now.
pub fn feed(app: &mut App, p: &Value) -> Result<Value, String> {
    let id = match p["id"].as_u64() {
        Some(id) => id,
        // That command's assist key off since its terminal started: git's own pager.
        None if p["command"].as_str().is_some_and(|c| !app.settings.git_assisted(c)) => return Ok(json!({"open": false, "want": false, "off": true})),
        None => {
            let id = NEXT.fetch_add(1, Ordering::SeqCst) + 1;
            let s = |k: &str| p[k].as_str().unwrap_or("").to_string();
            app.pager = Some(Pager::new(id, p["title"].as_str().unwrap_or("git"), &s("branch"), &s("repo")));
            id
        }
    };
    let Some(pg) = app.pager.as_mut().filter(|pg| pg.id == id) else { return Ok(json!({"id": id, "open": false, "want": false})) };
    pg.push(p["text"].as_str().unwrap_or(""), p["done"].as_bool().unwrap_or(false));
    Ok(json!({"id": id, "open": true, "want": pg.wants()}))
}

/// A key while the window is open (it takes them all), as less reads them.
pub fn key(app: &mut App, k: KeyEvent) {
    let Some(pg) = app.pager.as_mut() else { return };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    if pg.help {
        pg.help = false;
        return;
    }
    if let Some((kind, text)) = pg.input.as_mut() {
        match k.code {
            KeyCode::Enter => {
                let (kind, text) = (*kind, std::mem::take(text));
                pg.input = None;
                if kind == '&' {
                    pg.keep(text);
                    return;
                }
                if !text.is_empty() {
                    pg.search = Some((text, kind == '/'));
                }
                pg.search(true);
            }
            KeyCode::Esc => pg.input = None,
            KeyCode::Backspace if text.is_empty() => pg.input = None,
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c) if !ctrl => text.push(c),
            _ => {}
        }
        return;
    }
    // `-` then a letter: an option, as less's -S -N -i.
    if std::mem::take(&mut pg.opt) {
        pg.msg = match k.code {
            KeyCode::Char('S') => {
                pg.wrap = !pg.wrap;
                pg.left = 0;
                (if pg.wrap { "Fold long lines" } else { "Chop long lines" }).into()
            }
            KeyCode::Char('N') => {
                pg.numbers = !pg.numbers;
                (if pg.numbers { "Line numbers" } else { "No line numbers" }).into()
            }
            KeyCode::Char('i') => {
                pg.icase = !pg.icase;
                (if pg.icase { "Search ignores case" } else { "Search minds case" }).into()
            }
            _ => "Options: -S -N -i".into(),
        };
        return;
    }
    if let KeyCode::Char(c @ '0'..='9') = k.code {
        if !ctrl {
            pg.count.push(c);
            return;
        }
    }
    let count: Option<usize> = std::mem::take(&mut pg.count).parse().ok();
    let n = count.unwrap_or(1) as isize;
    pg.msg.clear();
    let page = pg.height.max(1) as isize;
    match (k.code, ctrl) {
        (KeyCode::Char('c'), true) => copy(app),
        (KeyCode::Char('q' | 'Q'), false) | (KeyCode::Esc, _) => app.pager = None,
        (KeyCode::Char('j' | 'e'), false) | (KeyCode::Down | KeyCode::Enter, _) | (KeyCode::Char('n' | 'e' | 'j'), true) => pg.scroll(n),
        (KeyCode::Char('k' | 'y'), false) | (KeyCode::Up, _) | (KeyCode::Char('p' | 'y' | 'k'), true) => pg.scroll(-n),
        (KeyCode::Char(' ' | 'f' | 'z'), false) | (KeyCode::PageDown, _) | (KeyCode::Char('f' | 'v'), true) => pg.scroll(count.map_or(page, |c| c as isize)),
        (KeyCode::Char('b' | 'w'), false) | (KeyCode::PageUp, _) | (KeyCode::Char('b'), true) => pg.scroll(-count.map_or(page, |c| c as isize)),
        (KeyCode::Char('d'), _) => pg.scroll(count.map_or(page / 2, |c| c as isize)),
        (KeyCode::Char('u'), _) => pg.scroll(-count.map_or(page / 2, |c| c as isize)),
        (KeyCode::Char('g' | '<'), false) | (KeyCode::Home, _) => {
            pg.to_end = false;
            pg.top = count.map_or(0, |c| c.saturating_sub(1)).min(pg.max_top());
        }
        (KeyCode::Char('G' | '>'), false) | (KeyCode::End, _) => match count {
            Some(c) => pg.top = c.saturating_sub(1).min(pg.max_top()),
            None => {
                pg.to_end = true;
                pg.top = pg.max_top();
            }
        },
        (KeyCode::Char('/'), false) => pg.input = Some(('/', String::new())),
        (KeyCode::Char('?'), false) => pg.input = Some(('?', String::new())),
        (KeyCode::Char('&'), false) => pg.input = Some(('&', String::new())),
        (KeyCode::Char('-'), false) => pg.opt = true,
        (KeyCode::Char('h' | 'H'), false) => pg.help = true,
        (KeyCode::Char('n'), false) => pg.search(true),
        (KeyCode::Char('N'), false) => pg.search(false),
        (KeyCode::Right, _) if !pg.wrap => pg.left += (pg.width / 2).max(1),
        (KeyCode::Left, _) if !pg.wrap => pg.left = pg.left.saturating_sub((pg.width / 2).max(1)),
        _ => {}
    }
}

/// The selection (else nothing) onto the clipboard.
fn copy(app: &mut App) {
    let Some(text) = app.pager.as_ref().and_then(Pager::selected) else { return };
    let how = crate::panel::clipboard_set(&text);
    let lines = text.lines().count();
    app.clip = text;
    if let Some(pg) = app.pager.as_mut() {
        pg.msg = format!("copied {lines} line{} via {how}", if lines == 1 { "" } else { "s" });
    }
}

/// The wheel scrolls it; a drag selects (copied with Ctrl+C); a click on a commit
/// hash copies the hash. The rest of the screen waits under it.
pub fn mouse(app: &mut App, m: MouseEvent) -> bool {
    let Some(pg) = app.pager.as_mut() else { return false };
    let p = Position::new(m.column, m.row);
    match m.kind {
        MouseEventKind::ScrollDown => pg.scroll(3),
        MouseEventKind::ScrollUp => pg.scroll(-3),
        MouseEventKind::Down(MouseButton::Left) => {
            pg.sel = pg.at(p).map(|at| (at, at));
            pg.dragging = pg.sel.is_some();
        }
        MouseEventKind::Drag(MouseButton::Left) if pg.dragging => {
            if let (Some(at), Some(s)) = (pg.at(p), pg.sel.as_mut()) {
                s.1 = at;
            }
        }
        MouseEventKind::Up(MouseButton::Left) if pg.dragging => {
            pg.dragging = false;
            // A click (no drag) on a commit hash: the hash, copied.
            if let Some(((line, at), b)) = pg.sel.filter(|(a, b)| a == b) {
                let _ = b;
                let chars: Vec<char> = plain(&pg.lines[line]).chars().collect();
                let hex = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
                if hex(at) {
                    let (mut a, mut b) = (at, at);
                    while a > 0 && hex(a - 1) {
                        a -= 1;
                    }
                    while hex(b + 1) {
                        b += 1;
                    }
                    let word: String = chars[a..=b].iter().collect();
                    let bounded = (a == 0 || !chars[a - 1].is_alphanumeric()) && chars.get(b + 1).is_none_or(|c| !c.is_alphanumeric());
                    if bounded && word.len() >= 7 && word.chars().any(|c| c.is_ascii_digit()) {
                        pg.sel = Some(((line, a), (line, b + 1)));
                        copy(app);
                        if let Some(pg) = app.pager.as_mut() {
                            pg.msg = format!("copied {}", &word[..word.len().min(12)]);
                        }
                        return true;
                    }
                }
                pg.sel = None;
            }
        }
        _ => return false,
    }
    true
}

/// The window over everything, most of the screen: its title row (the command, branch,
/// repository), git's lines in their colours (the search's matches lit, the selection),
/// and less's prompt at the bottom.
pub fn draw(app: &mut App, f: &mut Frame) {
    let Some(pg) = app.pager.as_mut() else { return };
    let area = f.area();
    // 85% by 80% of the screen, at least 60 by 12 (or all of a smaller one).
    let w = (area.width * 85 / 100).max(area.width.min(60));
    let h = (area.height * 80 / 100).max(area.height.min(12));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    crate::ui::pixels::tint(r, P.focus_border);
    // Its border in the focus colour: the window that has the keys. Its title is inside.
    let block = crate::ui::kit::popup_block(P.editor_bg, P.editor_fg, (1, 0)).border_style(Style::new().fg(P.focus_border).bg(P.editor_bg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 3 || inner.width < 8 {
        return;
    }
    let base = Style::new().fg(P.editor_fg).bg(P.editor_bg);
    let dim = Style::new().fg(P.dim).bg(P.editor_bg);
    // The title row: the command, where git runs.
    let mut info = vec![Span::styled(pg.title.clone(), base.bold())];
    if !pg.branch.is_empty() {
        info.push(Span::styled(format!("   {} {}", crate::theme::icons().branch, pg.branch), Style::new().fg(P.focus_border).bg(P.editor_bg).bold()));
    }
    if !pg.repo.is_empty() {
        info.push(Span::styled(format!("   {}", pg.repo), dim));
    }
    if !pg.filter.is_empty() {
        info.push(Span::styled(format!("   & {}", pg.filter), Style::new().fg(P.git_modified).bg(P.editor_bg)));
    }
    f.render_widget(Line::from(info).style(base), Rect::new(inner.x, inner.y, inner.width, 1));
    let body = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 2);
    (pg.height, pg.width, pg.body) = (body.height as usize, body.width as usize, body);
    if pg.to_end {
        pg.top = pg.max_top();
    }
    pg.top = pg.top.min(pg.max_top());
    if pg.help {
        for (k, l) in HELP.iter().take(body.height as usize).enumerate() {
            f.render_widget(Line::styled(format!(" {l}"), base), Rect::new(body.x, body.y + k as u16, body.width, 1));
        }
        status(f, pg, inner, base, dim);
        crate::ui::cover();
        return;
    }
    let pat = pg.search.as_ref().map(|s| s.0.clone()).filter(|s| !s.is_empty());
    let sel = pg.sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    let gutter = pg.gutter();
    let tw = pg.text_width().max(1);
    let mut rows = Vec::new();
    let mut y = 0usize;
    let mut row = pg.top;
    while y < body.height as usize && row < pg.shown() {
        let line = pg.line(row);
        let cells = cells(&pg.lines[line], base);
        let lit = pat.as_deref().map_or_else(Vec::new, |p| matches(&cells, p, pg.icase));
        // Its rows: folded at the text's width, or one row from `left`.
        let mut starts = vec![0usize];
        if pg.wrap {
            let mut col = 0;
            for (i, c) in cells.iter().enumerate() {
                let cw = c.0.width().unwrap_or(0);
                if col + cw > tw && col > 0 {
                    starts.push(i);
                    col = 0;
                }
                col += cw;
            }
        }
        for (k, &start) in starts.iter().enumerate() {
            if y >= body.height as usize {
                break;
            }
            let end = starts.get(k + 1).copied().unwrap_or(cells.len());
            let mut spans: Vec<Span> = Vec::new();
            if gutter > 0 {
                let n = if k == 0 { (line + 1).to_string() } else { String::new() };
                spans.push(Span::styled(format!("{n:>w$} ", w = gutter - 1), dim));
            }
            let (mut col, mut shown) = (0usize, 0usize);
            let skip = if pg.wrap { 0 } else { pg.left };
            for (i, (c, st)) in cells.iter().enumerate().take(end).skip(start) {
                let cw = c.width().unwrap_or(0);
                if col >= skip && shown + cw <= tw {
                    let mut st = *st;
                    if lit.get(i).copied().unwrap_or(false) {
                        st = st.add_modifier(Modifier::REVERSED);
                    }
                    if sel.is_some_and(|(a, b)| (line, i) >= a && (line, i) < b) {
                        st = st.bg(P.selection);
                    }
                    spans.push(Span::styled(c.to_string(), st));
                    shown += cw;
                }
                col += cw;
            }
            f.render_widget(Line::from(spans).style(base), Rect::new(body.x, body.y + y as u16, body.width, 1));
            rows.push((line, start));
            y += 1;
        }
        row += 1;
    }
    pg.rows = rows;
    status(f, pg, inner, base, dim);
    crate::ui::cover();
}

/// less's prompt: what is typed, a message, (END), or ":"; where it is on the right.
fn status(f: &mut Frame, pg: &Pager, inner: Rect, base: Style, dim: Style) {
    let at = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    let end = pg.done && pg.top >= pg.max_top() && !pg.help;
    let left = match (&pg.input, pg.msg.is_empty()) {
        (Some((kind, t)), _) => format!("{kind}{t}"),
        _ if pg.help => "HELP -- any key goes back".into(),
        _ if pg.opt => "-".into(),
        (None, false) => pg.msg.clone(),
        (None, true) if end => "(END)".into(),
        _ => format!(":{}", pg.count),
    };
    let n = pg.shown();
    let to = (pg.top + pg.rows.iter().map(|r| r.0).collect::<std::collections::BTreeSet<_>>().len()).min(n);
    let right = format!("lines {}-{to} of {n}{} ", (pg.top + 1).min(to.max(1)), if pg.done { "" } else { "+" });
    let gap = (at.width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let st = if end || pg.input.is_some() || pg.help { base.add_modifier(Modifier::REVERSED) } else { base };
    f.render_widget(Line::from(vec![Span::styled(left, st), Span::styled(" ".repeat(gap), base), Span::styled(right, dim)]), at);
}

/// `raw` without its escape sequences: what a search looks in.
fn plain(raw: &str) -> String {
    cells(raw, Style::new()).into_iter().map(|c| c.0).collect()
}

/// Where `pat` is in a line's characters (`icase`: whatever their case): one flag each.
fn matches(cells: &[(char, Style)], pat: &str, icase: bool) -> Vec<bool> {
    let low = |c: char| if icase { c.to_lowercase().next().unwrap_or(c) } else { c };
    let text: Vec<char> = cells.iter().map(|c| low(c.0)).collect();
    let pat: Vec<char> = pat.chars().map(low).collect();
    let mut lit = vec![false; text.len()];
    let mut i = 0;
    while !pat.is_empty() && i + pat.len() <= text.len() {
        if text[i..i + pat.len()] == pat[..] {
            lit[i..i + pat.len()].iter_mut().for_each(|l| *l = true);
            i += pat.len();
        } else {
            i += 1;
        }
    }
    lit
}

/// A line of git's output as characters, each in its style: SGR colours and attributes
/// read (16, 256 and 24-bit colours), other escape sequences dropped, tabs to the next 8.
pub(crate) fn cells(raw: &str, base: Style) -> Vec<(char, Style)> {
    let mut out = Vec::new();
    let mut st = base;
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => {
                if it.peek() == Some(&'[') {
                    it.next();
                    let mut params = String::new();
                    let mut fin = '\0';
                    for d in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&d) {
                            fin = d;
                            break;
                        }
                        params.push(d);
                    }
                    if fin == 'm' {
                        st = sgr(st, base, &params);
                    }
                } else {
                    it.next();
                }
            }
            '\t' => {
                let n = 8 - out.len() % 8;
                out.extend(std::iter::repeat_n((' ', st), n));
            }
            c if c.is_control() => {}
            c => out.push((c, st)),
        }
    }
    out
}

/// `st` after an SGR sequence's parameters (`1;31`, `38;5;208`, `38;2;1;2;3`, `` = reset).
fn sgr(mut st: Style, base: Style, params: &str) -> Style {
    let n: Vec<u16> = params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect();
    let mut i = 0;
    while i < n.len() {
        let color = |i: &mut usize| -> Option<Color> {
            match n.get(*i + 1) {
                Some(5) => {
                    *i += 2;
                    n.get(*i).map(|&c| Color::Indexed(c as u8))
                }
                Some(2) => {
                    *i += 4;
                    Some(Color::Rgb(*n.get(*i - 2)? as u8, *n.get(*i - 1)? as u8, *n.get(*i)? as u8))
                }
                _ => None,
            }
        };
        match n[i] {
            0 => st = base,
            1 => st = st.add_modifier(Modifier::BOLD),
            2 => st = st.add_modifier(Modifier::DIM),
            3 => st = st.add_modifier(Modifier::ITALIC),
            4 => st = st.add_modifier(Modifier::UNDERLINED),
            7 => st = st.add_modifier(Modifier::REVERSED),
            9 => st = st.add_modifier(Modifier::CROSSED_OUT),
            22 => st = st.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => st = st.remove_modifier(Modifier::ITALIC),
            24 => st = st.remove_modifier(Modifier::UNDERLINED),
            27 => st = st.remove_modifier(Modifier::REVERSED),
            29 => st = st.remove_modifier(Modifier::CROSSED_OUT),
            c @ 30..=37 => st = st.fg(Color::Indexed((c - 30) as u8)),
            38 => {
                if let Some(c) = color(&mut i) {
                    st = st.fg(c);
                }
            }
            39 => st = st.fg(base.fg.unwrap_or(Color::Reset)),
            c @ 40..=47 => st = st.bg(Color::Indexed((c - 40) as u8)),
            48 => {
                if let Some(c) = color(&mut i) {
                    st = st.bg(c);
                }
            }
            49 => st = st.bg(base.bg.unwrap_or(Color::Reset)),
            c @ 90..=97 => st = st.fg(Color::Indexed((c - 90 + 8) as u8)),
            c @ 100..=107 => st = st.bg(Color::Indexed((c - 100 + 8) as u8)),
            _ => {}
        }
        i += 1;
    }
    st
}

// ---------------------------------------------------------------- `termcode pager`

/// `termcode pager [--title T]`: git's pager in termcode's terminal. Its stdin goes to the
/// termcode of that terminal ($TERMCODE_SOCKET) as the window wants it; it ends when the
/// window closes. Anywhere else (no termcode, the API off): less -R, else plain output.
pub fn run(args: &[String]) -> Result<(), String> {
    use std::io::Read;
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};
    use std::time::Duration;
    let title = args.iter().position(|a| a == "--title").and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(git_command);
    let (branch, repo) = where_git_runs();
    let command = args.iter().position(|a| a == "--command").and_then(|i| args.get(i + 1)).cloned();
    // The user's own pager.<command> wins: false pages not at all, a program (delta…) is theirs.
    match command.as_deref().and_then(|c| own_pager(c, &|k| std::env::var(k).ok(), None)).as_deref().map(choice) {
        Some(Own::AsIs) => return std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).map(|_| ()).map_err(|e| e.to_string()),
        Some(Own::Run(p)) => return std::process::Command::new("sh").args(["-c", &p]).status().map(|_| ()).map_err(|e| e.to_string()),
        Some(Own::Window) | None => {}
    }
    let sock = std::env::var_os(crate::api::ENV).filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
    let feed = |sock: &std::path::Path, p: Value| -> Result<Value, String> {
        let r = crate::api::rpc(sock, &json!({"jsonrpc": "2.0", "id": 1, "method": "pager.feed", "params": p}))?;
        r.get("result").cloned().ok_or_else(|| r["error"]["message"].as_str().unwrap_or("refused").to_string())
    };
    let Some((sock, first)) = sock.and_then(|s| feed(&s, json!({"title": title, "branch": branch, "repo": repo, "command": command, "text": ""})).ok().map(|r| (s, r))) else { return fallback() };
    if first["open"] != true {
        return fallback();
    }
    let id = first["id"].as_u64().ok_or("no window")?;
    // git's output, read on its own thread in whole lines; two chunks ahead at most, so a
    // window that wants no more holds git back (as a pipe to less does).
    let (tx, rx) = sync_channel::<Option<String>>(2);
    std::thread::spawn(move || {
        let (mut stdin, mut buf, mut held) = (std::io::stdin().lock(), vec![0u8; 64 << 10], Vec::new());
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Some(String::from_utf8_lossy(&held).into_owned()));
                    let _ = tx.send(None);
                    return;
                }
                Ok(n) => {
                    held.extend_from_slice(&buf[..n]);
                    if let Some(at) = held.iter().rposition(|&b| b == b'\n') {
                        let rest = held.split_off(at + 1);
                        if tx.send(Some(String::from_utf8_lossy(&held).into_owned())).is_err() {
                            return;
                        }
                        held = rest;
                    }
                }
            }
        }
    });
    let (mut want, mut done) = (first["want"].as_bool().unwrap_or(true), false);
    loop {
        let r = if want && !done {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(Some(text)) => feed(&sock, json!({"id": id, "text": text})),
                Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                    done = true;
                    feed(&sock, json!({"id": id, "text": "", "done": true}))
                }
                // Slow git: still open?
                Err(RecvTimeoutError::Timeout) => feed(&sock, json!({"id": id, "text": ""})),
            }
        } else {
            std::thread::sleep(Duration::from_millis(150));
            feed(&sock, json!({"id": id, "text": ""}))
        };
        // The window closed (or termcode went): git's next write ends it.
        match r {
            Ok(v) if v["open"] == true => want = v["want"].as_bool().unwrap_or(false),
            _ => return Ok(()),
        }
    }
}

/// The git command this pager pages for, as typed ("git diff --stat"): the nearest git
/// above it (git starts a pager with arguments through `sh -c`), cut to 60 characters.
fn git_command() -> String {
    // SAFETY: getppid has no preconditions.
    let mut pid = unsafe { libc::getppid() };
    for _ in 0..3 {
        let args = crate::api::cmdline(pid);
        if let Some((_, rest)) = args.split_first().filter(|(f, _)| f.rsplit('/').next() == Some("git")) {
            let line = std::iter::once("git").chain(rest.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
            return if line.chars().count() > 60 { line.chars().take(59).collect::<String>() + "…" } else { line };
        }
        match crate::api::proc_stat(pid) {
            Some((_, parent, _)) if parent > 1 => pid = parent,
            _ => break,
        }
    }
    "git".into()
}

/// Where git runs: its branch (or the commit it is detached at) and its repository's name.
fn where_git_runs() -> (String, String) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let branch = git(&["symbolic-ref", "--short", "-q", "HEAD"]).or_else(|| git(&["rev-parse", "--short", "HEAD"]).map(|c| format!("({c})"))).unwrap_or_default();
    let repo = git(&["rev-parse", "--show-toplevel"]).and_then(|t| t.rsplit('/').next().map(str::to_string)).unwrap_or_default();
    (branch, repo)
}

/// What the user's own pager.<command> asks for.
#[derive(Debug, PartialEq)]
enum Own {
    Window,
    AsIs,
    Run(String),
}

/// git's reading of a pager.<command> value: a boolean (true: the usual pager, here the
/// window), else the program to page with.
fn choice(v: &str) -> Own {
    match v.trim().to_ascii_lowercase().as_str() {
        "false" | "no" | "off" | "0" => Own::AsIs,
        "" | "true" | "yes" | "on" | "1" => Own::Window,
        // termcode's own (never run it again from here: it would call itself).
        _ if v.contains(" pager --command ") => Own::Window,
        _ => Own::Run(v.trim().to_string()),
    }
}

/// The user's own pager.<command> in git's configuration, termcode's (git_env's entries,
/// the last in GIT_CONFIG_COUNT) left out; `get` reads the environment, `dir` is where git
/// runs (none: here).
fn own_pager(command: &str, get: &dyn Fn(&str) -> Option<String>, dir: Option<&std::path::Path>) -> Option<String> {
    let n: usize = get("GIT_CONFIG_COUNT").and_then(|c| c.parse().ok()).unwrap_or(0);
    let ours = (0..n).find(|i| get(&format!("GIT_CONFIG_VALUE_{i}")).is_some_and(|v| v.contains(" pager --command "))).unwrap_or(n);
    let mut git = std::process::Command::new("git");
    if let Some(d) = dir {
        git.current_dir(d);
    }
    let out = git
        .args(["config", "--get", &format!("pager.{command}")])
        .env("GIT_CONFIG_COUNT", ours.to_string())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Not in termcode's window (no termcode, the API off, that command's assist key off):
/// the pager git would run without termcode (its GIT_PAGER, core.pager, PAGER, else less;
/// pager.<command> is termcode's own here), else the text as it is.
fn fallback() -> Result<(), String> {
    let pager = std::process::Command::new("git")
        .args(["var", "GIT_PAGER"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "less".into());
    if pager == "cat" {
        return std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).map(|_| ()).map_err(|e| e.to_string());
    }
    // git set LESS (FRX) for its pager; it is ours too.
    match std::process::Command::new("sh").args(["-c", &pager]).status() {
        Ok(_) => Ok(()),
        Err(_) => std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).map(|_| ()).map_err(|e| e.to_string()),
    }
}

/// The environment a termcode terminal's shell gets so git pages here: git's pager for
/// each of `commands` (pager.<command>, the assist keys turned on), after any configuration
/// already in GIT_CONFIG_COUNT (`count` is its value). None turned on: nothing, so git is
/// as in any terminal; one off: its own pager, as is.
pub fn git_env(count: Option<&str>, exe: &std::path::Path, commands: &[&str]) -> Vec<(String, String)> {
    if commands.is_empty() {
        return Vec::new();
    }
    let n: usize = count.and_then(|c| c.parse().ok()).unwrap_or(0);
    // A termcode replaced on disk while it runs (an install) reads as "… (deleted)".
    let exe = exe.to_string_lossy();
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), (n + commands.len()).to_string())];
    for (k, c) in commands.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{}", n + k), format!("pager.{c}")));
        env.push((format!("GIT_CONFIG_VALUE_{}", n + k), format!("{quoted} pager --command {c}")));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Client, Reply};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    fn app() -> App {
        App::new(std::env::temp_dir(), None)
    }

    fn press(app: &mut App, code: KeyCode) {
        key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typed(app: &mut App, s: &str) {
        s.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        t.draw(|f| draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// git log's shape: n commits of 6 lines ("commit <n>", "Author", "Date", "", "    change n", "").
    fn log(n: usize) -> String {
        (0..n).rev().map(|i| format!("\u{1b}[33mcommit {i:040x}\u{1b}[m\nAuthor: T <t@t>\nDate:   now\n\n    change {i}\n\n")).collect()
    }

    /// A window with `text` in it, all come.
    fn open(app: &mut App, text: &str) -> u64 {
        let id = feed(app, &json!({"title": "git log --stat", "branch": "main", "repo": "proj", "text": ""})).unwrap()["id"].as_u64().unwrap();
        feed(app, &json!({"id": id, "text": text, "done": true})).unwrap();
        id
    }

    fn pg(app: &App) -> &Pager {
        app.pager.as_ref().unwrap()
    }

    fn mouse_at(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        mouse(app, MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
    }

    #[test]
    fn git_colours_and_attributes_read_as_styles_and_other_escapes_go() {
        let base = Style::new().fg(Color::Rgb(1, 1, 1));
        let c = cells("\u{1b}[33mab\u{1b}[m\u{1b}[1;36mC\u{1b}[38;5;208md\u{1b}[38;2;1;2;3me\u{1b}[44mf\u{1b}[Kg\u{1b}[0mh\tx\r", base);
        let text: String = c.iter().map(|c| c.0).collect();
        assert_eq!(text, format!("abCdefgh{}x", " ".repeat(8)), "escapes gone, the tab to the next stop (16), \\r dropped");
        assert_eq!(c[0].1.fg, Some(Color::Indexed(3)));
        assert_eq!(c[2].1, base.add_modifier(Modifier::BOLD).fg(Color::Indexed(6)), "a reset, then bold cyan");
        assert_eq!(c[3].1.fg, Some(Color::Indexed(208)));
        assert_eq!(c[4].1.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(c[5].1.bg, Some(Color::Indexed(4)));
        assert_eq!(c[7].1, base, "0: back to the window's own");
    }

    #[test]
    fn it_reads_ahead_only_as_far_as_the_window_needs_and_ends_when_it_closes() {
        let mut app = app();
        let r = feed(&mut app, &json!({"title": "git log", "text": ""})).unwrap();
        let id = r["id"].as_u64().unwrap();
        assert_eq!((r["open"].as_bool(), r["want"].as_bool()), (Some(true), Some(true)));
        // A chunk ending mid-line keeps its tail for the next.
        let r = feed(&mut app, &json!({"id": id, "text": "one\ntw"})).unwrap();
        assert_eq!(pg(&app).lines, ["one"]);
        assert_eq!(r["want"], true);
        feed(&mut app, &json!({"id": id, "text": "o\n"})).unwrap();
        assert_eq!(pg(&app).lines, ["one", "two"]);
        // Far enough ahead of the window: no more until it scrolls on.
        let r = feed(&mut app, &json!({"id": id, "text": "x\n".repeat(AHEAD + 100)})).unwrap();
        assert_eq!(r["want"], false);
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(feed(&mut app, &json!({"id": id, "text": ""})).unwrap()["want"], true, "G: to the end, so all of it");
        // q: closed; the pager hears so and ends (git with it).
        press(&mut app, KeyCode::Char('q'));
        assert!(app.pager.is_none());
        let r = feed(&mut app, &json!({"id": id, "text": "late\n"})).unwrap();
        assert_eq!((r["open"].as_bool(), r["want"].as_bool()), (Some(false), Some(false)));
    }

    #[test]
    fn a_window_smaller_than_the_screen_says_the_command_branch_and_repository() {
        let mut app = app();
        open(&mut app, &log(30));
        let mut t = Terminal::new(TestBackend::new(100, 40)).unwrap();
        let s = screen(&mut app, &mut t);
        let rows: Vec<&str> = s.lines().collect();
        let top = rows.iter().position(|l| l.contains('╭')).expect("its frame");
        let edge: String = rows[top].chars().filter(|c| *c != ' ').collect();
        assert!(edge.trim_matches(['╭', '╮']).chars().all(|c| c == '─'), "the top line unbroken (in pixels a title there leaves a gap): {s}");
        let head = rows[top + 1];
        assert!(top >= 3 && head.contains("git log --stat") && head.contains(&format!("{} main", crate::theme::icons().branch)) && head.contains("proj"), "the command, branch and repository inside: {s}");
        let left = rows[top].chars().position(|c| c != ' ').unwrap();
        assert!((6..=10).contains(&left), "about 85% wide, centred: {left}\n{s}");
        assert_eq!(t.backend().buffer()[(left as u16, top as u16 + 2)].fg, P.focus_border, "a blue border");
        assert!(s.contains("lines 1-") && s.contains(" of 180 "), "{s}");
    }

    #[test]
    fn its_keys_are_lesss_with_counts_and_its_prompt_says_where_it_is() {
        let mut app = app();
        let id = feed(&mut app, &json!({"title": "git log", "text": ""})).unwrap()["id"].as_u64().unwrap();
        feed(&mut app, &json!({"id": id, "text": log(30)})).unwrap();
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let s = screen(&mut app, &mut t);
        let h = pg(&app).height;
        assert!(s.contains(&format!("lines 1-{h} of 180+")), "not done yet: +\n{s}");
        let top = |app: &App| pg(app).top;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        assert_eq!(top(&app), 2);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(top(&app), 1);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(top(&app), 1 + h, "a page: the window's rows");
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(top(&app), h / 2, "half a page");
        // A count before a key: 5j, 50g (line 50), 3k.
        typed(&mut app, "5j");
        assert_eq!(top(&app), h / 2 + 5);
        typed(&mut app, "50g");
        assert_eq!(top(&app), 49);
        assert!(screen(&mut app, &mut t).contains(&format!("lines 50-{} ", 49 + h)));
        typed(&mut app, "3");
        assert!(screen(&mut app, &mut t).contains(":3"), "the count in the prompt");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(top(&app), 46);
        press(&mut app, KeyCode::Char('g'));
        // /: the next line with it, at the top, lit; n the one after; N back; ?: backwards.
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "change 2");
        assert!(screen(&mut app, &mut t).contains("/change 2"), "what is typed, in the prompt");
        press(&mut app, KeyCode::Enter);
        let line = |app: &App| pg(app).lines[pg(app).line(pg(app).top)].clone();
        assert_eq!(line(&app), "    change 29");
        let s = screen(&mut app, &mut t);
        let b = t.backend().buffer();
        let y = s.lines().position(|l| l.contains("change 29")).unwrap() as u16;
        let x = s.lines().nth(y as usize).unwrap().chars().position(|c| c == 'c').unwrap() as u16;
        assert!(b[(x, y)].modifier.contains(Modifier::REVERSED), "the match lit");
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(line(&app), "    change 28");
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(line(&app), "    change 29");
        // -i: whatever the case.
        typed(&mut app, "-i");
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "CHANGE 27");
        press(&mut app, KeyCode::Enter);
        assert_eq!(line(&app), "    change 27");
        press(&mut app, KeyCode::Char('?'));
        typed(&mut app, "zzz");
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, &mut t).contains("Pattern not found"));
        // Past what has come: it waits for more, then goes there.
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "later");
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, &mut t).contains("searching"));
        feed(&mut app, &json!({"id": id, "text": "x\nlater on\n", "done": true})).unwrap();
        assert_eq!(line(&app), "later on");
        // G: the end, (END) in the prompt; h: the keys, any key back; q closes it.
        press(&mut app, KeyCode::Char('G'));
        let s = screen(&mut app, &mut t);
        assert!(s.contains("(END)") && s.contains("of 182 "), "{s}");
        press(&mut app, KeyCode::Char('h'));
        let s = screen(&mut app, &mut t);
        assert!(s.contains("HELP") && s.contains("&text"), "{s}");
        press(&mut app, KeyCode::Char('x'));
        assert!(!screen(&mut app, &mut t).contains("HELP"));
        press(&mut app, KeyCode::Esc);
        assert!(app.pager.is_none());
    }

    #[test]
    fn and_keeps_only_matching_lines_and_options_fold_chop_and_number_them() {
        let mut app = app();
        let long = format!("{}END", "x".repeat(150));
        open(&mut app, &format!("{}{long}\nshort\n", log(5)));
        let mut t = Terminal::new(TestBackend::new(80, 30)).unwrap();
        // &: only the lines with it, said in the title row; & alone: all again.
        press(&mut app, KeyCode::Char('&'));
        typed(&mut app, "change");
        press(&mut app, KeyCode::Enter);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("& change") && s.contains("change 4") && s.contains("change 0") && !s.contains("Author") && s.contains("of 5 "), "{s}");
        press(&mut app, KeyCode::Char('&'));
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, &mut t).contains("Author"));
        // A long line folds (git's less does), its end on the next row; -S chops it, → shows it.
        press(&mut app, KeyCode::Char('G'));
        let s = screen(&mut app, &mut t);
        assert!(s.contains("xEND") || s.lines().any(|l| l.trim_start_matches(['│', ' ']).starts_with("xxEND") || l.contains("END")), "folded: its end shows\n{s}");
        typed(&mut app, "-S");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Chop long lines") && !s.contains("END"), "{s}");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        assert!(screen(&mut app, &mut t).contains("END"), "sideways to its end");
        // -N: each line's number before it (back at the left edge, where "short" shows).
        (0..3).for_each(|_| press(&mut app, KeyCode::Left));
        typed(&mut app, "-N");
        let s = screen(&mut app, &mut t);
        assert!(s.lines().any(|l| l.contains(" 32 short")), "{s}");
    }

    #[test]
    fn a_drag_selects_and_ctrl_c_copies_and_a_click_on_a_commit_hash_copies_it() {
        let mut app = app();
        open(&mut app, &log(3));
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let s = screen(&mut app, &mut t);
        let rows: Vec<&str> = s.lines().collect();
        let y = rows.iter().position(|l| l.contains("Author")).unwrap() as u16;
        let x = rows[y as usize].chars().position(|c| c == 'A').unwrap() as u16;
        // Author: T  -> "Author" dragged over, then Ctrl+C.
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse_at(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 6, y);
        mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), x + 6, y);
        assert!(t.draw(|f| draw(&mut app, f)).is_ok() && t.backend().buffer()[(x + 1, y)].bg == P.selection, "drawn selected");
        key(&mut app, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.clip, "Author");
        // Across lines: joined by newlines.
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse_at(&mut app, MouseEventKind::Drag(MouseButton::Left), x + 4, y + 1);
        key(&mut app, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.clip, "Author: T <t@t>\nDate");
        // A click on a commit hash: the whole hash.
        let cy = rows.iter().position(|l| l.contains("commit 0000")).unwrap() as u16;
        let cx = rows[cy as usize].find("commit").map(|b| rows[cy as usize][..b].chars().count()).unwrap() as u16 + 12;
        mouse_at(&mut app, MouseEventKind::Down(MouseButton::Left), cx, cy);
        mouse_at(&mut app, MouseEventKind::Up(MouseButton::Left), cx, cy);
        assert_eq!(app.clip, format!("{:040x}", 2));
        assert!(screen(&mut app, &mut t).contains("copied 000000000000"));
    }

    #[test]
    fn a_terminals_shell_pages_the_git_commands_turned_on_here_after_any_configuration_it_had() {
        let env = git_env(None, std::path::Path::new("/opt/it's/termcode (deleted)"), &["log", "diff"]);
        assert_eq!(env[0], ("GIT_CONFIG_COUNT".to_string(), "2".to_string()));
        assert_eq!((env[1].0.as_str(), env[1].1.as_str()), ("GIT_CONFIG_KEY_0", "pager.log"));
        assert_eq!(env[2].1, r"'/opt/it'\''s/termcode' pager --command log", "quoted, the old binary's mark gone");
        assert_eq!((env[3].1.as_str(), env[4].1.as_str()), ("pager.diff", r"'/opt/it'\''s/termcode' pager --command diff"));
        let env = git_env(Some("2"), std::path::Path::new("/t"), &["log"]);
        assert_eq!((env[0].1.as_str(), env[1].0.as_str()), ("3", "GIT_CONFIG_KEY_2"), "after the two already set");
        assert!(git_env(None, std::path::Path::new("/t"), &[]).is_empty(), "all off: git as in any terminal");
        // From the settings: Git off, or one command off.
        let mut s = crate::settings::Settings::default();
        assert_eq!(s.git_assisted_commands(), crate::settings::GIT_ASSIST, "all on at first");
        s.assist_git_off = vec!["blame".into()];
        assert!(s.git_assisted("log") && !s.git_assisted("blame") && !s.git_assisted("commit"));
        s.assist_git = false;
        assert!(s.git_assisted_commands().is_empty());
    }

    #[test]
    fn the_users_own_pager_setting_for_a_command_wins_over_termcodes() {
        assert_eq!((choice("false"), choice("Off"), choice("0")), (Own::AsIs, Own::AsIs, Own::AsIs), "no paging");
        assert_eq!((choice("true"), choice("1")), (Own::Window, Own::Window), "the usual pager: the window");
        assert_eq!(choice("delta --side-by-side"), Own::Run("delta --side-by-side".into()), "their program");
        assert_eq!(choice("'/t/termcode' pager --command log"), Own::Window, "termcode's own, never run again");
        // Read from git, with termcode's own entries (the last ones) left out.
        let dir = std::env::temp_dir().join(format!("termcode-ownpager-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |a: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(a).status().unwrap().success());
        git(&["init", "-q"]);
        git(&["config", "pager.branch", "false"]);
        let env = |k: &str| match k {
            "GIT_CONFIG_COUNT" => Some("1".to_string()),
            "GIT_CONFIG_VALUE_0" => Some("'/t/termcode' pager --command branch".to_string()),
            _ => None,
        };
        assert_eq!((own_pager("branch", &env, Some(&dir)), own_pager("log", &env, Some(&dir))), (Some("false".to_string()), None), "theirs, or none set");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_command_turned_off_since_its_terminal_started_pages_as_git_would() {
        let mut app = app();
        app.settings.assist_git_off = vec!["diff".into()];
        let r = feed(&mut app, &json!({"title": "git diff", "command": "diff", "text": ""})).unwrap();
        assert_eq!((r["open"].as_bool(), r["off"].as_bool()), (Some(false), Some(true)));
        assert!(app.pager.is_none(), "no window");
        let r = feed(&mut app, &json!({"title": "git log", "command": "log", "text": ""})).unwrap();
        assert_eq!(r["open"], true);
        app.settings.assist_git = false;
        app.pager = None;
        assert_eq!(feed(&mut app, &json!({"command": "log", "text": ""})).unwrap()["open"], false, "Git off: none");
    }

    #[test]
    fn only_programs_from_its_own_terminals_may_open_the_window() {
        assert!(crate::api::descends(std::process::id() as i32), "this termcode itself");
        assert!(!crate::api::descends(1), "init is no child of it");
        let mut app = app();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "pager.feed", "params": {"text": "x\n"}});
        let outsider = Client { key: (1, 0), label: "pid 1".into(), ..Client::default() };
        let Reply::Now(v) = crate::api::respond(&mut app, &req, &outsider, true) else { panic!("answered at once") };
        assert!(v["error"]["message"].as_str().unwrap().contains("only from this termcode's terminals"));
        assert!(app.pager.is_none());
    }
}

