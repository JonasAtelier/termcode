//! Block boundaries, `herdr-code.blocks.style`: "cards" frames each block (the activity
//! bar, each sidebar section, each editor group) with rounded corners and leaves a 1-cell
//! gutter of workbench background between them; "lines" draws one shared line between
//! blocks, joined with ├ ┤ ┬ ┴ ┼ where they meet; "none" is the flat look. The title and
//! status bars stay flat rows.
//!
//! Frames come from the rects the layout hands out, and every hit rect is recorded from
//! the inside a block was drawn in, so hit-testing follows the frames by construction.

use crate::theme::{icons, P};
use ratatui::prelude::*;
use ratatui::widgets::Block;
use serde_json::Value;
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum BlockStyle {
    #[default]
    Cards,
    Lines,
    None,
}

/// `herdr-code.blocks.style`; unset, compact density draws lines (no room for frames).
pub fn parse(v: Option<&Value>, compact: bool) -> BlockStyle {
    match v.and_then(Value::as_str) {
        Some("cards") => BlockStyle::Cards,
        Some("lines") => BlockStyle::Lines,
        Some("none") => BlockStyle::None,
        _ if compact => BlockStyle::Lines,
        _ => BlockStyle::Cards,
    }
}

/// Blank cells after a tree icon: `icon_gap`, but always the rest of a 2-cell slot and one
/// more, so a non-Mono Nerd Font glyph drawn wider than its cell never touches the name.
pub fn icon_gap(icon_gap: u16) -> u16 {
    icon_gap.max(2)
}

/// Columns a block's frame adds to its content width (cards only).
pub fn frame(st: BlockStyle) -> u16 {
    if st == BlockStyle::Cards {
        2
    } else {
        0
    }
}

/// `activityBar.activeBackground` behind the active item; unset (VS Code's dark default),
/// a subtle lift of the bar's own background.
pub fn active_bg(theme: &serde_json::Map<String, Value>) -> Color {
    match crate::theme::lookup(theme, "activityBar.activeBackground") {
        Some(c) => c.over(P.activity_bg),
        None => {
            let (Color::Rgb(r, g, b), Color::Rgb(fr, fg, fb)) = (P.activity_bg, P.activity_fg) else { return P.activity_bg };
            let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * 0.06).round() as u8;
            Color::Rgb(mix(r, fr), mix(g, fg), mix(b, fb))
        }
    }
}

/// The border colour; a focused block's is a little toward the foreground (~#5A5A5A on
/// vs-dark's #444444), never the bright focus blue.
pub fn border(focused: bool) -> Color {
    if !focused {
        return P.group_border;
    }
    let (Color::Rgb(r, g, b), Color::Rgb(fr, fg, fb)) = (P.group_border, P.editor_fg) else { return P.group_border };
    let mix = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * 0.15).round() as u8;
    Color::Rgb(mix(r, fr), mix(g, fg), mix(b, fb))
}

/// The workbench background the gutters show.
pub fn gutter() -> Color {
    P.editor_bg
}

/// Draws `r` as a card and returns its inside, filled with `bg`. Other styles, and a
/// block too small for a frame (tiny terminals), get `r` back unframed.
pub fn card(st: BlockStyle, f: &mut Frame, r: Rect, bg: Color, focused: bool) -> Rect {
    let r = r.intersection(f.area());
    if st != BlockStyle::Cards || r.width < 4 || r.height < 3 {
        return r;
    }
    let s = icons().border;
    let set = ratatui::symbols::border::Set {
        top_left: s[0],
        top_right: s[1],
        bottom_left: s[2],
        bottom_right: s[3],
        horizontal_top: s[4],
        horizontal_bottom: s[4],
        vertical_left: s[5],
        vertical_right: s[5],
    };
    let block = Block::bordered().border_set(set).border_style(Style::new().fg(border(focused)).bg(gutter())).style(Style::new().bg(bg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    inner
}

/// Two blocks stacked in `top` and `bottom` (adjacent rows): what goes between them per
/// style. Cards: a gutter row (taken from `top`). Lines: a shared line row (from `top`)
/// running from column `x0` to `x1`, the vertical lines either side. None: nothing.
pub fn stack(st: BlockStyle, top: Rect, bottom: Rect, x0: u16, x1: u16) -> (Rect, Rect) {
    if bottom.is_empty() || st == BlockStyle::None || top.height < 2 {
        return (top, bottom);
    }
    let top = Rect { height: top.height - 1, ..top };
    if st == BlockStyle::Lines {
        hline(top.bottom(), x0, x1);
    }
    (top, bottom)
}

thread_local! {
    /// This frame's lines: (x0, y0, x1, y1), vertical or horizontal, inclusive.
    static LINES: RefCell<Vec<(u16, u16, u16, u16)>> = const { RefCell::new(Vec::new()) };
}

/// Starts a frame's lines ("lines" style).
pub fn begin() {
    LINES.with(|l| l.borrow_mut().clear());
}

pub fn vline(x: u16, y0: u16, y1: u16) {
    LINES.with(|l| l.borrow_mut().push((x, y0, x, y1)));
}

pub fn hline(y: u16, x0: u16, x1: u16) {
    LINES.with(|l| l.borrow_mut().push((x0, y, x1, y)));
}

const U: u8 = 1;
const D: u8 = 2;
const L: u8 = 4;
const R: u8 = 8;

/// The box character for the directions a cell's lines leave it in.
fn glyph(m: u8, ascii: bool) -> &'static str {
    let joint = m.count_ones() > 2 || (m & (U | D) != 0 && m & (L | R) != 0);
    match (ascii, m) {
        (true, _) if joint => "+",
        (true, _) if m & (L | R) != 0 => "-",
        (true, _) => "|",
        (_, x) if x == U | D | L | R => "┼",
        (_, x) if x == U | D | R => "├",
        (_, x) if x == U | D | L => "┤",
        (_, x) if x == L | R | D => "┬",
        (_, x) if x == L | R | U => "┴",
        (_, x) if x == D | R => "┌",
        (_, x) if x == D | L => "┐",
        (_, x) if x == U | R => "└",
        (_, x) if x == U | L => "┘",
        (_, x) if x & (L | R) != 0 && x & (U | D) == 0 => "─",
        _ => "│",
    }
}

/// Draws this frame's lines, each cell joined to its neighbours' directions.
pub fn finish(f: &mut Frame) {
    let segs = LINES.with(|l| std::mem::take(&mut *l.borrow_mut()));
    if segs.is_empty() {
        return;
    }
    let mut cells: std::collections::HashMap<(u16, u16), u8> = std::collections::HashMap::new();
    for (x0, y0, x1, y1) in segs {
        if x0 == x1 {
            for y in y0..=y1 {
                *cells.entry((x0, y)).or_default() |= if y > y0 { U } else { 0 } | if y < y1 { D } else { 0 } | if y0 == y1 { U | D } else { 0 };
            }
        } else {
            for x in x0..=x1 {
                *cells.entry((x, y0)).or_default() |= if x > x0 { L } else { 0 } | if x < x1 { R } else { 0 };
            }
        }
    }
    let area = f.area();
    let (ascii, fg) = (crate::theme::ascii(), border(false));
    let buf = f.buffer_mut();
    for ((x, y), m) in cells {
        if x < area.right() && y < area.bottom() {
            // A lone line end keeps its own direction (a line running into a flat bar).
            let m = if m == U || m == D { U | D } else if m == L || m == R { L | R } else { m };
            buf[(x, y)].set_symbol(glyph(m, ascii)).set_fg(fg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junctions_and_styles() {
        assert_eq!(glyph(U | D | R, false), "├");
        assert_eq!(glyph(U | D | L, false), "┤");
        assert_eq!(glyph(L | R | D, false), "┬");
        assert_eq!(glyph(U | D | L | R, false), "┼");
        assert_eq!(glyph(L | R, false), "─");
        assert_eq!(glyph(U | D | R, true), "+");
        assert_eq!(parse(None, false), BlockStyle::Cards);
        assert_eq!(parse(None, true), BlockStyle::Lines, "compact density draws lines");
        assert_eq!(parse(Some(&"none".into()), true), BlockStyle::None);
        assert_eq!((icon_gap(1), icon_gap(2), icon_gap(3)), (2, 2, 3));
        assert_eq!((frame(BlockStyle::Cards), frame(BlockStyle::Lines), frame(BlockStyle::None)), (2, 0, 0));
    }
}
