//! Text buffer with VS Code-style editing. The buffer (model) is shared by every
//! tab showing the file; each tab keeps its own `View` (cursor, selection, scroll).

use crate::{semantic, theme};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

/// Typing pauses longer than this start a new undo step (VS Code-like).
const UNDO_PAUSE: Duration = Duration::from_secs(1);
const UNDO_LIMIT: usize = 1000;
use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use syntect::highlighting::{Color as SColor, HighlightState, Highlighter, RangedHighlightIterator, Style as SStyle};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use unicode_width::UnicodeWidthChar;

pub const MAX_BYTES: u64 = 1 << 20;
pub const MAX_LINES: usize = 20_000;
static TAB: AtomicUsize = AtomicUsize::new(4);

/// `editor.tabSize`: tab stops, the indent unit for space-indented files, guides.
pub fn tab() -> usize {
    TAB.load(Ordering::Relaxed)
}

pub fn set_tab(n: usize) {
    TAB.store(n.clamp(1, 16), Ordering::Relaxed);
}
/// Lines longer than this are not highlighted (minified code); state carries over unchanged.
const HL_MAX_LINE: usize = 4000;

/// A parser state is kept every this many lines: a state is 1.3-1.5 KB, one per line made
/// a 20k-line file cost ~28 MB. An edit re-parses from the checkpoint at or above it.
const HL_CHECKPOINT: usize = 32;
/// Lines above a far view that its speculative pass starts at, for some context.
const HL_SPEC_CONTEXT: usize = 20;

/// Highlighting time per frame, shared by everything one frame draws (text, minimap, peek).
pub const FRAME_BUDGET: Duration = Duration::from_millis(6);

thread_local! {
    /// Highlighting time per frame; None: unlimited (a test's reference frames).
    static BUDGET: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(Some(FRAME_BUDGET)) };
    /// When the current frame's highlighting stops; None outside a frame.
    static DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

fn out_of_time() -> bool {
    DEADLINE.with(|d| d.get().is_some_and(|t| Instant::now() >= t))
}

/// Starts a frame's highlighting budget (`ui::draw`); dropping it ends the frame. Outside
/// a frame highlighting has no budget, so every other caller gets complete results.
pub struct FrameBudget(());

impl FrameBudget {
    pub fn start() -> FrameBudget {
        DEADLINE.with(|d| d.set(BUDGET.with(|b| b.get()).map(|b| Instant::now() + b)));
        FrameBudget(())
    }
}

impl Drop for FrameBudget {
    fn drop(&mut self) {
        DEADLINE.with(|d| d.set(None));
    }
}

#[cfg(test)]
thread_local! {
    /// Speculative passes built (a test counts rebuilds).
    pub static SPEC_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub fn set_frame_budget(b: Option<Duration>) {
    BUDGET.with(|c| c.set(b));
}

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
    /// In a side-by-side diff: the side the cursor and selection are on (0 the
    /// original, 1 the modified); their lines are diff rows.
    pub side: u8,
    /// Sticky display column for Up/Down.
    pub want_x: Option<usize>,
    pub scroll: usize,
    pub hscroll: usize,
    pub fold_ends: Vec<Option<usize>>,
    pub collapsed: std::collections::BTreeSet<usize>,
    pub(crate) visible: Vec<usize>,
    pub(crate) fold_version: Option<(u64, usize)>,
    pub(crate) fold_ids: Vec<u64>,
}

impl View {
    /// ponytail: indentation fallback; a syntax provider is needed for unindented bodies.
    /// Blank lines belong to
    /// their surrounding block; a dedented closing brace stays visible.
    fn sync_folds(&mut self, lines: &[String], ids: &[u64], version: u64) {
        if self.fold_version == Some((version, tab())) && self.fold_ids.first() == ids.first() { return; }
        let keep: std::collections::BTreeSet<u64> = self.collapsed.iter().filter_map(|&i| self.fold_ids.get(i).copied()).collect();
        self.fold_version = Some((version, tab()));
        self.collapsed.clear();
        self.visible.clear();
        self.fold_ends = vec![None; lines.len()];
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut previous = None;
        for (i, line) in lines.iter().enumerate() {
            if line.trim().is_empty() { continue; }
            let indent = line.chars().take_while(|c| c.is_whitespace()).fold(0, advance);
            while stack.last().is_some_and(|&(_, level)| indent <= level) {
                let (start, _) = stack.pop().unwrap();
                self.fold_ends[start] = Some(i - 1);
            }
            if let Some((start, level)) = previous {
                if indent > level { stack.push((start, level)); }
            }
            previous = Some((i, indent));
        }
        for (start, _) in stack { self.fold_ends[start] = Some(lines.len() - 1); }
        self.collapsed.extend(ids.iter().enumerate().filter_map(|(i, id)| (self.fold_ends[i].is_some() && keep.contains(id)).then_some(i)));
        self.fold_ids.clear();
        self.fold_ids.extend_from_slice(ids);
        self.rebuild_folds();
    }

    pub fn rebuild_folds(&mut self) {
        self.visible.clear();
        if self.collapsed.is_empty() { return; }
        let mut i = 0;
        while i < self.fold_ends.len() {
            self.visible.push(i);
            i = if self.collapsed.contains(&i) { self.fold_ends[i].unwrap_or(i) + 1 } else { i + 1 };
        }
    }

    pub fn row_of(&self, line: usize) -> usize {
        if self.visible.is_empty() { line } else { self.visible.partition_point(|&l| l <= line).saturating_sub(1) }
    }

    pub fn line_of(&self, row: usize) -> usize {
        if self.visible.is_empty() { row } else { self.visible[row.min(self.visible.len() - 1)] }
    }

    pub fn hidden(&self, line: usize) -> bool { self.line_of(self.row_of(line)) != line }

    pub fn shift_line(&self, line: usize, delta: isize) -> usize {
        self.line_of(self.row_of(line).saturating_add_signed(delta))
    }

    pub fn unfold_at(&mut self, line: usize) {
        let before = self.collapsed.len();
        self.collapsed.retain(|&start| !(start < line && self.fold_ends[start].is_some_and(|end| line <= end)));
        if self.collapsed.len() != before { self.rebuild_folds(); }
    }

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
    /// `states[c]` is the parser state at the start of line `c * HL_CHECKPOINT` (valid prefix).
    states: Vec<(ParseState, HighlightState, semantic::Ctx)>,
    /// The state at the start of line `segs.len()` when that is not a checkpoint.
    front: Option<(ParseState, HighlightState, semantic::Ctx)>,
    /// Highlighted segments `(end byte, style)` per line (valid prefix).
    segs: Vec<Vec<(usize, SStyle)>>,
    /// `theme::Syntax::gen` the cache was built with.
    gen: u64,
    /// The furthest line a view asked for: the sequential pass works toward it.
    want: usize,
    /// Segments of far views (each from its first line, `.0`), tokenised from a fresh state
    /// because the sequential pass is not there yet; shown until it is, then dropped. One
    /// region per view (editor groups, the peek), so views do not rebuild each other's.
    spec: Vec<(usize, Vec<Segs>)>,
    /// The last view was drawn before its lines were all exact (a later call this
    /// frame, the minimap's, may have finished them): draw it once more.
    /// ponytail: one flag per buffer, so an exact second view of it drawn later in the
    /// frame masks a stale first one (cosmetic, next event fixes it); per-frame id if seen.
    stale_view: bool,
}

/// One line's highlighted segments: `(end byte, style)`.
type Segs = Vec<(usize, SStyle)>;

/// One line's segments, advancing `st` (parse, highlight and semantic state) past it.
fn tokenise(line: &str, st: &mut (ParseState, HighlightState, semantic::Ctx), hl: &Highlighter, look: &theme::Syntax, lang: semantic::Lang) -> Segs {
    let plain = vec![(line.len(), PLAIN)];
    if line.len() > HL_MAX_LINE {
        return plain;
    }
    match st.0.parse_line(line, syntaxes()) {
        Ok(mut ops) => {
            semantic::alias_ops(&mut ops);
            let start = st.1.path.clone();
            let pieces = RangedHighlightIterator::new(&mut st.1, &ops, line, hl).map(|(s, _, r)| (r.end, s)).collect();
            semantic::refine(lang, line, &ops, start, pieces, &mut st.2, &look.sem)
        }
        Err(_) => plain,
    }
}

pub struct Buffer {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    /// Stable line identities let independent views retain their folded headers.
    line_ids: Vec<u64>,
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
    /// Boxed: a far jump's state makes it big, and every `Doc::Text` carries one.
    hl: Option<Box<Hl>>,
}

pub fn syntaxes() -> &'static SyntaxSet {
    static S: OnceLock<SyntaxSet> = OnceLock::new();
    S.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

/// Text the grammar does not colour: the editor foreground.
const PLAIN: SStyle = SStyle {
    foreground: SColor { r: 0, g: 0, b: 0, a: 0 },
    background: SColor { r: 0, g: 0, b: 0, a: 0 },
    font_style: syntect::highlighting::FontStyle::empty(),
};

/// Text as buffer lines, exactly as a file is loaded: (lines, crlf, final newline).
/// The EOL style is the first line's; in CRLF mode only the '\r' of a line break
/// is dropped, so a lone '\r' (mixed endings, an unterminated last line) is kept.
pub fn split_lines(text: &str) -> (Vec<String>, bool, bool) {
    let crlf = text.find('\n').is_some_and(|i| text[..i].ends_with('\r'));
    let final_newline = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    let pieces: Vec<&str> = body.split('\n').collect();
    let n = pieces.len();
    let lines = pieces
        .iter()
        .enumerate()
        .map(|(k, l)| {
            let ended = k + 1 < n || final_newline;
            if crlf && ended { l.strip_suffix('\r').unwrap_or(l) } else { l }.to_string()
        })
        .collect();
    (lines, crlf, final_newline)
}

/// Advance a display column over `c` (tabs to the next stop, wide chars = 2).
pub fn advance(x: usize, c: char) -> usize {
    match c {
        '\t' => (x / tab() + 1) * tab(),
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

/// The byte range of the word (or run of spaces, or of punctuation) at byte `col` of
/// `l`, as a double-click selects it; None past the line's end.
pub fn word_at(l: &str, col: usize) -> Option<(usize, usize)> {
    let ch = l.get(col..)?.chars().next()?;
    let k = class(ch);
    let start = l[..col].char_indices().rev().take_while(|(_, c)| class(*c) == k).last().map_or(col, |(i, _)| i);
    let end = l[col..].char_indices().find(|(_, c)| class(*c) != k).map_or(l.len(), |(i, _)| col + i);
    Some((start, end))
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

/// Literal find over numbered lines, also used by either side of a diff.
pub(crate) fn find_in<'a>(lines: impl IntoIterator<Item = (usize, &'a str)>, q: &str, case: bool, word: bool) -> Vec<(Pos, Pos)> {
    let mut out = Vec::new();
    if q.is_empty() || q.contains('\n') {
        return out;
    }
    let qb = q.as_bytes();
    let is_word = |s: &str, i: usize| s[i..].chars().next().is_some_and(|c| class(c) == 1);
    let before_word = |s: &str, i: usize| s[..i].chars().next_back().is_some_and(|c| class(c) == 1);
    for (li, l) in lines {
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

fn fresh_line_ids(n: usize) -> std::ops::Range<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let start = NEXT.fetch_add(n as u64, Ordering::Relaxed);
    start..start + n as u64
}

impl Buffer {
    fn empty(path: Option<PathBuf>) -> Buffer {
        Buffer {
            path,
            lines: vec![String::new()],
            line_ids: fresh_line_ids(1).collect(),
            crlf: false,
            final_newline: true,
            readonly: None,
            binary: false,
            mtime: None,
            mode: None,
            indent: " ".repeat(tab()),
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

    /// A read-only buffer of exactly `lines`, highlighted as `path`'s language (a
    /// diff side or hover signature).
    pub fn from_lines(path: &str, lines: Vec<String>, why: &str) -> Buffer {
        let mut b = Buffer::empty(None);
        if !lines.is_empty() {
            b.lines = lines;
            b.line_ids = fresh_line_ids(b.lines.len()).collect();
        }
        b.readonly = Some(why.into());
        let (ss, p) = (syntaxes(), Path::new(path));
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        b.set_syntax(ss.find_syntax_by_extension(ext).or_else(|| ss.find_syntax_by_extension(name)).or_else(|| ss.find_syntax_by_first_line(&b.lines[0])));
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
            b.line_ids.truncate(MAX_LINES);
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
        let look = theme::syntax();
        let start = (
            ParseState::new(syn),
            HighlightState::new(&Highlighter::new(&look.theme), ScopeStack::new()),
            semantic::Ctx::default(),
        );
        self.hl = Some(Box::new(Hl { syntax: syn, states: vec![start], front: None, segs: Vec::new(), gen: look.gen, want: 0, spec: Vec::new(), stale_view: false }));
    }

    fn set_text(&mut self, text: &str) {
        (self.lines, self.crlf, self.final_newline) = split_lines(text);
        self.line_ids = fresh_line_ids(self.lines.len()).collect();
        let tabs = self.lines.iter().take(2000).filter(|l| l.starts_with('\t')).count();
        let spaces = self.lines.iter().take(2000).filter(|l| l.starts_with(' ')).count();
        self.indent = if tabs > spaces { "\t".into() } else { " ".repeat(tab()) };
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
        v.sync_folds(&self.lines, &self.line_ids, self.version());
        v.cursor = self.clamp(v.cursor);
        v.anchor = v.anchor.map(|a| self.clamp(a));
        v.scroll = v.line_of(v.row_of(v.scroll.min(self.lines.len() - 1)));
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
            // Back to the checkpoint at or above the edit; the pass resumes from its state.
            // An edit the pass has not reached yet changes nothing it has: its front state
            // stays (a resume from the checkpoint below would start on the wrong line).
            if line < h.segs.len() {
                let c = line / HL_CHECKPOINT;
                h.segs.truncate(c * HL_CHECKPOINT);
                h.states.truncate(c + 1);
                h.front = None;
            }
            h.spec.clear();
        }
    }

    /// Highlight lines up to `upto` (exclusive), resuming from the cached state:
    /// grammar scopes renamed to VS Code's (`semantic::alias`), themed, then the
    /// semantic pass for C/C++/Rust identifiers.
    /// Tokenise lines `..upto` (no view to colour ahead of the pass).
    pub fn highlight_upto(&mut self, upto: usize) {
        self.highlight_view(upto, upto);
    }

    /// Tokenise for a view of lines `start..end`. The sequential pass (exact: syntect's
    /// state runs from the top) works toward the furthest line any view asked for, but
    /// inside a draw only until the frame's budget is spent, one line at the least (a page
    /// down fits the budget). View lines it has not reached yet (a jump to the end) are
    /// tokenised from a fresh state just above the view instead, close for most code, until
    /// the pass gets there. `hl_pending` tells when to draw again.
    pub fn highlight_view(&mut self, start: usize, end: usize) {
        let Some(h) = &mut self.hl else { return };
        let look = theme::syntax();
        let hl = Highlighter::new(&look.theme);
        if h.gen != look.gen {
            // A settings reload swapped the theme: recolour from the top.
            h.gen = look.gen;
            h.segs.clear();
            h.states.truncate(1);
            h.states[0] = (ParseState::new(h.syntax), HighlightState::new(&hl, ScopeStack::new()), Default::default());
            h.front = None;
            h.spec.clear();
        }
        let lang = semantic::lang(&h.syntax.name);
        let n = self.lines.len();
        let end = end.min(n);
        h.want = h.want.max(end).min(n);
        let mut done = 0;
        while h.segs.len() < h.want {
            let i = h.segs.len();
            if done > 0 && out_of_time() {
                break;
            }
            // Without a front state the prefix ends on a checkpoint (`invalidate`, a reset).
            let mut st = h.front.take().unwrap_or_else(|| h.states[i / HL_CHECKPOINT].clone());
            let segs = tokenise(&self.lines[i], &mut st, &hl, &look, lang);
            h.segs.push(segs);
            if (i + 1) % HL_CHECKPOINT == 0 {
                h.states.push(st.clone());
            }
            h.front = Some(st);
            done += 1;
        }
        let have = h.segs.len();
        if start < end {
            h.stale_view = have < end;
        }
        h.spec.retain(|(f, v)| have < f + v.len()); // the exact pass has caught up with these
        // Only a view (start < end) gets speculative lines: the minimap asks with none.
        let from = start.saturating_sub(HL_SPEC_CONTEXT).max(have);
        let covered = h.spec.iter().any(|(f, v)| *f <= from && f + v.len() >= end);
        if start < end && have < end && !covered {
            let mut st = (ParseState::new(h.syntax), HighlightState::new(&hl, ScopeStack::new()), Default::default());
            if h.spec.len() >= 4 {
                h.spec.remove(0);
            }
            h.spec.push((from, self.lines[from..end].iter().map(|l| tokenise(l, &mut st, &hl, &look, lang)).collect()));
            #[cfg(test)]
            SPEC_BUILDS.with(|c| c.set(c.get() + 1));
        }
    }

    /// The exact pass still has lines a view asked for: draw again soon. It never stays
    /// true while the buffer is drawn: every frame does at least one line, toward a target
    /// no larger than the buffer.
    pub fn hl_pending(&self) -> bool {
        self.hl.as_ref().is_some_and(|h| h.segs.len() < h.want.min(self.lines.len()) || h.stale_view)
    }

    /// A line's segments: exact, else the speculative ones of a far view, else None
    /// (drawn in the editor's foreground).
    pub fn segs(&self, line: usize) -> Option<&[(usize, SStyle)]> {
        let h = self.hl.as_ref()?;
        h.segs.get(line).or_else(|| h.spec.iter().rev().find_map(|(f, v)| v.get(line.checked_sub(*f)?))).map(Vec::as_slice)
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
        self.line_ids.splice(at.line + 1..at.line + 1, fresh_line_ids(end.line - at.line));
        if at.col == 0 {
            // At a line's start, its original text (and fold) moves after the insertion.
            self.line_ids.swap(at.line, end.line);
        }
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
        if a.col == 0 {
            self.line_ids[a.line] = if b.col == 0 { self.line_ids[b.line] } else { fresh_line_ids(1).next().unwrap() };
        }
        self.line_ids.drain(a.line + 1..=b.line);
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

    fn begin(&mut self, v: &mut View, kind: Kind) {
        self.clamp_view(v);
        v.unfold_at(v.cursor.line);
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

    fn end(&mut self, v: &mut View) {
        if self.undo.last().is_some_and(|g| g.ops.is_empty()) {
            self.undo.pop();
            return;
        }
        if let Some(g) = self.undo.last_mut() {
            g.after = (v.cursor, v.anchor);
            g.ver_after = self.version;
        }
        self.last_edit = Some(Instant::now());
        let excess = self.undo.len().saturating_sub(UNDO_LIMIT);
        self.undo.drain(..excess);
        self.redo.clear();
        self.clamp_view(v);
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
        self.clamp_view(v);
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
        self.clamp_view(v);
        true
    }

    pub fn redo(&mut self, v: &mut View) -> bool {
        self.clamp_view(v);
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
        self.clamp_view(v);
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
                " ".repeat(tab() - display_col(&self.lines[c.line], c.col) % tab())
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
            let n = if line.starts_with('\t') { 1 } else { line.bytes().take(tab()).take_while(|b| *b == b' ').count() };
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

    /// Replace lines `start..end` with `with` (a quick-diff Revert): one undo
    /// step, the cursor at the start of the range. `start == end` inserts.
    pub fn replace_lines(&mut self, v: &mut View, start: usize, end: usize, with: &[String]) -> bool {
        let n = self.lines.len();
        if self.readonly.is_some() || start > end || end > n {
            return false;
        }
        let eol = |l: usize| Pos::new(l, self.lines[l].len());
        // Keep the line break structure: a range that reaches the end of the
        // buffer takes the break before it instead of the one after it.
        let (a, b, text) = if end < n {
            (Pos::new(start, 0), Pos::new(end, 0), with.iter().map(|l| format!("{l}\n")).collect::<String>())
        } else if start > 0 {
            (eol(start - 1), eol(n - 1), with.iter().map(|l| format!("\n{l}")).collect::<String>())
        } else {
            (Pos::new(0, 0), eol(n - 1), with.join("\n"))
        };
        self.begin(v, Kind::Other);
        if a != b {
            self.do_del(a, b);
        }
        if !text.is_empty() {
            self.do_ins(a, &text);
        }
        v.anchor = None;
        v.cursor = self.clamp(Pos::new(start, 0));
        self.end(v);
        true
    }

    /// Before a save: trailing whitespace goes from the lines edited since the file on disk
    /// (every line of a new file), so a save never adds noise to a diff, and the file ends
    /// with a newline. Markdown and patches keep theirs, where it can mean something.
    pub fn tidy(&mut self, v: &mut View, trim: bool, final_newline: bool) {
        if self.readonly.is_some() {
            return;
        }
        let extension = self.path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()).unwrap_or("");
        if trim && !matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx" | "patch" | "diff") {
            let bare = |l: &String| l.trim_end_matches([' ', '\t']).len();
            let spans: Vec<(usize, usize)> = self.edited_lines().into_iter().map(|l| (l, bare(&self.lines[l]))).filter(|&(l, keep)| keep < self.lines[l].len()).collect();
            if !spans.is_empty() {
                self.begin(v, Kind::Other);
                for &(line, keep) in spans.iter().rev() {
                    let end = self.lines[line].len();
                    self.do_del(Pos::new(line, keep), Pos::new(line, end));
                }
                v.cursor = self.clamp(v.cursor);
                v.anchor = v.anchor.map(|a| self.clamp(a));
                self.end(v);
            }
        }
        if final_newline && self.lines.iter().any(|l| !l.is_empty()) {
            self.final_newline = true;
        }
    }

    /// The lines that differ from the file on disk, or all of them without one.
    fn edited_lines(&self) -> Vec<usize> {
        let Some(disk) = self.path.as_ref().and_then(|p| fs::read_to_string(p).ok()) else {
            return (0..self.lines.len()).collect();
        };
        let (old, ..) = split_lines(&disk);
        let a: Vec<&str> = old.iter().map(String::as_str).collect();
        let b: Vec<&str> = self.lines.iter().map(String::as_str).collect();
        capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(Instant::now() + Duration::from_millis(100)))
            .into_iter()
            .flat_map(|op| {
                let (tag, _, new) = op.as_tag_tuple();
                if matches!(tag, DiffTag::Insert | DiffTag::Replace) { new } else { 0..0 }
            })
            .collect()
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
        v.unfold_at(p.line);
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
        let target = if v.visible.is_empty() { c.line as isize + delta } else { v.shift_line(c.line, delta) as isize };
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
        let Some((start, end)) = word_at(&self.lines[p.line], p.col) else { return self.move_to(v, p, false) };
        v.anchor = Some(Pos::new(p.line, start));
        v.cursor = Pos::new(p.line, end);
        self.open_kind = None;
    }

    // ---- find ----------------------------------------------------------------

    /// Literal matches of `q` (ASCII case folding unless `case`; `word` = whole
    /// word only), capped at 10k.
    pub fn find_all(&self, q: &str, case: bool, word: bool) -> Vec<(Pos, Pos)> {
        find_in(self.lines.iter().enumerate().map(|(i, s)| (i, s.as_str())), q, case, word)
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
        // the normal external-change refusal must fire on the next save. A record
        // without base_mtime cannot prove anything, so it counts as stale: the
        // epoch never matches the disk, and the first save refuses.
        let base = rec["base_mtime"].as_str().and_then(|s| s.parse::<u128>().ok());
        self.mtime = Some(base.map_or(SystemTime::UNIX_EPOCH, |n| {
            SystemTime::UNIX_EPOCH + Duration::new((n / 1_000_000_000) as u64, (n % 1_000_000_000) as u32)
        }));
        true
    }

    /// Did the file on disk change after the version the backup's edits were
    /// made on? A record without `base_mtime` is always stale.
    pub fn backup_is_stale(rec: &serde_json::Value, path: &Path) -> bool {
        let now = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().to_string());
        match rec["base_mtime"].as_str() {
            Some(base) => Some(base) != now.as_deref(),
            None => true,
        }
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
/// session (`scope`, its socket path), the panel `instance` (pane id or `local`,
/// plus pid) and the file path: two panels editing one file keep two backups.
pub fn backup_file(dir: &Path, scope: &str, instance: &str, file: &Path) -> PathBuf {
    dir.join(format!("{}.json", crate::parse::lock_key(&format!("{scope}\0{instance}"), &file.to_string_lossy())))
}

/// Write `b`'s unsaved content atomically, owner-only (it may hold secrets).
/// The record names its writer (`instance`, `pid`) so a running panel's is left alone.
pub fn write_backup(dir: &Path, scope: &str, instance: &str, b: &Buffer) -> std::io::Result<()> {
    let (Some(mut rec), Some(path)) = (b.backup_record(), b.path.as_ref()) else { return Ok(()) };
    rec["scope"] = scope.into();
    rec["instance"] = instance.into();
    rec["pid"] = std::process::id().into();
    write_record(dir, &backup_file(dir, scope, instance, path), &rec)
}

/// Write a backup record to `dst` in `dir`: temp file, fsync, rename; 0600 in a 0700 dir.
pub fn write_record(dir: &Path, dst: &Path, rec: &serde_json::Value) -> std::io::Result<()> {
    std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700).create(dir)?;
    let tmp = dst.with_extension("tmp");
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(rec.to_string().as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, dst)
}

/// The record in backup file `bak`, if it is one for `file`.
pub fn read_backup(bak: &Path, file: &Path) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(bak).ok()?).ok()?;
    (v["path"].as_str() == Some(file.to_string_lossy().as_ref())).then_some(v)
}

/// Backups in `dir` no running panel owns, newest first: any session, any panel,
/// and the older key formats too (found by content, so never renamed). A record
/// is a running panel's while its `pid` lives under the command name `live`.
// ponytail: pid + comm liveness; a dead panel's pid reused by another herdr-code
// hides that backup until the other one exits. A per-panel flock if that bites.
pub fn orphan_backups(dir: &Path, live: &str) -> Vec<(PathBuf, serde_json::Value)> {
    let running = |v: &serde_json::Value| {
        v["pid"].as_u64().is_some_and(|p| fs::read_to_string(format!("/proc/{p}/comm")).is_ok_and(|c| c.trim_end() == live))
    };
    let mut v: Vec<(PathBuf, serde_json::Value)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| Some((p.clone(), serde_json::from_slice::<serde_json::Value>(&fs::read(&p).ok()?).ok()?)))
        .filter(|(_, r)| r["path"].is_string() && !running(r))
        .collect();
    v.sort_by_key(|(p, r)| (std::cmp::Reverse(r["taken"].as_u64().unwrap_or(0)), p.clone()));
    v
}

pub fn remove_backup(dir: &Path, scope: &str, instance: &str, file: &Path) {
    let _ = fs::remove_file(backup_file(dir, scope, instance, file));
}

#[cfg(test)]
thread_local! {
    /// Test seam: make the next in-place write fail after truncating, like ENOSPC.
    pub static FAIL_IN_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame that draws a view one line short of exact, then finishes that line in a
    /// later call (the minimap's), must still ask for one more frame: else the view stays
    /// drawn with that line's speculative colours.
    #[test]
    fn a_view_drawn_before_its_last_line_was_exact_asks_for_one_more_frame() {
        let mut lines = vec!["/* a comment opened far above".to_string()];
        lines.extend((1..40).map(|i| format!("int v{i} = {i};")));
        lines.push("*/".into());
        let mut b = Buffer::from_lines("a.c", lines, "test");
        let (scroll, end) = (20, 30);
        b.highlight_upto(end - 2); // outside a frame: unbudgeted, two lines short of the view's end
        set_frame_budget(Some(Duration::ZERO)); // one line per call
        {
            let _frame = FrameBudget::start();
            b.highlight_view(scroll, end); // the text: line 28 exact, line 29 drawn speculative
            assert_eq!(b.hl.as_ref().unwrap().segs.len(), end - 1);
            b.highlight_upto(end); // the minimap, after the text: finishes line 29
        }
        set_frame_budget(Some(FRAME_BUDGET));
        assert!(b.hl_pending(), "line 29 was drawn speculative: one more frame");
        {
            let _frame = FrameBudget::start();
            b.highlight_view(scroll, end);
        }
        assert!(!b.hl_pending(), "drawn exact: settled");
    }

    #[test]
    fn backup_round_trip_restores_unsaved_text() {
        let d = tmpdir("bak");
        let p = d.join("f.txt");
        fs::write(&p, "one\r\ntwo\r\n").unwrap();
        let bak = d.join("backup");
        let mut b = Buffer::open(&p);
        let mut v = View::default();
        b.insert(&mut v, "zero ");
        write_backup(&bak, "s1", "w1:p1-7", &b).unwrap();
        let file = backup_file(&bak, "s1", "w1:p1-7", &p);
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&bak).unwrap().permissions().mode() & 0o777, 0o700);
        assert_ne!(file, backup_file(&bak, "s1", "w1:p2-8", &p), "another panel writes its own");
        assert_ne!(file, backup_file(&bak, "s2", "w1:p1-7", &p), "and so does another session");
        assert!(read_backup(&file, &d.join("other.txt")).is_none(), "a record for another file");
        // A fresh session: the file on disk is unchanged, the backup holds the edit.
        let mut b2 = Buffer::open(&p);
        let rec = read_backup(&file, &p).unwrap();
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
        remove_backup(&bak, "s1", "w1:p1-7", &p);
        assert!(read_backup(&file, &p).is_none());
        fs::remove_dir_all(&d).unwrap();
    }

    /// A running panel's backup is its own; any other (dead pid, none: an older
    /// format) is offered, newest first.
    #[test]
    fn only_backups_no_running_panel_owns_are_offered() {
        let d = tmpdir("orphan");
        let bak = d.join("backup");
        fs::create_dir_all(&bak).unwrap();
        let rec = |pid: Option<u32>, taken: u64| serde_json::json!({ "path": "/r/f.c", "text": "t", "pid": pid, "taken": taken });
        let me = std::process::id();
        for (name, r) in [("live", rec(Some(me), 30)), ("dead", rec(Some(u32::MAX), 10)), ("old", rec(None, 20))] {
            fs::write(bak.join(format!("{name}.json")), r.to_string()).unwrap();
        }
        fs::write(bak.join("x.tmp"), "{}").unwrap();
        let comm = fs::read_to_string("/proc/self/comm").unwrap();
        let names = |live: &str| -> Vec<String> {
            orphan_backups(&bak, live).iter().map(|(p, _)| p.file_stem().unwrap().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(names(comm.trim_end()), ["old", "dead"], "our own pid is a running panel");
        assert_eq!(names("herdr-code"), ["live", "old", "dead"], "a pid under another name is no panel");
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
        write_backup(&bak, "s", "i", &b).unwrap();
        let rec = read_backup(&backup_file(&bak, "s", "i", &p), &p).unwrap();
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
    fn backup_without_base_mtime_is_stale_and_save_refuses_once() {
        let d = tmpdir("nobase");
        let p = d.join("f.txt");
        fs::write(&p, "disk\n").unwrap();
        let rec = serde_json::json!({ "path": p, "text": "mine", "crlf": false, "final_newline": true });
        assert!(Buffer::backup_is_stale(&rec, &p));
        let mut b = Buffer::open(&p);
        let mut v = View::default();
        assert!(b.restore_backup(&mut v, &rec));
        assert!(b.save().is_err(), "nothing proves the disk is the base: refuse once");
        assert_eq!(fs::read_to_string(&p).unwrap(), "disk\n");
        b.save().unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "mine\n");
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

    #[test]
    fn a_save_tidies_only_the_lines_that_were_edited() {
        let d = tmpdir("tidy-edited");
        let p = d.join("a.c");
        fs::write(&p, "old  \nkeep\n").unwrap();
        let mut b = Buffer::open(&p);
        let mut v = View::default();
        b.move_to(&mut v, Pos::new(1, 4), false);
        b.insert(&mut v, "  ");
        b.tidy(&mut v, true, true);
        assert_eq!(b.lines, ["old  ", "keep"], "an untouched line keeps its spaces");
        assert_eq!(v.cursor, Pos::new(1, 4), "pulled back to the new end of the line");
        assert!(b.undo(&mut v));
        assert_eq!(b.lines, ["old  ", "keep  "], "one undo step brings them back");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_new_file_is_tidied_on_every_line() {
        let d = tmpdir("tidy-new");
        let mut b = Buffer::empty(Some(d.join("new.c")));
        let mut v = View::default();
        b.insert(&mut v, "a  \n\tb\t");
        b.tidy(&mut v, true, true);
        assert_eq!(b.lines, ["a", "\tb"], "only trailing whitespace goes");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_save_ends_the_file_with_a_newline() {
        let d = tmpdir("tidy-eof");
        let p = d.join("a.c");
        fs::write(&p, "x").unwrap();
        let mut b = Buffer::open(&p);
        let mut v = View::default();
        b.tidy(&mut v, true, false);
        assert!(!b.final_newline, "off when the setting is off");
        b.tidy(&mut v, true, true);
        assert!(b.final_newline);
        let empty = d.join("empty.c");
        fs::write(&empty, "").unwrap();
        let mut e = Buffer::open(&empty);
        e.tidy(&mut v, true, true);
        assert!(!e.final_newline, "an empty file stays empty");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn markdown_and_patches_keep_their_trailing_spaces() {
        let d = tmpdir("tidy-exempt");
        for name in ["a.md", "a.patch", "a.diff"] {
            fs::write(d.join(name), "").unwrap();
            let mut b = Buffer::open(&d.join(name));
            let mut v = View::default();
            b.insert(&mut v, "x  ");
            b.tidy(&mut v, true, true);
            assert_eq!(b.lines, ["x  "], "{name}");
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn trimming_can_be_turned_off() {
        let d = tmpdir("tidy-off");
        fs::write(d.join("a.c"), "").unwrap();
        let mut b = Buffer::open(&d.join("a.c"));
        let mut v = View::default();
        b.insert(&mut v, "x  ");
        b.tidy(&mut v, false, true);
        assert_eq!(b.lines, ["x  "]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn folding_nested_blocks_blanks_tabs_and_edits() {
        let mut b = buf("def outer():\n\tif ready:\n\t\twork()\n\n\tfinish()\nnext()");
        let mut v = View::default();
        b.clamp_view(&mut v);
        assert_eq!(v.fold_ends, vec![Some(4), Some(3), None, None, None, None]);
        v.collapsed.extend([0, 1]);
        v.rebuild_folds();
        assert_eq!(v.visible, vec![0, 5]);
        v.collapsed.remove(&0);
        v.rebuild_folds();
        assert_eq!(v.visible, vec![0, 1, 4, 5]);
        b.move_to(&mut v, Pos::new(2, 0), false);
        assert!(v.collapsed.is_empty(), "jumping to a hidden result reveals it");
        v.collapsed.insert(0);
        v.rebuild_folds();
        b.insert(&mut v, "changed ");
        b.clamp_view(&mut v);
        assert!(v.collapsed.is_empty(), "editing inside hidden text reveals that block");
        assert!(b.undo(&mut v));
        assert_eq!(b.lines[2], "\t\twork()");
        b.clamp_view(&mut v);
        assert_eq!(v.fold_ends[0], Some(4));
    }

    #[test]
    fn folded_blocks_survive_edits_line_moves_and_undo_in_both_views() {
        let mut b = buf("fn one() {\n    first();\n}\n\nfn two() {\n    second();\n}\nafter();");
        let mut v = View::default();
        b.clamp_view(&mut v);
        v.collapsed.extend([0, 4]);
        v.rebuild_folds();
        let mut other = v.clone();
        b.insert(&mut v, "// header\n");
        b.clamp_view(&mut v);
        b.clamp_view(&mut other);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![1, 5], "insertion at a folded header moves its identity");
        assert_eq!(other.collapsed, v.collapsed);
        assert!(b.undo(&mut v));
        b.clamp_view(&mut v);
        b.clamp_view(&mut other);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0, 4]);
        v.cursor = Pos::new(7, 0);
        b.insert(&mut v, "// ");
        b.clamp_view(&mut v);
        b.clamp_view(&mut other);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0, 4], "typing outside folds preserves them");
        assert_eq!(other.collapsed, v.collapsed, "a second view preserves its folds too");
        v.cursor = Pos::new(3, 0);
        b.insert(&mut v, "prefix\n");
        b.clamp_view(&mut v);
        b.clamp_view(&mut other);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0, 5]);
        assert_eq!(other.collapsed, v.collapsed);
        assert!(v.hidden(1) && v.hidden(6));
        assert!(b.undo(&mut v));
        b.clamp_view(&mut v);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0, 4]);
        assert!(b.redo(&mut v));
        b.clamp_view(&mut v);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0, 5]);
        assert!(b.replace_lines(&mut v, 3, 5, &[]));
        b.clamp_view(&mut v);
        b.clamp_view(&mut other);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0, 3], "deleting lines above a fold moves it");
        assert_eq!(other.collapsed, v.collapsed);
        assert!(b.replace_lines(&mut v, 3, 6, &[]));
        b.clamp_view(&mut v);
        assert_eq!(v.collapsed.iter().copied().collect::<Vec<_>>(), vec![0], "only the deleted block loses its fold");
    }

    #[test]
    fn deleting_a_fold_header_never_folds_another_identical_header() {
        let mut b = buf("fn same() {\n    first();\n}\nfn same() {\n    second();\n}");
        let mut v = View::default();
        b.clamp_view(&mut v);
        v.collapsed.insert(0);
        v.rebuild_folds();
        assert!(b.replace_lines(&mut v, 0, 3, &[]));
        b.clamp_view(&mut v);
        assert!(v.collapsed.is_empty());
        assert_eq!(b.lines[1], "    second();");
    }

    #[test]
    fn undo_keeps_only_the_latest_thousand_steps_and_noops_do_not_evict() {
        let mut b = buf("");
        let mut v = View::default();
        for _ in 0..1005 { b.insert(&mut v, "xx"); }
        b.delete(&mut v); // Delete at EOF does not consume history.
        for _ in 0..1000 { assert!(b.undo(&mut v)); }
        assert!(!b.undo(&mut v), "older steps were discarded");
        assert_eq!(b.text(), "x".repeat(10));
        for _ in 0..1000 { assert!(b.redo(&mut v)); }
        assert!(!b.redo(&mut v));
        assert_eq!(b.text(), "x".repeat(2010));
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
    fn replace_lines_is_one_undo_step_at_every_edge() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        for (text, start, end, with, want) in [
            ("a\nX\nc\n", 1, 2, s(&["b"]), "a\nb\nc\n"),        // modified line
            ("a\nnew\nb\n", 1, 2, s(&[]), "a\nb\n"),             // added line removed
            ("a\nc\n", 1, 1, s(&["b"]), "a\nb\nc\n"),            // deleted line restored
            ("a\nb\nX\n", 2, 3, s(&["c", "d"]), "a\nb\nc\nd\n"), // at the end
            ("X\nY\n", 0, 2, s(&["a"]), "a\n"),                  // the whole buffer
            ("a\n", 1, 1, s(&["b"]), "a\nb\n"),                  // insert at the end
        ] {
            let mut b = buf(text);
            let mut v = View::default();
            assert!(b.replace_lines(&mut v, start, end, &with));
            assert_eq!(b.text(), want, "{text:?} {start}..{end}");
            b.undo(&mut v);
            assert_eq!(b.text(), text, "one undo restores {text:?}");
        }
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

    /// A C file whose line 2500 opens a block comment that line 2600 closes: a parse that
    /// starts fresh inside it colours the comment as code.
    fn far_comment_c(n: usize) -> Buffer {
        let text: String = (0..n)
            .map(|i| match i {
                2500 => "/* a comment opened far above".to_string(),
                2600 => "*/".to_string(),
                _ => format!("int v{i} = {i};"),
            } + "\n")
            .collect();
        let mut b = buf(&text);
        b.set_syntax(syntaxes().find_syntax_by_extension("c"));
        b
    }

    /// Draws one frame's worth of highlighting for a view, with the frame already out of time.
    fn starved_frame(b: &mut Buffer, start: usize, end: usize) {
        set_frame_budget(Some(Duration::ZERO));
        let _frame = FrameBudget::start();
        b.highlight_view(start, end);
    }

    #[test]
    fn a_far_jump_is_highlighted_in_slices_and_settles_exactly() {
        let (start, end) = (2550, 2590); // a view inside the comment
        let mut exact = far_comment_c(3000);
        exact.highlight_upto(end); // outside a frame: no budget, all at once
        let mut b = far_comment_c(3000);
        starved_frame(&mut b, start, end);
        assert!(b.hl_pending(), "one frame does not tokenise 2500 lines");
        assert!((start..end).all(|l| b.segs(l).is_some()), "the view is coloured at once");
        assert_ne!(b.segs(2560), exact.segs(2560), "speculative: a fresh state cannot know the comment");
        let mut frames = 1;
        while b.hl_pending() {
            starved_frame(&mut b, start, end);
            frames += 1;
            assert!(frames < 10_000, "never settles");
        }
        for l in 0..end {
            assert_eq!(b.segs(l), exact.segs(l), "line {l} after {frames} frames");
        }
        set_frame_budget(Some(FRAME_BUDGET));
    }

    #[test]
    fn states_are_kept_every_32_lines_and_edits_resume_exactly() {
        let mut b = far_comment_c(3000);
        b.highlight_upto(3000);
        let kept = b.hl.as_ref().unwrap().states.len();
        assert!(kept <= 3000 / 32 + 2, "{kept} parser states kept for 3000 lines");
        // Deleting the comment's opener (line 2500) recolours everything below it, from the
        // checkpoint under the edit; the result equals a buffer parsed fresh.
        let mut v = View { cursor: Pos::new(2500, 0), ..Default::default() };
        b.move_to(&mut v, Pos::new(2500, 2), true);
        b.insert(&mut v, "//");
        b.highlight_upto(3000);
        let mut fresh = buf(&b.lines.join("\n"));
        fresh.set_syntax(syntaxes().find_syntax_by_extension("c"));
        fresh.highlight_upto(3000);
        for l in 0..3000 {
            assert_eq!(b.segs(l), fresh.segs(l), "line {l}");
        }
    }

    /// hc-sup6's probe: typing in a far view before the exact pass reaches it.
    #[test]
    fn an_edit_beyond_the_pass_still_settles_exactly() {
        let text: String = (0..3000).map(|i| match i { 35 => "/* open".to_string(), 50 => "*/".to_string(), _ => format!("int v{i} = {i};") } + "\n").collect();
        let mut b = buf(&text);
        b.set_syntax(syntaxes().find_syntax_by_extension("c"));
        for _ in 0..40 {
            starved_frame(&mut b, 2550, 2590); // one exact line a frame: the pass is at 40
        }
        assert_eq!(b.hl.as_ref().unwrap().segs.len(), 40);
        let mut v = View { cursor: Pos::new(2560, 0), ..Default::default() };
        b.insert(&mut v, "x"); // typing in the far view
        set_frame_budget(Some(FRAME_BUDGET));
        b.highlight_upto(3000);
        let mut fresh = buf(&b.lines.join("\n"));
        fresh.set_syntax(syntaxes().find_syntax_by_extension("c"));
        fresh.highlight_upto(3000);
        let bad: Vec<usize> = (0..3000).filter(|&l| b.segs(l) != fresh.segs(l)).collect();
        assert!(bad.is_empty(), "{} lines differ from a fresh parse, first {:?}", bad.len(), &bad[..bad.len().min(5)]);
    }

    /// Random far views, starved frames and edits (comment openers and closers among
    /// them, before and beyond the pass): settled, every line equals a fresh parse.
    #[test]
    fn random_edits_during_catch_up_settle_exactly() {
        for seed in 1..=12u64 {
            let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = |n: usize| {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n as u64) as usize
            };
            let text: String = (0..600).map(|i| format!("int v{i} = {i};\n")).collect();
            let mut b = buf(&text);
            b.set_syntax(syntaxes().find_syntax_by_extension("c"));
            let mut v = View::default();
            for _ in 0..60 {
                let n = b.lines.len();
                let start = next(n);
                for _ in 0..next(6) {
                    starved_frame(&mut b, start, (start + 40).min(n));
                }
                let line = next(b.lines.len());
                let col = next(b.lines[line].len() + 1);
                v.cursor = Pos::new(line, col);
                v.anchor = None;
                let s = ["x", "/*", "*/", "\n", "\"", "int a;\n"][next(6)];
                b.insert(&mut v, s);
            }
            set_frame_budget(Some(FRAME_BUDGET));
            let n = b.lines.len();
            b.highlight_upto(n);
            let mut fresh = buf(&b.lines.join("\n"));
            fresh.set_syntax(syntaxes().find_syntax_by_extension("c"));
            fresh.highlight_upto(n);
            let bad: Vec<usize> = (0..n).filter(|&l| b.segs(l) != fresh.segs(l)).collect();
            assert!(bad.is_empty(), "seed {seed}: {} lines differ, first {:?}", bad.len(), &bad[..bad.len().min(5)]);
        }
    }

    /// The minimap (no view of its own) and a second view of the buffer (a split, the
    /// peek) keep the first view's speculative lines instead of rebuilding them each frame.
    #[test]
    fn views_keep_their_speculative_lines() {
        let mut b = far_comment_c(3000);
        starved_frame(&mut b, 2550, 2590);
        let first = b.segs(2560).map(<[_]>::to_vec);
        assert!(first.is_some());
        SPEC_BUILDS.with(|c| c.set(0));
        for _ in 0..5 {
            starved_frame(&mut b, 2550, 2590); // the editor
            starved_frame(&mut b, 2620, 2620); // the minimap: no view
            starved_frame(&mut b, 1200, 1240); // a split elsewhere in the file
        }
        assert_eq!(SPEC_BUILDS.with(|c| c.get()), 1, "only the split's view was new");
        assert!((1200..1240).all(|l| b.segs(l).is_some()) && b.segs(2560).map(<[_]>::to_vec) == first);
        set_frame_budget(Some(FRAME_BUDGET));
    }

    #[test]
    fn pending_ends_after_an_edit_mid_catch_up_and_on_truncated_buffers() {
        let mut b = far_comment_c(3000);
        let mut v = View::default();
        for frame in 0.. {
            starved_frame(&mut b, 2950, 2990);
            if frame == 50 {
                v.cursor = Pos::new(10, 0);
                b.insert(&mut v, "x\ny\n"); // an edit while catching up
            }
            if !b.hl_pending() {
                break;
            }
            assert!(frame < 10_000, "an edit kept it pending");
        }
        // Cut to MAX_LINES: the pass ends at the last line kept, and stays done.
        let d = std::env::temp_dir().join(format!("herdr-code-ed-trunc-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("big.c"), "int x;\n".repeat(MAX_LINES + 500)).unwrap();
        let mut t = Buffer::open(&d.join("big.c"));
        assert!(t.readonly.is_some() && t.lines.len() == MAX_LINES);
        for frame in 0.. {
            starved_frame(&mut t, MAX_LINES - 30, MAX_LINES + 400);
            if !t.hl_pending() {
                break;
            }
            assert!(frame < 100_000, "a truncated file kept it pending");
        }
        starved_frame(&mut t, MAX_LINES - 30, MAX_LINES + 400);
        assert!(!t.hl_pending(), "stays done");
        let mut ro = Buffer::from_text("a\nb\n", "C", "read-only view");
        for _ in 0..5 {
            starved_frame(&mut ro, 0, 50); // one line a frame at the least: done in three
        }
        assert!(!ro.hl_pending(), "a read-only view ends too");
        set_frame_budget(Some(FRAME_BUDGET));
        fs::remove_dir_all(&d).unwrap();
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
