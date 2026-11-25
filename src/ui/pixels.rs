//! The kit's shapes in pixels where the terminal shows Kitty pictures (Ghostty, kitty):
//! a button's rounded ends, a scrollbar's slider (rounded, placed to the pixel), a
//! checkbox, an icon bigger than the font draws it, a popup's rounded frame. The kit draws its cells as ever, then asks for the shape here; once the
//! frame is drawn, `place` puts the picture over the cells that still show what the kit
//! drew (a popup drawn over them since keeps them: it is in front).

use crate::panel::App;
use crate::theme::Rgba;
use image::RgbaImage;
use ratatui::buffer::Cell;
use ratatui::prelude::*;
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Shape {
    /// A filled run's left or right end (`right`), rounded: `fill` over `under`.
    Cap { right: bool, fill: Color, under: Color },
    /// A bar `filled` of `cells` cells full, rounded, in `fill` on a faint track.
    Bar { cells: u16, filled: u16, fill: Color, track: Rgba },
    /// A vertical slider `len` pixels long, `start` pixels into its cells, in `color`, on
    /// the track whose top row is `top`.
    Slider { start: f32, len: f32, color: Rgba, top: u16 },
    /// A checkbox, ticked or not, lit under the pointer; `round`: a radio button (a dot).
    Check { on: bool, fill: Color, mark: Color, under: Color, round: bool },
    /// A value's slider `cells` wide: a thin track, `fill` up to the knob in cell `pos`,
    /// `rest` after it.
    Range { pos: u16, fill: Color, rest: Color, knob: Color },
    /// A cell of a popup's frame: its `line` along the cell's outer edges `sides` (1 left,
    /// 2 top, 4 right, 8 bottom; two make a rounded corner), `fill` inside; `tee`: a line
    /// inside meets the edge there (`├`, `┤` across the cell's middle; `┬`, `┴` down it).
    Frame { sides: u8, line: Color, fill: Color, tee: bool },
    /// A Nerd Font icon in `color`, `size` of the picture's height, centred.
    Icon { glyph: char, color: Color, size: f32 },
}

struct Ask {
    at: Rect,
    shape: Shape,
    /// The cells as the kit left them, and the background each takes under the picture.
    cells: Vec<Cell>,
    under: Vec<Color>,
    /// Overview ruler marks over a slider.
    ticks: Vec<Tick>,
}

/// An overview ruler mark: `h` pixels from `y` down, in `lane` (0: the left part, 1: the
/// right, 2: across), in `color`.
pub(crate) struct Tick {
    pub y: f32,
    pub h: f32,
    pub lane: u8,
    pub color: Rgba,
}

/// `c` as an opaque `Rgba` (grey for a named colour).
pub(crate) fn opaque(c: Color) -> Rgba {
    match c {
        Color::Rgb(r, g, b) => Rgba(r, g, b, 255),
        _ => Rgba(128, 128, 128, 255),
    }
}

thread_local! {
    /// This frame's cell size in pixels, when its shapes go out as pictures.
    static CELL: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
    static ASKS: RefCell<Vec<Ask>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// While Some: each shape placed, its cells and pixels (a test's picture of the screen).
    pub(crate) static SHOTS: RefCell<Option<Vec<(Rect, RgbaImage)>>> = const { RefCell::new(None) };
}

/// A frame starts: `cell` (`media::pixel_cell`), None where pictures do not show.
pub(crate) fn begin(cell: Option<(u16, u16)>) {
    CELL.set(cell);
    ASKS.with_borrow_mut(Vec::clear);
}

/// Shapes go out in pixels this frame: their cell size.
pub(crate) fn cell() -> Option<(u16, u16)> {
    CELL.get()
}

/// `shape` over `at`, whose cells the kit has drawn; `under` the background each cell
/// takes under the picture (its clear pixels show it).
pub(crate) fn ask(f: &mut Frame, at: Rect, shape: Shape, under: Vec<Color>) {
    ask_ticks(f, at, shape, under, Vec::new());
}

/// `ask`, with overview ruler marks over the shape (a slider).
pub(crate) fn ask_ticks(f: &mut Frame, at: Rect, shape: Shape, under: Vec<Color>, ticks: Vec<Tick>) {
    let at = at.intersection(f.area());
    if cell().is_none() || at.is_empty() {
        return;
    }
    let buf = f.buffer_mut();
    let cells = at.positions().map(|p| buf[p].clone()).collect();
    ASKS.with_borrow_mut(|a| a.push(Ask { at, shape, cells, under, ticks }));
}

/// The font with the Nerd Font icons (any patched font has them all), once.
fn icon_font() -> Option<&'static fontdue::Font> {
    static FONT: std::sync::OnceLock<Option<fontdue::Font>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let o = std::process::Command::new("fc-match").args(["-f", "%{file}", ":charset=ea60"]).stderr(std::process::Stdio::null()).output().ok()?;
        let bytes = std::fs::read(String::from_utf8_lossy(&o.stdout).trim()).ok()?;
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok()
    })
    .as_ref()
}

/// Whether `c` can go out as a picture: the icon font has it (fc-match finds some font
/// anywhere, one without the icons where no Nerd Font is installed, as on CI).
pub(crate) fn has_icon(c: char) -> bool {
    icon_font().is_some_and(|font| font.lookup_glyph_index(c) != 0)
}

/// `glyph` (a Nerd Font icon the kit drew in `at`) again in pixels, `size` of `at`'s
/// height: bigger than the font draws it. Other icon sets, and a font without it: text.
pub(crate) fn icon(f: &mut Frame, at: Rect, glyph: &str, color: Color, size: f32) {
    let mut cs = glyph.chars();
    let (Some(c), None) = (cs.next(), cs.next()) else { return };
    if cell().is_none() || !crate::theme::nerd() || !has_icon(c) {
        return;
    }
    let at = at.intersection(f.area());
    let under = at.positions().map(|p| f.buffer_mut()[p].bg).collect();
    ask(f, at, Shape::Icon { glyph: c, color, size }, under);
}

/// `r` (a filled box: a field, a selected row) with rounded corners where pictures show:
/// each corner cell still blank (padding) takes the corner, in its own background, `under`
/// round it. A focus mark there (`▎`) becomes the corner's line in its colour.
pub(crate) fn round_box(f: &mut Frame, r: Rect, under: Color) {
    let r = r.intersection(f.area());
    if cell().is_none() || r.width < 2 {
        return;
    }
    let (x1, y1) = (r.right() - 1, r.bottom() - 1);
    let mut corners = vec![(r.x, r.y), (x1, r.y), (r.x, y1), (x1, y1)];
    corners.sort_unstable();
    corners.dedup();
    for (x, y) in corners {
        let c = f.buffer_mut()[(x, y)].clone();
        let line = match c.symbol() {
            " " => c.bg,
            "▎" | "▏" => c.fg,
            _ => continue,
        };
        if !matches!((c.bg, line), (Color::Rgb(..), Color::Rgb(..))) {
            continue;
        }
        let sides = u8::from(x == r.x) | u8::from(y == r.y) << 1 | u8::from(x == x1) << 2 | u8::from(y == y1) << 3;
        ask(f, Rect::new(x, y, 1, 1), Shape::Frame { sides, line, fill: c.bg, tee: false }, vec![under]);
    }
}

/// The frame's caps and bars as the kit's text draws them, found in its cells: a cap's
/// glyph (a button's, a badge's: `kit::CAP_L`, `CAP_R`) and a bar of `█` then `░` 10
/// (progress), 20 (a part's size) or 24 cells long (a memory region: green, then amber
/// past 75%, red past 90%).
// ponytail: found by their text, not told by the kit (badges are spans placed later);
// a file showing exactly such a run gets a bar too.
fn found(f: &mut Frame) -> Vec<Ask> {
    let buf = f.buffer_mut();
    let area = buf.area;
    let rgb = |c: Color| matches!(c, Color::Rgb(..));
    let mut out = Vec::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let c = &buf[(x, y)];
            let right = c.symbol() == super::kit::CAP_R;
            if (right || c.symbol() == super::kit::CAP_L) && rgb(c.fg) && rgb(c.bg) {
                out.push(Ask { at: Rect::new(x, y, 1, 1), shape: Shape::Cap { right, fill: c.fg, under: c.bg }, cells: vec![c.clone()], under: vec![c.bg], ticks: Vec::new() });
                x += 1;
                continue;
            }
            let run = (x..area.right()).take_while(|&k| matches!(buf[(k, y)].symbol(), "█" | "░")).count() as u16;
            if run == 0 {
                x += 1;
                continue;
            }
            let filled = (x..x + run).take_while(|&k| buf[(k, y)].symbol() == "█").count() as u16;
            let ordered = (x + filled..x + run).all(|k| buf[(k, y)].symbol() == "░");
            if ordered && matches!(run, 10 | 20 | 24) {
                let share = f64::from(filled) / f64::from(run);
                let fill = match run {
                    24 if share > 0.9 => crate::theme::P.error_fg,
                    24 if share > 0.75 => crate::theme::P.git_modified,
                    24 => crate::theme::P.git_added,
                    _ => crate::theme::P.button_bg,
                };
                let Color::Rgb(r, g, b) = super::kit::lit_fg() else { unreachable!() };
                let at = Rect::new(x, y, run, 1);
                let cells: Vec<Cell> = at.positions().map(|p| buf[p].clone()).collect();
                let under = cells.iter().map(|c| c.bg).collect();
                out.push(Ask { at, shape: Shape::Bar { cells: run, filled, fill, track: Rgba(r, g, b, 36) }, cells, under, ticks: Vec::new() });
            }
            x += run;
        }
    }
    frames(buf, &mut out);
    out
}

/// The popups' frames (`kit::popup_block`'s rounded borders): each cell of a whole box
/// (its four corners found) in pixels, the line at the box's outer edge, the corners clear
/// outside so what is next to the popup shows there. Titles in a border stay text.
fn frames(buf: &ratatui::buffer::Buffer, out: &mut Vec<Ask>) {
    let area = buf.area;
    let line = crate::theme::P.widget_border;
    let is = |x: u16, y: u16, s: &str| buf[(x, y)].symbol() == s && buf[(x, y)].fg == line && matches!(buf[(x, y)].bg, Color::Rgb(..));
    for y0 in area.top()..area.bottom() {
        for x0 in area.left()..area.right() {
            if !is(x0, y0, "╭") {
                continue;
            }
            let Some(x1) = (x0 + 1..area.right()).find(|&x| is(x, y0, "╮")) else { continue };
            let Some(y1) = (y0 + 1..area.bottom()).find(|&y| is(x0, y, "╰")) else { continue };
            if !is(x1, y1, "╯") {
                continue;
            }
            // Inside: the frame's own background (a popup's border is drawn on it).
            let fill = buf[(x0, y0)].bg;
            let beside = |x: Option<u16>, y: u16| x.filter(|x| area.left() <= *x && *x < area.right()).map_or(fill, |x| buf[(x, y)].bg);
            let mut put = |x: u16, y: u16, sides: u8, under: Color| {
                let c = buf[(x, y)].clone();
                let tee = !matches!(c.symbol(), "─" | "│" | "╭" | "╮" | "╰" | "╯");
                out.push(Ask { at: Rect::new(x, y, 1, 1), shape: Shape::Frame { sides, line, fill, tee }, cells: vec![c], under: vec![under], ticks: Vec::new() });
            };
            put(x0, y0, 3, beside(x0.checked_sub(1), y0));
            put(x1, y0, 6, beside(x1.checked_add(1), y0));
            put(x0, y1, 9, beside(x0.checked_sub(1), y1));
            put(x1, y1, 12, beside(x1.checked_add(1), y1));
            for x in x0 + 1..x1 {
                for (y, sides, tee) in [(y0, 2, "┬"), (y1, 8, "┴")] {
                    if is(x, y, "─") || is(x, y, tee) {
                        put(x, y, sides, fill);
                    }
                }
            }
            for y in y0 + 1..y1 {
                for (x, sides, tee) in [(x0, 1, "├"), (x1, 4, "┤")] {
                    if is(x, y, "│") || is(x, y, tee) {
                        put(x, y, sides, fill);
                    }
                }
            }
        }
    }
}

/// The frame drawn: each shape asked for over its cells, where they are still the kit's;
/// then the caps and bars its text has (`found`).
pub(crate) fn place(app: &mut App, f: &mut Frame) {
    let mut asks = ASKS.take();
    let Some((cw, ch)) = cell() else { return };
    asks.extend(found(f));
    for a in asks {
        let buf = f.buffer_mut();
        if !a.at.positions().zip(&a.cells).all(|(p, c)| buf[p] == *c) {
            continue;
        }
        let (w, h) = (u32::from(a.at.width) * u32::from(cw), u32::from(a.at.height) * u32::from(ch));
        let make = || paint(a.shape, w, h, (cw, ch));
        let id_seq = match a.shape {
            // A slider moves with every scroll: one picture a scrollbar, swapped.
            // (by its track: two lists can share a column).
            Shape::Slider { top, .. } => {
                let slot = format!("slider:{}:{top}", a.at.x);
                let under = a.under.clone();
                let mut img = make();
                draw_ticks(&mut img, &a.ticks);
                #[cfg(test)]
                SHOTS.with_borrow_mut(|s| s.as_mut().map(|v| v.push((a.at, img.clone()))));
                crate::media::show_pixels(app, f, &slot, a.at, &img, &|y| under.get(usize::from(y)).copied().unwrap_or(Color::Reset));
                continue;
            }
            _ => {
                #[cfg(test)]
                SHOTS.with_borrow_mut(|s| s.as_mut().map(|v| v.push((a.at, make()))));
                let mut hk = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(&(format!("{:?}", a.shape), a.at.width, a.at.height, cw, ch), &mut hk);
                crate::media::kept(app, std::hash::Hasher::finish(&hk), (a.at.width, a.at.height), |_| Some(make()))
            }
        };
        let Some((id, seq)) = id_seq else { continue };
        let buf = f.buffer_mut();
        for (i, p) in a.at.positions().enumerate() {
            let (sym, fg) = crate::settingsgraphics::placeholder(id, p.y - a.at.y, p.x - a.at.x);
            let sym = if i == 0 { format!("{seq}{sym}") } else { sym };
            buf[p].set_symbol(&sym).set_fg(fg).set_bg(a.under.get(i).copied().unwrap_or(Color::Reset));
        }
        crate::settingsgraphics::mark_drawn(a.at);
    }
}

/// `shape` in a `w`×`h` picture of cells `cell` pixels, anti-aliased, clear around it.
pub(crate) fn paint(shape: Shape, w: u32, h: u32, (cw, ch): (u16, u16)) -> RgbaImage {
    if let Shape::Icon { glyph, color, size } = shape {
        return icon_image(glyph, color, w, h, size);
    }
    let (cw, ch) = (f32::from(cw), f32::from(ch));
    let mut img = RgbaImage::new(w, h);
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => [r, g, b, 255],
        _ => [128, 128, 128, 255],
    };
    // How much of the pixel at (x, y) a rounded box covers: the box from (x0, y0) to (x1,
    // y1), its corners `r` round.
    let rbox = |x: f32, y: f32, (x0, y0, x1, y1): (f32, f32, f32, f32), r: f32| {
        let (qx, qy) = ((x0 + r - x).max(x - (x1 - r)).max(0.0), (y0 + r - y).max(y - (y1 - r)).max(0.0));
        let outside = (qx * qx + qy * qy).sqrt() - r;
        let edge = (x0 - x).max(x - x1).max(y0 - y).max(y - y1);
        (0.5 - outside.max(edge)).clamp(0.0, 1.0)
    };
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (color, a) = match shape {
                Shape::Cap { right, fill, .. } => {
                    // The button's box goes on past the cell: only its rounded end shows.
                    let r = (ch * 0.36).min(cw);
                    let b = if right { (-cw, 0.0, w as f32, h as f32) } else { (0.0, 0.0, w as f32 + cw, h as f32) };
                    (rgb(fill), rbox(fx, fy, b, r))
                }
                Shape::Bar { cells, filled, fill, track } => {
                    // A rounded bar across the cells' middle; its full part over the track.
                    let th = (ch * 0.42).round();
                    let (y0, x1) = (((h as f32 - th) / 2.0).round(), w as f32 - 1.0);
                    let edge = (1.0, y0, x1, y0 + th);
                    let full = (1.0, y0, 1.0 + (x1 - 1.0) * f32::from(filled) / f32::from(cells.max(1)), y0 + th);
                    let on = if filled > 0 { rbox(fx, fy, full, th / 2.0) } else { 0.0 };
                    if on > 0.0 {
                        (rgb(fill), on.max(rbox(fx, fy, edge, th / 2.0) * f32::from(track.3) / 255.0))
                    } else {
                        ([track.0, track.1, track.2, 255], rbox(fx, fy, edge, th / 2.0) * f32::from(track.3) / 255.0)
                    }
                }
                Shape::Slider { start, len, color, .. } => {
                    let inset = (cw * 0.15).max(1.0);
                    let r = (cw / 2.0 - inset).max(1.0);
                    ([color.0, color.1, color.2, 255], rbox(fx, fy, (inset, start, w as f32 - inset, start + len), r) * f32::from(color.3) / 255.0)
                }
                Shape::Range { pos, fill, rest, knob } => {
                    let (th, yc) = ((ch * 0.16).max(2.0), h as f32 / 2.0);
                    let kx = (f32::from(pos) + 0.5) * cw;
                    let track = rbox(fx, fy, (cw * 0.3, yc - th / 2.0, w as f32 - cw * 0.3, yc + th / 2.0), th / 2.0);
                    let k = (ch * 0.26 + 0.5 - ((fx - kx).powi(2) + (fy - yc).powi(2)).sqrt()).clamp(0.0, 1.0);
                    if k > 0.0 {
                        let (a, b) = (rgb(knob), rgb(if fx < kx { fill } else { rest }));
                        let mix = |i: usize| (f32::from(a[i]) * k + f32::from(b[i]) * track * (1.0 - k)).round() as u8;
                        let alpha = k + track * (1.0 - k);
                        let unmix = |i: usize| (f32::from(mix(i)) / alpha.max(1e-6)).round().min(255.0) as u8;
                        ([unmix(0), unmix(1), unmix(2), 255], alpha)
                    } else {
                        (rgb(if fx < kx { fill } else { rest }), track)
                    }
                }
                Shape::Check { on, fill, mark, round, .. } => {
                    let s = (w as f32).min(h as f32) * 0.78;
                    let (x0, y0) = ((w as f32 - s) / 2.0, (h as f32 - s) / 2.0);
                    let b = rbox(fx, fy, (x0, y0, x0 + s, y0 + s), if round { s / 2.0 } else { s * 0.22 });
                    // An empty radio button: its ring in `mark`.
                    if round && !on {
                        let inner = rbox(fx, fy, (x0 + 1.0, y0 + 1.0, x0 + s - 1.0, y0 + s - 1.0), s / 2.0 - 1.0);
                        let (m, f) = (rgb(mark), rgb(fill));
                        let mix = |i: usize| ((f32::from(m[i]) * (b - inner) + f32::from(f[i]) * inner) / b.max(1e-6)).round() as u8;
                        img.put_pixel(x, y, image::Rgba([mix(0), mix(1), mix(2), (b * 255.0).round() as u8]));
                        continue;
                    }
                    // The tick: two strokes, from the left middle down, then up to the right.
                    let t = s * 0.11;
                    let pts = [(x0 + s * 0.24, y0 + s * 0.52), (x0 + s * 0.43, y0 + s * 0.7), (x0 + s * 0.77, y0 + s * 0.31)];
                    let tick = match (on, round) {
                        (false, _) => 0.0,
                        // A radio button's dot.
                        (true, true) => (s * 0.2 + 0.5 - ((fx - x0 - s / 2.0).powi(2) + (fy - y0 - s / 2.0).powi(2)).sqrt()).clamp(0.0, 1.0),
                        (true, false) => pts.windows(2).map(|p| (t - dist(fx, fy, p[0], p[1])).clamp(0.0, 1.0)).fold(0.0, f32::max),
                    };
                    if tick > 0.0 && b > 0.0 {
                        let (m, f) = (rgb(mark), rgb(fill));
                        let mix = |i: usize| (f32::from(m[i]) * tick + f32::from(f[i]) * (1.0 - tick)).round() as u8;
                        ([mix(0), mix(1), mix(2), 255], b)
                    } else {
                        (rgb(fill), b)
                    }
                }
                Shape::Frame { sides, line, fill, tee } => {
                    // The popup's box runs on past this cell where it has no edge.
                    let (big, wf, hf) = (1e4, w as f32, h as f32);
                    let b = (
                        if sides & 1 != 0 { 0.0 } else { -big },
                        if sides & 2 != 0 { 0.0 } else { -big },
                        if sides & 4 != 0 { wf } else { wf + big },
                        if sides & 8 != 0 { hf } else { hf + big },
                    );
                    let r = (cw * 0.7).min(ch * 0.5);
                    let t = (ch / 20.0).round().max(1.0);
                    let outer = rbox(fx, fy, b, r);
                    let mut inner = rbox(fx, fy, (b.0 + t, b.1 + t, b.2 - t, b.3 - t), (r - t).max(0.0));
                    if tee {
                        // The inner line from the edge on, through the cell's middle.
                        let across = if sides & 5 != 0 { (fy - hf / 2.0).abs() } else { (fx - wf / 2.0).abs() };
                        inner = inner.min((across + 0.5 - t / 2.0).clamp(0.0, 1.0));
                    }
                    let (l, f) = (rgb(line), rgb(fill));
                    let mix = |i: usize| ((f32::from(l[i]) * (outer - inner) + f32::from(f[i]) * inner) / outer.max(1e-6)).round() as u8;
                    ([mix(0), mix(1), mix(2), 255], outer)
                }
                Shape::Icon { .. } => unreachable!("drawn by icon_image"),
            };
            if a > 0.0 {
                img.put_pixel(x, y, image::Rgba([color[0], color[1], color[2], (a * 255.0).round() as u8]));
            }
        }
    }
    img
}

/// `glyph` in `color`, as big as fits `size` × `h` high (and `w` wide), centred.
fn icon_image(glyph: char, color: Color, w: u32, h: u32, size: f32) -> RgbaImage {
    let mut img = RgbaImage::new(w, h);
    let Some(font) = icon_font() else { return img };
    let Color::Rgb(r, g, b) = color else { return img };
    let m = font.metrics(glyph, 100.0);
    let (bw, bh) = ((w as f32 * 0.95).min(size * h as f32), size * h as f32);
    let px = 100.0 * (bw / m.width.max(1) as f32).min(bh / m.height.max(1) as f32);
    let (m, cover) = font.rasterize(glyph, px);
    let (x0, y0) = ((i64::from(w) - m.width as i64) / 2, (i64::from(h) - m.height as i64) / 2);
    for (i, &a) in cover.iter().enumerate().filter(|(_, a)| **a > 0) {
        let (x, y) = (x0 + (i % m.width) as i64, y0 + (i / m.width) as i64);
        if (0..i64::from(w)).contains(&x) && (0..i64::from(h)).contains(&y) {
            img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, a]));
        }
    }
    img
}

/// The marks over `img` (a scrollbar's), each over what is under it.
fn draw_ticks(img: &mut RgbaImage, ticks: &[Tick]) {
    let (w, h) = (img.width(), img.height());
    for t in ticks {
        let (x0, x1) = match t.lane {
            0 => (0, w * 2 / 5),
            1 => (w - w * 2 / 5, w),
            _ => (0, w),
        };
        let y0 = (t.y.round().max(0.0) as u32).min(h.saturating_sub(1));
        let y1 = ((t.y + t.h).round() as u32).clamp(y0 + 1, h);
        let a = f32::from(t.color.3) / 255.0;
        for y in y0..y1 {
            for x in x0..x1 {
                let d = img.get_pixel(x, y).0;
                let da = f32::from(d[3]) / 255.0;
                let oa = a + da * (1.0 - a);
                let mix = |s: u8, d: u8| ((f32::from(s) * a + f32::from(d) * da * (1.0 - a)) / oa.max(1e-6)).round() as u8;
                img.put_pixel(x, y, image::Rgba([mix(t.color.0, d[0]), mix(t.color.1, d[1]), mix(t.color.2, d[2]), (oa * 255.0).round() as u8]));
            }
        }
    }
}

/// From (`x`, `y`) to the segment `a`–`b`.
fn dist(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let u = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
    ((x - a.0 - u * dx).powi(2) + (y - a.1 - u * dy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_round_off_a_box_sliders_sit_to_the_pixel_and_a_tick_shows() {
        let fill = Color::Rgb(0, 120, 212);
        let under = Color::Rgb(30, 30, 30);
        let cap = paint(Shape::Cap { right: false, fill, under }, 8, 16, (8, 16));
        assert_eq!(cap.get_pixel(0, 0)[3], 0, "the corner is cut");
        assert_eq!(*cap.get_pixel(7, 8), image::Rgba([0, 120, 212, 255]), "the inside is the button's");
        assert_eq!(cap.get_pixel(0, 8)[3], 255, "a rounded corner, not a half circle: the middle of the edge is filled");
        let right = paint(Shape::Cap { right: true, fill, under }, 8, 16, (8, 16));
        assert_eq!((right.get_pixel(7, 0)[3], right.get_pixel(0, 0)[3]), (0, 255), "mirrored");
        // A slider from 20 px down for 30 px: clear above and below, its colour's alpha within.
        let s = paint(Shape::Slider { start: 20.0, len: 30.0, color: Rgba(200, 200, 200, 102), top: 0 }, 8, 64, (8, 16));
        assert_eq!((s.get_pixel(4, 10)[3], s.get_pixel(4, 35)[3], s.get_pixel(4, 60)[3]), (0, 102, 0));
        let on = paint(Shape::Check { on: true, fill, mark: Color::Rgb(255, 255, 255), under, round: false }, 24, 16, (8, 16));
        let off = paint(Shape::Check { on: false, fill, mark: Color::Rgb(255, 255, 255), under, round: false }, 24, 16, (8, 16));
        let white = |i: &RgbaImage| i.pixels().filter(|p| p[0] > 200 && p[1] > 200 && p[3] > 200).count();
        assert!(white(&on) > 6 && white(&off) == 0, "the tick only when ticked");
    }

    #[test]
    fn shapes_go_over_the_kits_cells_but_never_over_what_was_drawn_on_them() {
        let d = std::env::temp_dir().join(format!("termcode-pixels-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.clone(), None);
        app.media.kitty_for_tests();
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(20, 6)).unwrap();
        let mut out = None;
        t.draw(|f| {
            begin(Some((8, 16)));
            let under = Color::Rgb(30, 30, 30);
            crate::ui::kit::button(f, Rect::new(0, 0, 8, 1), "Save", crate::ui::kit::Look::Primary, under, false);
            crate::ui::kit::button(f, Rect::new(0, 1, 8, 1), "Undo", crate::ui::kit::Look::Primary, under, false);
            crate::ui::kit::slider(f, Rect::new(19, 0, 1, 6), 100, 20, 50);
            // A popup drawn later over the second button's right end.
            f.buffer_mut()[(7, 1)].set_symbol("X");
            place(&mut app, f);
            out = Some(f.buffer_mut().clone());
        })
        .unwrap();
        let b = out.take().unwrap();
        let pic = |x: u16, y: u16| b[(x, y)].symbol().contains('\u{10EEEE}');
        assert!(pic(0, 0) && pic(7, 0) && pic(0, 1), "the buttons' ends, in pixels");
        assert_eq!(b[(7, 1)].symbol(), "X", "what is in front stays");
        assert!(!pic(3, 0), "the label stays text");
        assert_eq!(b[(0, 0)].bg, Color::Rgb(30, 30, 30), "round the end, the surface behind");
        assert!((0..6).any(|y| pic(19, y)) && !(0..6).all(|y| pic(19, y)), "the slider, in pixels, only where it is");
        // Text the kit drew as spans (a badge), and bars written as text: found in the frame.
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(40, 4)).unwrap();
        t.draw(|f| {
            begin(Some((8, 16)));
            let under = Color::Rgb(30, 30, 30);
            f.render_widget(Line::from(crate::ui::kit::badge(3, under)), Rect::new(0, 0, 5, 1));
            f.render_widget(Line::raw("80% ████████░░ done"), Rect::new(0, 1, 40, 1));
            f.render_widget(Line::raw("FLASH ██████████████████████░░ 92%"), Rect::new(0, 2, 40, 1));
            f.render_widget(Line::raw("art ██░ not a bar"), Rect::new(0, 3, 40, 1));
            place(&mut app, f);
            out = Some(f.buffer_mut().clone());
        })
        .unwrap();
        let b = out.unwrap();
        let pic = |x: u16, y: u16| b[(x, y)].symbol().contains('\u{10EEEE}');
        assert!(pic(0, 0) && b[(1, 0)].symbol() == "3" && pic(2, 0), "the badge's caps in pixels, its count text");
        assert!((4..14).all(|x| pic(x, 1)), "a progress bar of 10 cells");
        assert!((6..30).all(|x| pic(x, 2)), "a memory region's bar of 24 cells");
        assert!(!pic(4, 3) && b[(4, 3)].symbol() == "█", "a short run stays text");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_scrollbars_marks_go_out_in_its_picture_over_the_whole_track() {
        let d = std::env::temp_dir().join(format!("termcode-pixels-ruler-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.clone(), None);
        app.media.kitty_for_tests();
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(4, 6)).unwrap();
        let mut out = None;
        let red = Rgba(255, 0, 0, 255);
        t.draw(|f| {
            begin(Some((8, 16)));
            let track = Rect::new(3, 0, 1, 6);
            crate::ui::kit::ruler_px(f, track, (100, 20, 0, 80), Rgba(200, 200, 200, 102), &[(0, 0, red), (90, 1, red)], vec![Color::Rgb(30, 30, 30); 6]);
            place(&mut app, f);
            out = Some(f.buffer_mut().clone());
        })
        .unwrap();
        let b = out.unwrap();
        assert!((0..6).all(|y| b[(3, y)].symbol().contains('\u{10EEEE}')), "the whole track, not the slider's cells only");
        // The marks: line 0 at the top in the left lane, line 90 near the bottom in the right.
        let mut img = paint(Shape::Slider { start: 0.0, len: 20.0, color: Rgba(200, 200, 200, 102), top: 0 }, 10, 96, (10, 16));
        draw_ticks(&mut img, &[Tick { y: 0.0, h: 3.0, lane: 0, color: red }, Tick { y: 86.0, h: 3.0, lane: 1, color: red }]);
        assert_eq!(*img.get_pixel(1, 1), image::Rgba([255, 0, 0, 255]));
        assert_eq!(img.get_pixel(7, 10)[0], 200, "the right lane by the first mark is the slider's");
        assert_eq!(*img.get_pixel(8, 87), image::Rgba([255, 0, 0, 255]));
        assert_eq!(img.get_pixel(1, 87)[3], 0, "clear elsewhere");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_popups_frame_goes_out_in_pixels_its_title_stays_text_its_corners_show_whats_beside() {
        let d = std::env::temp_dir().join(format!("termcode-pixels-frame-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.clone(), None);
        app.media.kitty_for_tests();
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(16, 6)).unwrap();
        let mut out = None;
        let (behind, bg) = (Color::Rgb(30, 30, 30), Color::Rgb(37, 37, 38));
        t.draw(|f| {
            begin(Some((8, 16)));
            crate::ui::fill(f, f.area(), behind);
            let r = Rect::new(2, 1, 12, 4);
            f.render_widget(ratatui::widgets::Clear, r);
            f.render_widget(crate::ui::popup_block(bg, Color::Rgb(200, 200, 200), (0, 0)).title(" Find "), r);
            place(&mut app, f);
            out = Some(f.buffer_mut().clone());
        })
        .unwrap();
        let b = out.unwrap();
        let pic = |x: u16, y: u16| b[(x, y)].symbol().contains('\u{10EEEE}');
        assert!([(2, 1), (13, 1), (2, 4), (13, 4), (10, 1), (5, 4), (2, 2), (13, 3)].iter().all(|&(x, y)| pic(x, y)), "corners and edges");
        assert_eq!(b[(4, 1)].symbol(), "F", "the title stays text");
        assert_eq!((b[(2, 1)].bg, b[(5, 4)].bg), (behind, bg), "a corner shows what is beside the popup; an edge is the popup's");
        assert!(!pic(5, 2), "inside stays text");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn fields_round_their_blank_corners_and_sliders_and_radio_buttons_paint() {
        let d = std::env::temp_dir().join(format!("termcode-pixels-round-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.clone(), None);
        app.media.kitty_for_tests();
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(12, 3)).unwrap();
        let mut out = None;
        let (under, field, mark) = (Color::Rgb(30, 30, 30), Color::Rgb(60, 60, 60), Color::Rgb(0, 120, 212));
        t.draw(|f| {
            begin(Some((8, 16)));
            crate::ui::fill(f, f.area(), under);
            f.render_widget(Line::styled(" abc ", Style::new().bg(field)), Rect::new(0, 0, 5, 1));
            f.render_widget(Line::styled(" abcde", Style::new().bg(field)), Rect::new(0, 1, 6, 1));
            f.render_widget(Line::styled("▎ab ", Style::new().fg(mark).bg(field)), Rect::new(0, 2, 4, 1));
            for y in 0..3 {
                round_box(f, Rect::new(0, y, if y == 1 { 6 } else { 5 - y }, 1), under);
            }
            place(&mut app, f);
            out = Some(f.buffer_mut().clone());
        })
        .unwrap();
        let b = out.unwrap();
        let pic = |x: u16, y: u16| b[(x, y)].symbol().contains('\u{10EEEE}');
        assert!(pic(0, 0) && pic(4, 0) && b[(0, 0)].bg == under, "both blank ends, round on what is behind");
        assert!(pic(0, 1) && b[(5, 1)].symbol() == "e", "text at an end stays");
        assert!(pic(0, 2), "the focus mark becomes the corner's line");
        // A value's slider: the knob in its cell, the fill before it, the rest after.
        let (fill, rest, knob) = (Color::Rgb(0, 120, 212), Color::Rgb(80, 80, 80), Color::Rgb(220, 220, 220));
        let s = paint(Shape::Range { pos: 2, fill, rest, knob }, 64, 16, (8, 16));
        assert_eq!((*s.get_pixel(20, 8), *s.get_pixel(12, 8), *s.get_pixel(50, 8)), (image::Rgba([220, 220, 220, 255]), image::Rgba([0, 120, 212, 255]), image::Rgba([80, 80, 80, 255])));
        assert_eq!(s.get_pixel(50, 2)[3], 0, "a thin track");
        // A radio button: a dot when chosen, a ring when not.
        let on = paint(Shape::Check { on: true, fill, mark: knob, under, round: true }, 24, 16, (8, 16));
        let off = paint(Shape::Check { on: false, fill: rest, mark: knob, under, round: true }, 24, 16, (8, 16));
        assert_eq!((on.get_pixel(12, 8)[0], off.get_pixel(12, 8)[0]), (220, 80));
        assert!(off.get_pixel(12, 2)[0] > 150, "the ring, at the edge");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_icon_goes_out_bigger_than_its_glyph_where_the_icon_font_is_else_stays_text() {
        let d = std::env::temp_dir().join(format!("termcode-pixels-icon-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let mut app = App::new(d.clone(), None);
        app.media.kitty_for_tests();
        let glyph = crate::theme::icons().settings;
        let mut t = Terminal::new(ratatui::backend::TestBackend::new(6, 2)).unwrap();
        let mut out = None;
        t.draw(|f| {
            begin(Some((8, 16)));
            f.render_widget(Span::raw(glyph), Rect::new(0, 0, 2, 1));
            icon(f, Rect::new(0, 0, 2, 1), glyph, Color::Rgb(200, 200, 200), 0.9);
            icon(f, Rect::new(3, 0, 2, 1), "ab", Color::Rgb(200, 200, 200), 0.9);
            place(&mut app, f);
            out = Some(f.buffer_mut().clone());
        })
        .unwrap();
        let b = out.unwrap();
        let pic = |x: u16| b[(x, 0)].symbol().contains('\u{10EEEE}');
        assert!(!pic(3), "not one glyph: text");
        // Without the icon font (CI), it stays the font's glyph; here, a picture 0.9 of the row high.
        if !has_icon(glyph.chars().next().unwrap()) {
            assert_eq!(b[(0, 0)].symbol(), glyph);
        } else {
            assert!(pic(0) && pic(1), "the icon's two cells, in pixels");
            let img = icon_image(glyph.chars().next().unwrap(), Color::Rgb(200, 200, 200), 16, 16, 0.9);
            let rows: Vec<u32> = (0..16).filter(|&y| (0..16).any(|x| img.get_pixel(x, y)[3] > 0)).collect();
            assert!(rows.len() >= 12, "as tall as asked: {rows:?}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }
}
