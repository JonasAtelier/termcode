//! A log file's **log** look: any size (its lines indexed on a thread, only the rows in
//! view read), each line's level found (ERROR, WARN, INFO, DEBUG; ESP-IDF's `E (123)`;
//! a stack trace takes its error's), errors and warnings coloured, the time since the line
//! before when lines start with a timestamp, a filter (text and levels), F8 / n to the next
//! problem, and the end followed as the file grows (`tail -f`) while the view is there.

use crate::panel::{App, Focus, GroupHit};
use crate::theme::P;
use crate::ui::{fill, fit, row};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// Levels, most severe first; NONE: a line before any level.
const ERROR: u8 = 0;
const WARN: u8 = 1;
const INFO: u8 = 2;
const DEBUG: u8 = 3;
const NONE: u8 = 4;
/// In a line's level byte: the line names its level itself (not taken from above).
const NAMED: u8 = 8;
const NAMES: [&str; 4] = ["errors", "warnings", "info", "debug"];
/// How often a log in view is checked for new lines.
const WATCH: Duration = Duration::from_millis(500);

pub fn is_log(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    name.ends_with(".log") || name.contains(".log.") || matches!(name.as_str(), "syslog" | "messages" | "dmesg" | "kern.log")
}

/// A line's level, from its first bytes: a level word in capitals (`ERROR`, `[WARN]`),
/// one in any case bracketed, after `level=` or before a colon (`error:`), or ESP-IDF's
/// and Android's one letter (`E (1234) wifi:`).
pub fn level_of(line: &[u8]) -> Option<u8> {
    let s = &line[..line.len().min(200)];
    if s.len() > 3 && s[1] == b' ' && s[2] == b'(' && s[3].is_ascii_digit() {
        match s[0] {
            b'E' => return Some(ERROR),
            b'W' => return Some(WARN),
            b'I' => return Some(INFO),
            b'D' | b'V' => return Some(DEBUG),
            _ => {}
        }
    }
    const WORDS: [(&[u8], u8); 19] = [
        (b"FATAL", ERROR), (b"CRITICAL", ERROR), (b"CRIT", ERROR), (b"ERROR", ERROR), (b"ERR", ERROR), (b"SEVERE", ERROR), (b"PANIC", ERROR), (b"EMERG", ERROR), (b"ALERT", ERROR), (b"FAIL", ERROR), (b"FAILED", ERROR),
        (b"WARNING", WARN), (b"WARN", WARN), (b"INFO", INFO), (b"NOTICE", INFO), (b"DEBUG", DEBUG), (b"TRACE", DEBUG), (b"VERBOSE", DEBUG), (b"FINE", DEBUG),
    ];
    // A word's level, any case (no allocation: this runs for every line of a big log).
    let word = |w: &[u8]| (3..=8).contains(&w.len()).then(|| WORDS.iter().find(|(k, _)| k.eq_ignore_ascii_case(w)).map(|(_, l)| *l)).flatten();
    let mut i = 0;
    while i < s.len() {
        if !s[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < s.len() && s[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let w = &s[start..i];
        let Some(lv) = word(w) else { continue };
        let caps = w.iter().all(u8::is_ascii_uppercase);
        let before = if start > 0 { s[start - 1] } else { b' ' };
        let after = s.get(i).copied().unwrap_or(b' ');
        let bracketed = matches!(before, b'[' | b'<' | b'(') && matches!(after, b']' | b'>' | b')');
        let keyed = s[..start].ends_with(b"level=") || s[..start].ends_with(b"lvl=") || s[..start].ends_with(b"severity=");
        // "FAIL" only in capitals: a test runner's verdict, not the word in a sentence.
        if (caps && w.len() > 2 && (lv != ERROR || w != b"FAIL" || after != b'e')) || bracketed || keyed || (after == b':' && lv <= WARN) {
            return Some(lv);
        }
    }
    None
}

/// A line's time, from its start: an ISO or plain date and time, a time alone,
/// syslog's `Nov 17 12:00:00`, dmesg's `[  12.345678]`, ESP-IDF's `I (12345)` (ms).
pub fn time_of(line: &str) -> Option<f64> {
    let t = line.trim_start().trim_start_matches('[').trim_start();
    let num = |s: &str| s.parse::<f64>().ok();
    let clock = |s: &str| -> Option<(f64, usize)> {
        let b = s.as_bytes();
        if b.len() < 8 || b[2] != b':' || b[5] != b':' {
            return None;
        }
        let (h, m) = (num(&s[0..2])?, num(&s[3..5])?);
        let end = s[6..].find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).map_or(s.len(), |e| e + 6);
        Some((h * 3600.0 + m * 60.0 + num(&s[6..end].replace(',', "."))?, end))
    };
    // 2026-11-17 12:00:00.123 / 2026-11-17T12:00:00Z
    if t.len() >= 19 && t.as_bytes()[4] == b'-' && t.as_bytes()[7] == b'-' {
        let days = days_from(num(&t[0..4])? as i64, num(&t[5..7])? as i64, num(&t[8..10])? as i64);
        let (s, _) = clock(&t[11..])?;
        return Some(days as f64 * 86400.0 + s);
    }
    if let Some((s, _)) = clock(t) {
        return Some(s);
    }
    // Nov 17 12:00:00
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    if let Some(m) = MONTHS.iter().position(|m| t.starts_with(m)) {
        let rest = t[3..].trim_start();
        let (day, rest) = rest.split_once(' ')?;
        let (s, _) = clock(rest)?;
        return Some(days_from(2000, m as i64 + 1, num(day)? as i64) as f64 * 86400.0 + s);
    }
    // ESP-IDF: E (1234) tag
    let b = line.as_bytes();
    if b.len() > 4 && b[1] == b' ' && b[2] == b'(' {
        let end = line[3..].find(')')? + 3;
        return num(&line[3..end]).map(|ms| ms / 1000.0);
    }
    // dmesg: [   12.345678]
    let end = t.find(']')?;
    t[..end].trim().parse::<f64>().ok().filter(|_| line.trim_start().starts_with('['))
}

/// Days from 1970-01-01 to a date (civil calendar).
fn days_from(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// "+12 ms", "+1.25 s", "+3m02s".
fn gap(s: f64) -> String {
    match s {
        s if s < 1.0 => format!("+{:.0} ms", s * 1000.0),
        s if s < 60.0 => format!("+{s:.2} s"),
        s => format!("+{}m{:02}s", (s / 60.0) as u64, (s % 60.0) as u64),
    }
}

/// A line as shown: escapes (colours) out, tabs as spaces, other controls dropped.
fn clean(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
            }
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

enum Msg {
    /// New lines of `key` (from the indexing of generation `g`).
    Lines(String, u64, Result<Indexed, String>),
    /// Rows matching `key`'s filter from `from` on, for filter generation `gen`.
    Rows(String, u64, usize, Vec<u32>),
}

struct Indexed {
    starts: Vec<u64>,
    levels: Vec<u8>,
    end: u64,
    partial: bool,
}

/// Lines of `path` from `from` (a line's start) to its end: where each starts, its
/// level (`carry`: the level above, for lines naming none). Read in 1 MB chunks, the
/// newlines found by memchr.
fn index(path: &Path, from: u64, mut carry: u8) -> Result<Indexed, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
    let mut out = Indexed { starts: Vec::new(), levels: Vec::new(), end: from, partial: false };
    let mut buf = vec![0u8; 1 << 20];
    // The line begun in an earlier chunk: its start and first bytes.
    let mut head: Vec<u8> = Vec::with_capacity(200);
    let mut open: Option<u64> = None;
    let mut at = from;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        let mut i = 0;
        while i < n {
            let start = open.take().unwrap_or(at + i as u64);
            match memchr::memchr(b'\n', &chunk[i..]) {
                Some(nl) => {
                    let line = &chunk[i..i + nl];
                    let lv = if head.is_empty() {
                        level_of(line)
                    } else {
                        head.extend_from_slice(&line[..line.len().min(200 - head.len().min(200))]);
                        level_of(&head)
                    };
                    head.clear();
                    carry = lv.map_or(carry & !NAMED, |l| l | NAMED);
                    out.starts.push(start);
                    out.levels.push(carry);
                    i += nl + 1;
                }
                None => {
                    let rest = &chunk[i..];
                    head.extend_from_slice(&rest[..rest.len().min(200 - head.len().min(200))]);
                    open = Some(start);
                    i = n;
                }
            }
        }
        at += n as u64;
    }
    out.end = at;
    // A last line without its newline: shown, and read again once it ends.
    if let Some(start) = open {
        out.starts.push(start);
        out.levels.push(level_of(&head).map_or(carry & !NAMED, |l| l | NAMED));
        out.partial = true;
    }
    Ok(out)
}

/// The lines (from `from`) whose level is shown and that hold `query` (any case), read
/// in one pass; given up once `live` moves past `gen` (a newer filter was asked for).
fn filter(path: &Path, starts: &[u64], levels: &[u8], from: usize, query: &[u8], shown: [bool; 5], (live, gen): (&AtomicU64, u64)) -> Vec<u32> {
    let q = query.to_ascii_lowercase();
    let finder = memchr::memmem::Finder::new(&q);
    let mut out = Vec::new();
    let lines = starts.len().saturating_sub(1);
    if q.is_empty() {
        return (from..lines).filter(|&i| shown[(levels[i] & 7) as usize]).map(|i| i as u32).collect();
    }
    let Ok(mut f) = std::fs::File::open(path) else { return out };
    if f.seek(SeekFrom::Start(starts[from.min(lines)])).is_err() {
        return out;
    }
    let mut r = std::io::BufReader::with_capacity(1 << 20, f);
    let mut line = Vec::new();
    for i in from..lines {
        if i % 65_536 == 0 && live.load(Ordering::Relaxed) != gen {
            return Vec::new();
        }
        line.clear();
        if std::io::BufRead::read_until(&mut r, b'\n', &mut line).unwrap_or(0) == 0 {
            break;
        }
        if !shown[(levels[i] & 7) as usize] {
            continue;
        }
        line.make_ascii_lowercase();
        if finder.find(&line).is_some() {
            out.push(i as u32);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Filter,
    Level(u8),
    Prev,
    Next,
}

/// One log view.
pub struct Log {
    path: PathBuf,
    /// Each line's start, then the end of the last (one more than the lines).
    starts: Vec<u64>,
    levels: Vec<u8>,
    partial: bool,
    gen: u64,
    indexing: bool,
    err: Option<String>,
    counts: [usize; 5],
    filter: String,
    editing: bool,
    shown: [bool; 5],
    /// The filtered rows (line numbers); None: every line.
    rows: Option<Vec<u32>>,
    filter_gen: u64,
    /// The filter wanted now (a scan for an older one stops).
    live: Arc<AtomicU64>,
    filtering: bool,
    /// The row selected (a line number), its view's last bottom (following).
    sel: Option<u32>,
    bottom: Option<usize>,
    checked: Option<Instant>,
    hscroll: usize,
    /// The rows last read: (first line, lines).
    cache: Option<(usize, Vec<String>)>,
}

impl Log {
    fn lines(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    fn row_count(&self) -> usize {
        self.rows.as_ref().map_or(self.lines(), Vec::len)
    }

    fn line_at(&self, row: usize) -> Option<usize> {
        match &self.rows {
            Some(r) => r.get(row).map(|&l| l as usize),
            None => (row < self.lines()).then_some(row),
        }
    }

    /// Lines `a..b` of the file, read once per frame's need.
    fn read(&mut self, a: usize, b: usize) -> Vec<String> {
        let b = b.min(self.lines());
        if a >= b {
            return Vec::new();
        }
        if let Some((first, lines)) = &self.cache {
            if *first <= a && b <= first + lines.len() {
                return lines[a - first..b - first].to_vec();
            }
        }
        let (from, to) = (self.starts[a], self.starts[b]);
        let mut buf = vec![0u8; (to - from) as usize];
        let ok = std::fs::File::open(&self.path).and_then(|mut f| f.seek(SeekFrom::Start(from)).and_then(|_| f.read_exact(&mut buf))).is_ok();
        let lines: Vec<String> = if ok { buf.split(|&c| c == b'\n').take(b - a).map(clean).collect() } else { vec![String::new(); b - a] };
        self.cache = Some((a, lines.clone()));
        lines
    }
}

pub struct State {
    views: HashMap<String, Log>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    hits: Vec<(Rect, Hit)>,
    rows_at: Option<(Rect, usize)>,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = channel();
        State { views: HashMap::new(), tx, rx, hits: Vec::new(), rows_at: None }
    }
}

impl State {
    /// Work under way (the loop wakes for it).
    pub fn busy(&self) -> bool {
        self.views.values().any(|v| v.indexing || v.filtering)
    }

    /// A log in a tab: its file checked for new lines now and then.
    pub fn watching(&self) -> bool {
        !self.views.is_empty()
    }

    pub fn forget(&mut self, key: &str) {
        self.views.remove(key);
    }
}

fn start_index(tx: &Sender<Msg>, key: &str, v: &mut Log, from: u64) {
    v.indexing = true;
    v.gen += 1;
    let carry = v.levels.last().copied().unwrap_or(NONE);
    let (k, p, g, tx) = (key.to_string(), v.path.clone(), v.gen, tx.clone());
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Lines(k, g, index(&p, from, carry)));
    });
}

fn start_filter(tx: &Sender<Msg>, key: &str, v: &mut Log, from: usize) {
    if v.filter.is_empty() && v.shown.iter().all(|s| *s) {
        (v.rows, v.filtering) = (None, false);
        return;
    }
    if from == 0 {
        v.filter_gen += 1;
        v.live.store(v.filter_gen, Ordering::Relaxed);
        v.rows = Some(Vec::new());
    }
    v.filtering = true;
    let (k, p, g, tx, live) = (key.to_string(), v.path.clone(), v.filter_gen, tx.clone(), v.live.clone());
    let (starts, levels, q, shown) = (v.starts.clone(), v.levels.clone(), v.filter.clone().into_bytes(), v.shown);
    std::thread::spawn(move || {
        let rows = filter(&p, &starts, &levels, from, &q, shown, (&live, g));
        let _ = tx.send(Msg::Rows(k, g, from, rows));
    });
}

/// What the jobs found, and logs checked for new lines (the event loop, each turn).
pub fn tick(app: &mut App) -> bool {
    let mut changed = false;
    let tx = app.logs.tx.clone();
    while let Ok(m) = app.logs.rx.try_recv() {
        changed = true;
        match m {
            Msg::Lines(key, g, r) => {
                let Some(v) = app.logs.views.get_mut(&key).filter(|v| v.gen == g) else { continue };
                v.indexing = false;
                let r = match r {
                    Ok(r) => r,
                    Err(e) => {
                        v.err = Some(e);
                        continue;
                    }
                };
                // The end goes; an unfinished last line too (it was read again whole).
                v.starts.pop();
                if v.partial {
                    v.starts.pop();
                    v.levels.pop();
                }
                let from = v.starts.len();
                v.starts.extend(&r.starts);
                v.levels.extend(&r.levels);
                v.starts.push(r.end);
                v.partial = r.partial;
                v.counts = [0; 5];
                for l in &v.levels {
                    v.counts[(l & 7) as usize] += usize::from(l & NAMED != 0 || l & 7 == NONE);
                }
                v.cache = None;
                if v.rows.is_some() {
                    start_filter(&tx, &key, v, from.min(v.lines()));
                }
            }
            Msg::Rows(key, g, from, rows) => {
                let Some(v) = app.logs.views.get_mut(&key).filter(|v| v.filter_gen == g) else { continue };
                v.filtering = false;
                let r = v.rows.get_or_insert_with(Vec::new);
                r.retain(|&l| (l as usize) < from);
                r.extend(rows);
            }
        }
    }
    // Shown logs: grown (read on), or cut short (read again).
    for (key, v) in app.logs.views.iter_mut() {
        if v.indexing || v.checked.is_some_and(|t| t.elapsed() < WATCH) {
            continue;
        }
        v.checked = Some(Instant::now());
        let size = std::fs::metadata(&v.path).map_or(0, |m| m.len());
        let have = v.starts.last().copied().unwrap_or(0);
        if size < have {
            (v.starts, v.levels, v.partial) = (vec![0], Vec::new(), false);
            start_index(&tx, key, v, 0);
        } else if size > have || v.partial && size != have {
            let from = if v.partial { v.starts[v.starts.len() - 2] } else { have };
            start_index(&tx, key, v, from);
        }
    }
    changed
}

fn view<'a>(app: &'a mut App, key: &str, rel: &str) -> &'a mut Log {
    let tx = app.logs.tx.clone();
    let path = app.root.join(rel);
    let v = app.logs.views.entry(key.to_string()).or_insert_with(|| Log {
        path,
        starts: vec![0],
        levels: Vec::new(),
        partial: false,
        gen: 0,
        indexing: false,
        err: None,
        counts: [0; 5],
        filter: String::new(),
        editing: false,
        shown: [true; 5],
        rows: None,
        filter_gen: 0,
        live: Arc::new(AtomicU64::new(0)),
        filtering: false,
        sel: None,
        bottom: None,
        checked: None,
        hscroll: 0,
        cache: None,
    });
    if v.gen == 0 {
        start_index(&tx, key, v, 0);
        v.checked = Some(Instant::now());
    }
    v
}

fn human(n: usize) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 10_000 => format!("{:.0}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// The view: a bar (filter, levels with counts, the problems' arrows, lines), then the
/// rows: line number, time since the line before, the line (its level coloured).
pub fn draw(app: &mut App, f: &mut Frame, g: usize, key: &str, rel: &str, body: Rect, gh: &mut GroupHit) {
    fill(f, body, P.editor_bg);
    if body.height < 3 {
        return;
    }
    let focused = g == app.ag && app.focus == Focus::Editor;
    let scroll = app.groups[g].tabs[app.groups[g].active].view.scroll;
    let v = view(app, key, rel);
    let bar = Rect::new(body.x, body.y, body.width, 1);
    row(f, bar, P.widget_bg, vec![]);
    // The filter box.
    let fw = 28.min(bar.width / 3);
    let fr = Rect::new(bar.x + 1, bar.y, fw, 1);
    let shown = if v.filter.is_empty() && !v.editing { "Filter (/)".to_string() } else { v.filter.clone() };
    let st = if v.editing { Style::new().fg(P.editor_fg).bg(P.editor_bg) } else { Style::new().fg(if v.filter.is_empty() { P.dim } else { P.editor_fg }).bg(P.editor_bg) };
    f.render_widget(Span::styled(format!(" {:w$}", fit(&shown, fw as usize - 1), w = fw as usize - 1), st), fr);
    if v.editing && focused {
        f.set_cursor_position(Position::new((fr.x + 1 + v.filter.width() as u16).min(fr.right() - 1), fr.y));
    }
    // The lines' count (and what is under way) first: the buttons get what is left.
    let total = v.lines();
    let status = match (&v.err, v.indexing, v.filtering) {
        (Some(e), ..) => e.clone(),
        (_, true, _) => format!("{} lines…", human(total)),
        (_, _, true) => "filtering…".into(),
        _ if v.rows.is_some() => format!("{} of {} lines", human(v.row_count()), human(total)),
        _ => format!("{} lines", human(total)),
    };
    let sw = (status.width() as u16).min(bar.width / 2);
    f.render_widget(Span::styled(fit(&status, sw as usize), Style::new().fg(P.dim).bg(P.widget_bg)), Rect::new(bar.right().saturating_sub(sw + 1), bar.y, sw, 1));
    let room = bar.right().saturating_sub(sw + 2);
    let mut hits = vec![(fr, Hit::Filter)];
    let mut x = fr.right() + 1;
    let colors = [P.problem[2], P.problem[1], P.problem[0], P.dim];
    for lv in 0..4u8 {
        let t = format!(" ● {} {} ", human(v.counts[lv as usize]), NAMES[lv as usize]);
        let w = t.width() as u16;
        if x + w >= room {
            break;
        }
        let r = Rect::new(x, bar.y, w, 1);
        let on = v.shown[lv as usize];
        let base = crate::ui::kit::look_at(crate::ui::kit::Look::Secondary, r);
        f.render_widget(Line::from(vec![Span::styled(" ●", base.fg(if on { colors[lv as usize] } else { P.dim })), Span::styled(t[4..].to_string(), if on { base } else { base.fg(P.dim).add_modifier(Modifier::CROSSED_OUT) })]), r);
        hits.push((r, Hit::Level(lv)));
        x += w + 1;
    }
    for (label, hit) in [("‹", Hit::Prev), ("›", Hit::Next)] {
        if x + 3 >= room {
            break;
        }
        let r = Rect::new(x, bar.y, 3, 1);
        crate::ui::kit::icon_button(f, r, label, P.widget_bg, false);
        hits.push((r, hit));
        x += 4;
    }
    // The rows.
    let area = Rect::new(body.x, body.y + 1, body.width.saturating_sub(1), body.height - 1);
    let h = area.height as usize;
    let n = v.row_count();
    let max = n.saturating_sub(h);
    // At the bottom before: the bottom still (the file grew).
    let at_end = v.bottom.is_some_and(|b| scroll >= b);
    let off = if at_end { max } else { scroll.min(max) };
    // Not while the first lines are still being read (it opens at the top).
    if !(v.indexing && n == 0) {
        v.bottom = Some(max);
    }
    let digits = total.max(1).to_string().len();
    let lines: Vec<usize> = (off..(off + h).min(n)).filter_map(|r| v.line_at(r)).collect();
    // Each line and the one before it in the file (its time's gap).
    let mut text: HashMap<usize, String> = HashMap::new();
    if let (Some(&a), Some(&b)) = (lines.first(), lines.last()) {
        if v.rows.is_none() {
            for (i, t) in v.read(a.saturating_sub(1), b + 1).into_iter().enumerate() {
                text.insert(a.saturating_sub(1) + i, t);
            }
        } else {
            for &l in &lines {
                for (i, t) in v.read(l.saturating_sub(1), l + 1).into_iter().enumerate() {
                    text.insert(l.saturating_sub(1) + i, t);
                }
            }
        }
    }
    let gaps = lines.iter().any(|l| text.get(l).and_then(|t| time_of(t)).is_some());
    let gw = if gaps { 10 } else { 0 };
    let tw = (area.width as usize).saturating_sub(digits + 2 + gw);
    let hs = v.hscroll;
    for (k, &l) in lines.iter().enumerate() {
        let y = area.y + k as u16;
        let t = text.get(&l).cloned().unwrap_or_default();
        let lv = v.levels.get(l).copied().unwrap_or(NONE);
        let sel = v.sel == Some(l as u32);
        let bg = if sel { P.list_active_bg } else { P.editor_bg };
        let mut spans = vec![Span::styled(format!("{:>digits$} ", l + 1), Style::new().fg(P.line_nr).bg(bg))];
        if gaps {
            let d = match (time_of(&t), l.checked_sub(1).and_then(|p| text.get(&p)).and_then(|p| time_of(p))) {
                (Some(a), Some(b)) if a >= b => gap(a - b),
                _ => String::new(),
            };
            spans.push(Span::styled(format!("{d:>9} "), Style::new().fg(P.dim).bg(bg)));
        }
        spans.push(Span::styled(" ", Style::new().bg(bg)));
        let fg = match (lv & 7, lv & NAMED != 0) {
            (ERROR, _) => P.problem[2],
            (WARN, _) => P.problem[1],
            (DEBUG, _) => P.dim,
            _ => P.editor_fg,
        };
        let shown: String = t.chars().skip(hs).collect();
        let st = Style::new().fg(fg).bg(bg);
        let st = if lv & NAMED != 0 && lv & 7 == ERROR { st.add_modifier(Modifier::BOLD) } else { st };
        spans.push(Span::styled(fit(&shown, tw), st));
        row(f, Rect::new(area.x, y, area.width, 1), bg, spans);
    }
    if n == 0 && !v.indexing {
        let msg = if v.rows.is_some() { "No lines match." } else { "Empty." };
        f.render_widget(Span::styled(msg, Style::new().fg(P.dim)), Rect::new(area.x + 2, area.y, area.width.saturating_sub(2), 1));
    }
    app.logs.hits = hits;
    app.logs.rows_at = Some((area, off));
    let grp = &mut app.groups[g];
    grp.tabs[grp.active].view.scroll = off;
    gh.text = area;
    gh.preview_rows = n;
    if n > h {
        let track = Rect::new(body.right().saturating_sub(1), area.y, 1, area.height);
        crate::ui::scrollbar(app, f, track, crate::panel::ScrollTarget::Editor(g), n, h, off, max);
    }
}

#[cfg(test)]
impl App {
    /// The rect of the bar's level button named `name` (tests).
    pub fn logs_hit_for_test(&self, name: &str) -> Rect {
        let lv = NAMES.iter().position(|n| *n == name).expect("a level") as u8;
        self.logs.hits.iter().find(|(_, h)| *h == Hit::Level(lv)).map(|(r, _)| *r).expect("drawn")
    }
}

/// The active tab's log view, when it shows one.
fn active(app: &App) -> Option<String> {
    let g = app.groups.get(app.ag)?;
    let key = &g.tabs.get(g.active)?.key;
    app.logs.views.contains_key(key).then(|| key.clone())
}

/// The next (`dir` 1) or previous (-1) error or warning from the selection (else the
/// view's top), selected and a third down the view.
fn problem(app: &mut App, key: &str, dir: isize) {
    let h = app.logs.rows_at.map_or(20, |(r, _)| r.height as usize);
    let top = app.groups[app.ag].tabs[app.groups[app.ag].active].view.scroll;
    let Some(v) = app.logs.views.get_mut(key) else { return };
    let rows = v.row_count();
    let at = v.sel.and_then(|s| (0..rows).find(|&r| v.line_at(r) == Some(s as usize)));
    let is = |v: &Log, r: usize| v.line_at(r).is_some_and(|l| v.levels[l] & NAMED != 0 && v.levels[l] & 7 <= WARN);
    let found = if dir > 0 { (at.map_or(top, |a| a + 1)..rows).find(|&r| is(v, r)) } else { (0..at.unwrap_or(top)).rev().find(|&r| is(v, r)) };
    let Some(r) = found else {
        app.msg = format!("no {} error or warning", if dir > 0 { "later" } else { "earlier" });
        return;
    };
    v.sel = v.line_at(r).map(|l| l as u32);
    v.bottom = None;
    let grp = &mut app.groups[app.ag];
    grp.tabs[grp.active].view.scroll = r.saturating_sub(h / 3);
}

fn refilter(app: &mut App, key: &str) {
    let tx = app.logs.tx.clone();
    if let Some(v) = app.logs.views.get_mut(key) {
        v.bottom = None;
        start_filter(&tx, key, v, 0);
    }
}

/// Keys on a log view; false: not ours (the editor's scrolling takes them).
pub fn key(app: &mut App, k: KeyEvent) -> bool {
    let Some(key) = active(app) else { return false };
    let Some(v) = app.logs.views.get_mut(&key) else { return false };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    if v.editing {
        match k.code {
            KeyCode::Char(c) if !ctrl => v.filter.push(c),
            KeyCode::Backspace => {
                v.filter.pop();
            }
            KeyCode::Enter | KeyCode::Down => {
                v.editing = false;
                return true;
            }
            KeyCode::Esc => {
                v.filter.clear();
                v.editing = false;
            }
            _ => return true,
        }
        refilter(app, &key);
        return true;
    }
    match k.code {
        KeyCode::Char('/') if !ctrl => v.editing = true,
        KeyCode::Char('f') if ctrl => v.editing = true,
        KeyCode::Esc if !v.filter.is_empty() => {
            v.filter.clear();
            refilter(app, &key);
        }
        KeyCode::Esc => v.sel = None,
        KeyCode::F(8) => problem(app, &key, if k.modifiers.contains(KeyModifiers::SHIFT) { -1 } else { 1 }),
        KeyCode::Char('n') => problem(app, &key, 1),
        KeyCode::Char('N') => problem(app, &key, -1),
        KeyCode::Char('c') if ctrl => {
            let Some(l) = v.sel.map(|s| s as usize) else {
                app.msg = "click a line first".into();
                return true;
            };
            let t = v.read(l, l + 1).pop().unwrap_or_default();
            app.msg = format!("copied line {} via {}", l + 1, crate::panel::clipboard_set(&t));
            app.clip = t;
        }
        KeyCode::Left => v.hscroll = v.hscroll.saturating_sub(8),
        KeyCode::Right => v.hscroll += 8,
        KeyCode::Home | KeyCode::Char('g') => {
            v.bottom = None;
            let grp = &mut app.groups[app.ag];
            grp.tabs[grp.active].view.scroll = 0;
        }
        // The end, followed as lines come.
        KeyCode::End | KeyCode::Char('G') => {
            let grp = &mut app.groups[app.ag];
            grp.tabs[grp.active].view.scroll = usize::MAX / 2;
        }
        // Edits have nothing to change; the rest scrolls as any preview.
        _ => return ctrl && matches!(k.code, KeyCode::Char('v' | 'x' | 'z' | 'y')),
    }
    true
}

/// A click: the bar's filter, levels and arrows; a row selects its line.
pub fn click(app: &mut App, p: Position) -> bool {
    let Some(key) = active(app) else { return false };
    let hit = app.logs.hits.iter().find(|(r, _)| r.contains(p)).map(|(_, h)| *h);
    let rows_at = app.logs.rows_at;
    let Some(v) = app.logs.views.get_mut(&key) else { return false };
    match hit {
        Some(Hit::Filter) => v.editing = true,
        Some(Hit::Level(l)) => {
            v.shown[l as usize] = !v.shown[l as usize];
            // A stack trace's lines (no level of their own) go with their error.
            v.shown[NONE as usize] = v.shown[..4].iter().any(|s| *s);
            refilter(app, &key);
        }
        Some(Hit::Prev) => problem(app, &key, -1),
        Some(Hit::Next) => problem(app, &key, 1),
        None => {
            let Some((area, off)) = rows_at.filter(|(a, _)| a.contains(p)) else { return false };
            let row = off + (p.y - area.y) as usize;
            v.sel = v.line_at(row).map(|l| l as u32);
            v.editing = false;
        }
    }
    app.focus = Focus::Editor;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_found_as_logs_write_them() {
        let lv = |s: &str| level_of(s.as_bytes());
        assert_eq!(lv("2026-11-17 12:00:00 ERROR worker crashed"), Some(ERROR));
        assert_eq!(lv("[warn] disk almost full"), Some(WARN));
        assert_eq!(lv("ts=1 level=info msg=started"), Some(INFO));
        assert_eq!(lv("E (1234) wifi: connect failed"), Some(ERROR));
        assert_eq!(lv("W (99) adc: noisy"), Some(WARN));
        assert_eq!(lv("main.c:3: error: expected ';'"), Some(ERROR));
        assert_eq!(lv("I saw no error in it"), None, "a word in a sentence is no level");
        assert_eq!(lv("    at com.x.Main.run(Main.java:10)"), None);
        assert_eq!(lv("12:00:01 DEBUG cache hit"), Some(DEBUG));
    }

    #[test]
    fn times_read_from_line_starts() {
        let a = time_of("2026-11-17 12:00:00.250 INFO x").unwrap();
        let b = time_of("2026-11-17T12:00:01.500Z INFO y").unwrap();
        assert!((b - a - 1.25).abs() < 1e-6);
        assert_eq!(time_of("[12:00:02] boot").map(|t| t - 43200.0), Some(2.0));
        assert!((time_of("Nov 17 12:00:03 host sshd[1]: ok").unwrap() - time_of("Nov 17 12:00:00 host x").unwrap() - 3.0).abs() < 1e-6);
        assert_eq!(time_of("[   12.500000] usb 1-1: new device"), Some(12.5));
        assert_eq!(time_of("I (1500) wifi: up"), Some(1.5));
        assert_eq!(time_of("no time here"), None);
        assert_eq!((gap(0.012), gap(1.25), gap(185.0)), ("+12 ms".into(), "+1.25 s".into(), "+3m05s".into()));
    }

    #[test]
    fn a_file_indexes_whole_and_on_as_it_grows_with_traces_under_their_errors() {
        let dir = std::env::temp_dir().join(format!("termcode-logidx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.log");
        std::fs::write(&p, "INFO start\nERROR boom\n  at x()\nWARN slow").unwrap();
        let r = index(&p, 0, NONE).unwrap();
        assert_eq!(r.starts, [0, 11, 22, 31]);
        assert_eq!(r.levels.iter().map(|l| l & 7).collect::<Vec<_>>(), [INFO, ERROR, ERROR, WARN], "the trace takes its error's level");
        assert_eq!((r.levels[2] & NAMED, r.partial, r.end), (0, true, 40));
        // Grown: the unfinished line read again from its start.
        std::fs::write(&p, "INFO start\nERROR boom\n  at x()\nWARN slower\nDEBUG d\n").unwrap();
        let more = index(&p, 31, ERROR).unwrap();
        assert_eq!((more.starts, more.partial), (vec![31, 43], false));
        let live = AtomicU64::new(1);
        let (starts, levels) = ([0, 11, 22, 31, 43, 51], [INFO | NAMED, ERROR | NAMED, ERROR, WARN | NAMED, DEBUG | NAMED]);
        assert_eq!(filter(&p, &starts, &levels, 0, b"BOOM", [true; 5], (&live, 1)), [1]);
        assert_eq!(filter(&p, &starts, &levels, 3, b"d", [true; 5], (&live, 1)), [4], "from a line on");
        assert_eq!(filter(&p, &starts, &levels, 0, b"", [true, false, false, false, false], (&live, 1)), [1, 2], "errors and their traces");
        // A long line, a chunk's border inside it: its level still read from its start.
        let long = format!("{}\nERROR {}\n", "x".repeat((1 << 20) - 3), "y".repeat(300));
        std::fs::write(&p, &long).unwrap();
        let r = index(&p, 0, NONE).unwrap();
        assert_eq!((r.starts, r.levels[1] & 7), (vec![0, (1 << 20) - 2], ERROR));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn colours_and_tabs_are_cleaned() {
        assert_eq!(clean(b"\x1b[0;31mE (12) x: bad\x1b[0m\tend\r"), "E (12) x: bad    end");
    }
}

