//! The minimap (VS Code `editor.minimap`): settings, geometry and colour
//! sampling, kept free of drawing for tests. Each cell row covers `slots`
//! slots (4 braille dot rows, VS Code's default `renderCharacters`, or 2
//! half-block halves) of editor rows: one row a slot, or the file's share of the
//! height when it fills; each dot (or half-block) column
//! covers `cpc` characters, so `maxColumn` fits the width. A dot is on where
//! its run of a line has code: indentation and short lines show as VS Code's
//! silhouette. Colours come from the editor's highlight cache (per line,
//! invalidated from the edit point), so nothing is re-tokenised per frame;
//! only the visible rows are sampled.

use crate::theme::Rgba;
use ratatui::style::Color;
use serde_json::{Map, Value};
use std::ops::Range;
use syntect::highlighting::Style as SStyle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Size {
    /// 1 line per slot; with a long file the minimap shows a stretch of it, which stays
    /// put while the editor's rows are in it and moves at its edges (the default).
    Proportional,
    /// The whole file over the minimap height, at most one row of it per cell row.
    Fill,
    /// Proportional while the file fits, else filled.
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
    /// `editor.minimap.scale` (1-3): drawn in pixels, a line is 2 × scale px tall and a
    /// character scale px wide.
    pub scale: u8,
    /// `termcode.minimap.minEditorWidth`: text columns the minimap must leave.
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
            scale: 3,
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
                Some("proportional") => Size::Proportional,
                Some("fill") => Size::Fill,
                Some("fit") => Size::Fit,
                _ => d.size,
            },
            scale: g("scale").and_then(Value::as_u64).map_or(d.scale, |n| n.clamp(1, 3) as u8),
            min_text: user
                .get("termcode.minimap.minEditorWidth")
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

/// A pixel minimap's cells: its characters (up to `max_column`, at most 72) at `scale`
/// px each in `cw`-px cells, plus the lane's cell; at most 24 cells and 18% of the
/// editor, leaving `min_text` columns of text. 0: no room.
pub fn pixel_width(text_w: u16, editor_w: u16, max_column: usize, min_text: u16, scale: u8, cw: u16) -> u16 {
    let want = (max_column.min(72) * usize::from(scale)).div_ceil(usize::from(cw.max(1))) as u16 + 1;
    let w = want.clamp(8, 24).min(editor_w * 18 / 100).min(text_w.saturating_sub(min_text));
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

/// A shrunk glyph's strength (its coverage is the rest): quieter than the code beside it.
pub const GLYPH_INK: f64 = 0.7;

/// A token colour as the minimap draws it: `INK` (scaled by
/// `minimap.foregroundOpacity`'s alpha) of it over `bg`.
pub fn ink(c: Color, opacity: u8, bg: Color) -> Color {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (0xCC, 0xCC, 0xCC),
    };
    Rgba(r, g, b, opacity).alpha(INK).over(bg)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mini {
    pub rows: u16,
    /// The editor's rows (buffer lines less those a fold hides) and how many it shows.
    pub n: usize,
    pub view: usize,
    pub slots: usize,
    proportional: bool,
    /// Editor rows a cell row holds at the fixed scale: `slots`, or a fraction of them
    /// (a pixel minimap's lines, a whole number of pixels each).
    per_row: f64,
    /// The first editor row shown (a long file at a fixed scale): `follow` keeps it.
    pub first: usize,
}

impl Mini {
    pub fn new(rows: u16, n: usize, view: usize, slots: usize, size: Size) -> Mini {
        let proportional = match size {
            Size::Proportional => true,
            Size::Fill => false,
            Size::Fit => n <= rows as usize * slots,
        };
        Mini { rows, n, view, slots, proportional, per_row: slots as f64, first: 0 }
    }

    /// At the fixed scale, `per_row` editor rows a cell row (a fraction: each line a whole
    /// number of pixels, however tall the cells).
    pub fn per_row(mut self, per_row: f64) -> Mini {
        self.per_row = per_row.max(0.5);
        self
    }

    /// Editor rows the minimap holds at once.
    fn cap(&self) -> usize {
        (self.rows as f64 * self.per_row) as usize
    }

    /// Whether it shows a stretch of the file (a long file at a fixed scale).
    fn stretch(&self) -> bool {
        self.proportional && self.n > self.cap()
    }

    /// This minimap with its stretch for `scroll`, from the one it showed (`prev`): the
    /// same while the editor's rows are in it, else moved just far enough to hold them.
    pub fn follow(mut self, prev: usize, scroll: usize) -> Mini {
        self.first = if self.stretch() {
            let (max, end) = (self.n - self.cap(), (scroll + self.view).min(self.n));
            let s = prev.min(max);
            let s = if scroll < s {
                scroll
            } else if end > s + self.cap() {
                end - self.cap()
            } else {
                s
            };
            s.min(max)
        } else {
            0
        };
        self
    }

    /// The stretch with the editor's rows (at `scroll`) in its middle: after a click.
    pub fn centred(&self, scroll: usize) -> usize {
        if !self.stretch() {
            return 0;
        }
        (scroll + self.view / 2).saturating_sub(self.cap() / 2).min(self.n - self.cap())
    }

    /// Editor rows per minimap row: fractional, so a fill always reaches the bottom.
    pub fn lines_per_row(&self) -> f64 {
        if self.proportional {
            self.per_row
        } else {
            (self.n as f64 / self.rows.max(1) as f64).max(1.0)
        }
    }

    /// First editor row the minimap shows: its stretch's (`follow`), else the top.
    pub fn start(&self, _scroll: usize) -> usize {
        if self.stretch() {
            self.first
        } else {
            0
        }
    }

    /// The editor rows `slot` of minimap row `row` covers: at least one, none past the end.
    pub fn slot_rows(&self, scroll: usize, row: u16, slot: usize) -> Range<usize> {
        let first = |slot: usize| {
            self.start(scroll) + ((row as f64 + slot as f64 / self.slots as f64) * self.lines_per_row()) as usize
        };
        let a = first(slot).min(self.n);
        a..first(slot + 1).max(a + 1).min(self.n)
    }

    /// The minimap row an editor row is drawn on, if visible.
    #[cfg(test)]
    pub fn row_of(&self, scroll: usize, line: usize) -> Option<u16> {
        let r = (line.checked_sub(self.start(scroll))? as f64 / self.lines_per_row()) as usize;
        (r < self.rows as usize).then_some(r as u16)
    }

    /// Slider (first row, rows) over the rows the editor shows, fewer once the file
    /// ends inside the viewport. Nearest cell: a constant length while scrolling.
    pub fn slider(&self, scroll: usize) -> (u16, u16) {
        let lpr = self.lines_per_row();
        let scroll = scroll.min(self.n.saturating_sub(1));
        let shown = self.view.min(self.n - scroll);
        let len = ((shown as f64 / lpr).round() as u16).clamp(1, self.rows.max(1));
        let top = (scroll.saturating_sub(self.start(scroll)) as f64 / lpr).round() as u16;
        (top.min(self.rows.saturating_sub(len)), len)
    }

    /// Scroll that centres the row under a click on minimap row `row`.
    pub fn click_scroll(&self, scroll: usize, row: u16) -> usize {
        let line = self.slot_rows(scroll, row, 0).start + (self.lines_per_row() / 2.0) as usize;
        line.saturating_sub(self.view / 2).min(self.n.saturating_sub(1))
    }

    /// Scroll that puts the slider's top at row `top` while dragging: in a stretch, its
    /// rows from where it was (past its edge the editor goes on, and the stretch with it);
    /// else the largest scroll whose slider starts at or above it.
    pub fn drag_scroll(&self, top: i32) -> usize {
        if self.stretch() {
            let at = self.first as f64 + f64::from(top) * self.lines_per_row();
            return (at.max(0.0) as usize).min(self.n.saturating_sub(1));
        }
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

/// The printable ASCII characters as a pixel minimap draws them (VS Code's minimap
/// renderer): each glyph of the monospace font drawn 4× larger, then shrunk into its
/// `w`×`h` pixels, each pixel how much of the glyph covers it (0-255). `.` is faint, `M`
/// solid, lowercase sits low. None without a font (characters are then solid blocks).
pub struct Glyphs {
    pub w: usize,
    pub h: usize,
    masks: Vec<Vec<u8>>,
}

impl Glyphs {
    /// Character `c`'s pixels (row by row), when it is printable ASCII.
    pub fn mask(&self, c: char) -> Option<&[u8]> {
        let i = (c as usize).checked_sub(33)?;
        self.masks.get(i).map(Vec::as_slice)
    }
}

/// The glyphs at `scale` (a character `scale` px wide, `2 × scale` tall), made once.
pub fn glyphs(scale: u8) -> Option<&'static Glyphs> {
    static MADE: [std::sync::OnceLock<Option<Glyphs>>; 3] = [const { std::sync::OnceLock::new() }; 3];
    let scale = scale.clamp(1, 3);
    MADE[usize::from(scale) - 1].get_or_init(|| make_glyphs(&font_bytes()?, usize::from(scale), usize::from(scale) * 2)).as_ref()
}

/// The monospace font fontconfig picks (else the common ones).
fn font_bytes() -> Option<Vec<u8>> {
    let picked = std::process::Command::new("fc-match").args(["-f", "%{file}", "monospace"]).stderr(std::process::Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    picked
        .into_iter()
        .chain(["/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".to_string(), "/usr/share/fonts/TTF/DejaVuSansMono.ttf".to_string()])
        .filter(|p| p.ends_with(".ttf") || p.ends_with(".otf"))
        .find_map(|p| std::fs::read(p).ok())
}

fn make_glyphs(font: &[u8], w: usize, h: usize) -> Option<Glyphs> {
    const SS: usize = 4;
    let font = fontdue::Font::from_bytes(font, fontdue::FontSettings::default()).ok()?;
    // The size whose line fills the cell's height, SS times over.
    let unit = font.horizontal_line_metrics(1.0)?;
    let px = (h * SS) as f32 / unit.new_line_size.max(0.1);
    let ascent = unit.ascent * px;
    let (cw, ch) = (w * SS, h * SS);
    let masks = (33u8..=126)
        .map(|c| {
            let (m, bits) = font.rasterize(char::from(c), px);
            let mut big = vec![0u16; cw * ch];
            let top = (ascent - (m.ymin as f32 + m.height as f32)).round() as i64;
            for y in 0..m.height {
                for x in 0..m.width {
                    let (bx, by) = (m.xmin as i64 + x as i64, top + y as i64);
                    if (0..cw as i64).contains(&bx) && (0..ch as i64).contains(&by) {
                        big[by as usize * cw + bx as usize] = u16::from(bits[y * m.width + x]);
                    }
                }
            }
            // Shrunk: each pixel the mean of its SS×SS, lifted (a thin stroke still shows).
            (0..h * w)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let sum: u32 = (0..SS).flat_map(|dy| (0..SS).map(move |dx| (dx, dy))).map(|(dx, dy)| u32::from(big[(y * SS + dy) * cw + x * SS + dx])).sum();
                    ((sum / (SS * SS) as u32) * 2).min(255) as u8
                })
                .collect()
        })
        .collect();
    Some(Glyphs { w, h, masks })
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
    fn glyphs_keep_their_shapes_when_shrunk() {
        let Some(font) = font_bytes() else { return };
        let g = make_glyphs(&font, 2, 4).expect("glyphs");
        let ink = |c| g.mask(c).unwrap().iter().map(|&a| u32::from(a)).sum::<u32>();
        assert!(ink('M') > 2 * ink('.'), "a dot is faint, M solid: {} {}", ink('M'), ink('.'));
        // Lowercase sits low: 'n' has less in its top row than 'N'.
        let top = |c| g.mask(c).unwrap()[..2].iter().map(|&a| u32::from(a)).sum::<u32>();
        assert!(top('n') < top('N'));
        assert!(g.mask(' ').is_none() && g.mask('é').is_none(), "no glyph: drawn otherwise");
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
        // 300 lines, 20 rows of 2 lines: a stretch of 40 lines at a time.
        let m = Mini::new(20, 300, 30, 2, Size::Proportional).follow(0, 0);
        assert_eq!(m.start(0), 0);
        let end = m.follow(0, 299);
        assert_eq!(end.start(299), 260, "at the end the last lines are shown");
        assert_eq!(m.slider(0), (0, 15));
        assert_eq!(end.slider(299), (19, 1), "only the last line is left");
        assert_eq!(m.row_of(0, 7), Some(3));
        // A click centres the clicked line.
        assert_eq!(m.click_scroll(0, 10), 6, "row 10 = lines 20..22, centred: 21 - 15");
        // Dragging the slider to row t: its top lands on t.
        for t in 0..=5 {
            let s = m.drag_scroll(t);
            assert_eq!(m.slider(s).0 as i32, t, "drag to {t}");
        }
        assert_eq!((m.drag_scroll(99), m.follow(0, 299).drag_scroll(99)), (198, 299), "past the bottom: on from the stretch, to the end at most");
        // A short file: no scrolling, a line per slot from the top.
        let short = Mini::new(20, 10, 30, 2, Size::Proportional);
        assert_eq!((short.start(5), short.slider(5)), (0, (3, 3)), "5 lines left at 2 a row");
        // Fill squeezes the whole file into the rows.
        let fill = Mini::new(10, 1000, 30, 2, Size::Fill);
        assert_eq!((fill.start(500), fill.row_of(500, 999)), (0, Some(9)));
    }

    #[test]
    fn slider_tracks_the_viewport_to_the_end_of_a_long_file() {
        // 8000 lines, 47 rows of 4 lines (188 lines a stretch), 47 lines on screen.
        let mut first = 0;
        for scroll in [0, 2000, 4000, 6000, 7000, 7953] {
            let m = Mini::new(47, 8000, 47, 4, Size::Proportional).follow(first, scroll);
            first = m.first;
            let (top, len) = m.slider(scroll);
            let at = m.start(scroll) + top as usize * 4;
            assert!(at.abs_diff(scroll) <= 2, "scroll {scroll}: slider starts at line {at}");
            assert_eq!(len, 12, "47 lines at 4 a row, at any length: easy to grab");
        }
    }

    #[test]
    fn the_stretch_stays_while_the_view_is_in_it_and_moves_at_its_edges() {
        let m = |prev, scroll| Mini::new(47, 8000, 47, 4, Size::Proportional).follow(prev, scroll);
        // Scrolling inside the stretch (lines 1000..1188): it stays.
        assert_eq!([m(1000, 1000).first, m(1000, 1100).first, m(1000, 1141).first], [1000; 3]);
        // Down past its bottom edge: moved just enough; up past its top: the same.
        assert_eq!(m(1000, 1150).first, 1150 + 47 - 188);
        assert_eq!(m(1000, 990).first, 990);
        // A click: the view to the middle of the stretch.
        let c = m(0, 0).centred(5000);
        assert_eq!(c, 5000 + 23 - 94);
        let (top, len) = m(c, 5000).slider(5000);
        assert_eq!(top + len / 2, 24, "the slider's middle about half way down 47 rows");
        assert_eq!(m(0, 0).centred(10), 0, "near the top: from the top");
        // Dragging in a stretch: its rows from where it was; past its top edge, on up.
        let d = m(1000, 1000);
        assert_eq!((d.drag_scroll(10), d.drag_scroll(-5)), (1040, 980));
    }

    #[test]
    fn slider_shrinks_when_the_file_ends_inside_the_viewport() {
        let m = Mini::new(47, 8000, 47, 4, Size::Proportional).follow(7999, 7999);
        assert_eq!(m.slider(7975).1, 6, "25 lines left at 4 a row");
        assert_eq!(m.slider(7999), (46, 1), "only the last line is left");
    }

    #[test]
    fn fill_reaches_the_last_row_at_any_length() {
        for n in [100, 250, 600, 8000] {
            assert_eq!(Mini::new(47, n, 47, 4, Size::Fill).row_of(0, n - 1), Some(46), "{n} lines");
        }
    }

    #[test]
    fn fill_gives_a_file_shorter_than_the_editor_one_row_a_line() {
        let m = Mini::new(47, 20, 47, 4, Size::Fill);
        assert_eq!(m.row_of(0, 19), Some(19));
        assert_eq!(m.slider(0), (0, 20), "the whole file is on screen");
    }

    #[test]
    fn fit_keeps_a_file_that_fits_and_fills_with_a_longer_one() {
        assert_eq!(Mini::new(47, 100, 47, 4, Size::Fit).row_of(0, 99), Some(24), "4 lines a row");
        assert_eq!(Mini::new(47, 250, 47, 4, Size::Fit).row_of(0, 249), Some(46));
    }

    #[test]
    fn the_size_setting_defaults_to_a_fixed_scale() {
        let size = |v: Value| Cfg::parse(v.as_object().unwrap()).size;
        assert_eq!(size(serde_json::json!({})), Size::Proportional);
        assert_eq!(size(serde_json::json!({ "editor.minimap.size": "fill" })), Size::Fill);
        let scale = |v: Value| Cfg::parse(v.as_object().unwrap()).scale;
        assert_eq!((scale(serde_json::json!({})), scale(serde_json::json!({ "editor.minimap.scale": 1 })), scale(serde_json::json!({ "editor.minimap.scale": 9 }))), (3, 1, 3));
        assert_eq!(size(serde_json::json!({ "editor.minimap.size": "proportional" })), Size::Proportional);
        assert_eq!(size(serde_json::json!({ "editor.minimap.size": "fit" })), Size::Fit);
    }
}
