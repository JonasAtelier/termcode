//! `git log` in termcode's terminal, in a window. git's pager for `log` there is
//! `termcode pager` (term.rs sets it for its shells only): it hands git's output to this
//! termcode a chunk at a time (`pager.feed`), as the window wants more, and stays until the
//! window closes, as less does; then git ends and the shell's prompt is back. The window
//! reads as less: j/k, Space/b, d/u, g/G, / and ? to search, n/N, ←/→, q.

use crate::panel::App;
use crate::theme::P;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Clear;
use ratatui::Frame;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_width::UnicodeWidthChar;

/// How many lines past the window's last row are read ahead (more as it scrolls on).
const AHEAD: usize = 2000;

/// The window: git's lines as they came (colours and all), where it is, its search.
pub struct Pager {
    pub id: u64,
    title: String,
    lines: Vec<String>,
    /// The part of a line a chunk ended in.
    partial: String,
    /// git is done: no more lines will come.
    done: bool,
    top: usize,
    left: usize,
    /// Rows the last frame showed.
    height: usize,
    width: usize,
    /// The last search: its text, forward.
    search: Option<(String, bool)>,
    /// A search being typed: forward, its text.
    input: Option<(bool, String)>,
    /// A forward search past what has come: the line it goes on from.
    pending: Option<usize>,
    /// G: the end, as far as it goes.
    to_end: bool,
    msg: String,
}

impl Pager {
    fn new(id: u64, title: &str) -> Pager {
        Pager {
            id,
            title: title.to_string(),
            lines: Vec::new(),
            partial: String::new(),
            done: false,
            top: 0,
            left: 0,
            height: 20,
            width: 80,
            search: None,
            input: None,
            pending: None,
            to_end: false,
            msg: String::new(),
        }
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
        self.lines.extend(text.lines().map(str::to_string));
        self.done |= done;
        if let Some(from) = self.pending {
            self.find_from(from, true);
        }
    }

    fn max_top(&self) -> usize {
        self.lines.len().saturating_sub(self.height)
    }

    /// More lines wanted now: the window near their end, a search or G going on.
    fn wants(&self) -> bool {
        !self.done && (self.to_end || self.pending.is_some() || self.lines.len() < self.top + self.height + AHEAD)
    }

    fn scroll(&mut self, by: isize) {
        self.to_end = false;
        self.top = self.top.saturating_add_signed(by).min(self.max_top());
    }

    /// The next line from `from` (back: before it) whose text has the search, as the top.
    fn find_from(&mut self, from: usize, forward: bool) {
        let Some((pat, _)) = self.search.clone() else { return };
        let hit = |l: &String| plain(l).contains(&pat);
        let found = if forward { (from..self.lines.len()).find(|&i| hit(&self.lines[i])) } else { (0..from.min(self.lines.len())).rev().find(|&i| hit(&self.lines[i])) };
        self.pending = None;
        match found {
            Some(i) => {
                self.top = i;
                self.msg.clear();
            }
            None if forward && !self.done => {
                self.pending = Some(self.lines.len());
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
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// `pager.feed`: the first call (no `id`) opens a window titled `title` (one at a time: a
/// new one takes the old one's place); each then adds `text` (whole lines; `done` with the
/// last). The answer: whether the window is still open, and whether it wants more now.
pub fn feed(app: &mut App, p: &Value) -> Result<Value, String> {
    let id = match p["id"].as_u64() {
        Some(id) => id,
        None => {
            let id = NEXT.fetch_add(1, Ordering::SeqCst) + 1;
            app.pager = Some(Pager::new(id, p["title"].as_str().unwrap_or("git log")));
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
    if let Some((forward, text)) = pg.input.as_mut() {
        match k.code {
            KeyCode::Enter => {
                let (forward, text) = (*forward, std::mem::take(text));
                pg.input = None;
                if !text.is_empty() {
                    pg.search = Some((text, forward));
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
    pg.msg.clear();
    let page = pg.height.max(1) as isize;
    match (k.code, ctrl) {
        (KeyCode::Char('q' | 'Q'), false) | (KeyCode::Esc, _) => app.pager = None,
        (KeyCode::Char('j' | 'e'), false) | (KeyCode::Down | KeyCode::Enter, _) | (KeyCode::Char('n' | 'e' | 'j'), true) => pg.scroll(1),
        (KeyCode::Char('k' | 'y'), false) | (KeyCode::Up, _) | (KeyCode::Char('p' | 'y' | 'k'), true) => pg.scroll(-1),
        (KeyCode::Char(' ' | 'f' | 'z'), false) | (KeyCode::PageDown, _) | (KeyCode::Char('f' | 'v'), true) => pg.scroll(page),
        (KeyCode::Char('b' | 'w'), false) | (KeyCode::PageUp, _) | (KeyCode::Char('b'), true) => pg.scroll(-page),
        (KeyCode::Char('d'), _) => pg.scroll(page / 2),
        (KeyCode::Char('u'), _) => pg.scroll(-page / 2),
        (KeyCode::Char('g' | '<'), false) | (KeyCode::Home, _) => {
            pg.to_end = false;
            pg.top = 0;
        }
        (KeyCode::Char('G' | '>'), false) | (KeyCode::End, _) => {
            pg.to_end = true;
            pg.top = pg.max_top();
        }
        (KeyCode::Char('/'), false) => pg.input = Some((true, String::new())),
        (KeyCode::Char('?'), false) => pg.input = Some((false, String::new())),
        (KeyCode::Char('n'), false) => pg.search(true),
        (KeyCode::Char('N'), false) => pg.search(false),
        (KeyCode::Right, _) => pg.left += (pg.width / 2).max(1),
        (KeyCode::Left, _) => pg.left = pg.left.saturating_sub((pg.width / 2).max(1)),
        _ => {}
    }
}

/// The wheel scrolls it; the rest of the screen waits under it.
pub fn mouse(app: &mut App, m: MouseEvent) -> bool {
    let Some(pg) = app.pager.as_mut() else { return false };
    match m.kind {
        MouseEventKind::ScrollDown => pg.scroll(3),
        MouseEventKind::ScrollUp => pg.scroll(-3),
        _ => return false,
    }
    true
}

/// The window over everything, most of the screen: git's lines in their colours (the
/// search's matches lit), and less's prompt at the bottom.
pub fn draw(app: &mut App, f: &mut Frame) {
    let Some(pg) = app.pager.as_mut() else { return };
    let area = f.area();
    let (w, h) = (area.width.saturating_sub(6).max(area.width.min(20)), area.height.saturating_sub(4).max(area.height.min(6)));
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    let block = crate::ui::kit::popup_block(P.editor_bg, P.editor_fg, (1, 0)).title(format!(" {} ", pg.title));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.height < 2 || inner.width < 4 {
        return;
    }
    let body = Rect { height: inner.height - 1, ..inner };
    (pg.height, pg.width) = (body.height as usize, body.width as usize);
    if pg.to_end {
        pg.top = pg.max_top();
    }
    pg.top = pg.top.min(pg.max_top());
    let base = Style::new().fg(P.editor_fg).bg(P.editor_bg);
    let pat = pg.search.as_ref().map(|s| s.0.as_str()).filter(|s| !s.is_empty());
    for (k, raw) in pg.lines.iter().skip(pg.top).take(body.height as usize).enumerate() {
        let cells = cells(raw, base);
        let lit = pat.map_or_else(Vec::new, |pat| matches(&cells, pat));
        let mut spans: Vec<Span> = Vec::new();
        let (mut col, mut shown) = (0usize, 0usize);
        for (i, (c, st)) in cells.iter().enumerate() {
            let cw = c.width().unwrap_or(0);
            if col >= pg.left && shown + cw <= body.width as usize {
                let st = if lit.get(i).copied().unwrap_or(false) { st.add_modifier(Modifier::REVERSED) } else { *st };
                spans.push(Span::styled(c.to_string(), st));
                shown += cw;
            }
            col += cw;
        }
        f.render_widget(Line::from(spans).style(base), Rect::new(body.x, body.y + k as u16, body.width, 1));
    }
    // less's prompt: what is typed, a message, (END), or ":"; where it is on the right.
    let at = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
    let dim = Style::new().fg(P.dim).bg(P.editor_bg);
    let end = pg.done && pg.top >= pg.max_top();
    let left = match (&pg.input, pg.msg.is_empty()) {
        (Some((fwd, t)), _) => format!("{}{t}", if *fwd { '/' } else { '?' }),
        (None, false) => pg.msg.clone(),
        (None, true) if end => "(END)".into(),
        _ => ":".into(),
    };
    let shown_to = (pg.top + pg.height).min(pg.lines.len());
    let right = format!("lines {}-{shown_to} of {}{} ", (pg.top + 1).min(shown_to), pg.lines.len(), if pg.done { "" } else { "+" });
    let gap = (at.width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let st = if end || pg.input.is_some() { base.add_modifier(Modifier::REVERSED) } else { base };
    f.render_widget(Line::from(vec![Span::styled(left, st), Span::styled(" ".repeat(gap), base), Span::styled(right, dim)]), at);
    crate::ui::cover();
}

/// `raw` without its escape sequences: what a search looks in.
fn plain(raw: &str) -> String {
    cells(raw, Style::new()).into_iter().map(|c| c.0).collect()
}

/// Where `pat` is in a line's characters: one flag each.
fn matches(cells: &[(char, Style)], pat: &str) -> Vec<bool> {
    let text: Vec<char> = cells.iter().map(|c| c.0).collect();
    let pat: Vec<char> = pat.chars().collect();
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
    let title = args.iter().position(|a| a == "--title").and_then(|i| args.get(i + 1)).map_or("git log", String::as_str);
    let sock = std::env::var_os(crate::api::ENV).filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
    let feed = |sock: &std::path::Path, p: Value| -> Result<Value, String> {
        let r = crate::api::rpc(sock, &json!({"jsonrpc": "2.0", "id": 1, "method": "pager.feed", "params": p}))?;
        r.get("result").cloned().ok_or_else(|| r["error"]["message"].as_str().unwrap_or("refused").to_string())
    };
    let Some((sock, first)) = sock.and_then(|s| feed(&s, json!({"title": title, "text": ""})).ok().map(|r| (s, r))) else { return fallback() };
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

/// Outside termcode's window: less as git would run it, else the text as it is.
fn fallback() -> Result<(), String> {
    match std::process::Command::new("less").arg("-FRX").status() {
        Ok(_) => Ok(()),
        Err(_) => std::io::copy(&mut std::io::stdin(), &mut std::io::stdout()).map(|_| ()).map_err(|e| e.to_string()),
    }
}

/// The environment a termcode terminal's shell gets so `git log` pages here: git's pager
/// for `log`, after any configuration already in GIT_CONFIG_COUNT (`count` is its value).
pub fn git_env(count: Option<&str>, exe: &std::path::Path) -> Vec<(String, String)> {
    let n: usize = count.and_then(|c| c.parse().ok()).unwrap_or(0);
    // A termcode replaced on disk while it runs (an install) reads as "… (deleted)".
    let exe = exe.to_string_lossy();
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    vec![
        ("GIT_CONFIG_COUNT".into(), (n + 1).to_string()),
        (format!("GIT_CONFIG_KEY_{n}"), "pager.log".into()),
        (format!("GIT_CONFIG_VALUE_{n}"), format!("{quoted} pager --title 'git log'")),
    ]
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
        (0..n).rev().map(|i| format!("\u{1b}[33mcommit {i:040}\u{1b}[m\nAuthor: T <t@t>\nDate:   now\n\n    change {i}\n\n")).collect()
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
        assert_eq!(app.pager.as_ref().unwrap().lines, ["one"]);
        assert_eq!(r["want"], true);
        feed(&mut app, &json!({"id": id, "text": "o\n"})).unwrap();
        assert_eq!(app.pager.as_ref().unwrap().lines, ["one", "two"]);
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
    fn its_keys_are_lesss_and_its_prompt_says_where_it_is() {
        let mut app = app();
        let id = feed(&mut app, &json!({"title": "git log", "text": ""})).unwrap()["id"].as_u64().unwrap();
        feed(&mut app, &json!({"id": id, "text": log(30)})).unwrap();
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let s = screen(&mut app, &mut t);
        assert!(s.contains(" git log ") && s.contains("change 29") && s.contains("lines 1-17 of 180+"), "not done yet: +\n{s}");
        let top = |app: &App| app.pager.as_ref().unwrap().top;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Down);
        assert_eq!(top(&app), 2);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(top(&app), 1);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(top(&app), 18, "a page: the window's rows");
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(top(&app), 9, "half a page");
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(top(&app), 0);
        // /: the next line with it, at the top, lit; n the one after; N back; ?: backwards.
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "change 2");
        assert!(screen(&mut app, &mut t).contains("/change 2"), "what is typed, in the prompt");
        press(&mut app, KeyCode::Enter);
        let line = |app: &App| app.pager.as_ref().unwrap().lines[top(app)].clone();
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
        // G: the end, (END) in the prompt; q closes it.
        press(&mut app, KeyCode::Char('G'));
        let s = screen(&mut app, &mut t);
        assert!(s.contains("(END)") && s.contains("of 182 "), "{s}");
        press(&mut app, KeyCode::Esc);
        assert!(app.pager.is_none());
    }

    #[test]
    fn a_terminals_shell_pages_git_log_here_after_any_configuration_it_had() {
        let env = git_env(None, std::path::Path::new("/opt/it's/termcode (deleted)"));
        assert_eq!(env[0], ("GIT_CONFIG_COUNT".to_string(), "1".to_string()));
        assert_eq!(env[1], ("GIT_CONFIG_KEY_0".to_string(), "pager.log".to_string()));
        assert_eq!(env[2].1, r"'/opt/it'\''s/termcode' pager --title 'git log'", "quoted, the old binary's mark gone");
        let env = git_env(Some("2"), std::path::Path::new("/t"));
        assert_eq!((env[0].1.as_str(), env[1].0.as_str()), ("3", "GIT_CONFIG_KEY_2"), "after the two already set");
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
