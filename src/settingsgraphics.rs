//! Small raster controls; Kitty in known terminals, half-blocks elsewhere.

use crate::theme::Pal;
use image::{DynamicImage, Rgba, RgbaImage};
use ratatui::prelude::*;
use ratatui_image::{
    protocol::{halfblocks::Halfblocks, kitty::Kitty, Protocol},
    Image,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Kitty,
    Blocks,
    Text,
}

fn mode(get: impl Fn(&str) -> Option<String>) -> Mode {
    match get("TERMCODE_GRAPHICS").as_deref() {
        Some("kitty") => return Mode::Kitty,
        Some("blocks") => return Mode::Blocks,
        Some("text") => return Mode::Text,
        _ => {}
    }
    // Multiplexers must opt in: an inherited terminal name does not prove passthrough.
    if get("TMUX").is_some() || get("STY").is_some() {
        return Mode::Blocks;
    }
    if matches!(
        get("TERM").as_deref(),
        Some("xterm-kitty" | "xterm-ghostty")
    ) || matches!(get("TERM_PROGRAM").as_deref(), Some("kitty" | "ghostty"))
    {
        Mode::Kitty
    } else {
        Mode::Blocks
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Graphic {
    Switch(bool, [Color; 3]),
    Palette([Color; 8]),
}

struct Cached {
    graphic: Graphic,
    size: (u16, u16),
    protocol: Protocol,
}

pub struct Graphics {
    mode: Mode,
    cell: (u16, u16),
    tmux: bool,
    // Switch states, focused/unfocused, and two palette cards. IDs survive resizing.
    cache: [Option<Cached>; 6],
}

fn cell_size() -> (u16, u16) {
    ratatui::crossterm::terminal::window_size()
        .ok()
        .and_then(|s| {
            (s.columns > 0 && s.rows > 0).then(|| (s.width / s.columns, s.height / s.rows))
        })
        .filter(|&(w, h)| w > 0 && h > 0)
        .map(|(w, h)| (w.min(64), h.min(128)))
        .unwrap_or((8, 16))
}

impl Graphics {
    pub fn new() -> Self {
        let mode = if cfg!(test) {
            Mode::Blocks
        } else {
            mode(|k| std::env::var(k).ok())
        };
        let cell = cell_size();
        Self {
            mode,
            cell,
            tmux: std::env::var_os("TMUX").is_some(),
            cache: std::array::from_fn(|_| None),
        }
    }

    pub fn enabled(&self) -> bool {
        self.mode != Mode::Text
    }

    pub fn pixel_controls(&self) -> bool {
        // A 6x2-cell half-block image has only 6x4 samples: too coarse for a round knob.
        self.mode == Mode::Kitty
    }

    pub fn refresh_size(&mut self) {
        if self.mode == Mode::Kitty {
            let cell = cell_size();
            if cell != self.cell {
                self.cell = cell;
                self.cache = std::array::from_fn(|_| None);
            }
        }
    }

    pub fn switch(
        &mut self,
        f: &mut Frame,
        area: Rect,
        on: bool,
        focused: bool,
        colors: [Color; 3],
    ) {
        self.draw(
            f,
            area,
            usize::from(on) + 2 * usize::from(focused),
            Graphic::Switch(on, colors),
        );
    }

    pub fn palette(&mut self, f: &mut Frame, area: Rect, saved: bool, p: &Pal) {
        self.draw(
            f,
            area,
            4 + usize::from(saved),
            Graphic::Palette([
                p.editor_bg,
                p.editor_fg,
                p.sidebar_bg,
                p.tabs_bg,
                p.status_bg,
                p.focus_border,
                p.selection,
                p.gutter_added,
            ]),
        );
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, slot: usize, graphic: Graphic) {
        if !self.enabled() || area.is_empty() {
            return;
        }
        let size = (area.width, area.height);
        if !self.cache[slot]
            .as_ref()
            .is_some_and(|c| c.size == size && c.graphic == graphic)
        {
            let pixels = raster(
                &graphic,
                u32::from(area.width) * u32::from(self.cell.0),
                u32::from(area.height) * u32::from(self.cell.1),
            );
            let local = Rect::new(0, 0, area.width, area.height);
            let protocol = if self.mode == Mode::Kitty {
                // Stable, process-local IDs keep repeated dialog opens bounded in the terminal.
                let id = 0x48000000 | ((std::process::id() & 0xffff) << 4) | slot as u32;
                Kitty::new(DynamicImage::ImageRgba8(pixels), local, id, self.tmux)
                    .map(Protocol::Kitty)
            } else {
                Halfblocks::new(DynamicImage::ImageRgba8(pixels), local).map(Protocol::Halfblocks)
            };
            let Ok(protocol) = protocol else { return };
            self.cache[slot] = Some(Cached {
                graphic,
                size,
                protocol,
            });
        }
        f.render_widget(
            Image::new(&self.cache[slot].as_ref().unwrap().protocol),
            area,
        );
    }
}

fn pixel(color: Color) -> Rgba<u8> {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (128, 128, 128),
    };
    Rgba([r, g, b, 255])
}

fn raster(graphic: &Graphic, w: u32, h: u32) -> RgbaImage {
    match *graphic {
        Graphic::Switch(on, [bg, track, knob]) => RgbaImage::from_fn(w, h, |x, y| {
            let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
            let r = h as f32 * 0.42;
            let cy = h as f32 / 2.0;
            let cx = if on { w as f32 - r - 1.0 } else { r + 1.0 };
            let track_x = x.clamp(r + 1.0, (w as f32 - r - 1.0).max(r + 1.0));
            let color = if (x - cx).powi(2) + (y - cy).powi(2) < (r * 0.73).powi(2) {
                knob
            } else if (x - track_x).powi(2) + (y - cy).powi(2) < r * r {
                track
            } else {
                bg
            };
            pixel(color)
        }),
        Graphic::Palette([bg, fg, side, tabs, status, accent, selection, added]) => {
            let mut img = RgbaImage::from_pixel(w, h, pixel(bg));
            let mut rect = |x: u32, y: u32, rw: u32, rh: u32, c: Color| {
                for yy in y.min(h)..(y + rh).min(h) {
                    for xx in x.min(w)..(x + rw).min(w) {
                        img.put_pixel(xx, yy, pixel(c));
                    }
                }
            };
            rect(0, 0, w, h / 7, tabs);
            rect(0, h / 7, w / 5, h, side);
            rect(w / 5, 0, w / 3, 2, accent);
            rect(0, h - (h / 9).max(1), w, h / 9 + 1, status);
            rect(w / 4, h / 2, w * 2 / 3, h / 9, selection);
            for (i, len) in [35, 48, 26, 43, 19].into_iter().enumerate() {
                let y = h / 4 + i as u32 * h / 9;
                rect(
                    w / 4,
                    y,
                    w * len / 100,
                    (h / 35).max(1),
                    if i % 2 == 0 { fg } else { accent },
                );
                rect(w / 5 + 2, y, 2, (h / 15).max(1), added);
            }
            img
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::CrosstermBackend, widgets::Block, TerminalOptions, Viewport};

    #[test]
    fn terminal_identity_and_explicit_overrides_choose_safe_graphics() {
        for (vars, want) in [
            (vec![], Mode::Blocks),
            (vec![("TERM", "xterm-kitty")], Mode::Kitty),
            (vec![("TERM_PROGRAM", "ghostty")], Mode::Kitty),
            (vec![("TERM_PROGRAM", "gnome-terminal")], Mode::Blocks),
            (
                vec![("TERM_PROGRAM", "ghostty"), ("TMUX", "socket")],
                Mode::Blocks,
            ),
            (
                vec![("TERM", "xterm-kitty"), ("STY", "screen")],
                Mode::Blocks,
            ),
            (
                vec![("TERM_PROGRAM", "ghostty"), ("TERMCODE_GRAPHICS", "text")],
                Mode::Text,
            ),
            (
                vec![("TERM", "xterm-kitty"), ("TERMCODE_GRAPHICS", "blocks")],
                Mode::Blocks,
            ),
            (
                vec![("TERMCODE_GRAPHICS", "kitty"), ("TMUX", "socket")],
                Mode::Kitty,
            ),
        ] {
            assert_eq!(
                mode(|key| vars
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())),
                want,
                "{vars:?}"
            );
        }
    }

    #[test]
    fn kitty_controls_upload_once_and_reappear_after_clearing_without_reupload() {
        #[derive(Clone, Default)]
        struct Output(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Output::default();
        let mut graphics = Graphics {
            mode: Mode::Kitty,
            cell: (8, 16),
            tmux: false,
            cache: std::array::from_fn(|_| None),
        };
        let mut t = Terminal::with_options(
            CrosstermBackend::new(output.clone()),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 30, 10)),
            },
        )
        .unwrap();
        for x in [1, 10] {
            t.draw(|f| {
                graphics.switch(
                    f,
                    Rect::new(x, 1, 6, 2),
                    true,
                    false,
                    [
                        Color::Rgb(20, 20, 20),
                        Color::Rgb(0, 120, 200),
                        Color::Rgb(255, 255, 255),
                    ],
                )
            })
            .unwrap();
        }
        let out = String::from_utf8(output.0.take()).unwrap();
        assert_eq!(
            out.matches("a=T,U=1").count(),
            1,
            "moving a control must reuse its image"
        );
        assert!(out.contains("\u{10eeee}") && out.contains("q=2,"));
        t.draw(|f| f.render_widget(Block::new(), f.area())).unwrap();
        output.0.borrow_mut().clear();
        t.draw(|f| {
            graphics.switch(
                f,
                Rect::new(1, 1, 6, 2),
                true,
                false,
                [
                    Color::Rgb(20, 20, 20),
                    Color::Rgb(0, 120, 200),
                    Color::Rgb(255, 255, 255),
                ],
            )
        })
        .unwrap();
        let out = String::from_utf8(output.0.take()).unwrap();
        assert!(out.contains("\u{10eeee}"));
        assert!(!out.contains("a=T,U=1"));
    }
}
