//! Editor decorations computed from text, kept pure for testing: indent rainbow
//! (oderwat.indent-rainbow's algorithm and defaults), indent guides, quick-diff
//! gutter marks, and scrollbar geometry.

use crate::theme::Rgba;
use fancy_regex::Regex;
use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use std::time::{Duration, Instant};

/// `indentRainbow.*` settings.
#[derive(Clone, Debug)]
pub struct Rainbow {
    pub enabled: bool,
    pub colors: Vec<Rgba>,
    pub error: Rgba,
    pub tabmix: Option<Rgba>,
    /// indicatorStyle "light": a thin line at each level instead of a background.
    pub light: bool,
    pub included: Vec<String>,
    pub excluded: Vec<String>,
    pub ignore_errors: Vec<String>,
    pub ignore_lines: Vec<Regex>,
}

impl Default for Rainbow {
    fn default() -> Rainbow {
        let c = |s| Rgba::parse(s).unwrap_or(Rgba(0, 0, 0, 0));
        Rainbow {
            enabled: true,
            colors: ["rgba(255,255,64,0.07)", "rgba(127,255,127,0.07)", "rgba(255,127,255,0.07)", "rgba(79,236,236,0.07)"]
                .into_iter()
                .map(c)
                .collect(),
            error: c("rgba(128,32,32,0.6)"),
            tabmix: Some(c("rgba(128,32,96,0.6)")),
            light: false,
            included: Vec::new(),
            excluded: vec!["plaintext".into()],
            ignore_errors: vec!["markdown".into()],
            ignore_lines: ["/[ \t]* [*]/g", "/[ \t]+[/]{2}/g"].iter().filter_map(|p| js_regex(p).ok()).collect(),
        }
    }
}

impl Rainbow {
    /// Whether the rainbow draws for a VS Code language id.
    pub fn applies(&self, lang: &str) -> bool {
        self.enabled
            && !self.colors.is_empty()
            && (self.included.is_empty() || self.included.iter().any(|l| l == lang))
            && !self.excluded.iter().any(|l| l == lang)
    }

    pub fn skip_errors(&self, lang: &str) -> bool {
        self.ignore_errors.iter().any(|l| l == lang || l == "*")
    }

    /// Paint for one line (`skip_all`: the language ignores errors).
    pub fn line(&self, line: &str, tab: usize, skip_all: bool) -> Vec<(usize, usize, Paint)> {
        let skip = skip_all || self.ignore_lines.iter().any(|r| r.is_match(line).unwrap_or(false));
        rainbow_line(line, tab, skip, self.tabmix.is_some())
    }
}

/// A JS regex literal from settings ("/pat/flags", or a bare pattern) as a Rust regex.
pub fn js_regex(s: &str) -> Result<Regex, String> {
    let (pat, flags) = match s.strip_prefix('/').and_then(|r| r.rsplit_once('/')) {
        Some((p, f)) if f.chars().all(|c| matches!(c, 'g' | 'i' | 'm' | 's' | 'u' | 'y')) => (p, f),
        _ => (s, ""),
    };
    let inline: String = flags.chars().filter(|c| matches!(c, 'i' | 'm' | 's')).collect();
    let full = if inline.is_empty() { pat.to_string() } else { format!("(?{inline}){pat}") };
    // A runaway pattern gives up (no match) instead of stalling a frame.
    fancy_regex::RegexBuilder::new(&full)
        .backtrack_limit(10_000)
        .build()
        .map_err(|e| format!("indentRainbow.ignoreLinePatterns {s:?} skipped: {e}"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paint {
    /// Indent level n (colour n % colors.len()).
    Level(usize),
    Error,
    Tabmix,
}

/// indent-rainbow's updateDecorations for one line, as byte ranges: the leading
/// whitespace is an error when its width (each tab counted as `tab` spaces) is
/// not a multiple of `tab`; otherwise each level is a tab or `tab` characters
/// (the last may run into the text, as the extension does), tab-mixed when the
/// indentation holds both tabs and spaces.
pub fn rainbow_line(line: &str, tab: usize, skip: bool, tabmix: bool) -> Vec<(usize, usize, Paint)> {
    let tab = tab.max(1);
    let ws = line.bytes().take_while(|b| *b == b' ' || *b == b'\t').count();
    if ws == 0 {
        return Vec::new();
    }
    let m = &line.as_bytes()[..ws];
    let width: usize = m.iter().map(|b| if *b == b'\t' { tab } else { 1 }).sum();
    if !skip && !width.is_multiple_of(tab) {
        return vec![(0, ws, Paint::Error)];
    }
    let mixed = !skip && tabmix && m.contains(&b'\t') && m.contains(&b' ');
    // Character n as a byte offset (the extension counts characters).
    let at = |n: usize| if n <= ws { n } else { line.char_indices().nth(n).map_or(line.len(), |(i, _)| i) };
    let (mut out, mut n, mut level) = (Vec::new(), 0, 0);
    while n < ws {
        let s = n;
        n += if m[n] == b'\t' { 1 } else { tab };
        out.push((s, at(n), if mixed { Paint::Tabmix } else { Paint::Level(level) }));
        level += 1;
    }
    out
}

/// Leading indentation in display columns; None for a blank line.
pub fn indent_cols(line: &str, tab: usize) -> Option<usize> {
    let mut x = 0;
    for c in line.chars() {
        match c {
            ' ' => x += 1,
            '\t' => x = (x / tab + 1) * tab,
            _ => return Some(x),
        }
    }
    None
}

/// Indent level of each line for the guides. A blank line takes VS Code's
/// rule from the content lines around it (`_getIndentLevelForWhitespaceLine`).
pub struct Guides<'a> {
    lines: &'a [String],
    tab: usize,
}

/// How far the guides look for a content line or the cursor's block (keeps a frame cheap).
const SCAN: usize = 1000;
const NEAR: usize = 200;

impl<'a> Guides<'a> {
    pub fn new(lines: &'a [String], tab: usize) -> Guides<'a> {
        Guides { lines, tab: tab.max(1) }
    }

    fn content(&self, i: usize) -> Option<usize> {
        indent_cols(&self.lines[i], self.tab).map(|c| c.div_ceil(self.tab))
    }

    pub fn level(&self, i: usize) -> usize {
        if let Some(l) = self.content(i) {
            return l;
        }
        let above = (i.saturating_sub(NEAR)..i).rev().find_map(|j| self.content(j));
        let below = (i + 1..self.lines.len().min(i + 1 + NEAR)).find_map(|j| self.content(j));
        match (above, below) {
            (Some(a), Some(b)) if a < b => a + 1,
            (Some(a), Some(b)) if a == b => b,
            (Some(_), Some(b)) => b + 1,
            _ => 0,
        }
    }

    /// The guide VS Code highlights for the cursor line: (level index, first, last line).
    pub fn active(&self, cur: usize) -> Option<(usize, usize, usize)> {
        if cur >= self.lines.len() {
            return None;
        }
        let l = self.level(cur);
        let next = if cur + 1 < self.lines.len() { self.level(cur + 1) } else { 0 };
        let (k, start) = if next > l {
            (l, cur + 1) // the cursor line opens a block: its body's guide
        } else if l > 0 {
            let up = (cur.saturating_sub(SCAN)..cur).rev().take_while(|&j| self.level(j) >= l).last().unwrap_or(cur);
            (l - 1, up)
        } else {
            return None;
        };
        let end = (start..self.lines.len().min(start + SCAN)).take_while(|&j| self.level(j) > k).last()?;
        Some((k, start, end))
    }
}

/// Quick-diff gutter mark for one buffer line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Added,
    Modified,
    /// Lines were deleted just below this one (above it for line 0).
    Deleted,
}

/// One change between the index version (`base` lines) and the buffer (`cur` lines).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub base: std::ops::Range<usize>,
    pub cur: std::ops::Range<usize>,
}

impl Hunk {
    /// The buffer line whose gutter shows this hunk's mark.
    pub fn mark_line(&self) -> usize {
        if self.cur.is_empty() {
            self.cur.start.saturating_sub(1)
        } else {
            self.cur.start
        }
    }

    /// Whether a gutter mark on `line` belongs to this hunk.
    pub fn has_line(&self, line: usize) -> bool {
        self.cur.contains(&line) || (self.cur.is_empty() && line == self.mark_line())
    }

    /// The peek opens below the hunk: before this buffer line (== lines.len() at the end).
    pub fn peek_before(&self) -> usize {
        self.cur.end
    }
}

/// VS Code's dirty-diff decorations (one mark per buffer line) and the hunks
/// behind them: the buffer against the index version.
pub fn quick_diff(base: &[String], cur: &[String]) -> (Vec<Option<Mark>>, Vec<Hunk>) {
    let a: Vec<&str> = base.iter().map(String::as_str).collect();
    let b: Vec<&str> = cur.iter().map(String::as_str).collect();
    let mut marks = vec![None; cur.len()];
    let mut hunks = Vec::new();
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(Instant::now() + Duration::from_millis(200)));
    for op in ops {
        let (tag, ra, rb) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => continue,
            DiffTag::Insert => rb.clone().for_each(|i| marks[i] = Some(Mark::Added)),
            DiffTag::Replace => rb.clone().for_each(|i| marks[i] = Some(Mark::Modified)),
            DiffTag::Delete => {
                if let Some(m) = marks.get_mut(rb.start.saturating_sub(1)) {
                    m.get_or_insert(Mark::Deleted);
                }
            }
        }
        hunks.push(Hunk { base: ra, cur: rb });
    }
    (marks, hunks)
}

/// Byte ranges that differ word by word between an old and a new line (the
/// peek's word highlights).
pub fn word_changes(old: &str, new: &str) -> (Vec<std::ops::Range<usize>>, Vec<std::ops::Range<usize>>) {
    // Drawn per row per frame: long lines get no word highlights, and a deadline bounds the rest.
    if old.len() > 2048 || new.len() > 2048 {
        return (Vec::new(), Vec::new());
    }
    let d = similar::TextDiff::configure().deadline(Instant::now() + Duration::from_millis(5)).diff_words(old, new);
    let pre = |it: &mut dyn Iterator<Item = usize>| {
        let mut v = vec![0];
        for len in it {
            v.push(v.last().copied().unwrap_or(0) + len);
        }
        v
    };
    let po = pre(&mut d.iter_old_slices().map(str::len));
    let pn = pre(&mut d.iter_new_slices().map(str::len));
    let (mut o, mut n) = (Vec::new(), Vec::new());
    for op in d.ops() {
        let (tag, ro, rn) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        if !ro.is_empty() {
            o.push(po[ro.start]..po[ro.end]);
        }
        if !rn.is_empty() {
            n.push(pn[rn.start]..pn[rn.end]);
        }
    }
    (o, n)
}

/// VS Code's tree sticky scroll (`findStickyState`) on whole rows: the rows to
/// pin over list slots 0, 1, … for a list scrolled to `off`, at most `max`. Slot
/// `k` pins `ancestor(off + k, k)`: the depth-`k` folder holding the row under it,
/// None when that row is not deeper than `k` (it is the folder itself, shown as it
/// is, or lies outside the folder pinned at `k - 1`) or past the end.
///
/// The pinned folders must hold the first row visible below them and the `sep`
/// rows of their separator. When the deepest one's last row is under them, VS Code
/// pushes it up as that row scrolls away; on whole rows it goes at once, so a
/// folder never sits over rows outside it.
pub fn sticky(ancestor: impl Fn(usize, usize) -> Option<usize>, off: usize, max: usize, sep: usize) -> Vec<usize> {
    let mut pinned: Vec<usize> = (0..max).map_while(|k| ancestor(off + k, k)).collect();
    while let Some(&last) = pinned.last() {
        if ancestor(off + pinned.len() + sep, pinned.len() - 1) == Some(last) {
            break;
        }
        pinned.pop();
    }
    pinned
}

/// `sticky`'s `ancestor` for a list given by row depths (a row's children follow
/// it, deeper): walks back from row `i`, so it costs the rows walked. For short
/// groups (Search, CHANGES); a tree keeps parent links instead.
pub fn walk_back(depth: impl Fn(usize) -> Option<usize>, i: usize, k: usize) -> Option<usize> {
    if depth(i)? <= k {
        return None;
    }
    let mut a = i;
    while a > 0 && depth(a)? > k {
        a -= 1;
    }
    (depth(a)? == k).then_some(a)
}

/// Scrollbar slider (first row, rows) on a `track` of rows, for a viewport of
/// `view` rows at `off` (0..=max_off) over `total` rows of content.
pub fn slider(track: u16, total: usize, view: usize, off: usize, max_off: usize) -> (u16, u16) {
    if track == 0 {
        return (0, 0);
    }
    if max_off == 0 || total == 0 {
        return (0, track);
    }
    let len = ((track as usize * view) as f64 / total as f64).round().clamp(1.0, track as f64) as u16;
    let free = (track - len) as usize;
    let start = ((free * off.min(max_off)) as f64 / max_off as f64).round() as u16;
    (start.min(track - len), len)
}

/// The track row for content row `at` (an overview-ruler mark) with the view at `off`:
/// under the slider exactly when that row is on screen (its place among the rows shown),
/// else where it falls in the whole, above or below the slider as it is.
pub fn mark_row(track: u16, total: usize, view: usize, at: usize, off: usize, max_off: usize) -> u16 {
    let (top, len) = slider(track, total, view, off, max_off);
    let (top, len, last) = (top as usize, len as usize, track.saturating_sub(1) as usize);
    let off = off.min(max_off);
    let r = if (off..off + view).contains(&at) {
        top + (at - off) * len / view.max(1)
    } else {
        let whole = at * track as usize / total.max(1);
        if at < off { whole.min(top.saturating_sub(1)) } else { whole.max(top + len) }
    };
    r.min(last) as u16
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackHit {
    PageUp,
    PageDown,
    /// On the slider, `n` rows below its top (kept while dragging).
    Slider(u16),
}

/// A click at track row `y` (0-based) with the slider at (start, len).
pub fn track_hit(y: u16, (start, len): (u16, u16)) -> TrackHit {
    if y < start {
        TrackHit::PageUp
    } else if y >= start + len {
        TrackHit::PageDown
    } else {
        TrackHit::Slider(y - start)
    }
}

/// Scroll offset for a slider dragged so that its grab point is at row `y`.
pub fn drag_offset(y: i32, grab: u16, track: u16, len: u16, max_off: usize) -> usize {
    let free = track.saturating_sub(len) as i32;
    if free == 0 {
        return 0;
    }
    let top = (y - grab as i32).clamp(0, free);
    ((top as f64 * max_off as f64) / free as f64).round() as usize
}

/// The colours written in `line`, as VS Code's colour decorators find them: `#rgb`,
/// `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb(r, g, b)` and `rgba(r, g, b, a)`. Each: its bytes
/// and the colour. A short `#123` with no letter is an issue's number, not a colour.
pub fn color_literals(line: &str) -> Vec<(usize, usize, Rgba)> {
    let b = line.as_bytes();
    let word = |i: usize| b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' && (i == 0 || !(word(i - 1) || b[i - 1] == b'&')) {
            let n = b[i + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            let hex = &line[i + 1..i + 1 + n];
            let letter = hex.bytes().any(|c| c.is_ascii_alphabetic());
            if !word(i + 1 + n) && (matches!(n, 6 | 8) || (matches!(n, 3 | 4) && letter)) {
                let d = |k: usize, w: usize| u8::from_str_radix(&hex[k * w..k * w + w], 16).map(|v| if w == 1 { v * 17 } else { v }).unwrap_or(0);
                let w = if n > 4 { 2 } else { 1 };
                let a = if n == 4 || n == 8 { d(3, w) } else { 255 };
                out.push((i, i + 1 + n, Rgba(d(0, w), d(1, w), d(2, w), a)));
                i += 1 + n;
                continue;
            }
        }
        if (line[i..].starts_with("rgb(") || line[i..].starts_with("rgba(")) && (i == 0 || !word(i - 1)) {
            let open = i + line[i..].find('(').unwrap_or(0);
            if let Some(close) = line[open..].find(')').map(|c| open + c) {
                let parts: Vec<&str> = line[open + 1..close].split(',').map(str::trim).collect();
                let byte = |s: &str| s.parse::<u8>().ok();
                let alpha = |s: &str| s.parse::<f64>().ok().filter(|a| (0.0..=1.0).contains(a)).map(|a| (a * 255.0).round() as u8);
                let c = match parts.as_slice() {
                    [r, g, bl] => (|| Some(Rgba(byte(r)?, byte(g)?, byte(bl)?, 255)))(),
                    [r, g, bl, a] => (|| Some(Rgba(byte(r)?, byte(g)?, byte(bl)?, alpha(a)?)))(),
                    _ => None,
                };
                if let Some(c) = c {
                    out.push((i, close + 1, c));
                    i = close + 1;
                    continue;
                }
            }
        }
        i += line[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ruler_mark_is_under_the_slider_whenever_its_row_shows() {
        for (track, n, view) in [(43u16, 602usize, 43usize), (40, 5000, 38), (30, 45, 30), (12, 3, 12)] {
            let (total, max_off) = (n + view - 1, n - 1);
            for off in 0..=max_off {
                let (top, len) = slider(track, total, view, off, max_off);
                let mut last = 0;
                for at in 0..n {
                    let m = mark_row(track, total, view, at, off, max_off);
                    let shown = (off..off + view).contains(&at);
                    // Off screen: outside the slider, where there is room for it.
                    let room = if at < off { top > 0 } else { top + len < track };
                    if shown || room {
                        assert_eq!((top..top + len).contains(&m), shown, "track {track}, {n} rows, at {off}: row {at} marked at {m}, slider {top}+{len}");
                    }
                    assert!(m >= last, "in order");
                    last = m;
                }
            }
        }
    }

    /// arch/{sh/{boards/{a,b,c,e,f}, g}, d}, block/{h, i}, z: the expanded tree's depths.
    const TREE: [usize; 14] = [
        0, //  0 arch/
        1, //  1   sh/
        2, //  2     boards/
        3, //  3       a
        3, //  4       b
        3, //  5       c
        3, //  6       e
        3, //  7       f
        2, //  8     g
        1, //  9   d
        0, // 10 block/
        1, // 11   h
        1, // 12   i
        0, // 13 z
    ];

    fn st(off: usize, max: usize) -> Vec<usize> {
        sticky(|i, k| walk_back(|j| TREE.get(j).copied(), i, k), off, max, 0)
    }

    #[test]
    fn sticky_pins_the_folders_above_the_top_row() {
        assert!(st(0, 7).is_empty(), "at the top nothing is hidden");
        // Slot k pins the folder holding the row under it, as VS Code's overlay does.
        assert_eq!(st(1, 7), [0, 1, 2]);
        assert_eq!(st(4, 7), [0, 1, 2], "deep in boards: arch, sh, boards");
        assert_eq!(st(4, 2), [0, 1], "stickyScrollMaxItemCount");
        assert_eq!(st(4, 1), [0], "one row: the outermost folder, arch, not boards");
        assert_eq!(st(11, 1), [10], "inside block: block");
        assert!(st(4, 0).is_empty(), "off");
    }

    #[test]
    fn sticky_rows_leave_when_their_folder_ends() {
        assert_eq!(st(5, 7), [0, 1], "f, boards' last row, under the sticky rows: boards is pushed out over g");
        assert_eq!(st(6, 7), [0, 1], "f scrolled under sh: boards is pushed out");
        assert_eq!(st(8, 7), [0], "slot 1 holds d, outside sh: sh goes too");
        assert!(st(10, 7).is_empty(), "block is the top row itself");
        assert_eq!(st(11, 7), [10], "the next folder takes over");
        assert!(st(12, 7).is_empty(), "i, block's last row, under it: pushed out over z");
        assert!(st(99, 7).is_empty(), "past the end");
        // One row: arch stays while anything of arch is under it, then block replaces it.
        assert!(st(9, 1).is_empty(), "d, arch's last row, under the sticky row: arch is pushed out over block");
        assert!(st(10, 1).is_empty(), "arch pushed out: block is the top row");
        assert!(sticky(|i, k| walk_back(|_| None, i, k), 0, 7, 0).is_empty());
        // A separator row under them covers one more row: off 4 hides b, c, e and f.
        let sep = |off, max| sticky(|i, k| walk_back(|j| TREE.get(j).copied(), i, k), off, max, 1);
        assert_eq!(sep(4, 3), [0, 1], "g would be the first visible row: outside boards");
        assert_eq!(sep(2, 3), [0, 1, 2], "e, the first visible row, is in boards");
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn rainbow_levels_errors_tabmix_and_ignored_lines() {
        use Paint::*;
        assert_eq!(rainbow_line("        x", 4, false, true), vec![(0, 4, Level(0)), (4, 8, Level(1))]);
        assert_eq!(rainbow_line("\t\tx", 4, false, true), vec![(0, 1, Level(0)), (1, 2, Level(1))]);
        assert_eq!(rainbow_line("   x", 4, false, true), vec![(0, 3, Error)], "3 spaces with tabSize 4");
        assert_eq!(rainbow_line("    \tx", 4, false, true), vec![(0, 4, Tabmix), (4, 5, Tabmix)]);
        assert_eq!(rainbow_line("    \tx", 4, false, false), vec![(0, 4, Level(0)), (4, 5, Level(1))]);
        // Skipped (ignored line or language): no error, levels run into the text.
        assert_eq!(rainbow_line("   x", 4, true, true), vec![(0, 4, Level(0))]);
        assert_eq!(rainbow_line("x", 4, false, true), vec![]);
        assert_eq!(rainbow_line("    ", 4, false, true), vec![(0, 4, Level(0))], "blank lines too");
        let r = Rainbow::default();
        assert_eq!(r.line("     * comment", 4, false), vec![(0, 4, Level(0)), (4, 8, Level(1))], "` *` line is ignored");
        assert_eq!(r.line("   // note", 4, false), vec![(0, 4, Level(0))], "`  //` line is ignored");
        assert_eq!(r.line("   x = 1;", 4, false), vec![(0, 3, Error)]);
        assert!(r.applies("c") && !r.applies("plaintext"));
        assert!(r.skip_errors("markdown") && !r.skip_errors("c"));
        assert_eq!(r.colors[0], Rgba(255, 255, 64, 18));
        assert!(js_regex("/[a/").is_err());
        assert!(js_regex("/ABC/i").unwrap().is_match("abc").unwrap());
    }

    #[test]
    fn guides_follow_vscode_blank_line_rules() {
        let l = s(&["fn f() {", "    a();", "", "    if x {", "        b();", "", "    }", "}", "", "g();"]);
        let g = Guides::new(&l, 4);
        let levels: Vec<usize> = (0..l.len()).map(|i| g.level(i)).collect();
        assert_eq!(levels, vec![0, 1, 1, 1, 2, 2, 1, 0, 0, 0]);
        assert_eq!(g.active(4), Some((1, 4, 5)), "inside the if block");
        assert_eq!(g.active(3), Some((1, 4, 5)), "on the line opening it");
        assert_eq!(g.active(1), Some((0, 1, 6)));
        assert_eq!(g.active(9), None);
    }

    #[test]
    fn quick_diff_marks_added_modified_deleted() {
        let base = s(&["a", "b", "c", "d", "e"]);
        let cur = s(&["a", "B", "c", "new", "d"]);
        let (marks, hunks) = quick_diff(&base, &cur);
        assert_eq!(marks, vec![None, Some(Mark::Modified), None, Some(Mark::Added), Some(Mark::Deleted)]);
        assert_eq!(
            hunks,
            vec![Hunk { base: 1..2, cur: 1..2 }, Hunk { base: 3..3, cur: 3..4 }, Hunk { base: 4..5, cur: 5..5 }]
        );
        assert!(hunks[2].has_line(4) && !hunks[2].has_line(5), "a deletion's mark sits on the line above");
        assert_eq!(hunks[2].peek_before(), 5);
        assert_eq!(quick_diff(&base, &s(&["b", "c", "d", "e"])).0[0], Some(Mark::Deleted), "deleted at the top");
        let (m, h) = quick_diff(&base, &base);
        assert!(m.iter().all(Option::is_none) && h.is_empty());
        let (o, n) = word_changes("let x = 1;", "let y = 1;");
        assert_eq!((o.len(), o[0].clone(), n.len(), n[0].clone()), (1, 4..5, 1, 4..5), "the changed word: x / y");
    }

    #[test]
    fn slider_geometry_and_hits() {
        assert_eq!(slider(10, 100, 10, 0, 90), (0, 1));
        assert_eq!(slider(10, 100, 10, 90, 90), (9, 1));
        assert_eq!(slider(10, 20, 10, 5, 10), (3, 5));
        assert_eq!(slider(10, 5, 10, 0, 0), (0, 10), "nothing to scroll: full track");
        let s = slider(10, 20, 10, 5, 10);
        assert_eq!(track_hit(0, s), TrackHit::PageUp);
        assert_eq!(track_hit(4, s), TrackHit::Slider(1));
        assert_eq!(track_hit(8, s), TrackHit::PageDown);
        assert_eq!(drag_offset(6, 1, 10, 5, 10), 10);
        assert_eq!(drag_offset(1, 1, 10, 5, 10), 0);
        assert_eq!(drag_offset(4, 1, 10, 5, 10), 6);
    }

    #[test]
    fn colours_written_in_a_line_are_found_and_issue_numbers_are_not() {
        let found = |l: &str| color_literals(l).into_iter().map(|(a, e, c)| (l[a..e].to_string(), c)).collect::<Vec<_>>();
        assert_eq!(found(r##""fg": "#0e639c", "bg": "#fff8", x: rgba(128, 32, 32, 0.6);"##), [
            ("#0e639c".to_string(), Rgba(14, 99, 156, 255)),
            ("#fff8".to_string(), Rgba(255, 255, 255, 136)),
            ("rgba(128, 32, 32, 0.6)".to_string(), Rgba(128, 32, 32, 153)),
        ]);
        assert_eq!(found("rgb(1,2,3) #abc"), [("rgb(1,2,3)".to_string(), Rgba(1, 2, 3, 255)), ("#abc".to_string(), Rgba(170, 187, 204, 255))]);
        assert!(found("fixes #123 and #4567, &#123; a#fff, #define, #12345, rgb(300,0,0)").is_empty());
    }
}
