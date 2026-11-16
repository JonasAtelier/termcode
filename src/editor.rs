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
/// How far a bracket search goes from the cursor, each way.
const BRACKET_LINES: usize = 5000;
const UNDO_PAUSE: Duration = Duration::from_secs(1);
const UNDO_LIMIT: usize = 1000;
use fancy_regex::Regex;
use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use syntect::highlighting::{Color as SColor, HighlightState, Highlighter, RangedHighlightIterator, Style as SStyle};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use unicode_width::UnicodeWidthChar;

/// The most of a file opened: past either, the rest is cut and the buffer is read-only.
/// Measured at 300,000 lines (12 MB): a key and its frame 7 ms, opening 90 ms, the
/// quick diff 31 ms.
pub const MAX_BYTES: u64 = 16 << 20;
pub const MAX_LINES: usize = 500_000;
/// Past this many lines a file is large: the minimap colours only what is already
/// highlighted instead of highlighting the whole file behind it (8 s of CPU at 100,000
/// lines, 37 s at 300,000).
pub const LARGE_LINES: usize = 50_000;
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

/// `hexdump -C`'s lines for `bytes`: the offset, 16 bytes in two groups of 8, and those
/// that are printable ASCII between bars (others as dots).
pub fn hexdump(bytes: &[u8]) -> Vec<String> {
    use std::fmt::Write as _;
    bytes
        .chunks(16)
        .enumerate()
        .map(|(i, c)| {
            let mut s = format!("{:08x} ", i * 16);
            for k in 0..16 {
                if k == 8 {
                    s.push(' ');
                }
                match c.get(k) {
                    Some(b) => {
                        let _ = write!(s, " {b:02x}");
                    }
                    None => s.push_str("   "),
                }
            }
            s.push_str("  |");
            s.extend(c.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }));
            s.push('|');
            s
        })
        .collect()
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
    /// More cursors (multi-cursor mode). Never alongside a selection.
    pub extra: Vec<Pos>,
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
pub(crate) enum Kind {
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
    /// Shown as a hexdump.
    pub binary: bool,
    /// View: Toggle Hexdump's choice (None: a hexdump for binary files and .bin).
    pub hex: Option<bool>,
    /// An ELF file shown as readelf's report; the file is an ELF one (whichever way shown).
    pub elf: bool,
    pub elf_file: bool,
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
    /// Inside `at_cursors`: the edits share its one undo step.
    batch: bool,
    last_edit: Option<Instant>,
    /// Boxed: a far jump's state makes it big, and every `Doc::Text` carries one.
    hl: Option<Box<Hl>>,
    width_cache: Option<(u64, usize, usize)>,
    /// Boxed: every `Doc::Text` carries one.
    derived: Box<Derived>,
}

/// What is worked out from the text, kept for the version it was worked out for.
#[derive(Default)]
struct Derived {
    /// Bracket nesting depth at each line's start.
    brackets: Option<(u64, Vec<u32>)>,
    symbols: Option<(u64, Vec<crate::symbols::Symbol>)>,
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
/// Display columns where each row of `line` wrapped at `w` cells starts (`[0]` for one
/// row): after the last blank that fits, else mid-word; a wide character never splits.
pub fn wrap_starts(line: &str, w: usize) -> Vec<usize> {
    let w = w.max(1);
    let (mut starts, mut x, mut blank) = (vec![0], 0, None);
    for c in line.chars() {
        let nx = advance(x, c);
        while let Some(&row) = starts.last().filter(|&&row| nx - row > w && x > row) {
            starts.push(blank.filter(|&b| b > row).unwrap_or(x));
            blank = None;
        }
        if c == ' ' || c == '\t' {
            blank = Some(nx);
        }
        x = nx;
    }
    starts
}

/// The wrapped row display column `x` is on.
pub fn wrap_row(starts: &[usize], x: usize) -> usize {
    starts.partition_point(|&s| s <= x).saturating_sub(1)
}

/// The line comment of a language (`(open, "")`), or its block pair where it has none,
/// by syntax name, then by file name for plain text (Kconfig, *.conf, device trees).
pub fn comment_marks(syntax: &str, path: &str) -> Option<(&'static str, &'static str)> {
    let s = syntax.to_ascii_lowercase();
    let line = |m| Some((m, ""));
    match s.as_str() {
        "c" | "c++" | "c#" | "rust" | "go" | "java" | "javascript" | "typescript" | "typescriptreact" | "javascript (babel)"
        | "objective-c" | "objective-c++" | "scala" | "groovy" | "d" | "php" | "actionscript" | "swift" | "kotlin" | "dart" | "json" => line("//"),
        "python" | "ruby" | "perl" | "bourne again shell (bash)" | "shell-unix-generic" | "makefile" | "yaml" | "toml" | "r" | "tcl"
        | "cmake" | "dockerfile" | "nginx" | "git ignore" | "git config" | "git attributes" | "ssh config" | "diff" => line("#"),
        "lua" | "sql" | "haskell" | "ada" => line("--"),
        "lisp" | "clojure" | "ini" => line(";"),
        "latex" | "tex" | "erlang" | "matlab" => line("%"),
        "batch file" => line("@REM"),
        "html" | "xml" | "markdown" | "multimarkdown" => Some(("<!--", "-->")),
        "css" | "scss" | "less" => Some(("/*", "*/")),
        "ocaml" | "pascal" => Some(("(*", "*)")),
        _ => {
            let name = path.rsplit('/').next().unwrap_or(path);
            let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
            if name.starts_with("Kconfig") || matches!(ext, "conf" | "cfg" | "cnf" | "mk" | "service" | "env" | "properties") || name.starts_with(".env") {
                line("#")
            } else if matches!(ext, "dts" | "dtsi" | "jsonc" | "proto") {
                line("//")
            } else {
                None
            }
        }
    }
}

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

/// `editor.renderWhitespace`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Whitespace {
    None,
    Boundary,
    Selection,
    Trailing,
    All,
}

/// The whitespace to draw on `line`: byte offset, and whether it is a tab.
pub fn whitespace_marks(line: &str, mode: Whitespace, selected: Option<(usize, usize)>) -> Vec<(usize, bool)> {
    let bytes = line.as_bytes();
    let content_end = line.trim_end_matches([' ', '\t']).len();
    let blank = |b: Option<&u8>| b.is_none_or(|c| *c == b' ' || *c == b'\t');
    let shown = |i: usize, tab: bool| match mode {
        Whitespace::None => false,
        Whitespace::All => true,
        Whitespace::Trailing => i >= content_end,
        Whitespace::Selection => selected.is_some_and(|(start, end)| (start..end).contains(&i)),
        Whitespace::Boundary => tab || blank(i.checked_sub(1).and_then(|p| bytes.get(p))) || blank(bytes.get(i + 1)),
    };
    bytes.iter().enumerate().filter(|&(i, &b)| (b == b' ' || b == b'\t') && shown(i, b == b'\t')).map(|(i, &b)| (i, b == b'\t')).collect()
}

/// The word the cursor is in, at the start of, or just after.
pub fn cursor_word(line: &str, col: usize) -> Option<&str> {
    let is_word = |&(start, _): &(usize, usize)| line[start..].chars().next().is_some_and(|c| class(c) == 1);
    let here = word_at(line, col).filter(is_word);
    let before = || line.get(..col)?.char_indices().next_back().and_then(|(at, _)| word_at(line, at)).filter(is_word);
    here.or_else(before).map(|(start, end)| &line[start..end])
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

/// Where a position ends up after an edit elsewhere in the text.
fn through(p: Pos, op: &Op) -> Pos {
    match op {
        Op::Ins(at, s) if p >= *at => {
            let end = end_pos(*at, s);
            let col = if p.line == at.line { end.col + (p.col - at.col) } else { p.col };
            Pos::new(p.line + (end.line - at.line), col)
        }
        Op::Del(at, s) if p > *at => {
            let end = end_pos(*at, s);
            if p <= end {
                *at
            } else if p.line == end.line {
                Pos::new(at.line, at.col + (p.col - end.col))
            } else {
                Pos::new(p.line - (end.line - at.line), p.col)
            }
        }
        _ => p,
    }
}

fn end_pos(at: Pos, s: &str) -> Pos {
    match s.rfind('\n') {
        None => Pos::new(at.line, at.col + s.len()),
        Some(i) => Pos::new(at.line + s.matches('\n').count(), s.len() - i - 1),
    }
}

/// Literal find over numbered lines, also used by either side of a diff.
/// Find's query as a regular expression, honouring its case and whole-word toggles.
pub(crate) fn find_regex(q: &str, case: bool, word: bool) -> Result<Regex, String> {
    let body = if word { format!(r"\b(?:{q})\b") } else { q.to_string() };
    Regex::new(&format!("{}{body}", if case { "" } else { "(?i)" })).map_err(|e| e.to_string())
}

/// Every non-empty match of `re`, line by line: a match never spans lines.
pub(crate) fn find_re<'a>(lines: impl IntoIterator<Item = (usize, &'a str)>, re: &Regex) -> Vec<(Pos, Pos)> {
    let mut out = Vec::new();
    for (li, l) in lines {
        let mut at = 0;
        while let Ok(Some(m)) = re.find_from_pos(l, at) {
            if m.end() > m.start() {
                out.push((Pos::new(li, m.start()), Pos::new(li, m.end())));
                if out.len() >= 10_000 {
                    return out;
                }
                at = m.end();
            } else {
                let Some(c) = l[m.end()..].chars().next() else { break };
                at = m.end() + c.len_utf8();
            }
        }
    }
    out
}

/// The replacement for the match of `re` at byte `at` of `line`: `$1`, `${name}`, `$$`.
pub(crate) fn expand(re: &Regex, line: &str, at: usize, with: &str) -> String {
    let mut out = String::new();
    match re.captures_from_pos(line, at) {
        Ok(Some(c)) => fancy_regex::Expander::default().append_expansion(&mut out, with, &c),
        _ => out.push_str(with),
    }
    out
}

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
            hex: None,
            elf: false,
            elf_file: false,
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
            batch: false,
            last_edit: None,
            hl: None,
            width_cache: None,
            derived: Box::default(),
        }
    }

    /// Read-only text (commit view, notices).
    /// Cut to `max` lines and read-only: what `open` does past MAX_BYTES or MAX_LINES.
    fn truncate_lines(&mut self, max: usize) {
        self.lines.truncate(max);
        self.line_ids.truncate(max);
        self.readonly = Some(format!("truncated to {} MB / {MAX_LINES} lines: read-only, cannot be saved", MAX_BYTES >> 20));
    }

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
        Buffer::open_as(path, None)
    }

    /// `open`, as a hexdump (`hex` Some(true)) or as text (Some(false)) whatever the file.
    pub fn open_as(path: &Path, hex: Option<bool>) -> Buffer {
        let mut b = Buffer::empty(Some(path.to_path_buf()));
        b.hex = hex;
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
        // An ELF file: what readelf says about it (its bytes: View: Toggle Hexdump).
        b.elf_file = crate::elf::is_elf(&buf);
        if b.elf_file && hex != Some(true) {
            if let Some(report) = crate::elf::report(path) {
                let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let mut lines = crate::elf::view(&name, &report);
                lines.truncate(MAX_LINES);
                b.lines = lines;
                b.line_ids = fresh_line_ids(b.lines.len()).collect();
                b.elf = true;
                b.readonly = Some("ELF file: what readelf says about it".into());
                b.set_syntax(syntaxes().find_syntax_by_name("Markdown"));
                return b;
            }
        }
        // A binary file (a NUL early on, or a .bin) is shown as `hexdump -C` shows it.
        let nul = buf[..buf.len().min(8192)].contains(&0);
        if hex.unwrap_or(nul || path.extension().is_some_and(|e| e == "bin")) {
            let shown = buf.len().min(MAX_LINES * 16);
            b.binary = true;
            b.readonly = Some(if shown < buf.len() {
                format!("binary file: a hexdump of its first {} MB, cannot be edited", shown / 1_000_000)
            } else {
                "binary file: shown as a hexdump, cannot be edited".into()
            });
            if shown > 0 {
                b.lines = hexdump(&buf[..shown]);
                b.line_ids = fresh_line_ids(b.lines.len()).collect();
            }
            b.set_syntax(None);
            return b;
        }
        let capped = buf.len() as u64 > MAX_BYTES;
        buf.truncate(MAX_BYTES as usize);
        if nul {
            // Asked for as text: saving would corrupt it.
            b.readonly = Some("binary file shown as text: cannot be edited".into());
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => {
                if !capped && b.readonly.is_none() {
                    b.readonly = Some("not valid UTF-8: read-only so saving cannot corrupt it".into());
                }
                String::from_utf8_lossy(e.as_bytes()).into_owned()
            }
        };
        b.set_text(&text);
        if capped || b.lines.len() > MAX_LINES {
            b.truncate_lines(MAX_LINES);
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

    /// Widest display line; scrolling does not rescan the file on every frame.
    pub fn max_display_width(&mut self) -> usize {
        let stamp = (self.version, tab());
        if let Some((_, _, width)) = self.width_cache.filter(|&(v, t, _)| (v, t) == stamp) {
            return width;
        }
        let width = self.lines.iter().map(|s| display_col(s, s.len())).max().unwrap_or(0);
        self.width_cache = Some((stamp.0, stamp.1, width));
        width
    }

    /// Bracket nesting depth at the start of each line, every kind together as VS Code's
    /// bracket pair colouring counts them. Text only, like `bracket_pair`.
    pub fn bracket_depths(&mut self) -> &[u32] {
        if self.derived.brackets.as_ref().is_none_or(|(v, _)| *v != self.version) {
            let mut d = 0u32;
            let depths = self.lines.iter().map(|l| {
                let start = d;
                for b in l.bytes() {
                    match b {
                        b'(' | b'[' | b'{' => d += 1,
                        b')' | b']' | b'}' => d = d.saturating_sub(1),
                        _ => {}
                    }
                }
                start
            });
            self.derived.brackets = Some((self.version, depths.collect()));
        }
        &self.derived.brackets.as_ref().unwrap().1
    }

    /// Go to Symbol's and the breadcrumbs' symbols, for this version.
    pub fn symbols(&mut self) -> &[crate::symbols::Symbol] {
        if self.derived.symbols.as_ref().is_none_or(|(v, _)| *v != self.version) {
            // readelf's report is Markdown: its parts are the outline.
            let path = if self.elf { "readelf.md" } else { self.path.as_deref().and_then(Path::to_str).unwrap_or("") };
            self.derived.symbols = Some((self.version, crate::symbols::symbols(path, &self.lines)));
        }
        &self.derived.symbols.as_ref().unwrap().1
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
        self.settle_cursors(v);
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
        if self.batch {
            return;
        }
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
        if self.batch {
            return;
        }
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
        v.extra.clear();
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
        v.extra.clear();
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

    /// The bracket beside `cur` (just before it first, as VS Code picks) and its partner.
    /// Text only: a bracket inside a string or a comment counts like any other.
    pub fn bracket_pair(&self, cur: Pos) -> Option<(Pos, Pos)> {
        let line = self.lines.get(cur.line)?.as_bytes();
        let beside = [cur.col.checked_sub(1), Some(cur.col)];
        let col = beside.into_iter().flatten().find(|&c| line.get(c).is_some_and(|b| b"()[]{}".contains(b)))?;
        let mine = line[col];
        let (other, down) = match mine {
            b'(' => (b')', true),
            b'[' => (b']', true),
            b'{' => (b'}', true),
            b')' => (b'(', false),
            b']' => (b'[', false),
            _ => (b'{', false),
        };
        let mut depth = 0usize;
        let mut step = |c: u8| {
            if c == mine {
                depth += 1;
            } else if c == other {
                depth -= 1;
            }
            depth == 0
        };
        if down {
            for l in cur.line..self.lines.len().min(cur.line + BRACKET_LINES) {
                let bytes = self.lines[l].as_bytes();
                let from = if l == cur.line { col } else { 0 };
                if let Some(c) = (from..bytes.len()).find(|&c| step(bytes[c])) {
                    return Some((Pos::new(cur.line, col), Pos::new(l, c)));
                }
            }
        } else {
            for l in (cur.line.saturating_sub(BRACKET_LINES)..=cur.line).rev() {
                let bytes = self.lines[l].as_bytes();
                let to = if l == cur.line { col + 1 } else { bytes.len() };
                if let Some(c) = (0..to).rev().find(|&c| step(bytes[c])) {
                    return Some((Pos::new(cur.line, col), Pos::new(l, c)));
                }
            }
        }
        None
    }

    /// Ctrl+Shift+Up/Down: one more cursor past the outermost one, at the same column.
    /// It becomes the main cursor, so the view follows it.
    pub fn add_cursor(&mut self, v: &mut View, down: bool) {
        self.clamp_view(v);
        let lines = v.extra.iter().chain([&v.cursor]).map(|p| p.line);
        let edge = if down { lines.max() } else { lines.min() }.unwrap_or(v.cursor.line);
        let next = v.shift_line(edge, if down { 1 } else { -1 }).min(self.lines.len() - 1);
        if next == edge {
            return;
        }
        let x = v.want_x.unwrap_or_else(|| display_col(&self.lines[v.cursor.line], v.cursor.col));
        v.extra.push(v.cursor);
        v.anchor = None;
        v.cursor = Pos::new(next, byte_at_display(&self.lines[next], x));
        v.want_x = Some(x);
        self.settle_cursors(v);
    }

    /// Extra cursors in order, inside the text, none on another.
    fn settle_cursors(&self, v: &mut View) {
        if v.extra.is_empty() {
            return;
        }
        let main = self.clamp(v.cursor);
        let mut extra: Vec<Pos> = v.extra.iter().map(|p| self.clamp(*p)).filter(|p| *p != main).collect();
        extra.sort();
        extra.dedup();
        v.extra = extra;
    }

    /// Run an edit at every cursor, as one undo step. Each edit moves the text under the
    /// other cursors, so they follow the operations it recorded.
    pub(crate) fn at_cursors(&mut self, v: &mut View, kind: Kind, mut edit: impl FnMut(&mut Buffer, &mut View)) {
        if v.extra.is_empty() {
            return edit(self, v);
        }
        self.begin(v, kind);
        let mut cursors: Vec<Pos> = std::iter::once(v.cursor).chain(v.extra.drain(..)).collect();
        self.batch = true;
        for i in 0..cursors.len() {
            (v.cursor, v.anchor, v.want_x) = (cursors[i], None, None);
            let done = self.undo.last().map_or(0, |g| g.ops.len());
            edit(self, v);
            cursors[i] = v.cursor;
            let ops = self.undo.last().map_or(Vec::new(), |g| g.ops[done..].to_vec());
            for (_, c) in cursors.iter_mut().enumerate().filter(|(j, _)| *j != i) {
                *c = ops.iter().fold(*c, through);
            }
        }
        self.batch = false;
        v.cursor = cursors[0];
        v.extra = cursors[1..].to_vec();
        self.settle_cursors(v);
        self.end(v);
    }

    /// Move every cursor the same way; cursors that meet become one.
    pub fn move_cursors(&mut self, v: &mut View, mut step: impl FnMut(&mut Buffer, &mut View)) {
        if v.extra.is_empty() {
            return step(self, v);
        }
        let extras = std::mem::take(&mut v.extra);
        step(self, v);
        let (main, want) = (v.cursor, v.want_x);
        let mut moved = Vec::with_capacity(extras.len());
        for p in extras {
            (v.cursor, v.anchor, v.want_x) = (p, None, None);
            step(self, v);
            moved.push(v.cursor);
        }
        (v.cursor, v.anchor, v.want_x) = (main, None, want);
        v.extra = moved;
        self.settle_cursors(v);
    }

    /// The lines an edit of whole lines acts on: the selection's, without a last line it
    /// only touches at column 0, or the cursor's.
    fn line_span(&self, v: &View) -> (usize, usize) {
        match v.selection() {
            Some((a, e)) if e.col == 0 && e.line > a.line => (a.line, e.line - 1),
            Some((a, e)) => (a.line, e.line),
            None => (self.clamp(v.cursor).line, self.clamp(v.cursor).line),
        }
    }

    fn shift_view(v: &mut View, down: bool, by: usize) {
        let line = |p: Pos| if down { p.line + by } else { p.line - by };
        v.cursor = Pos::new(line(v.cursor), v.cursor.col);
        v.anchor = v.anchor.map(|a| Pos::new(line(a), a.col));
        v.want_x = None;
    }

    /// Alt+Up/Down: the current lines swap with the line above or below. One undo step.
    pub fn move_lines(&mut self, v: &mut View, down: bool) {
        let (start, end) = self.line_span(v);
        let n = self.lines.len();
        if self.readonly.is_some() || if down { end + 1 >= n } else { start == 0 } {
            return;
        }
        self.begin(v, Kind::Other);
        if down {
            let below = self.lines[end + 1].clone();
            if end + 2 < n {
                self.do_del(Pos::new(end + 1, 0), Pos::new(end + 2, 0));
            } else {
                self.do_del(Pos::new(end, self.lines[end].len()), Pos::new(end + 1, below.len()));
            }
            self.do_ins(Pos::new(start, 0), &format!("{below}\n"));
        } else {
            let above = self.lines[start - 1].clone();
            self.do_del(Pos::new(start - 1, 0), Pos::new(start, 0));
            if end < self.lines.len() {
                self.do_ins(Pos::new(end, 0), &format!("{above}\n"));
            } else {
                self.do_ins(Pos::new(end - 1, self.lines[end - 1].len()), &format!("\n{above}"));
            }
        }
        Self::shift_view(v, down, 1);
        self.end(v);
    }

    /// Shift+Alt+Down: the current lines again below themselves, the cursor on the copy.
    pub fn duplicate_lines(&mut self, v: &mut View) {
        let (start, end) = self.line_span(v);
        if self.readonly.is_some() {
            return;
        }
        let block = self.lines[start..=end].join("\n");
        self.begin(v, Kind::Other);
        if end + 1 < self.lines.len() {
            self.do_ins(Pos::new(end + 1, 0), &format!("{block}\n"));
        } else {
            self.do_ins(Pos::new(end, self.lines[end].len()), &format!("\n{block}"));
        }
        Self::shift_view(v, true, end - start + 1);
        self.end(v);
    }

    /// A character typed in the editor, closing brackets and quotes: a selection is wrapped,
    /// a closer already there is stepped over, and a pair opens only where nothing would be
    /// swallowed by it. A quote after a word or a backslash stays single.
    pub fn type_char(&mut self, v: &mut View, c: char, brackets: bool, quotes: bool) {
        let is_quote = matches!(c, '"' | '\'' | '`');
        let close = match c {
            '(' if brackets => Some(')'),
            '[' if brackets => Some(']'),
            '{' if brackets => Some('}'),
            _ if quotes && is_quote => Some(c),
            _ => None,
        };
        let cur = self.clamp(v.cursor);
        let line = &self.lines[cur.line];
        let (before, after) = (line[..cur.col].chars().next_back(), line[cur.col..].chars().next());
        let closer = (brackets && matches!(c, ')' | ']' | '}')) || (quotes && is_quote);
        if self.readonly.is_some() {
            return self.insert(v, &c.to_string());
        }
        if let (Some(close), Some((a, e))) = (close, v.selection()) {
            self.begin(v, Kind::Other);
            self.do_ins(e, &close.to_string());
            self.do_ins(a, &c.to_string());
            let inside = |p: Pos| if p.line == a.line { Pos::new(p.line, p.col + c.len_utf8()) } else { p };
            (v.anchor, v.cursor) = (Some(inside(a)), inside(e));
            v.want_x = None;
            return self.end(v);
        }
        if closer && after == Some(c) && v.selection().is_none() {
            v.cursor = Pos::new(cur.line, cur.col + c.len_utf8());
            v.want_x = None;
            return;
        }
        let room = after.is_none_or(|a| a.is_whitespace() || ")]}>,;:.".contains(a));
        let single = is_quote && before.is_some_and(|b| b.is_alphanumeric() || b == '_' || b == '\\');
        match close.filter(|_| room && !single && v.selection().is_none()) {
            Some(close) => {
                self.begin(v, Kind::Type);
                self.do_ins(cur, &format!("{c}{close}"));
                v.cursor = Pos::new(cur.line, cur.col + c.len_utf8());
                v.want_x = None;
                self.end(v);
            }
            None => self.insert(v, &c.to_string()),
        }
    }

    /// Backspace in the editor: inside an empty pair both halves go.
    pub fn backspace_pair(&mut self, v: &mut View, brackets: bool, quotes: bool) {
        let c = self.clamp(v.cursor);
        let line = &self.lines[c.line];
        let pair = match (line[..c.col].chars().next_back(), line[c.col..].chars().next()) {
            (Some('('), Some(')')) | (Some('['), Some(']')) | (Some('{'), Some('}')) => brackets,
            (Some(a @ ('"' | '\'' | '`')), Some(b)) if a == b => quotes,
            _ => false,
        };
        if !pair || self.readonly.is_some() || v.selection().is_some() {
            return self.backspace(v);
        }
        self.begin(v, Kind::Erase);
        self.do_del(Pos::new(c.line, c.col - 1), Pos::new(c.line, c.col + 1));
        v.cursor = Pos::new(c.line, c.col - 1);
        v.want_x = None;
        self.end(v);
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

    /// Ctrl+/: comment the selected lines (the cursor's without a selection) at their least
    /// indent, or uncomment them when every non-blank one already is. One undo step; the
    /// cursor and selection stay on their text.
    pub fn toggle_comment(&mut self, v: &mut View, (open, close): (&str, &str)) {
        if self.readonly.is_some() {
            return;
        }
        let c = self.clamp(v.cursor);
        let (first, last) = self.sel_lines(v).unwrap_or((c.line, c.line));
        let blank = |l: &str| l.trim().is_empty();
        let lead = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
        let lines = &self.lines[first..=last];
        let commented = |l: &str| {
            let t = l.trim_start_matches([' ', '\t']).trim_end();
            t.starts_with(open) && t.ends_with(close) && t.len() >= open.len() + close.len()
        };
        if lines.iter().all(|l| blank(l)) {
            return;
        }
        let remove = lines.iter().filter(|l| !blank(l)).all(|l| commented(l));
        let indent = lines.iter().filter(|l| !blank(l)).map(|l| lead(l)).min().unwrap_or(0);
        v.extra.clear();
        self.begin(v, Kind::Other);
        for l in first..=last {
            let text = self.lines[l].clone();
            if blank(&text) {
                continue;
            }
            // (byte where the edit is, bytes removed there) for each end of the line.
            let mut edits: Vec<(usize, isize)> = Vec::new();
            if remove {
                let at = lead(&text);
                let after = &text[at + open.len()..];
                let n = open.len() + usize::from(after.starts_with(' '));
                if !close.is_empty() {
                    let end = text.trim_end().len();
                    let body = &text[..end - close.len()];
                    let m = close.len() + usize::from(body.ends_with(' ') && body.len() > at + n);
                    self.do_del(Pos::new(l, end - m), Pos::new(l, end));
                    edits.push((end - m, -(m as isize)));
                }
                self.do_del(Pos::new(l, at), Pos::new(l, at + n));
                edits.push((at, -(n as isize)));
            } else {
                if !close.is_empty() {
                    let end = text.trim_end().len();
                    self.do_ins(Pos::new(l, end), &format!(" {close}"));
                    edits.push((end, close.len() as isize + 1));
                }
                self.do_ins(Pos::new(l, indent), &format!("{open} "));
                edits.push((indent, open.len() as isize + 1));
            }
            for p in std::iter::once(&mut v.cursor).chain(v.anchor.as_mut()).filter(|p| p.line == l) {
                for &(at, d) in &edits {
                    if p.col > at || (d > 0 && p.col == at && at > 0) {
                        p.col = p.col.saturating_add_signed(d).max(at);
                    }
                }
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

    /// Replace every match with its own text (back to front so earlier positions stay valid). One undo step.
    pub fn replace_each(&mut self, v: &mut View, matches: &[(Pos, Pos, String)]) -> usize {
        if self.readonly.is_some() || matches.is_empty() {
            return 0;
        }
        self.begin(v, Kind::Other);
        for (a, b, with) in matches.iter().rev() {
            self.do_del(*a, *b);
            self.do_ins(*a, with);
        }
        v.anchor = None;
        v.cursor = self.clamp(v.cursor);
        self.end(v);
        matches.len()
    }

    // ---- movement ----------------------------------------------------------

    pub fn move_to(&mut self, v: &mut View, p: Pos, extend: bool) {
        v.extra.clear();
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

    /// Up/Down by `delta` screen rows of lines wrapped at `w` cells, keeping the column
    /// within the row (`want_x` is that, not the line's column, while wrapped).
    pub fn vertical_wrapped(&mut self, v: &mut View, delta: isize, extend: bool, w: usize) {
        let c = self.clamp(v.cursor);
        let (mut l, mut starts) = (c.line, wrap_starts(&self.lines[c.line], w));
        let dc = display_col(&self.lines[l], c.col);
        let mut row = wrap_row(&starts, dc);
        let mut x = v.want_x.unwrap_or(dc - starts[row]);
        let mut left = delta;
        while left != 0 {
            let next = if left > 0 { v.shift_line(l, 1) } else { v.shift_line(l, -1) };
            if left > 0 && row + 1 < starts.len() {
                row += 1;
            } else if left < 0 && row > 0 {
                row -= 1;
            } else if next != l && next < self.lines.len() {
                l = next;
                starts = wrap_starts(&self.lines[l], w);
                row = if left > 0 { 0 } else { starts.len() - 1 };
            } else {
                // Past the first or last row: the start or end of the line, as VS Code.
                x = if left > 0 { usize::MAX / 2 } else { 0 };
                row = if left > 0 { starts.len() - 1 } else { 0 };
                break;
            }
            left -= left.signum();
        }
        let end = starts.get(row + 1).map_or(usize::MAX, |&n| n.saturating_sub(1));
        let p = Pos::new(l, byte_at_display(&self.lines[l], (starts[row] + x.min(usize::MAX / 2)).min(end)));
        self.move_to(v, p, extend);
        v.want_x = Some(x.min(w));
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
            let tmp = dir.join(format!(".{name}.termcode.{}.tmp", std::process::id()));
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
        let fresh = Buffer::open_as(&path, self.hex);
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
// ponytail: pid + comm liveness; a dead panel's pid reused by another termcode
// hides that backup until the other one exits. A per-panel flock if that bites.
pub fn orphan_backups(dir: &Path, live: &[&str]) -> Vec<(PathBuf, serde_json::Value)> {
    let running = |v: &serde_json::Value| {
        v["pid"].as_u64().is_some_and(|p| fs::read_to_string(format!("/proc/{p}/comm")).is_ok_and(|c| live.contains(&c.trim_end())))
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
        let names = |live: &[&str]| -> Vec<String> {
            orphan_backups(&bak, live).iter().map(|(p, _)| p.file_stem().unwrap().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(names(&[comm.trim_end()]), ["old", "dead"], "our own pid is a running panel");
        assert_eq!(names(&["termcode"]), ["live", "old", "dead"], "a pid under another name is no panel");
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
    fn regex_find_reports_every_match_on_every_line() {
        let re = find_regex(r"foo\d+", true, false).unwrap();
        let hits = find_re([(0, "foo1 bar"), (1, "x foo22 foo3")], &re);
        assert_eq!(hits, vec![(Pos::new(0, 0), Pos::new(0, 4)), (Pos::new(1, 2), Pos::new(1, 7)), (Pos::new(1, 8), Pos::new(1, 12))]);
    }

    #[test]
    fn regex_find_follows_the_case_and_word_toggles() {
        let line = [(0, "Foo foo food")];
        assert_eq!(find_re(line, &find_regex("foo", true, false).unwrap()).len(), 2, "exact case: foo, food");
        assert_eq!(find_re(line, &find_regex("foo", false, false).unwrap()).len(), 3, "any case");
        assert_eq!(find_re(line, &find_regex("foo", false, true).unwrap()).len(), 2, "whole words: Foo, foo");
        assert_eq!(find_re([(0, "abc")], &find_regex("a|b", true, true).unwrap()).len(), 0, "the word test covers the whole alternation");
    }

    #[test]
    fn regex_find_skips_empty_matches_and_rejects_bad_patterns() {
        assert_eq!(find_re([(0, "aaé")], &find_regex("x*", true, false).unwrap()), vec![], "x* matches nothing visible");
        assert!(find_regex("(", true, false).is_err());
    }

    #[test]
    fn a_regex_replacement_expands_groups_in_their_line() {
        let re = find_regex(r"(\w+)=(\d+)", true, false).unwrap();
        assert_eq!(expand(&re, "a=1 bc=22", 4, "$2:$1"), "22:bc", "the match at that column");
        assert_eq!(expand(&re, "a=1", 0, "$$"), "$");
    }

    fn at(text: &str, line: usize, col: usize) -> (Buffer, View) {
        let mut b = buf(text);
        let mut v = View::default();
        b.clamp_view(&mut v);
        b.move_to(&mut v, Pos::new(line, col), false);
        (b, v)
    }

    #[test]
    fn line_comments_toggle_at_the_least_indent_and_keep_the_cursor_on_its_text() {
        let (mut b, mut v) = at("    x();\n\n        y();\n", 0, 6);
        v.anchor = Some(Pos::new(2, 12));
        b.toggle_comment(&mut v, ("//", ""));
        assert_eq!(b.lines[..3], ["    // x();", "", "    //     y();"]);
        assert_eq!((v.cursor, v.anchor), (Pos::new(0, 9), Some(Pos::new(2, 15))));
        b.toggle_comment(&mut v, ("//", ""));
        assert_eq!(b.lines[..3], ["    x();", "", "        y();"]);
        assert_eq!((v.cursor, v.anchor), (Pos::new(0, 6), Some(Pos::new(2, 12))));
        b.undo(&mut v);
        assert_eq!(b.lines[0], "    // x();", "each toggle is one undo step");
        let (mut b, mut v) = at("a\n", 0, 0);
        b.toggle_comment(&mut v, ("<!--", "-->"));
        assert_eq!(b.lines[0], "<!-- a -->");
        b.toggle_comment(&mut v, ("<!--", "-->"));
        assert_eq!(b.lines[0], "a");
        assert_eq!(comment_marks("Rust", "x.rs"), Some(("//", "")));
        assert_eq!(comment_marks("Plain Text", "drivers/Kconfig"), Some(("#", "")));
        assert_eq!(comment_marks("Plain Text", "notes.txt"), None);
    }

    #[test]
    fn wrapping_breaks_after_blanks_and_never_splits_a_wide_character() {
        assert_eq!(wrap_starts("aaa bbb ccc", 5), [0, 4, 8]);
        assert_eq!(wrap_starts("abcdefghij", 4), [0, 4, 8]);
        assert_eq!(wrap_starts("ab中文", 3), [0, 2, 4]);
        assert_eq!(wrap_starts("", 3), [0]);
        assert_eq!([wrap_row(&[0, 4, 8], 3), wrap_row(&[0, 4, 8], 4), wrap_row(&[0, 4, 8], 11)], [0, 1, 2]);
        // Up/Down by screen rows, the column kept within the row.
        let (mut b, mut v) = at("aaaa bbbb cccc\nxy\n", 0, 1);
        b.vertical_wrapped(&mut v, 1, false, 5);
        assert_eq!(v.cursor, Pos::new(0, 6));
        b.vertical_wrapped(&mut v, 1, false, 5);
        assert_eq!(v.cursor, Pos::new(0, 11));
        b.vertical_wrapped(&mut v, 1, false, 5);
        assert_eq!(v.cursor, Pos::new(1, 1));
        b.vertical_wrapped(&mut v, -2, false, 5);
        assert_eq!(v.cursor, Pos::new(0, 6));
        b.vertical_wrapped(&mut v, -5, false, 5);
        assert_eq!(v.cursor, Pos::new(0, 0), "past the top: the line's start");
    }

    #[test]
    fn a_line_moves_down_and_up_with_the_cursor() {
        let (mut b, mut v) = at("a\nb\nc", 0, 1);
        b.move_lines(&mut v, true);
        assert_eq!((b.lines.clone(), v.cursor), (vec!["b".to_string(), "a".into(), "c".into()], Pos::new(1, 1)));
        b.move_lines(&mut v, true);
        assert_eq!(b.lines, ["b", "c", "a"], "onto the last line");
        b.move_lines(&mut v, false);
        assert_eq!((b.lines.clone(), v.cursor.line), (vec!["b".to_string(), "a".into(), "c".into()], 1));
    }

    #[test]
    fn a_line_at_the_edge_stays_and_leaves_no_undo_step() {
        let (mut b, mut v) = at("a\nb", 0, 0);
        b.move_lines(&mut v, false);
        assert_eq!(b.lines, ["a", "b"]);
        assert!(!b.undo(&mut v), "nothing happened");
        let (mut b, mut v) = at("a\nb", 1, 0);
        b.move_lines(&mut v, true);
        assert_eq!(b.lines, ["a", "b"]);
    }

    #[test]
    fn moving_takes_every_selected_line_and_keeps_the_selection() {
        let (mut b, mut v) = at("a\nb\nc\nd", 0, 0);
        b.move_to(&mut v, Pos::new(1, 1), true);
        b.move_lines(&mut v, true);
        assert_eq!(b.lines, ["c", "a", "b", "d"]);
        assert_eq!((v.anchor, v.cursor), (Some(Pos::new(1, 0)), Pos::new(2, 1)));
        assert!(b.undo(&mut v));
        assert_eq!(b.lines, ["a", "b", "c", "d"], "one undo step");
    }

    #[test]
    fn a_selection_ending_at_column_zero_leaves_that_line_out() {
        let (mut b, mut v) = at("a\nb\nc", 0, 0);
        b.move_to(&mut v, Pos::new(1, 0), true);
        b.move_lines(&mut v, true);
        assert_eq!(b.lines, ["b", "a", "c"]);
    }

    #[test]
    fn duplicating_copies_below_and_follows_the_copy() {
        let (mut b, mut v) = at("a\nb", 0, 1);
        b.duplicate_lines(&mut v);
        assert_eq!((b.lines.clone(), v.cursor), (vec!["a".to_string(), "a".into(), "b".into()], Pos::new(1, 1)));
        let (mut b, mut v) = at("a", 0, 0);
        b.duplicate_lines(&mut v);
        assert_eq!(b.lines, ["a", "a"], "the last line too");
        let (mut b, mut v) = at("a\nb\nc", 0, 0);
        b.move_to(&mut v, Pos::new(1, 1), true);
        b.duplicate_lines(&mut v);
        assert_eq!(b.lines, ["a", "b", "a", "b", "c"]);
        assert_eq!((v.anchor, v.cursor), (Some(Pos::new(2, 0)), Pos::new(3, 1)));
        assert!(b.undo(&mut v));
        assert_eq!(b.lines, ["a", "b", "c"], "one undo step");
    }

    #[test]
    fn the_cursor_finds_the_partner_of_the_bracket_beside_it() {
        let b = buf("f(a, (b))[c]");
        assert_eq!(b.bracket_pair(Pos::new(0, 1)), Some((Pos::new(0, 1), Pos::new(0, 8))), "in front of it");
        assert_eq!(b.bracket_pair(Pos::new(0, 2)), Some((Pos::new(0, 1), Pos::new(0, 8))), "just after it");
        assert_eq!(b.bracket_pair(Pos::new(0, 9)), Some((Pos::new(0, 8), Pos::new(0, 1))), "from the closer, the inner pair skipped");
        assert_eq!(b.bracket_pair(Pos::new(0, 3)), None, "no bracket beside it");
    }

    #[test]
    fn a_bracket_pair_can_span_lines_and_may_have_no_partner() {
        let b = buf("if (x) {\n  y;\n}");
        assert_eq!(b.bracket_pair(Pos::new(0, 7)), Some((Pos::new(0, 7), Pos::new(2, 0))));
        assert_eq!(b.bracket_pair(Pos::new(2, 0)), Some((Pos::new(2, 0), Pos::new(0, 7))));
        assert_eq!(buf("(oops").bracket_pair(Pos::new(0, 0)), None);
        assert_eq!(buf("oops)").bracket_pair(Pos::new(0, 4)), None);
    }

    #[test]
    fn bracket_search_ignores_other_kinds_and_text_after_multibyte_characters() {
        let b = buf("中[(中]");
        assert_eq!(b.bracket_pair(Pos::new(0, 3)), Some((Pos::new(0, 3), Pos::new(0, 8))), "a [ does not count ( as nesting");
    }

    #[test]
    fn the_cursor_word_is_the_one_it_is_in_or_just_after() {
        assert_eq!(cursor_word("foo bar", 1), Some("foo"), "inside");
        assert_eq!(cursor_word("foo bar", 0), Some("foo"), "at its start");
        assert_eq!(cursor_word("foo bar", 3), Some("foo"), "just after it");
        assert_eq!(cursor_word("foo bar", 4), Some("bar"), "at the next word's start");
        assert_eq!(cursor_word("a  b", 2), None, "between spaces");
        assert_eq!(cursor_word("(+)", 1), None, "punctuation is no word");
        assert_eq!(cursor_word("中文 x", 3), Some("中文"), "after a multibyte word");
        assert_eq!(cursor_word("", 0), None);
    }

    #[test]
    fn whitespace_modes_choose_which_blanks_show() {
        let marks = |line, mode, sel| whitespace_marks(line, mode, sel);
        let line = "  a b\tc  ";
        let spaces = |v: &[usize]| v.iter().map(|&i| (i, false)).collect::<Vec<_>>();
        assert_eq!(marks(line, Whitespace::None, None), vec![]);
        let mut all = spaces(&[0, 1, 3]);
        all.push((5, true));
        all.extend(spaces(&[7, 8]));
        assert_eq!(marks(line, Whitespace::All, None), all);
        let mut boundary = spaces(&[0, 1]);
        boundary.push((5, true));
        boundary.extend(spaces(&[7, 8]));
        assert_eq!(marks(line, Whitespace::Boundary, None), boundary, "a single space between words stays bare");
        assert_eq!(marks(line, Whitespace::Trailing, None), spaces(&[7, 8]));
        assert_eq!(marks(line, Whitespace::Selection, Some((1, 4))), spaces(&[1, 3]));
        assert_eq!(marks(line, Whitespace::Selection, None), vec![]);
        assert_eq!(marks("   ", Whitespace::Trailing, None), spaces(&[0, 1, 2]), "a blank line is all trailing");
    }

    fn multi(text: &str, at: &[(usize, usize)]) -> (Buffer, View) {
        let b = buf(text);
        let mut v = View::default();
        b.clamp_view(&mut v);
        v.cursor = Pos::new(at[0].0, at[0].1);
        v.extra = at[1..].iter().map(|&(l, c)| Pos::new(l, c)).collect();
        (b, v)
    }

    #[test]
    fn a_new_cursor_lands_on_the_next_line_at_the_same_column() {
        let (mut b, mut v) = multi("abcd\nef\nghij", &[(0, 3)]);
        b.add_cursor(&mut v, true);
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(1, 2), vec![Pos::new(0, 3)]), "a short line ends the column");
        b.add_cursor(&mut v, true);
        assert_eq!(v.cursor, Pos::new(2, 3), "the column is remembered");
        assert_eq!(v.extra.len(), 2);
        b.add_cursor(&mut v, true);
        assert_eq!(v.extra.len(), 2, "no line below: nothing added");
        let (mut b, mut v) = multi("a\nb", &[(1, 0)]);
        b.add_cursor(&mut v, false);
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(0, 0), vec![Pos::new(1, 0)]), "upward");
    }

    #[test]
    fn adding_a_cursor_drops_the_selection() {
        let (mut b, mut v) = multi("ab\ncd", &[(0, 0)]);
        b.move_to(&mut v, Pos::new(0, 2), true);
        b.add_cursor(&mut v, true);
        assert_eq!(v.anchor, None);
    }

    #[test]
    fn typing_goes_to_every_cursor_as_one_undo_step() {
        let (mut b, mut v) = multi("ab\ncd", &[(1, 1), (0, 1)]);
        for c in "XY".chars() {
            b.at_cursors(&mut v, Kind::Type, |b, v| b.type_char(v, c, true, true));
        }
        assert_eq!(b.lines, ["aXYb", "cXYd"]);
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(1, 3), vec![Pos::new(0, 3)]));
        assert!(b.undo(&mut v));
        assert_eq!(b.lines, ["ab", "cd"]);
        assert_eq!(v.extra, [], "undo leaves one cursor");
    }

    #[test]
    fn cursors_keep_their_places_when_an_edit_moves_text() {
        let (mut b, mut v) = multi("ab\ncd", &[(0, 1), (1, 1)]);
        b.at_cursors(&mut v, Kind::Other, |b, v| b.newline(v));
        assert_eq!(b.lines, ["a", "b", "c", "d"], "Enter adds a line above the later cursor");
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(1, 0), vec![Pos::new(3, 0)]));
        let (mut b, mut v) = multi("ab\ncd", &[(0, 2), (1, 2)]);
        b.at_cursors(&mut v, Kind::Erase, |b, v| b.backspace(v));
        assert_eq!((b.lines.clone(), v.cursor, v.extra.clone()), (vec!["a".to_string(), "c".into()], Pos::new(0, 1), vec![Pos::new(1, 1)]));
        let (mut b, mut v) = multi("a b", &[(0, 0), (0, 2)]);
        b.at_cursors(&mut v, Kind::Type, |b, v| b.type_char(v, 'X', false, false));
        assert_eq!(b.lines, ["Xa Xb"], "two cursors on one line");
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(0, 1), vec![Pos::new(0, 4)]));
        let (mut b, mut v) = multi("ab\ncd", &[(1, 2), (0, 2)]);
        b.at_cursors(&mut v, Kind::Other, |b, v| b.insert(v, "x\ny"));
        assert_eq!(b.lines, ["abx", "y", "cdx", "y"], "a paste of several lines");
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(3, 1), vec![Pos::new(1, 1)]));
    }

    #[test]
    fn auto_closing_works_at_every_cursor() {
        let (mut b, mut v) = multi("\n\n", &[(0, 0), (1, 0)]);
        b.at_cursors(&mut v, Kind::Type, |b, v| b.type_char(v, '(', true, true));
        assert_eq!(b.lines, ["()", "()"]);
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(0, 1), vec![Pos::new(1, 1)]));
    }

    #[test]
    fn arrows_move_every_cursor_and_cursors_that_meet_merge() {
        let (mut b, mut v) = multi("ab\ncd", &[(0, 1), (1, 1)]);
        b.move_cursors(&mut v, |b, v| b.left(v, false, false));
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(0, 0), vec![Pos::new(1, 0)]));
        let (mut b, mut v) = multi("ab", &[(0, 0), (0, 1)]);
        b.move_cursors(&mut v, |b, v| b.left(v, false, false));
        assert_eq!((v.cursor, v.extra.clone()), (Pos::new(0, 0), vec![]), "they became one");
    }

    #[test]
    fn jumping_somewhere_ends_multi_cursor_mode() {
        let (mut b, mut v) = multi("ab\ncd", &[(0, 1), (1, 1)]);
        b.move_to(&mut v, Pos::new(0, 0), false);
        assert_eq!(v.extra, []);
    }

    fn typed(start: &str, col: usize, keys: &str) -> (Vec<String>, usize) {
        let mut b = buf(start);
        let mut v = View::default();
        b.clamp_view(&mut v);
        b.move_to(&mut v, Pos::new(0, col), false);
        for c in keys.chars() {
            b.type_char(&mut v, c, true, true);
        }
        (b.lines.clone(), v.cursor.col)
    }

    #[test]
    fn typing_an_opener_closes_it_where_nothing_follows() {
        assert_eq!(typed("", 0, "("), (vec!["()".to_string()], 1));
        assert_eq!(typed(")", 0, "["), (vec!["[])".to_string()], 1), "before a closer");
        assert_eq!(typed("x", 0, "("), (vec!["(x".to_string()], 1), "not in front of a word");
        assert_eq!(typed("", 0, "(("), (vec!["(())".to_string()], 2), "nested");
    }

    #[test]
    fn typing_the_closer_steps_over_it() {
        assert_eq!(typed("", 0, "()"), (vec!["()".to_string()], 2));
        assert_eq!(typed("", 0, "\"\""), (vec!["\"\"".to_string()], 2));
    }

    #[test]
    fn quotes_stay_single_after_a_word_or_a_backslash() {
        assert_eq!(typed("", 0, "'"), (vec!["''".to_string()], 1));
        assert_eq!(typed("a", 1, "'"), (vec!["a'".to_string()], 2), "an apostrophe");
        assert_eq!(typed("\\", 1, "\""), (vec!["\\\"".to_string()], 2), "an escape");
    }

    #[test]
    fn typing_an_opener_wraps_a_selection() {
        let mut b = buf("ab cd");
        let mut v = View::default();
        b.clamp_view(&mut v);
        b.move_to(&mut v, Pos::new(0, 2), true);
        b.type_char(&mut v, '(', true, true);
        assert_eq!(b.lines, ["(ab) cd"]);
        assert_eq!(b.selected_text(&v).as_deref(), Some("ab"), "the text stays selected");
        assert!(b.undo(&mut v));
        assert_eq!(b.lines, ["ab cd"], "one undo step");
    }

    #[test]
    fn backspace_in_an_empty_pair_removes_both_halves() {
        let mut b = buf("()");
        let mut v = View::default();
        b.clamp_view(&mut v);
        b.move_to(&mut v, Pos::new(0, 1), false);
        b.backspace_pair(&mut v, true, true);
        assert_eq!((b.lines.clone(), v.cursor.col), (vec![String::new()], 0));
        let mut b = buf("(x)");
        b.move_to(&mut v, Pos::new(0, 2), false);
        b.backspace_pair(&mut v, true, true);
        assert_eq!(b.lines, ["()"], "only an empty pair");
    }

    #[test]
    fn a_closed_pair_is_one_undo_step() {
        let mut b = buf("");
        let mut v = View::default();
        b.clamp_view(&mut v);
        b.type_char(&mut v, '(', true, true);
        assert!(b.undo(&mut v));
        assert_eq!(b.lines, [""]);
    }

    #[test]
    fn auto_closing_can_be_turned_off_per_kind() {
        let mut b = buf("");
        let mut v = View::default();
        b.clamp_view(&mut v);
        b.type_char(&mut v, '(', false, true);
        assert_eq!(b.lines, ["("], "brackets off");
        b.type_char(&mut v, '"', false, true);
        assert_eq!(b.lines, ["(\"\""], "quotes still on");
        let mut b = buf("()");
        b.clamp_view(&mut v);
        b.move_to(&mut v, Pos::new(0, 1), false);
        b.backspace_pair(&mut v, false, true);
        assert_eq!(b.lines, [")"], "off: a plain Backspace");
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

    fn find_all(b: &Buffer, q: &str, case: bool, word: bool) -> Vec<(Pos, Pos)> {
        find_in(b.lines.iter().enumerate().map(|(i, s)| (i, s.as_str())), q, case, word)
    }

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty(None);
        b.set_text(text);
        b
    }

    #[test]
    fn horizontal_range_counts_display_cells_and_updates_after_edit_undo_redo() {
        let mut b = buf("中e\u{301}\nlonger");
        let mut v = View::default();
        assert_eq!(b.max_display_width(), 6);
        b.insert(&mut v, "12345678");
        assert_eq!(b.max_display_width(), 11);
        b.undo(&mut v);
        assert_eq!(b.max_display_width(), 6);
        b.redo(&mut v);
        assert_eq!(b.max_display_width(), 11);
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("termcode-ed-{tag}-{}", std::process::id()));
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
    fn a_binary_file_or_a_bin_opens_as_a_hexdump() {
        let d = tmpdir("hex");
        let elf = d.join("prog");
        fs::write(&elf, b"\x7fELF\x02\x01\x01\0\0\0\0\0\0\0\0\0Hi!\n").unwrap();
        let b = Buffer::open(&elf);
        assert!(b.binary && b.readonly.as_deref() == Some("binary file: shown as a hexdump, cannot be edited"));
        assert_eq!(
            b.lines,
            [
                "00000000  7f 45 4c 46 02 01 01 00  00 00 00 00 00 00 00 00  |.ELF............|",
                "00000010  48 69 21 0a                                       |Hi!.|",
            ]
        );
        // A .bin is one whatever its bytes; an empty one has one blank line.
        fs::write(d.join("fw.bin"), "plain").unwrap();
        assert_eq!(Buffer::open(&d.join("fw.bin")).lines, ["00000000  70 6c 61 69 6e                                    |plain|"]);
        fs::write(d.join("empty.bin"), "").unwrap();
        assert_eq!(Buffer::open(&d.join("empty.bin")).lines, [""]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_elf_file_opens_as_readelfs_report_and_toggles_to_its_bytes() {
        if std::process::Command::new("readelf").arg("--version").output().is_err() {
            return; // no binutils here
        }
        let exe = std::env::current_exe().unwrap();
        let b = Buffer::open(&exe);
        assert!(b.elf && !b.binary && b.readonly.is_some() && b.lines[0].starts_with("# ") && b.syntax_name() == "Markdown", "{:?}", &b.lines[..2]);
        let hex = Buffer::open_as(&exe, Some(true));
        assert!(hex.binary && !hex.elf && hex.lines[0].starts_with("00000000  7f 45 4c 46"));
        assert!(Buffer::open_as(&exe, Some(false)).elf, "back to the report");
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
        assert_eq!(find_all(&b, "foo", true, false).len(), 2);
        assert_eq!(find_all(&b, "foo", false, true).len(), 3); // 中foo: 中 is a word char
        let m = find_all(&b, "foo", false, false);
        assert_eq!(m.len(), 4);
        assert_eq!(m[3], (Pos::new(1, 11), Pos::new(1, 14)));
        let mut v = View::default();
        let each: Vec<_> = m.iter().map(|&(a, e)| (a, e, "x".to_string())).collect();
        assert_eq!(b.replace_each(&mut v, &each), 4);
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
        // Cut (as `open` cuts at MAX_LINES; a small cut keeps this quick): the pass ends at
        // the last line kept, and stays done.
        let cut = 2_500;
        let mut t = Buffer::from_text(&"int x;\n".repeat(cut + 500), "C", "test");
        t.truncate_lines(cut);
        assert!(t.readonly.is_some() && t.lines.len() == cut);
        for frame in 0.. {
            starved_frame(&mut t, cut - 30, cut + 400);
            if !t.hl_pending() {
                break;
            }
            assert!(frame < 100_000, "a truncated file kept it pending");
        }
        starved_frame(&mut t, cut - 30, cut + 400);
        assert!(!t.hl_pending(), "stays done");
        let mut ro = Buffer::from_text("a\nb\n", "C", "read-only view");
        for _ in 0..5 {
            starved_frame(&mut ro, 0, 50); // one line a frame at the least: done in three
        }
        assert!(!ro.hl_pending(), "a read-only view ends too");
        set_frame_budget(Some(FRAME_BUDGET));
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
