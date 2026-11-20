//! The kit's shapes in pixels where the terminal shows Kitty pictures (Ghostty, kitty):
//! a button's rounded ends, a scrollbar's slider (rounded, placed to the pixel), a
//! checkbox, an icon bigger than the font draws it. The kit draws its cells as ever, then asks for the shape here; once the
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
    /// A vertical slider `len` pixels long, `start` pixels into its cells, in `color`.
    Slider { start: f32, len: f32, color: Rgba },
    /// A checkbox, ticked or not, lit under the pointer.
    Check { on: bool, fill: Color, mark: Color, under: Color },
    /// A Nerd Font icon in `color`, `size` of the picture's height, centred.
    Icon { glyph: char, color: Color, size: f32 },
}

struct Ask {
    at: Rect,
    shape: Shape,
    /// The cells as the kit left them, and the background each takes under the picture.
    cells: Vec<Cell>,
    under: Vec<Color>,
}

thread_local! {
    /// This frame's cell size in pixels, when its shapes go out as pictures.
    static CELL: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
    static ASKS: RefCell<Vec<Ask>> = const { RefCell::new(Vec::new()) };
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
    let at = at.intersection(f.area());
    if cell().is_none() || at.is_empty() {
        return;
    }
    let buf = f.buffer_mut();
    let cells = at.positions().map(|p| buf[p].clone()).collect();
    ASKS.with_borrow_mut(|a| a.push(Ask { at, shape, cells, under }));
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

/// `glyph` (a Nerd Font icon the kit drew in `at`) again in pixels, `size` of `at`'s
/// height: bigger than the font draws it. Other icon sets, and a font without it: text.
pub(crate) fn icon(f: &mut Frame, at: Rect, glyph: &str, color: Color, size: f32) {
    let mut cs = glyph.chars();
    let (Some(c), None) = (cs.next(), cs.next()) else { return };
    if cell().is_none() || !crate::theme::nerd() || !icon_font().is_some_and(|font| font.lookup_glyph_index(c) != 0) {
        return;
    }
    let at = at.intersection(f.area());
    let under = at.positions().map(|p| f.buffer_mut()[p].bg).collect();
    ask(f, at, Shape::Icon { glyph: c, color, size }, under);
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
                out.push(Ask { at: Rect::new(x, y, 1, 1), shape: Shape::Cap { right, fill: c.fg, under: c.bg }, cells: vec![c.clone()], under: vec![c.bg] });
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
                out.push(Ask { at, shape: Shape::Bar { cells: run, filled, fill, track: Rgba(r, g, b, 36) }, cells, under });
            }
            x += run;
        }
    }
    out
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
            Shape::Slider { .. } => {
                let slot = format!("slider:{}", a.at.x);
                let under = a.under.clone();
                crate::media::show_pixels(app, f, &slot, a.at, &make(), &|y| under.get(usize::from(y)).copied().unwrap_or(Color::Reset));
                continue;
            }
            _ => {
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
                Shape::Slider { start, len, color } => {
                    let inset = (cw * 0.15).max(1.0);
                    let r = (cw / 2.0 - inset).max(1.0);
                    ([color.0, color.1, color.2, 255], rbox(fx, fy, (inset, start, w as f32 - inset, start + len), r) * f32::from(color.3) / 255.0)
                }
                Shape::Check { on, fill, mark, .. } => {
                    let s = (w as f32).min(h as f32) * 0.78;
                    let (x0, y0) = ((w as f32 - s) / 2.0, (h as f32 - s) / 2.0);
                    let b = rbox(fx, fy, (x0, y0, x0 + s, y0 + s), s * 0.22);
                    // The tick: two strokes, from the left middle down, then up to the right.
                    let t = s * 0.11;
                    let pts = [(x0 + s * 0.24, y0 + s * 0.52), (x0 + s * 0.43, y0 + s * 0.7), (x0 + s * 0.77, y0 + s * 0.31)];
                    let tick = if on { pts.windows(2).map(|p| (t - dist(fx, fy, p[0], p[1])).clamp(0.0, 1.0)).fold(0.0, f32::max) } else { 0.0 };
                    if tick > 0.0 && b > 0.0 {
                        let (m, f) = (rgb(mark), rgb(fill));
                        let mix = |i: usize| (f32::from(m[i]) * tick + f32::from(f[i]) * (1.0 - tick)).round() as u8;
                        ([mix(0), mix(1), mix(2), 255], b)
                    } else {
                        (rgb(fill), b)
                    }
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
        let s = paint(Shape::Slider { start: 20.0, len: 30.0, color: Rgba(200, 200, 200, 102) }, 8, 64, (8, 16));
        assert_eq!((s.get_pixel(4, 10)[3], s.get_pixel(4, 35)[3], s.get_pixel(4, 60)[3]), (0, 102, 0));
        let on = paint(Shape::Check { on: true, fill, mark: Color::Rgb(255, 255, 255), under }, 24, 16, (8, 16));
        let off = paint(Shape::Check { on: false, fill, mark: Color::Rgb(255, 255, 255), under }, 24, 16, (8, 16));
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
        // Without the font (CI), it stays the font's glyph; here, a picture 0.9 of the row high.
        if icon_font().is_none() {
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
