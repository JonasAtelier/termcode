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

/// `pixels`: the terminal tells its size in pixels (TIOCGWINSZ).
fn mode(get: impl Fn(&str) -> Option<String>, pixels: bool) -> Mode {
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
    // herdr passes a pane's Kitty pictures on to a terminal that shows them, and then
    // gives the pane its size in pixels (none: it cannot).
    if get("HERDR_ENV").is_some() {
        return if pixels { Mode::Kitty } else { Mode::Blocks };
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
    /// A round picture, by its pixels' size and a sample of them, the background around
    /// it and its ring (what changes it).
    Picture(u32, u32, Vec<u8>, [Color; 2]),
    /// Pixels made by the caller, by a digest of what they show.
    Raw(u64),
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
    /// 24-bit colour (Kitty's placeholder cells carry the image id as their colour).
    truecolor: bool,
    // Switch states, focused/unfocused, and two palette cards. IDs survive resizing.
    // Then the GitHub avatar, its year and month graphs, and a repository's year graph;
    // then a picture or PDF page for each of four editor groups (media.rs).
    cache: [Option<Cached>; 14],
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
            mode(|k| std::env::var(k).ok(), ratatui::crossterm::terminal::window_size().is_ok_and(|s| s.width > 0 && s.height > 0))
        };
        let cell = cell_size();
        Self {
            mode,
            cell,
            tmux: std::env::var_os("TMUX").is_some(),
            truecolor: crate::colors::depth() == crate::colors::Depth::True,
            cache: std::array::from_fn(|_| None),
        }
    }

    #[cfg(test)]
    pub fn set_cell_for_tests(&mut self, cell: (u16, u16)) {
        self.cell = cell;
    }

    /// A Kitty terminal with 8×16 cells, for tests.
    #[cfg(test)]
    pub fn kitty() -> Self {
        Self { mode: Mode::Kitty, cell: (8, 16), tmux: false, truecolor: true, cache: std::array::from_fn(|_| None) }
    }

    pub fn enabled(&self) -> bool {
        self.mode != Mode::Text
    }

    /// A cell's size in pixels.
    pub fn cell(&self) -> (u16, u16) {
        self.cell
    }

    /// `make(w, h)` (the area's pixels) drawn into `area` in slot `slot` (7, 8), made again
    /// only when `key` (a digest of what it shows) or the size changes.
    pub fn raw(&mut self, f: &mut Frame, area: Rect, slot: usize, key: u64, make: impl FnOnce(u32, u32) -> RgbaImage) {
        self.draw_with(f, area, slot, Graphic::Raw(key), make);
    }

    pub fn pixel_controls(&self) -> bool {
        // A 6x2-cell half-block image has only 6x4 samples: too coarse for a round knob.
        self.mode == Mode::Kitty
    }

    pub fn refresh_size(&mut self) {
        // Tests set the cell they draw for.
        if self.mode == Mode::Kitty && !cfg!(test) {
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

    /// `img` scaled into `area` as a circle with a `ring` round it, `bg` in the corners
    /// (the GitHub account's picture); false where pictures do not show (text mode).
    pub fn picture(&mut self, f: &mut Frame, area: Rect, img: &RgbaImage, bg: Color, ring: Color) -> bool {
        if !self.enabled() {
            return false;
        }
        let key = Graphic::Picture(img.width(), img.height(), img.as_raw().iter().step_by(97).copied().collect(), [bg, ring]);
        self.draw_with(f, area, 6, key, |w, h| round(img, w, h, pixel(bg), pixel(ring)));
        true
    }

    fn draw(&mut self, f: &mut Frame, area: Rect, slot: usize, graphic: Graphic) {
        let g = graphic.clone();
        self.draw_with(f, area, slot, graphic, |w, h| raster(&g, w, h));
    }

    fn draw_with(&mut self, f: &mut Frame, area: Rect, slot: usize, graphic: Graphic, pixels: impl FnOnce(u32, u32) -> RgbaImage) {
        if !self.enabled() || area.is_empty() {
            return;
        }
        let size = (area.width, area.height);
        if !self.cache[slot]
            .as_ref()
            .is_some_and(|c| c.size == size && c.graphic == graphic)
        {
            let pixels = pixels(
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
        DRAWN.with(|d| d.borrow_mut().push(area));
    }
}

impl Graphics {
    /// Pictures as tiles the terminal keeps (Kitty's unicode placeholders, which need
    /// 24-bit colour): each sent once, then shown (and scrolled) by placeholder cells.
    pub fn tiles(&self) -> bool {
        self.mode == Mode::Kitty && self.truecolor
    }

    /// `img` sent as image `id` (PNG), shown by placeholder cells over `cols`×`rows` cells.
    pub fn transmit(&self, id: u32, img: &RgbaImage, cols: u16, rows: u16) -> String {
        use image::ImageEncoder;
        let mut png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive);
        if enc.write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8).is_err() {
            return String::new();
        }
        let (start, esc, end) = if self.tmux { ("\x1bPtmux;", "\x1b\x1b", "\x1b\\") } else { ("", "\x1b", "") };
        let data = crate::parse::base64(&png);
        let chunks: Vec<&str> = data.as_bytes().chunks(4096).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
        let mut out = String::from(start);
        for (i, c) in chunks.iter().enumerate() {
            let head = if i == 0 { format!("i={id},a=T,U=1,f=100,t=d,c={cols},r={rows},q=2,") } else { "q=2,".into() };
            let more = u8::from(i + 1 < chunks.len());
            out.push_str(&format!("{esc}_G{head}m={more};{c}{esc}\\"));
        }
        out.push_str(end);
        out
    }

    /// Image `id` gone from the terminal, its pixels with it.
    pub fn delete(&self, id: u32) -> String {
        let (start, esc, end) = if self.tmux { ("\x1bPtmux;", "\x1b\x1b", "\x1b\\") } else { ("", "\x1b", "") };
        format!("{start}{esc}_Ga=d,d=I,i={id},q=2{esc}\\{end}")
    }
}

/// A cell showing cell (`row`, `col`) of image `id`: its text and its colour (the id).
pub fn placeholder(id: u32, row: u16, col: u16) -> (String, Color) {
    let d = |n: u32| DIACRITICS.get(n as usize).copied().unwrap_or(DIACRITICS[0]);
    let [hi, r, g, b] = id.to_be_bytes();
    (format!("\u{10EEEE}{}{}{}", d(u32::from(row)), d(u32::from(col)), d(u32::from(hi))), Color::Rgb(r, g, b))
}

/// Cells holding a graphics escape on purpose (the scrub leaves them).
pub fn mark_drawn(r: Rect) {
    DRAWN.with(|d| d.borrow_mut().push(r));
}

/// Kitty's row and column marks for placeholder cells (its rowcolumn-diacritics.txt, as
/// ratatui-image has them, MIT).
const DIACRITICS: [char; 297] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}', '\u{346}', '\u{34A}',
    '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}', '\u{35B}', '\u{363}', '\u{364}', '\u{365}',
    '\u{366}', '\u{367}', '\u{368}', '\u{369}', '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}',
    '\u{483}', '\u{484}', '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}', '\u{59F}', '\u{5A0}', '\u{5A1}', '\u{5A8}', '\u{5A9}',
    '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}', '\u{610}', '\u{611}', '\u{612}', '\u{613}', '\u{614}', '\u{615}',
    '\u{616}', '\u{617}', '\u{657}', '\u{658}', '\u{659}', '\u{65A}', '\u{65B}', '\u{65D}', '\u{65E}', '\u{6D6}',
    '\u{6D7}', '\u{6D8}', '\u{6D9}', '\u{6DA}', '\u{6DB}', '\u{6DC}', '\u{6DF}', '\u{6E0}', '\u{6E1}', '\u{6E2}',
    '\u{6E4}', '\u{6E7}', '\u{6E8}', '\u{6EB}', '\u{6EC}', '\u{730}', '\u{732}', '\u{733}', '\u{735}', '\u{736}',
    '\u{73A}', '\u{73D}', '\u{73F}', '\u{740}', '\u{741}', '\u{743}', '\u{745}', '\u{747}', '\u{749}', '\u{74A}',
    '\u{7EB}', '\u{7EC}', '\u{7ED}', '\u{7EE}', '\u{7EF}', '\u{7F0}', '\u{7F1}', '\u{7F3}', '\u{816}', '\u{817}',
    '\u{818}', '\u{819}', '\u{81B}', '\u{81C}', '\u{81D}', '\u{81E}', '\u{81F}', '\u{820}', '\u{821}', '\u{822}',
    '\u{823}', '\u{825}', '\u{826}', '\u{827}', '\u{829}', '\u{82A}', '\u{82B}', '\u{82C}', '\u{82D}', '\u{951}',
    '\u{953}', '\u{954}', '\u{F82}', '\u{F83}', '\u{F86}', '\u{F87}', '\u{135D}', '\u{135E}', '\u{135F}', '\u{17DD}',
    '\u{193A}', '\u{1A17}', '\u{1A75}', '\u{1A76}', '\u{1A77}', '\u{1A78}', '\u{1A79}', '\u{1A7A}', '\u{1A7B}', '\u{1A7C}',
    '\u{1B6B}', '\u{1B6D}', '\u{1B6E}', '\u{1B6F}', '\u{1B70}', '\u{1B71}', '\u{1B72}', '\u{1B73}', '\u{1CD0}', '\u{1CD1}',
    '\u{1CD2}', '\u{1CDA}', '\u{1CDB}', '\u{1CE0}', '\u{1DC0}', '\u{1DC1}', '\u{1DC3}', '\u{1DC4}', '\u{1DC5}', '\u{1DC6}',
    '\u{1DC7}', '\u{1DC8}', '\u{1DC9}', '\u{1DCB}', '\u{1DCC}', '\u{1DD1}', '\u{1DD2}', '\u{1DD3}', '\u{1DD4}', '\u{1DD5}',
    '\u{1DD6}', '\u{1DD7}', '\u{1DD8}', '\u{1DD9}', '\u{1DDA}', '\u{1DDB}', '\u{1DDC}', '\u{1DDD}', '\u{1DDE}', '\u{1DDF}',
    '\u{1DE0}', '\u{1DE1}', '\u{1DE2}', '\u{1DE3}', '\u{1DE4}', '\u{1DE5}', '\u{1DE6}', '\u{1DFE}', '\u{20D0}', '\u{20D1}',
    '\u{20D4}', '\u{20D5}', '\u{20D6}', '\u{20D7}', '\u{20DB}', '\u{20DC}', '\u{20E1}', '\u{20E7}', '\u{20E9}', '\u{20F0}',
    '\u{2CEF}', '\u{2CF0}', '\u{2CF1}', '\u{2DE0}', '\u{2DE1}', '\u{2DE2}', '\u{2DE3}', '\u{2DE4}', '\u{2DE5}', '\u{2DE6}',
    '\u{2DE7}', '\u{2DE8}', '\u{2DE9}', '\u{2DEA}', '\u{2DEB}', '\u{2DEC}', '\u{2DED}', '\u{2DEE}', '\u{2DEF}', '\u{2DF0}',
    '\u{2DF1}', '\u{2DF2}', '\u{2DF3}', '\u{2DF4}', '\u{2DF5}', '\u{2DF6}', '\u{2DF7}', '\u{2DF8}', '\u{2DF9}', '\u{2DFA}',
    '\u{2DFB}', '\u{2DFC}', '\u{2DFD}', '\u{2DFE}', '\u{2DFF}', '\u{A66F}', '\u{A67C}', '\u{A67D}', '\u{A6F0}', '\u{A6F1}',
    '\u{A8E0}', '\u{A8E1}', '\u{A8E2}', '\u{A8E3}', '\u{A8E4}', '\u{A8E5}', '\u{A8E6}', '\u{A8E7}', '\u{A8E8}', '\u{A8E9}',
    '\u{A8EA}', '\u{A8EB}', '\u{A8EC}', '\u{A8ED}', '\u{A8EE}', '\u{A8EF}', '\u{A8F0}', '\u{A8F1}', '\u{AAB0}', '\u{AAB2}',
    '\u{AAB3}', '\u{AAB7}', '\u{AAB8}', '\u{AABE}', '\u{AABF}', '\u{AAC1}', '\u{FE20}', '\u{FE21}', '\u{FE22}', '\u{FE23}',
    '\u{FE24}', '\u{FE25}', '\u{FE26}', '\u{10A0F}', '\u{10A38}', '\u{1D185}', '\u{1D186}', '\u{1D187}', '\u{1D188}', '\u{1D189}',
    '\u{1D1AA}', '\u{1D1AB}', '\u{1D1AC}', '\u{1D1AD}', '\u{1D242}', '\u{1D243}', '\u{1D244}',
];

thread_local! {
    /// Where this frame drew pictures: their cells carry the graphics protocol's escape
    /// sequences, which the frame's last pass (ui::draw) must leave alone.
    static DRAWN: std::cell::RefCell<Vec<Rect>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The areas pictures were drawn in since the last call.
pub fn take_drawn() -> Vec<Rect> {
    DRAWN.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

pub(crate) fn pixel(color: Color) -> Rgba<u8> {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (128, 128, 128),
    };
    Rgba([r, g, b, 255])
}

/// `img` in a `w` x `h` image: the largest centred circle, edged by a ring a sixteenth of
/// its size (a pixel at least), `bg` outside it.
pub fn round(img: &RgbaImage, w: u32, h: u32, bg: Rgba<u8>, ring: Rgba<u8>) -> RgbaImage {
    let d = w.min(h).max(1);
    let face = image::imageops::resize(img, d, d, image::imageops::FilterType::Triangle);
    let (ox, oy) = ((w - d) / 2, (h - d) / 2);
    let r = d as f32 / 2.0;
    let edge = (d as f32 / 16.0).max(1.0);
    RgbaImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - (ox as f32 + r), y as f32 + 0.5 - (oy as f32 + r));
        let dist = (dx * dx + dy * dy).sqrt();
        if dist > r {
            bg
        } else if dist > r - edge {
            ring
        } else {
            *face.get_pixel(x - ox, y - oy)
        }
    })
}

fn raster(graphic: &Graphic, w: u32, h: u32) -> RgbaImage {
    match *graphic {
        // Drawn from its own pixels (`picture`), never from here.
        Graphic::Picture(..) | Graphic::Raw(_) => RgbaImage::new(w, h),
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

    #[test]
    fn a_picture_is_cut_round_with_a_ring() {
        let (bg, ring, face) = (Rgba([1, 1, 1, 255]), Rgba([200, 200, 200, 255]), Rgba([255, 0, 0, 255]));
        let out = round(&RgbaImage::from_pixel(8, 8, face), 32, 32, bg, ring);
        assert_eq!((*out.get_pixel(0, 0), *out.get_pixel(31, 31)), (bg, bg), "the corners");
        assert_eq!(*out.get_pixel(16, 0), ring, "the top of the circle");
        assert_eq!(*out.get_pixel(16, 16), face, "the middle");
        let wide = round(&RgbaImage::from_pixel(8, 8, face), 40, 32, bg, ring);
        assert_eq!((*wide.get_pixel(2, 16), *wide.get_pixel(20, 16)), (bg, face), "centred in a wider area");
    }
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
            // herdr: its TERM says nothing; its pane's pixel size says it passes pictures on.
            (vec![("HERDR_ENV", "1"), ("TERM", "xterm-256color"), ("PIXELS", "")], Mode::Kitty),
            (vec![("HERDR_ENV", "1"), ("TERM", "xterm-256color")], Mode::Blocks),
        ] {
            assert_eq!(
                mode(
                    |key| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string()),
                    vars.iter().any(|(k, _)| *k == "PIXELS")
                ),
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
        let mut graphics = Graphics::kitty();
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
