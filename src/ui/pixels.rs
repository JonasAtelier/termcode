//! The kit's shapes in pixels where the terminal shows Kitty pictures (Ghostty, kitty):
//! a button's rounded ends, a scrollbar's slider (rounded, placed to the pixel), a
//! checkbox. The kit draws its cells as ever, then asks for the shape here; once the
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
    /// A vertical slider `len` pixels long, `start` pixels into its cells, in `color`.
    Slider { start: f32, len: f32, color: Rgba },
    /// A checkbox, ticked or not, lit under the pointer.
    Check { on: bool, fill: Color, mark: Color, under: Color },
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

/// The frame drawn: each shape asked for over its cells, where they are still the kit's.
pub(crate) fn place(app: &mut App, f: &mut Frame) {
    let asks = ASKS.take();
    let Some((cw, ch)) = cell() else { return };
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
                    let r = (ch * 0.3).min(cw);
                    let b = if right { (-cw, 0.0, w as f32, h as f32) } else { (0.0, 0.0, w as f32 + cw, h as f32) };
                    (rgb(fill), rbox(fx, fy, b, r))
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
            };
            if a > 0.0 {
                img.put_pixel(x, y, image::Rgba([color[0], color[1], color[2], (a * 255.0).round() as u8]));
            }
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
        let b = out.unwrap();
        let pic = |x: u16, y: u16| b[(x, y)].symbol().contains('\u{10EEEE}');
        assert!(pic(0, 0) && pic(7, 0) && pic(0, 1), "the buttons' ends, in pixels");
        assert_eq!(b[(7, 1)].symbol(), "X", "what is in front stays");
        assert!(!pic(3, 0), "the label stays text");
        assert_eq!(b[(0, 0)].bg, Color::Rgb(30, 30, 30), "round the end, the surface behind");
        assert!((0..6).any(|y| pic(19, y)) && !(0..6).all(|y| pic(19, y)), "the slider, in pixels, only where it is");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
