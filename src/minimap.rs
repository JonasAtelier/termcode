//! The minimap (VS Code `editor.minimap`): settings, geometry and colour
//! sampling, kept free of drawing for tests. Each cell row covers `slots`
//! slots (4 braille dot rows, VS Code's default `renderCharacters`, or 2
//! half-block halves) of `lps` source lines; each dot (or half-block) column
//! covers `cpc` characters, so `maxColumn` fits the width. A dot is on where
//! its run of a line has code: indentation and short lines show as VS Code's
//! silhouette. Colours come from the editor's highlight cache (per line,
//! invalidated from the edit point), so nothing is re-tokenised per frame;
//! only the visible rows are sampled.

use crate::theme::Rgba;
use ratatui::style::Color;
use serde_json::{Map, Value};
use syntect::highlighting::Style as SStyle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    /// 1 line per slot; the minimap scrolls with long files.
    Proportional,
    /// The whole file stretched (or squeezed) over the minimap height.
    Fill,
    /// Proportional while the file fits, else squeezed.
    Fit,
}

/// `editor.minimap.*`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cfg {
    pub enabled: bool,
    pub max_column: usize,
    pub render_characters: bool,
    /// `showSlider: "always"`; explicit "mouseover" shows it only under the mouse.
    pub slider_always: bool,
    pub left: bool,
    pub size: Size,
    /// `herdr-code.minimap.minEditorWidth`: text columns the minimap must leave.
    pub min_text: u16,
}

impl Default for Cfg {
    fn default() -> Cfg {
        Cfg {
            enabled: true,
            max_column: 120,
            render_characters: true,
            slider_always: true,
            left: false,
            size: Size::Proportional,
            min_text: 40,
        }
    }
}

impl Cfg {
    pub fn parse(user: &Map<String, Value>) -> Cfg {
        let d = Cfg::default();
        let g = |k: &str| user.get(&format!("editor.minimap.{k}"));
        Cfg {
            enabled: g("enabled").and_then(Value::as_bool).unwrap_or(d.enabled),
            max_column: g("maxColumn").and_then(Value::as_u64).map_or(d.max_column, |n| n.clamp(20, 400) as usize),
            render_characters: g("renderCharacters").and_then(Value::as_bool).unwrap_or(d.render_characters),
            slider_always: g("showSlider").and_then(Value::as_str).map_or(d.slider_always, |s| s == "always"),
            left: g("side").and_then(Value::as_str) == Some("left"),
            size: match g("size").and_then(Value::as_str) {
                Some("fill") => Size::Fill,
                Some("fit") => Size::Fit,
                _ => Size::Proportional,
            },
            min_text: user
                .get("herdr-code.minimap.minEditorWidth")
                .and_then(Value::as_u64)
                .map_or(d.min_text, |n| n.clamp(10, 400) as u16),
        }
    }
}

/// Minimap columns (including its 1-column git lane): about 8 characters per
/// cell of `max_column`, 8..=16 cells, at most 15% of the editor (`editor_w`),
/// and shrunk (as VS Code does) so the text keeps `min_text` of its `text_w`
/// columns. Under 4 cells it hides: 0.
pub fn width(text_w: u16, editor_w: u16, max_column: usize, min_text: u16) -> u16 {
    let want = max_column.div_ceil(8).clamp(8, 16) as u16;
    let w = want.min(editor_w * 15 / 100).min(text_w.saturating_sub(min_text));
    if w < 4 {
        0
    } else {
        w
    }
}

/// Characters per sampled column (a braille dot column or a half-block cell)
/// when `cols` of them span `max_column`.
pub fn chars_per_col(cols: usize, max_column: usize) -> usize {
    max_column.div_ceil(cols.max(1)).max(1)
}

/// How strongly a token colour shows over the minimap background: VS Code's
/// 1x glyphs cover about 44% of their pixels (softened by 12/15) and its blocks
/// are drawn at 50%; use 45% to keep code quieter than navigation markers.
pub const INK: f64 = 0.45;

/// A token colour as the minimap draws it: `INK` (scaled by
/// `minimap.foregroundOpacity`'s alpha) of it over `bg`.
pub fn ink(c: Color, opacity: u8, bg: Color) -> Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0xCC, 0xCC, 0xCC),
    };
    Rgba(r, g, b, opacity).alpha(INK).over(bg)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mini {
    pub rows: u16,
    /// Buffer lines and the editor's visible lines.
    pub n: usize,
    pub view: usize,
    pub slots: usize,
    /// Source lines per slot.
    pub lps: usize,
    proportional: bool,
}

impl Mini {
    pub fn new(rows: u16, n: usize, view: usize, slots: usize, size: Size) -> Mini {
        let cap = (rows as usize * slots).max(1);
        let squeezed = n.div_ceil(cap).max(1);
        let (lps, proportional) = match size {
            Size::Proportional => (1, true),
            Size::Fill => (squeezed, false),
            Size::Fit if n <= cap => (1, false),
            Size::Fit => (squeezed, false),
        };
        Mini { rows, n, view, slots, lps, proportional }
    }

    pub fn lines_per_row(&self) -> usize {
        self.slots * self.lps
    }

    /// First line the minimap shows: a long file scrolls proportionally (VS Code).
    pub fn start(&self, scroll: usize) -> usize {
        let cap = self.rows as usize * self.lines_per_row();
        if !self.proportional || self.n <= cap {
            return 0;
        }
        let max = self.n.saturating_sub(1).max(1);
        ((scroll.min(max) as f64 * (self.n - cap) as f64) / max as f64).round() as usize
    }

    /// First source line of `slot` in `row`.
    pub fn slot_line(&self, scroll: usize, row: u16, slot: usize) -> usize {
        self.start(scroll) + row as usize * self.lines_per_row() + slot * self.lps
    }

    /// The row a source line is drawn on, if visible.
    #[cfg(test)]
    pub fn row_of(&self, scroll: usize, line: usize) -> Option<u16> {
        let r = line.checked_sub(self.start(scroll))? / self.lines_per_row();
        (r < self.rows as usize).then_some(r as u16)
    }

    /// Slider (first row, rows) over the editor's viewport.
    pub fn slider(&self, scroll: usize) -> (u16, u16) {
        let lpr = self.lines_per_row();
        let len = self.view.div_ceil(lpr).clamp(1, self.rows.max(1) as usize) as u16;
        let top = (scroll.saturating_sub(self.start(scroll)) / lpr).min(u16::MAX as usize) as u16;
        (top.min(self.rows.saturating_sub(len)), len)
    }

    /// Scroll that centres the line under a click on `row`.
    pub fn click_scroll(&self, scroll: usize, row: u16) -> usize {
        let line = self.slot_line(scroll, row, 0) + self.lines_per_row() / 2;
        line.saturating_sub(self.view / 2).min(self.n.saturating_sub(1))
    }

    /// Scroll that puts the slider's top at row `top` while dragging: the
    /// largest scroll whose slider starts at or above it.
    pub fn drag_scroll(&self, top: i32) -> usize {
        if top <= 0 {
            return 0;
        }
        let (mut lo, mut hi) = (0, self.n.saturating_sub(1));
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.slider(mid).0 as i32 <= top {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    }
}

/// Sample `lines` (text and highlight segments) into `cols` columns of `cpc`
/// display columns each, up to `max_column`: per column the most frequent colour
/// of its non-space characters, None where it has none (indentation, past the
/// end, an empty line). A segment colour with alpha 0 is the editor foreground `fg`.
pub fn sample<'a>(
    lines: impl IntoIterator<Item = (&'a str, Option<&'a [(usize, SStyle)]>)>,
    cols: usize,
    cpc: usize,
    max_column: usize,
    fg: Color,
    tab: usize,
) -> Vec<Option<Color>> {
    let mut counts: Vec<Vec<(Color, u32)>> = vec![Vec::new(); cols];
    for (text, segs) in lines {
        let (mut x, mut si) = (0usize, 0usize);
        for (i, c) in text.char_indices() {
            let nx = if c == '\t' { (x / tab.max(1) + 1) * tab.max(1) } else { x + 1 };
            let col = x / cpc;
            if col >= cols || x >= max_column {
                break;
            }
            if !c.is_whitespace() {
                let color = segs
                    .and_then(|s| {
                        while si < s.len() && s[si].0 <= i {
                            si += 1;
                        }
                        s.get(si)
                    })
                    .map(|(_, st)| st.foreground)
                    .filter(|c| c.a != 0)
                    .map_or(fg, |c| Color::Rgb(c.r, c.g, c.b));
                match counts[col].iter_mut().find(|(k, _)| *k == color) {
                    Some((_, n)) => *n += 1,
                    None => counts[col].push((color, 1)),
                }
            }
            x = nx;
        }
    }
    counts.into_iter().map(|c| most(&c)).collect()
}

/// The most frequent colour; the first seen wins a tie.
fn most(counts: &[(Color, u32)]) -> Option<Color> {
    counts.iter().fold(None, |best: Option<(Color, u32)>, &(k, n)| if best.is_some_and(|(_, m)| m >= n) { best } else { Some((k, n)) }).map(|(k, _)| k)
}

/// A braille cell from up to 4 dot rows (one per slot) of 2 sampled columns:
/// the dots where there is code, and the most frequent colour among them.
pub fn braille(rows: &[[Option<Color>; 2]]) -> (char, Option<Color>) {
    const BITS: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
    let (mut v, mut counts) = (0, Vec::<(Color, u32)>::new());
    for (r, row) in rows.iter().take(4).enumerate() {
        for (c, dot) in row.iter().enumerate() {
            if let Some(k) = *dot {
                v |= BITS[r][c];
                match counts.iter_mut().find(|(q, _)| *q == k) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((k, 1)),
                }
            }
        }
    }
    (char::from_u32(0x2800 + v).unwrap_or(' '), most(&counts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::highlighting::Color as SColor;

    fn st(r: u8) -> SStyle {
        SStyle { foreground: SColor { r, g: 0, b: 0, a: 255 }, ..SStyle::default() }
    }

    #[test]
    fn width_rules() {
        assert_eq!(width(180, 200, 120, 40), 15, "120 columns / 8");
        assert_eq!(width(180, 200, 400, 40), 16, "at most 16");
        assert_eq!(width(180, 200, 20, 40), 8, "at least 8");
        assert_eq!(width(50, 60, 120, 40), 9, "15% of a 60-column editor");
        assert_eq!(width(50, 100, 120, 40), 10, "shrunk so the text keeps 40 columns");
        assert_eq!(width(44, 100, 120, 40), 4, "down to 4 cells");
        assert_eq!(width(43, 100, 120, 40), 0, "then hidden");
        assert_eq!(width(43, 100, 120, 30), 13, "minEditorWidth is a setting");
        assert_eq!(chars_per_col(14, 120), 9, "half blocks: 14 content columns (15 minus the git lane)");
        assert_eq!(chars_per_col(28, 120), 5, "braille: 2 dot columns a cell");
    }

    #[test]
    fn colour_sampling_takes_the_dominant_non_space_colour() {
        let segs = [(4, st(1)), (12, st(2))];
        // "int " is colour 1, "foo_bar" colour 2; spaces are not counted.
        let s = sample([("int foo_bar", Some(&segs[..]))], 2, 8, 120, Color::White, 4);
        assert_eq!(s, [Some(Color::Rgb(2, 0, 0)); 2], "cols 0..8: 3 of colour 1, 4 of colour 2");
        assert_eq!(sample([("        ", None)], 1, 8, 120, Color::White, 4), [None], "blank span: nothing drawn");
        assert_eq!(sample([("x", None)], 1, 8, 120, Color::White, 4), [Some(Color::White)], "no highlight: the editor foreground");
        let tabbed = sample([("\tx", None)], 2, 4, 120, Color::White, 4);
        assert_eq!(tabbed, [None, Some(Color::White)], "a tab spans to the next stop");
    }

    #[test]
    fn braille_dots_are_lines_and_column_runs() {
        let w = Some(Color::White);
        // 4 lines, one dot row each, in 4 dot columns of 2 characters (maxColumn 8).
        let lines = ["ab", "    if x", "", "abcdefghijklmnop"];
        let cols: Vec<Vec<Option<Color>>> = lines.iter().map(|l| sample([(*l, None)], 4, 2, 8, Color::White, 4)).collect();
        assert_eq!(cols[0], [w, None, None, None], "a short line lights its own length");
        assert_eq!(cols[1], [None, None, w, w], "indentation leaves its dots off");
        assert_eq!(cols[2], [None; 4], "an empty line is a blank dot row");
        assert_eq!(cols[3], [w; 4], "a long line is clipped at maxColumn");
        assert_eq!(sample([("abcdefghij", None)], 8, 2, 5, Color::White, 4)[3..], [None; 5], "nothing past maxColumn (5) even with spare dots");
        // Cell 0 = dot columns 0..2, cell 1 = 2..4.
        let cell = |c: usize| braille(&cols.iter().map(|v| [v[2 * c], v[2 * c + 1]]).collect::<Vec<_>>()).0;
        // Dots: line 1 left; line 4 both. Bits 0x01 (row 1 left) + 0x40 | 0x80 (row 4).
        assert_eq!(cell(0), '\u{28C1}');
        // Line 2 both (0x02 | 0x10), line 4 both.
        assert_eq!(cell(1), '\u{28D2}');
        assert_eq!(braille(&[[None, None]]), ('\u{2800}', None), "no code: a blank cell");
    }

    #[test]
    fn a_cell_takes_the_most_frequent_colour_of_its_dots_muted() {
        let (a, b) = (Some(Color::Rgb(255, 0, 0)), Some(Color::Rgb(0, 0, 255)));
        let (ch, c) = braille(&[[a, b], [b, None], [None, None], [None, b]]);
        assert_eq!((ch, c), ('\u{288B}', b), "3 blue dots beat 1 red");
        assert_eq!(braille(&[[a, b]]).1, a, "a tie: the first dot's colour");
        // Muted over the background, scaled by minimap.foregroundOpacity.
        let bg = Color::Rgb(0x1E, 0x1E, 0x1E);
        assert_eq!(ink(Color::Rgb(255, 255, 255), 255, bg), Color::Rgb(131, 131, 131), "muted ink stays below the navigation overlays");
        assert_eq!(ink(Color::Rgb(255, 255, 255), 0, bg), bg, "opacity 0: invisible");
        assert_ne!(ink(Color::Rgb(255, 255, 255), 255, bg), Color::Rgb(255, 255, 255), "never the raw token colour");
    }

    #[test]
    fn geometry_click_and_drag() {
        // 300 lines, 20 rows of 2 lines: proportional, the minimap scrolls.
        let m = Mini::new(20, 300, 30, 2, Size::Proportional);
        assert_eq!(m.start(0), 0);
        assert_eq!(m.start(299), 260, "at the end the last lines are shown");
        assert_eq!(m.slider(0), (0, 15));
        assert_eq!(m.slider(299), (5, 15), "clamped to the bottom");
        assert_eq!(m.row_of(0, 7), Some(3));
        // A click centres the clicked line.
        assert_eq!(m.click_scroll(0, 10), 6, "row 10 = lines 20..22, centred: 21 - 15");
        // Dragging the slider to row t: its top lands on t.
        for t in 0..=5 {
            let s = m.drag_scroll(t);
            assert_eq!(m.slider(s).0 as i32, t, "drag to {t}");
        }
        assert_eq!(m.drag_scroll(99), 299);
        // A short file: no scrolling, a line per slot from the top.
        let short = Mini::new(20, 10, 30, 2, Size::Proportional);
        assert_eq!((short.start(5), short.slider(5)), (0, (2, 15)));
        // Fill squeezes the whole file into the rows.
        let fill = Mini::new(10, 1000, 30, 2, Size::Fill);
        assert_eq!((fill.lps, fill.start(500), fill.row_of(500, 999)), (50, 0, Some(9)));
        assert_eq!(Mini::new(10, 15, 30, 2, Size::Fit).lps, 1, "fit: proportional while it fits");
    }
}
