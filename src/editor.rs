//! Text buffer with VS Code-style editing. The buffer (model) is shared by every
//! tab showing the file; each tab keeps its own `View` (cursor, selection, scroll).

use crate::theme;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

/// Typing pauses longer than this start a new undo step (VS Code-like).
const UNDO_PAUSE: Duration = Duration::from_secs(1);
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, Style as SStyle};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use unicode_width::UnicodeWidthChar;

pub const MAX_BYTES: u64 = 1 << 20;
pub const MAX_LINES: usize = 20_000;
pub const TAB: usize = 4;
/// Lines longer than this are not highlighted (minified code); state carries over unchanged.
const HL_MAX_LINE: usize = 4000;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    /// Byte offset, always on a char boundary.
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

#[derive(Clone, Debug, Default)]
pub struct View {
    pub cursor: Pos,
    pub anchor: Option<Pos>,
    /// Sticky display column for Up/Down.
    pub want_x: Option<usize>,
    pub scroll: usize,
    pub hscroll: usize,
}

impl View {
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| if a < self.cursor { (a, self.cursor) } else { (self.cursor, a) })
    }
}

#[derive(Clone, Debug)]
enum Op {
    Ins(Pos, String),
    Del(Pos, String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Type,
    Erase,
    Other,
}

struct Group {
    ops: Vec<Op>,
    before: (Pos, Option<Pos>),
    after: (Pos, Option<Pos>),
    ver_before: u64,
    ver_after: u64,
}

struct Hl {
    syntax: &'static SyntaxReference,
    /// `states[i]` is the parser state at the start of line i (valid prefix).
    states: Vec<(ParseState, HighlightState)>,
    /// Highlighted segments `(end byte, style)` per line (valid prefix).
    segs: Vec<Vec<(usize, SStyle)>>,
}

pub struct Buffer {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub crlf: bool,
    pub final_newline: bool,
    /// Why this buffer can never be saved (truncated, not UTF-8, binary, …).
    pub readonly: Option<String>,
    pub binary: bool,
    pub mtime: Option<SystemTime>,
    mode: Option<u32>,
    pub indent: String,
    pub lang: String,
    /// The file changed on disk while this buffer was dirty.
    pub disk_changed: bool,
    /// Disk mtime a save was refused for; a second save overwrites only that version.
    armed_mtime: Option<SystemTime>,
    version: u64,
    saved: u64,
    ver_counter: u64,
    undo: Vec<Group>,
    redo: Vec<Group>,
    open_kind: Option<Kind>,
    last_edit: Option<Instant>,
    hl: Option<Hl>,
}

pub fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

fn highlighter() -> &'static Highlighter<'static> {
    static H: OnceLock<Highlighter<'static>> = OnceLock::new();
    H.get_or_init(|| Highlighter::new(theme::syntax_theme()))
}

/// Advance a display column over `c` (tabs to the next stop, wide chars = 2).
pub fn advance(x: usize, c: char) -> usize {
    match c {
        '\t' => (x / TAB + 1) * TAB,
        c if c.is_control() => x + 1,
        c => x + c.width().unwrap_or(0),
    }
}

/// Display column of byte offset `byte` in `line`.
pub fn display_col(line: &str, byte: usize) -> usize {
    line.char_indices().take_while(|(i, _)| *i < byte).fold(0, |x, (_, c)| advance(x, c))
}

/// Byte offset of the char boundary nearest to display column `target`.
pub fn byte_at_display(line: &str, target: usize) -> usize {
    let mut x = 0;
    for (i, c) in line.char_indices() {
        let nx = advance(x, c);
        if target < nx {
            return if (target - x) * 2 < nx - x { i } else { i + c.len_utf8() };
        }
        x = nx;
    }
    line.len()
}

fn prev_boundary(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(j, _)| j)
}

fn next_boundary(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn end_pos(at: Pos, s: &str) -> Pos {
    match s.rfind('\n') {
        None => Pos::new(at.line, at.col + s.len()),
        Some(i) => Pos::new(at.line + s.matches('\n').count(), s.len() - i - 1),
    }
}

impl Buffer {
    fn empty(path: Option<PathBuf>) -> Buffer {
        Buffer {
            path,
            lines: vec![String::new()],
            crlf: false,
            final_newline: true,
            readonly: None,
            binary: false,
            mtime: None,
            mode: None,
            indent: " ".repeat(TAB),
            lang: "Plain Text".into(),
            disk_changed: false,
            armed_mtime: None,
            version: 0,
            saved: 0,
            ver_counter: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            open_kind: None,
            last_edit: None,
            hl: None,
        }
    }

    /// Read-only text (commit view, notices).
    pub fn from_text(text: &str, syntax: &str, why: &str) -> Buffer {
        let mut b = Buffer::empty(None);
        b.set_text(text);
        b.readonly = Some(why.into());
        b.set_syntax(syntaxes().find_syntax_by_name(syntax));
        b
    }

    pub fn open(path: &Path) -> Buffer {
        let mut b = Buffer::empty(Some(path.to_path_buf()));
        match fs::metadata(path) {
            Ok(m) if m.is_file() => {
                b.mtime = m.modified().ok();
                b.mode = Some(m.permissions().mode() & 0o7777);
            }
            Ok(_) => {
                b.readonly = Some("not a regular file".into());
                return b;
            }
            Err(e) => {
                b.readonly = Some(e.to_string());
                return b;
            }
        }
        let mut buf = Vec::new();
        if let Err(e) = fs::File::open(path).and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut buf)) {
            b.readonly = Some(e.to_string());
            return b;
        }
        if buf[..buf.len().min(8192)].contains(&0) {
            b.binary = true;
            b.readonly = Some("binary file: not shown, cannot be edited".into());
            return b;
        }
        let capped = buf.len() as u64 > MAX_BYTES;
        buf.truncate(MAX_BYTES as usize);
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => {
                if !capped {
                    b.readonly = Some("not valid UTF-8: read-only so saving cannot corrupt it".into());
                }
                String::from_utf8_lossy(e.as_bytes()).into_owned()
            }
        };
        b.set_text(&text);
        if capped || b.lines.len() > MAX_LINES {
            b.lines.truncate(MAX_LINES);
            b.readonly = Some(format!("truncated to 1 MB / {MAX_LINES} lines: read-only, cannot be saved"));
        }
        let ss = syntaxes();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let syn = ss
            .find_syntax_by_extension(ext)
            .or_else(|| ss.find_syntax_by_extension(name))
            .or_else(|| ss.find_syntax_by_first_line(&b.lines[0]));
        b.set_syntax(syn);
        b
    }

    fn set_syntax(&mut self, syn: Option<&'static SyntaxReference>) {
        let syn = syn.unwrap_or_else(|| syntaxes().find_syntax_plain_text());
        self.lang = syn.name.clone();
        let start = (ParseState::new(syn), HighlightState::new(highlighter(), ScopeStack::new()));
        self.hl = Some(Hl { syntax: syn, states: vec![start], segs: Vec::new() });
    }

    fn set_text(&mut self, text: &str) {
        self.crlf = text.find('\n').is_some_and(|i| text[..i].ends_with('\r'));
        self.final_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let pieces: Vec<&str> = body.split('\n').collect();
        let n = pieces.len();
        // In CRLF mode drop only the '\r' that belonged to a line break; a lone
        // trailing '\r' on an unterminated last line is content and survives.
        self.lines = pieces
            .iter()
            .enumerate()
            .map(|(k, l)| {
                let ended = k + 1 < n || self.final_newline;
                if self.crlf && ended { l.strip_suffix('\r').unwrap_or(l) } else { l }.to_string()
            })
            .collect();
        let tabs = self.lines.iter().take(2000).filter(|l| l.starts_with('\t')).count();
        let spaces = self.lines.iter().take(2000).filter(|l| l.starts_with(' ')).count();
        self.indent = if tabs > spaces { "\t".into() } else { " ".repeat(TAB) };
    }

    /// The file content as it would be written.
    pub fn text(&self) -> String {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.lines.join(eol);
        if self.final_newline {
            s.push_str(eol);
        }
        s
    }

    pub fn dirty(&self) -> bool {
        self.version != self.saved
    }

    /// Changes with every edit; undo/redo restore earlier values.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        let l = &self.lines[line];
        let mut col = p.col.min(l.len());
        while !l.is_char_boundary(col) {
            col -= 1;
        }
        Pos::new(line, col)
    }

    pub fn clamp_view(&self, v: &mut View) {
        v.cursor = self.clamp(v.cursor);
        v.anchor = v.anchor.map(|a| self.clamp(a));
        v.scroll = v.scroll.min(self.lines.len() - 1);
    }

    pub fn text_range(&self, a: Pos, b: Pos) -> String {
        if a.line == b.line {
            return self.lines[a.line][a.col..b.col].to_string();
        }
        let mut s = self.lines[a.line][a.col..].to_string();
        for l in &self.lines[a.line + 1..b.line] {
            s.push('\n');
            s.push_str(l);
        }
        s.push('\n');
        s.push_str(&self.lines[b.line][..b.col]);
        s
    }

    pub fn selected_text(&self, v: &View) -> Option<String> {
        let (a, b) = v.selection()?;
        Some(self.text_range(self.clamp(a), self.clamp(b)))
    }

    // ---- highlighting ------------------------------------------------------

    fn invalidate(&mut self, line: usize) {
        if let Some(h) = &mut self.hl {
            h.segs.truncate(line);
            h.states.truncate(line + 1);
        }
    }

    /// Highlight lines up to `upto` (exclusive), resuming from the cached state.
    pub fn highlight_upto(&mut self, upto: usize) {
        let Some(h) = &mut self.hl else { return };
        let upto = upto.min(self.lines.len());
        while h.segs.len() < upto {
            let i = h.segs.len();
            let (mut ps, mut hs) = h.states[i].clone();
            let line = &self.lines[i];
            let plain = vec![(line.len(), SStyle::default())];
            let segs = if line.len() > HL_MAX_LINE {
                plain
            } else {
                match ps.parse_line(line, syntaxes()) {
                    Ok(ops) => {
                        let mut end = 0;
                        HighlightIterator::new(&mut hs, &ops, line, highlighter())
                            .map(|(st, t)| {
                                end += t.len();
                                (end, st)
                            })
                            .collect()
                    }
                    Err(_) => plain,
                }
            };
            h.segs.push(segs);
            h.states.push((ps, hs));
        }
    }

    pub fn segs(&self, line: usize) -> Option<&[(usize, SStyle)]> {
        self.hl.as_ref()?.segs.get(line).map(Vec::as_slice)
    }

    pub fn syntax_name(&self) -> &str {
        self.hl.as_ref().map_or("Plain Text", |h| &h.syntax.name)
    }

    // ---- raw edits ---------------------------------------------------------

    fn ins_raw(&mut self, at: Pos, s: &str) -> Pos {
        let tail = self.lines[at.line].split_off(at.col);
        let mut parts = s.split('\n');
        self.lines[at.line].push_str(parts.next().unwrap_or(""));
        let rest: Vec<String> = parts.map(String::from).collect();
        let end = match rest.last() {
            None => Pos::new(at.line, self.lines[at.line].len()),
            Some(last) => Pos::new(at.line + rest.len(), last.len()),
        };
        self.lines.splice(at.line + 1..at.line + 1, rest);
        self.lines[end.line].push_str(&tail);
        self.invalidate(at.line);
        end
    }

    fn del_raw(&mut self, a: Pos, b: Pos) -> String {
        if a.line == b.line {
            let s = self.lines[a.line].drain(a.col..b.col).collect();
            self.invalidate(a.line);
            return s;
        }
        let tail = self.lines[b.line][b.col..].to_string();
        let mut removed = self.lines[a.line].split_off(a.col);
        let mid: Vec<String> = self.lines.drain(a.line + 1..=b.line).collect();
        let n = mid.len();
        for (k, l) in mid.into_iter().enumerate() {
            removed.push('\n');
            removed.push_str(if k + 1 == n { &l[..b.col] } else { &l });
        }
        self.lines[a.line].push_str(&tail);
        self.invalidate(a.line);
        removed
    }

    // ---- undo groups -------------------------------------------------------

    fn begin(&mut self, v: &View, kind: Kind) {
        let merge = kind != Kind::Other
            && self.open_kind == Some(kind)
            && self.last_edit.is_some_and(|t| t.elapsed() < UNDO_PAUSE)
            && self.undo.last().is_some_and(|g| g.after == (v.cursor, v.anchor));
        if !merge {
            self.undo.push(Group {
                ops: Vec::new(),
                before: (v.cursor, v.anchor),
                after: (v.cursor, v.anchor),
                ver_before: self.version,
                ver_after: self.version,
            });
        }
        self.open_kind = (kind != Kind::Other).then_some(kind);
    }

    fn end(&mut self, v: &View) {
        if self.undo.last().is_some_and(|g| g.ops.is_empty()) {
            self.undo.pop();
            return;
        }
        if let Some(g) = self.undo.last_mut() {
            g.after = (v.cursor, v.anchor);
            g.ver_after = self.version;
        }
        self.last_edit = Some(Instant::now());
        self.redo.clear();
    }

    fn bump(&mut self) {
        self.ver_counter += 1;
        self.version = self.ver_counter;
    }

    fn do_ins(&mut self, at: Pos, s: &str) -> Pos {
        let e = self.ins_raw(at, s);
        self.bump();
        if let Some(g) = self.undo.last_mut() {
            g.ops.push(Op::Ins(at, s.to_string()));
        }
        e
    }

    fn do_del(&mut self, a: Pos, b: Pos) {
        let s = self.del_raw(a, b);
        self.bump();
        if let Some(g) = self.undo.last_mut() {
            g.ops.push(Op::Del(a, s));
        }
    }

    pub fn undo(&mut self, v: &mut View) -> bool {
        self.open_kind = None;
        let Some(g) = self.undo.pop() else { return false };
        for op in g.ops.iter().rev() {
            match op {
                Op::Ins(at, s) => {
                    self.del_raw(*at, end_pos(*at, s));
                }
                Op::Del(at, s) => {
                    self.ins_raw(*at, s);
                }
            }
        }
        self.version = g.ver_before;
        (v.cursor, v.anchor) = g.before;
        v.want_x = None;
        self.redo.push(g);
        true
    }

    pub fn redo(&mut self, v: &mut View) -> bool {
        self.open_kind = None;
        let Some(g) = self.redo.pop() else { return false };
        for op in &g.ops {
            match op {
                Op::Ins(at, s) => {
                    self.ins_raw(*at, s);
                }
                Op::Del(at, s) => {
                    self.del_raw(*at, end_pos(*at, s));
                }
            }
        }
        self.version = g.ver_after;
        (v.cursor, v.anchor) = g.after;
        v.want_x = None;
        self.undo.push(g);
        true
    }

    // ---- editing commands (no-ops on read-only buffers) --------------------

    fn delete_selection(&mut self, v: &mut View) -> bool {
        match v.selection() {
            Some((a, b)) => {
                let (a, b) = (self.clamp(a), self.clamp(b));
                self.do_del(a, b);
                v.cursor = a;
                v.anchor = None;
                true
            }
            None => {
                v.anchor = None;
                false
            }
        }
    }

    fn insert_kind(&mut self, v: &mut View, s: &str, kind: Kind) {
        if self.readonly.is_some() {
            return;
        }
        v.cursor = self.clamp(v.cursor);
        self.begin(v, kind);
        self.delete_selection(v);
        v.cursor = self.do_ins(v.cursor, s);
        v.want_x = None;
        self.end(v);
    }

    /// Type or paste text at the cursor, replacing the selection.
    pub fn insert(&mut self, v: &mut View, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let typing = s.chars().count() == 1 && s != "\n";
        // Word boundary: a space typed after a word starts a new undo step.
        let c = self.clamp(v.cursor);
        let prev = self.lines[c.line][..c.col].chars().next_back();
        if typing && s.starts_with(char::is_whitespace) && prev.is_some_and(|p| !p.is_whitespace()) {
            self.open_kind = None;
        }
        self.insert_kind(v, &s, if typing { Kind::Type } else { Kind::Other });
    }

    /// Enter: new line keeping the current line's indentation.
    pub fn newline(&mut self, v: &mut View) {
        let c = self.clamp(v.cursor);
        let indent: String = self.lines[c.line][..c.col].chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        self.insert_kind(v, &format!("\n{indent}"), Kind::Other);
    }

    pub fn backspace(&mut self, v: &mut View) {
        if self.readonly.is_some() {
            return;
        }
        v.cursor = self.clamp(v.cursor);
        self.begin(v, Kind::Erase);
        if !self.delete_selection(v) {
            let c = v.cursor;
            let a = if c.col > 0 {
                Pos::new(c.line, prev_boundary(&self.lines[c.line], c.col))
            } else if c.line > 0 {
                Pos::new(c.line - 1, self.lines[c.line - 1].len())
            } else {
                c
            };
            if a != c {
                self.do_del(a, c);
                v.cursor = a;
            }
        }
        v.want_x = None;
        self.end(v);
    }

    pub fn delete(&mut self, v: &mut View) {
        if self.readonly.is_some() {
            return;
        }
        v.cursor = self.clamp(v.cursor);
        self.begin(v, Kind::Other);
        if !self.delete_selection(v) {
            let c = v.cursor;
            let l = &self.lines[c.line];
            let b = if c.col < l.len() {
                Pos::new(c.line, next_boundary(l, c.col))
            } else if c.line + 1 < self.lines.len() {
                Pos::new(c.line + 1, 0)
            } else {
                c
            };
            if b != c {
                self.do_del(c, b);
            }
        }
        self.end(v);
    }

    /// Lines touched by the selection (a selection ending at column 0 excludes that line).
    fn sel_lines(&self, v: &View) -> Option<(usize, usize)> {
        let (a, b) = v.selection()?;
        let last = if b.col == 0 && b.line > a.line { b.line - 1 } else { b.line };
        (last > a.line || a.line != b.line).then_some((a.line, last))
    }

    /// Tab: indent selected lines, or insert indentation at the cursor.
    pub fn indent(&mut self, v: &mut View) {
        if self.readonly.is_some() {
            return;
        }
        let Some((first, last)) = self.sel_lines(v) else {
            let c = self.clamp(v.cursor);
            let unit = if self.indent == "\t" {
                "\t".to_string()
            } else {
                " ".repeat(TAB - display_col(&self.lines[c.line], c.col) % TAB)
            };
            return self.insert_kind(v, &unit, Kind::Other);
        };
        self.begin(v, Kind::Other);
        let unit = self.indent.clone();
        for l in first..=last {
            self.do_ins(Pos::new(l, 0), &unit);
        }
        for p in std::iter::once(&mut v.cursor).chain(v.anchor.as_mut()) {
            if (first..=last).contains(&p.line) && p.col > 0 {
                p.col += unit.len();
            }
        }
        self.end(v);
    }

    /// Shift+Tab: remove one indentation level from the selected or current lines.
    pub fn outdent(&mut self, v: &mut View) {
        if self.readonly.is_some() {
            return;
        }
        let (first, last) = self.sel_lines(v).unwrap_or((v.cursor.line, v.cursor.line));
        self.begin(v, Kind::Other);
        for l in first..=last.min(self.lines.len() - 1) {
            let line = &self.lines[l];
            let n = if line.starts_with('\t') { 1 } else { line.bytes().take(TAB).take_while(|b| *b == b' ').count() };
            if n > 0 {
                self.do_del(Pos::new(l, 0), Pos::new(l, n));
                for p in std::iter::once(&mut v.cursor).chain(v.anchor.as_mut()) {
                    if p.line == l {
                        p.col = p.col.saturating_sub(n);
                    }
                }
            }
        }
        self.end(v);
    }

    /// Replace every match (back to front so earlier positions stay valid). One undo step.
    pub fn replace_all(&mut self, v: &mut View, matches: &[(Pos, Pos)], with: &str) -> usize {
        if self.readonly.is_some() || matches.is_empty() {
            return 0;
        }
        self.begin(v, Kind::Other);
        for &(a, b) in matches.iter().rev() {
            self.do_del(a, b);
            self.do_ins(a, with);
        }
        v.anchor = None;
        v.cursor = self.clamp(v.cursor);
        self.end(v);
        matches.len()
    }

    // ---- movement ----------------------------------------------------------

    pub fn move_to(&mut self, v: &mut View, p: Pos, extend: bool) {
        if extend {
            v.anchor.get_or_insert(v.cursor);
        } else {
            v.anchor = None;
        }
        v.cursor = self.clamp(p);
        self.open_kind = None;
    }

    fn word_left(&self, c: Pos) -> Pos {
        if c.col == 0 {
            return if c.line > 0 { Pos::new(c.line - 1, self.lines[c.line - 1].len()) } else { c };
        }
        let l = &self.lines[c.line];
        let mut it = l[..c.col].char_indices().rev().skip_while(|(_, ch)| class(*ch) == 0).peekable();
        let Some(&(_, first)) = it.peek() else { return Pos::new(c.line, 0) };
        let k = class(first);
        let mut col = 0;
        for (i, ch) in it {
            if class(ch) != k {
                col = i + ch.len_utf8();
                break;
            }
        }
        Pos::new(c.line, col)
    }

    fn word_right(&self, c: Pos) -> Pos {
        let l = &self.lines[c.line];
        if c.col >= l.len() {
            return if c.line + 1 < self.lines.len() { Pos::new(c.line + 1, 0) } else { c };
        }
        let mut it = l[c.col..].char_indices().skip_while(|(_, ch)| class(*ch) == 0).peekable();
        let Some(&(_, first)) = it.peek() else { return Pos::new(c.line, l.len()) };
        let k = class(first);
        for (i, ch) in it {
            if class(ch) != k {
                return Pos::new(c.line, c.col + i);
            }
        }
        Pos::new(c.line, l.len())
    }

    pub fn left(&mut self, v: &mut View, extend: bool, word: bool) {
        if !extend && !word {
            if let Some((a, _)) = v.selection() {
                return self.move_to(v, a, false);
            }
        }
        let c = self.clamp(v.cursor);
        let p = if word {
            self.word_left(c)
        } else if c.col > 0 {
            Pos::new(c.line, prev_boundary(&self.lines[c.line], c.col))
        } else if c.line > 0 {
            Pos::new(c.line - 1, self.lines[c.line - 1].len())
        } else {
            c
        };
        self.move_to(v, p, extend);
        v.want_x = None;
    }

    pub fn right(&mut self, v: &mut View, extend: bool, word: bool) {
        if !extend && !word {
            if let Some((_, b)) = v.selection() {
                return self.move_to(v, b, false);
            }
        }
        let c = self.clamp(v.cursor);
        let l = &self.lines[c.line];
        let p = if word {
            self.word_right(c)
        } else if c.col < l.len() {
            Pos::new(c.line, next_boundary(l, c.col))
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            c
        };
        self.move_to(v, p, extend);
        v.want_x = None;
    }

    pub fn vertical(&mut self, v: &mut View, delta: isize, extend: bool) {
        let c = self.clamp(v.cursor);
        let x = v.want_x.unwrap_or_else(|| display_col(&self.lines[c.line], c.col));
        let target = c.line as isize + delta;
        let p = if target < 0 {
            Pos::new(0, 0)
        } else if target as usize >= self.lines.len() {
            let last = self.lines.len() - 1;
            Pos::new(last, self.lines[last].len())
        } else {
            let line = target as usize;
            Pos::new(line, byte_at_display(&self.lines[line], x))
        };
        self.move_to(v, p, extend);
        v.want_x = Some(x);
    }

    /// Home: first non-blank, or column 0 when already there.
    pub fn home(&mut self, v: &mut View, extend: bool) {
        let c = self.clamp(v.cursor);
        let l = &self.lines[c.line];
        let first = l.len() - l.trim_start().len();
        self.move_to(v, Pos::new(c.line, if c.col == first { 0 } else { first }), extend);
        v.want_x = None;
    }

    pub fn end_of_line(&mut self, v: &mut View, extend: bool) {
        let c = self.clamp(v.cursor);
        self.move_to(v, Pos::new(c.line, self.lines[c.line].len()), extend);
        v.want_x = None;
    }

    pub fn select_all(&mut self, v: &mut View) {
        let last = self.lines.len() - 1;
        v.anchor = Some(Pos::new(0, 0));
        v.cursor = Pos::new(last, self.lines[last].len());
        self.open_kind = None;
    }

    /// Select the word under `p` (double-click).
    pub fn select_word(&mut self, v: &mut View, p: Pos) {
        let p = self.clamp(p);
        let l = &self.lines[p.line];
        let Some(ch) = l[p.col..].chars().next() else { return self.move_to(v, p, false) };
        let k = class(ch);
        let start = l[..p.col].char_indices().rev().take_while(|(_, c)| class(*c) == k).last().map_or(p.col, |(i, _)| i);
        let end = l[p.col..].char_indices().find(|(_, c)| class(*c) != k).map_or(l.len(), |(i, _)| p.col + i);
        v.anchor = Some(Pos::new(p.line, start));
        v.cursor = Pos::new(p.line, end);
        self.open_kind = None;
    }

    // ---- find ----------------------------------------------------------------

    /// Literal matches of `q` (ASCII case folding unless `case`; `word` = whole
    /// word only), capped at 10k.
    pub fn find_all(&self, q: &str, case: bool, word: bool) -> Vec<(Pos, Pos)> {
        let mut out = Vec::new();
        if q.is_empty() || q.contains('\n') {
            return out;
        }
        let qb = q.as_bytes();
        let is_word = |s: &str, i: usize| s[i..].chars().next().is_some_and(|c| class(c) == 1);
        let before_word = |s: &str, i: usize| s[..i].chars().next_back().is_some_and(|c| class(c) == 1);
        for (li, l) in self.lines.iter().enumerate() {
            let lb = l.as_bytes();
            let mut i = 0;
            while i + qb.len() <= lb.len() {
                let e = i + qb.len();
                let hit = l.is_char_boundary(i)
                    && l.is_char_boundary(e)
                    && if case { &lb[i..e] == qb } else { lb[i..e].eq_ignore_ascii_case(qb) }
                    && (!word || (!before_word(l, i) && !is_word(l, e)));
                if hit {
                    out.push((Pos::new(li, i), Pos::new(li, i + qb.len())));
                    if out.len() >= 10_000 {
                        return out;
                    }
                    i += qb.len();
                } else {
                    i += 1;
                }
            }
        }
        out
    }

    // ---- disk ------------------------------------------------------------------

    /// Save. Normally atomic: temp file in the same directory (created with the
    /// file's mode, never wider), fsync, rename over the target (a symlink's
    /// target, not the link). A hard-linked file, or one in a directory we cannot
    /// create files in, is rewritten in place instead so links, owner and xattrs
    /// survive; callers write a hot-exit backup first.
    pub fn save(&mut self) -> Result<(), String> {
        if let Some(r) = &self.readonly {
            return Err(format!("read-only: {r}"));
        }
        let path = self.path.clone().ok_or("no file")?;
        let target = fs::canonicalize(&path).unwrap_or(path);
        let meta = fs::metadata(&target).ok();
        let now = meta.as_ref().and_then(|m| m.modified().ok());
        if self.mtime.is_some() && now != self.mtime && self.armed_mtime != now {
            self.armed_mtime = now;
            return Err("file changed on disk since it was loaded; Ctrl+S again overwrites it".into());
        }
        let data = self.text();
        let in_place = |why: &str| -> Result<(), String> {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&target)
                .map_err(|e| format!("save failed ({why}): {e}"))?;
            #[cfg(test)]
            if FAIL_IN_PLACE.with(|c| c.replace(false)) {
                return Err(format!("save failed ({why}): injected: no space left on device"));
            }
            f.write_all(data.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("save failed ({why}): {e}"))
        };
        if meta.as_ref().is_some_and(|m| m.nlink() > 1) {
            in_place("hard link")?;
        } else {
            let dir = target.parent().ok_or("no parent directory")?;
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let tmp = dir.join(format!(".{name}.herdr-code.{}.tmp", std::process::id()));
            let _ = fs::remove_file(&tmp);
            let mode = self.mode.unwrap_or(0o644);
            match fs::OpenOptions::new().write(true).create_new(true).mode(mode & 0o777).open(&tmp) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => in_place("read-only directory")?,
                Err(e) => return Err(format!("save failed: {e}")),
                Ok(mut f) => {
                    let mut write = || -> std::io::Result<()> {
                        f.write_all(data.as_bytes())?;
                        // umask may have narrowed the create mode; restore the exact bits.
                        f.set_permissions(fs::Permissions::from_mode(mode))?;
                        f.sync_all()?;
                        fs::rename(&tmp, &target)?;
                        if let Ok(d) = fs::File::open(dir) {
                            let _ = d.sync_all();
                        }
                        Ok(())
                    };
                    if let Err(e) = write() {
                        let _ = fs::remove_file(&tmp);
                        return Err(format!("save failed: {e}"));
                    }
                }
            }
        }
        self.saved = self.version;
        self.armed_mtime = None;
        self.disk_changed = false;
        self.mtime = fs::metadata(&target).and_then(|m| m.modified()).ok();
        Ok(())
    }

    // ---- hot-exit backups ---------------------------------------------------------

    /// Unsaved content as a backup record: the file path, the mtime of the disk
    /// version the edits were made on (not the disk now: an agent may have
    /// written the file since), and the text with its EOL style.
    pub fn backup_record(&self) -> Option<serde_json::Value> {
        let path = self.path.as_ref()?;
        let base = self.mtime.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
        Some(serde_json::json!({
            "path": path.to_string_lossy(),
            "base_mtime": base.map(|d| d.as_nanos().to_string()),
            "taken": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            "crlf": self.crlf,
            "final_newline": self.final_newline,
            "text": self.lines.join("\n"),
        }))
    }

    /// Replace the content with a backup as one undoable edit (so it is dirty).
    pub fn restore_backup(&mut self, v: &mut View, rec: &serde_json::Value) -> bool {
        let Some(text) = rec["text"].as_str() else { return false };
        if self.readonly.is_some() {
            return false;
        }
        self.select_all(v);
        self.insert_kind(v, text, Kind::Other);
        self.crlf = rec["crlf"].as_bool().unwrap_or(self.crlf);
        self.final_newline = rec["final_newline"].as_bool().unwrap_or(self.final_newline);
        // The edits are based on the old disk version: if the file changed since,
        // the normal external-change refusal must fire on the next save.
        self.mtime = rec["base_mtime"].as_str().and_then(|s| s.parse::<u128>().ok()).map(|n| {
            SystemTime::UNIX_EPOCH + Duration::new((n / 1_000_000_000) as u64, (n % 1_000_000_000) as u32)
        });
        true
    }

    /// Did the file on disk change after the version the backup's edits were made on?
    pub fn backup_is_stale(rec: &serde_json::Value, path: &Path) -> bool {
        let now = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().to_string());
        rec["base_mtime"].as_str().map(String::from) != now
    }

    /// Compare with the disk: reload a clean buffer, flag a dirty one.
    /// Returns Some(true) when reloaded, Some(false) when a dirty buffer went stale.
    pub fn check_disk(&mut self) -> Option<bool> {
        let path = self.path.clone()?;
        let now = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if now == self.mtime || self.disk_changed {
            return None;
        }
        if self.dirty() {
            self.disk_changed = true;
            return Some(false);
        }
        let fresh = Buffer::open(&path);
        *self = fresh;
        Some(true)
    }
}

/// Hot-exit backup file for `file` inside `dir`, keyed by a hash of the herdr
/// session (`scope`, its socket path) and the file path, so two sessions on the
/// same worktree never share or delete each other's backups.
pub fn backup_file(dir: &Path, scope: &str, file: &Path) -> PathBuf {
    dir.join(format!("{}.json", crate::parse::lock_key(scope, &file.to_string_lossy())))
}

/// Write `b`'s unsaved content atomically, owner-only (it may hold secrets).
pub fn write_backup(dir: &Path, scope: &str, b: &Buffer) -> std::io::Result<()> {
    let (Some(mut rec), Some(path)) = (b.backup_record(), b.path.as_ref()) else { return Ok(()) };
    rec["scope"] = scope.into();
    std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700).create(dir)?;
    let dst = backup_file(dir, scope, path);
    let tmp = dst.with_extension("tmp");
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(rec.to_string().as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, &dst)
}

pub fn read_backup(dir: &Path, scope: &str, file: &Path) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(backup_file(dir, scope, file)).ok()?).ok()?;
    (v["path"].as_str() == Some(file.to_string_lossy().as_ref())).then_some(v)
}

pub fn remove_backup(dir: &Path, scope: &str, file: &Path) {
    let _ = fs::remove_file(backup_file(dir, scope, file));
}

#[cfg(test)]
thread_local! {
    /// Test seam: make the next in-place write fail after truncating, like ENOSPC.
    pub static FAIL_IN_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_round_trip_restores_unsaved_text() {
        let d = tmpdir("bak");
        let p = d.join("f.txt");
        fs::write(&p, "one\r\ntwo\r\n").unwrap();
        let bak = d.join("backup");
        let mut b = Buffer::open(&p);
        let mut v = View::default();
        b.insert(&mut v, "zero ");
        write_backup(&bak, "s1", &b).unwrap();
        assert_eq!(fs::metadata(backup_file(&bak, "s1", &p)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&bak).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(read_backup(&bak, "s2", &p).is_none(), "another session does not see it");
        // A fresh session: the file on disk is unchanged, the backup holds the edit.
        let mut b2 = Buffer::open(&p);
        let rec = read_backup(&bak, "s1", &p).unwrap();
        assert!(!Buffer::backup_is_stale(&rec, &p));
        let mut v2 = View::default();
        assert!(b2.restore_backup(&mut v2, &rec));
        assert!(b2.dirty());
        assert_eq!(b2.text(), "zero one\r\ntwo\r\n");
        b2.undo(&mut v2);
        assert_eq!(b2.text(), "one\r\ntwo\r\n");
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "changed\n").unwrap();
        assert!(Buffer::backup_is_stale(&rec, &p));
        remove_backup(&bak, "s1", &p);
        assert!(read_backup(&bak, "s1", &p).is_none());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn backup_taken_after_an_external_write_is_stale_and_save_refuses_once() {
        let d = tmpdir("bakstale");
        let p = d.join("f.txt");
        fs::write(&p, "base\n").unwrap();
        let bak = d.join("backup");
        let mut b = Buffer::open(&p);
        let mut v = View::default();
        b.insert(&mut v, "mine ");
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "agent\n").unwrap(); // an agent writes before the backup timer fires
        write_backup(&bak, "s", &b).unwrap();
        let rec = read_backup(&bak, "s", &p).unwrap();
        assert!(Buffer::backup_is_stale(&rec, &p), "the backup is based on the old version");
        let mut b2 = Buffer::open(&p);
        let mut v2 = View::default();
        assert!(b2.restore_backup(&mut v2, &rec));
        assert!(b2.save().is_err(), "first save refuses: the disk has the agent's edit");
        assert_eq!(fs::read_to_string(&p).unwrap(), "agent\n");
        b2.save().unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "mine base\n");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn hardlinked_file_is_saved_in_place() {
        let d = tmpdir("link");
        let (a, b) = (d.join("a"), d.join("b"));
        fs::write(&a, "x\n").unwrap();
        fs::hard_link(&a, &b).unwrap();
        let mut buf = Buffer::open(&a);
        let mut v = View::default();
        buf.insert(&mut v, "y");
        buf.save().unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "yx\n", "the other link sees the save");
        assert_eq!(fs::metadata(&a).unwrap().nlink(), 2);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn undo_breaks_at_word_boundary() {
        let mut b = buf("");
        let mut v = View::default();
        for c in "ab cd".chars() {
            b.insert(&mut v, &c.to_string());
        }
        b.undo(&mut v);
        assert_eq!(b.text(), "ab");
    }

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty(None);
        b.set_text(text);
        b
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("herdr-code-ed-{tag}-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn insert_delete_undo_redo_with_grouping() {
        let mut b = buf("hello\n");
        let mut v = View::default();
        b.end_of_line(&mut v, false);
        for c in [" ", "w", "o"] {
            b.insert(&mut v, c);
        }
        b.newline(&mut v);
        b.insert(&mut v, "x");
        assert_eq!(b.text(), "hello wo\nx\n");
        assert!(b.dirty());
        b.backspace(&mut v);
        b.backspace(&mut v);
        assert_eq!(b.text(), "hello wo\n");
        b.undo(&mut v); // the two backspaces are one group
        assert_eq!(b.text(), "hello wo\nx\n");
        b.undo(&mut v); // typed "x"
        b.undo(&mut v); // newline
        assert_eq!(b.text(), "hello wo\n");
        b.undo(&mut v); // " wo" typed as one group
        assert_eq!(b.text(), "hello\n");
        assert!(!b.dirty(), "undo back to the saved text is clean");
        b.redo(&mut v);
        assert_eq!(b.text(), "hello wo\n");
        assert_eq!(v.cursor, Pos::new(0, 8));
        // Multi-line selection replaced by a paste.
        b.select_all(&mut v);
        b.insert(&mut v, "a\r\nb");
        assert_eq!(b.lines, vec!["a", "b"]);
        b.undo(&mut v);
        assert_eq!(b.text(), "hello wo\n");
    }

    #[test]
    fn cjk_and_tab_columns() {
        let l = "a中b\tc";
        assert_eq!(display_col(l, 0), 0);
        assert_eq!(display_col(l, 1), 1);
        assert_eq!(display_col(l, 4), 3); // after the 2-cell 中
        assert_eq!(display_col(l, 5), 4); // tab spans 4..8
        assert_eq!(display_col(l, 6), 8);
        assert_eq!(byte_at_display(l, 2), 4); // right cell of 中 -> after it
        assert_eq!(byte_at_display(l, 1), 1);
        let mut b = buf("中文字\nab\n");
        let mut v = View::default();
        b.right(&mut v, false, false);
        assert_eq!(v.cursor, Pos::new(0, 3));
        b.right(&mut v, false, false);
        b.vertical(&mut v, 1, false); // display x 4 clamps to end of "ab"
        assert_eq!(v.cursor, Pos::new(1, 2));
        b.vertical(&mut v, -1, false); // sticky x returns to after 文
        assert_eq!(v.cursor, Pos::new(0, 6));
        b.backspace(&mut v);
        assert_eq!(b.lines[0], "中字");
    }

    #[test]
    fn eol_and_final_newline_round_trip() {
        for t in ["a\r\nb\r\n", "a\r\nb", "a\nb\n", "a\nb", "", "\n", "x\r\n", "a\r\nb\r", "a\r"] {
            assert_eq!(buf(t).text(), t, "{t:?}");
        }
        let mut b = buf("a\r\nb\r\n");
        let mut v = View::default();
        b.end_of_line(&mut v, false);
        b.newline(&mut v);
        assert_eq!(b.text(), "a\r\n\r\nb\r\n");
    }

    #[test]
    fn atomic_save_keeps_mode_and_detects_external_change() {
        let d = tmpdir("save");
        let p = d.join("f.sh");
        fs::write(&p, "echo hi\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o750)).unwrap();
        let mut b = Buffer::open(&p);
        assert!(b.readonly.is_none());
        let mut v = View::default();
        b.insert(&mut v, "# ");
        b.save().unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "# echo hi\n");
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o750);
        assert!(!b.dirty());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no temp file left");
        // External change while dirty: first save refuses, second overwrites.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "other\n").unwrap();
        b.insert(&mut v, "x");
        assert_eq!(b.check_disk(), Some(false));
        assert!(b.save().is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "other\n");
        b.save().unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "# xecho hi\n");
        // Clean buffer reloads on external change.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&p, "new\n").unwrap();
        assert_eq!(b.check_disk(), Some(true));
        assert_eq!(b.lines[0], "new");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn truncated_non_utf8_and_binary_are_read_only() {
        let d = tmpdir("ro");
        let big = d.join("big.txt");
        fs::write(&big, "x".repeat(MAX_BYTES as usize + 10)).unwrap();
        let bad = d.join("bad.txt");
        fs::write(&bad, b"ok \xff\xfe\n").unwrap();
        let bin = d.join("bin");
        fs::write(&bin, b"a\0b").unwrap();
        for p in [&big, &bad, &bin] {
            let mut b = Buffer::open(p);
            assert!(b.readonly.is_some(), "{p:?}");
            let mut v = View::default();
            b.insert(&mut v, "z");
            b.backspace(&mut v);
            assert!(!b.dirty(), "{p:?} edited");
            assert!(b.save().is_err());
        }
        assert_eq!(fs::metadata(&big).unwrap().len(), MAX_BYTES + 10);
        assert_eq!(fs::read(&bad).unwrap(), b"ok \xff\xfe\n");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn find_and_replace_all_is_one_undo() {
        let mut b = buf("Foo foo\nbar FOO 中foo\n");
        assert_eq!(b.find_all("foo", true, false).len(), 2);
        assert_eq!(b.find_all("foo", false, true).len(), 3); // 中foo: 中 is a word char
        let m = b.find_all("foo", false, false);
        assert_eq!(m.len(), 4);
        assert_eq!(m[3], (Pos::new(1, 11), Pos::new(1, 14)));
        let mut v = View::default();
        assert_eq!(b.replace_all(&mut v, &m, "x"), 4);
        assert_eq!(b.text(), "x x\nbar x 中x\n");
        b.undo(&mut v);
        assert_eq!(b.text(), "Foo foo\nbar FOO 中foo\n");
    }

    #[test]
    fn indent_outdent_and_word_moves() {
        let mut b = buf("a\nb\n");
        let mut v = View::default();
        b.select_all(&mut v);
        b.indent(&mut v);
        assert_eq!(b.lines, vec!["    a", "    b"]);
        b.outdent(&mut v);
        assert_eq!(b.lines, vec!["a", "b"]);
        let mut b = buf("let foo_bar = 1;");
        let mut v = View::default();
        b.right(&mut v, false, true);
        assert_eq!(v.cursor.col, 3);
        b.right(&mut v, true, true);
        assert_eq!(b.selected_text(&v).as_deref(), Some(" foo_bar"));
        b.left(&mut v, false, true);
        assert_eq!(v.cursor.col, 4);
    }

    #[test]
    fn incremental_highlight_resumes_after_edit() {
        let mut b = buf("/* a\nb */\nint x;\n");
        b.set_syntax(syntaxes().find_syntax_by_extension("c"));
        b.highlight_upto(3);
        let comment = b.segs(1).unwrap()[0].1.foreground;
        assert_eq!((comment.r, comment.g, comment.b), (0x57, 0xA6, 0x4A));
        let mut v = View::default();
        b.insert(&mut v, "x"); // edits line 0; lines below must be recomputed lazily
        assert!(b.segs(1).is_none());
        b.highlight_upto(3);
        assert_eq!(b.segs(1).unwrap()[0].1.foreground, comment);
    }
}
