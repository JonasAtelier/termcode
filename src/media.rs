//! Pictures and PDFs in an editor tab. A picture shows fitted to the tab or zoomed (+ and
//! -, 0 to fit, 1 for actual size; the arrows pan). A PDF reads as a document: its pages
//! one under another (drawn by poppler's pdftoppm as they come into view), a scrollbar,
//! the wheel and the keys to move through it, two side by side while whole pages fit; its
//! text selected with the mouse (the words' places from pdftotext -bbox-layout) and copied
//! with Ctrl+C; its table of contents (pdftohtml) in the Outline. The banner's buttons show
//! the file's bytes as a hexdump instead, a PDF also as its text (pdftotext). Pictures go
//! out by the Kitty graphics protocol where the terminal has it, as half-blocks elsewhere
//! (settingsgraphics).

use crate::panel::{App, Doc, Drag, Focus, ScrollTarget};
use crate::symbols::Symbol;
use crate::theme::P;
use crate::ui::{fill, fit, row};
use image::{imageops, Rgba, RgbaImage};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::Frame;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use unicode_width::UnicodeWidthStr;

/// A picture is kept at most this long on its long side (a 24-megapixel photo would take
/// ~100 MB decoded); a pane is narrower anyway.
const MAX_SIDE: u32 = 2560;
/// Rendered PDF pages kept per file.
const PAGES_KEPT: usize = 6;
/// A PDF page is drawn at most this big (pixels): its zoom stops there.
const MAX_PAGE_PX: f32 = 8.0e6;
/// A tile the terminal keeps, in cells; and how many a file keeps there.
const TILE: (u16, u16) = (64, 16);
const TILES_KEPT: usize = 48;
/// The tools' time to answer.
const WAIT: Duration = Duration::from_secs(20);
/// Between a PDF's pages, in points.
const GAP: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Image,
    Pdf,
}

/// What a file is shown as, when it is one of these.
pub fn kind(p: &Path) -> Option<Kind> {
    let ext = p.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" => Some(Kind::Image),
        "pdf" => Some(Kind::Pdf),
        _ => None,
    }
}

/// How a file is shown (the banner's buttons): as it is meant to be (a picture, a PDF's
/// pages, readelf's report), as text, as a hexdump.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum As {
    Main,
    Text,
    Hex,
    /// An ELF file's size (size.rs).
    Size,
}

/// The banner's buttons for a file: (label, how).
pub fn buttons(path: &Path, elf: bool) -> Vec<(&'static str, As)> {
    match kind(path) {
        Some(Kind::Image) => vec![("picture", As::Main), ("hexdump", As::Hex)],
        Some(Kind::Pdf) => vec![("pages", As::Main), ("text", As::Text), ("hexdump", As::Hex)],
        None if elf => vec![("size", As::Size), ("readelf", As::Main), ("hexdump", As::Hex)],
        None => vec![("text", As::Text), ("hexdump", As::Hex)],
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    ZoomIn,
    ZoomOut,
    Fit,
    Actual,
    Prev,
    Next,
    Two,
}

/// A word on a PDF page: its line's number on the page, its box in points, its text.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    line: u32,
    b: [f32; 4],
    text: String,
}

enum Msg {
    Image(String, Result<(RgbaImage, (u32, u32)), String>),
    Info(String, Info),
    Page(String, usize, u32, Result<RgbaImage, String>),
    Words(String, usize, Result<Vec<Word>, String>),
    AllWords(String, Result<Vec<Vec<Word>>, String>),
    Toc(String, Vec<Symbol>),
}

/// A picture's pixels and its own size; a PDF's pages' sizes (points); a page drawn, by
/// (page, dpi); a page's words.
type Picture = Result<(Arc<RgbaImage>, (u32, u32)), String>;
type Info = Result<Vec<(f32, f32)>, String>;
type Drawn = ((usize, u32), Result<Arc<RgbaImage>, String>);
type Words = Result<Vec<Word>, String>;
/// A place in a PDF's text: (page, word).
type At = (usize, usize);
/// A page to draw, with its drawing so far (and that drawing's pixels a point).
type Shown = (usize, Option<Arc<RgbaImage>>);
/// A page with its drawing so far (and that drawing's pixels a point).
type Source = (usize, Option<(Arc<RgbaImage>, f32)>);
/// Where the pages sit: their sizes and places (points).
type Pages<'a> = (&'a [(f32, f32)], &'a [(f32, f32)]);
/// Boxes lit on the pages (the selection, Find's matches): page, box, tint.
type Lit = (usize, [f32; 4], Rgba<u8>);

/// A PDF's text for Find: a line of words a line, and where each line's words are (its
/// page, its first word, each word's bytes in the line).
struct FindText {
    ver: u64,
    lines: Rc<[String]>,
    map: Vec<LineWords>,
}
/// A line's page, its first word, its words' bytes in the line.
type LineWords = (usize, usize, Vec<(usize, usize)>);
/// Each page's top left in the document (points), and the document's size.
type Layout = (Vec<(f32, f32)>, (f32, f32));

/// Where the last frame put things: for the mouse.
#[derive(Clone, Copy, Debug, Default)]
struct Geo {
    area: Rect,
    cell: (u16, u16),
    scale: f32,
    /// The document's point at the canvas's top left.
    left: f32,
    scroll: f32,
}

/// One file's view: its zoom and where it looks, what came of it.
#[derive(Default)]
struct View {
    /// Screen pixels per unit (a picture's pixel, a page's point); None: fitted.
    scale: Option<f32>,
    /// The view's middle, as a fraction of the picture or the page's width.
    center: (f32, f32),
    mtime: Option<SystemTime>,
    img: Option<Picture>,
    info: Option<Info>,
    /// A PDF: the view's top, in points from the first page's top.
    scroll: f32,
    pages: Vec<Drawn>,
    asked: Vec<(usize, u32)>,
    words: HashMap<usize, Words>,
    words_asked: HashSet<usize>,
    /// Bumped as words come; the whole document's asked for (Find).
    words_ver: u64,
    all_asked: bool,
    find: Option<FindText>,
    /// Find's match shown (its index in the matches).
    find_cur: Option<usize>,
    /// The selected text: from, to (in either order).
    sel: Option<(At, At)>,
    geo: Option<Geo>,
    /// Two pages side by side asked for; shown so (whole pages fit); the page in view.
    two: bool,
    spread: bool,
    now: usize,
    /// The table of contents, as the Outline's headings (line: the page).
    toc: Rc<[Symbol]>,
    /// Pages scaled to the view, by (page, its drawing, scale).
    scaled: Vec<((usize, usize, u32), Arc<RgbaImage>)>,
    /// Tiles the terminal keeps, by (column, row): their image id and what they show;
    /// what they were made for (scale, layout, cell size).
    tiles: HashMap<(i64, i64), (u32, u64)>,
    tiles_of: u64,
    /// The scale fitting the last frame (zoom steps start from it).
    fit: f32,
}

pub struct State {
    views: HashMap<String, View>,
    gfx: Option<crate::settingsgraphics::Graphics>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    hits: Vec<(Rect, Hit)>,
    /// Where pdftoppm, pdfinfo and pdftotext are (tests); else PATH.
    pub tools: Option<PathBuf>,
    /// Tiles to take out of the terminal: replaced this frame; gone next frame (by then
    /// no cell shows them).
    trash: Vec<u32>,
    trash_old: Vec<u32>,
    next_id: u32,
    /// Pictures kept in the terminal whole (a Markdown preview's, the chrome's): key ->
    /// (image id, pixels).
    kept: HashMap<u64, (u32, u32)>,
    /// Pictures swapped in place (a minimap): by slot, the image id shown and its pixels' digest.
    swaps: HashMap<String, (u32, u64)>,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = channel();
        State { views: HashMap::new(), gfx: None, tx, rx, hits: Vec::new(), tools: None, trash: Vec::new(), trash_old: Vec::new(), next_id: 0, kept: HashMap::new(), swaps: HashMap::new() }
    }
}

impl State {
    /// A Kitty terminal with 8×16 px cells, for tests.
    #[cfg(test)]
    pub fn kitty_for_tests(&mut self) {
        self.gfx = Some(crate::settingsgraphics::Graphics::kitty());
    }

    /// A Kitty terminal with `cell`-px cells (Ghostty's 8×22), for tests.
    #[cfg(test)]
    pub fn kitty_cells_for_tests(&mut self, cell: (u16, u16)) {
        let mut g = crate::settingsgraphics::Graphics::kitty();
        g.set_cell_for_tests(cell);
        self.gfx = Some(g);
    }

    pub fn clear_hits(&mut self) {
        self.hits.clear();
    }

    /// A new tile's id: termcode's own range (the process in it), then a counter.
    fn new_id(&mut self) -> u32 {
        self.next_id = (self.next_id + 1) & 0xffff;
        0x4900_0000 | ((std::process::id() & 0xff) << 16) | self.next_id
    }

    fn tool(&self, name: &str) -> PathBuf {
        self.tools.as_ref().map_or_else(|| PathBuf::from(name), |d| d.join(name))
    }
}

// ---------------------------------------------------------------- the tools

/// `prog args` with its stdout, or why not (missing, failed, too slow).
fn output(prog: &Path, args: &[String]) -> Result<Vec<u8>, String> {
    let (tx, rx) = channel();
    let (run, args) = (prog.to_path_buf(), args.to_vec());
    std::thread::spawn(move || {
        let _ = tx.send(Command::new(&run).args(&args).stdin(Stdio::null()).stderr(Stdio::piped()).output());
    });
    let name = |p: &Path| p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    match rx.recv_timeout(WAIT) {
        Ok(Ok(o)) if o.status.success() => Ok(o.stdout),
        Ok(Ok(o)) => Err(format!("{}: {}", name(prog), String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("failed"))),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(format!("{} is not installed (sudo apt install poppler-utils)", name(prog))),
        Ok(Err(e)) => Err(format!("{}: {e}", name(prog))),
        Err(_) => Err(format!("{} took too long", name(prog))),
    }
}

/// A PDF's text, its pages as headings (the Outline lists them), for the text view.
pub fn pdf_text(app: &App, path: &Path) -> Vec<String> {
    let text = match output(&app.media.tool("pdftotext"), &["-layout".into(), path.display().to_string(), "-".into()]) {
        Ok(t) => String::from_utf8_lossy(&t).into_owned(),
        Err(e) => return vec![e],
    };
    let mut lines = vec!["## Page 1".to_string(), String::new()];
    for (i, page) in text.split('\u{c}').enumerate() {
        if i > 0 && !page.trim().is_empty() {
            lines.extend([String::new(), format!("## Page {}", i + 1), String::new()]);
        }
        lines.extend(page.lines().map(|l| l.trim_end().to_string()));
    }
    lines
}

/// A picture's pixels, downscaled to MAX_SIDE, and its own size.
fn decode(path: &Path) -> Result<(RgbaImage, (u32, u32)), String> {
    let img = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()).map_err(|e| e.to_string())?.decode().map_err(|e| e.to_string())?;
    let size = (img.width(), img.height());
    let img = if size.0.max(size.1) > MAX_SIDE { img.resize(MAX_SIDE, MAX_SIDE, imageops::FilterType::CatmullRom) } else { img };
    Ok((img.to_rgba8(), size))
}

/// pdfinfo: each page's size in points.
fn pdf_info(prog: &Path, path: &Path) -> Info {
    let out = String::from_utf8_lossy(&output(prog, &["-f".into(), "1".into(), "-l".into(), "999999".into(), path.display().to_string()])?).into_owned();
    let sizes = parse_sizes(&out);
    if sizes.is_empty() {
        return Err("pdfinfo: no pages".into());
    }
    Ok(sizes)
}

/// "Page    1 size: 612 x 792 pts (letter)" for each page; one "Page size:" for all.
fn parse_sizes(out: &str) -> Vec<(f32, f32)> {
    let size = |v: &str| {
        let mut it = v.split_whitespace();
        let w: f32 = it.next()?.parse().ok()?;
        it.next(); // "x"
        Some((w, it.next()?.parse::<f32>().ok()?))
    };
    let each: Vec<(f32, f32)> = out.lines().filter_map(|l| l.strip_prefix("Page")?.split_once("size:").filter(|(n, _)| !n.trim().is_empty()).and_then(|(_, v)| size(v))).collect();
    if !each.is_empty() {
        return each;
    }
    let pages: usize = out.lines().find_map(|l| l.strip_prefix("Pages:")).and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let one = out.lines().find_map(|l| l.strip_prefix("Page size:")).and_then(size).unwrap_or((612.0, 792.0));
    vec![one; pages]
}

fn render_page(prog: &Path, path: &Path, page: usize, dpi: u32) -> Result<RgbaImage, String> {
    let n = (page + 1).to_string();
    let png = output(prog, &["-f".into(), n.clone(), "-l".into(), n, "-r".into(), dpi.to_string(), "-png".into(), path.display().to_string()])?;
    image::load_from_memory_with_format(&png, image::ImageFormat::Png).map(|i| i.to_rgba8()).map_err(|e| format!("pdftoppm: {e}"))
}

/// pdftotext -bbox-layout: a page's words in reading order, each with its box and line.
fn page_words(prog: &Path, path: &Path, page: usize) -> Words {
    let n = (page + 1).to_string();
    let html = output(prog, &["-bbox-layout".into(), "-f".into(), n.clone(), "-l".into(), n, path.display().to_string(), "-".into()])?;
    Ok(parse_words(&String::from_utf8_lossy(&html)))
}

/// Every page's words (Find searches the whole document).
fn all_words(prog: &Path, path: &Path) -> Result<Vec<Vec<Word>>, String> {
    let html = output(prog, &["-bbox-layout".into(), path.display().to_string(), "-".into()])?;
    Ok(parse_pages(&String::from_utf8_lossy(&html)))
}

fn parse_words(html: &str) -> Vec<Word> {
    parse_pages(html).into_iter().next().unwrap_or_default()
}

fn parse_pages(html: &str) -> Vec<Vec<Word>> {
    let attr = |tag: &str, k: &str| -> f32 { tag.split(&format!("{k}=\"")).nth(1).and_then(|r| r.split('"').next()).and_then(|v| v.parse().ok()).unwrap_or(0.0) };
    let mut line = 0u32;
    let mut pages: Vec<Vec<Word>> = Vec::new();
    for l in html.lines() {
        let l = l.trim();
        if l.starts_with("<page ") {
            pages.push(Vec::new());
        } else if l.starts_with("<line ") {
            line += 1;
        } else if let Some(rest) = l.strip_prefix("<word ") {
            let (tag, after) = rest.split_once('>').unwrap_or((rest, ""));
            let text = after.strip_suffix("</word>").unwrap_or(after);
            if pages.is_empty() {
                pages.push(Vec::new());
            }
            pages.last_mut().expect("pushed").push(Word { line, b: [attr(tag, "xMin"), attr(tag, "yMin"), attr(tag, "xMax"), attr(tag, "yMax")], text: unescape(text) });
        }
    }
    pages
}

/// pdftohtml -xml: the PDF's outline (bookmarks), as headings at their pages.
fn pdf_toc(prog: &Path, path: &Path) -> Vec<Symbol> {
    let args = ["-xml", "-i", "-q", "-stdout", "-f", "1", "-l", "1"].map(String::from);
    let xml = output(prog, &[&args[..], &[path.display().to_string()]].concat()).unwrap_or_default();
    parse_toc(&String::from_utf8_lossy(&xml))
}

fn parse_toc(xml: &str) -> Vec<Symbol> {
    let (mut out, mut depth, mut page, mut rest) = (Vec::new(), 0usize, 0usize, xml);
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<outline>") {
            depth += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("</outline>") {
            depth = depth.saturating_sub(1);
            rest = r;
        } else if let Some(r) = rest.strip_prefix("<item") {
            let (tag, after) = r.split_once('>').unwrap_or((r, ""));
            let (text, after) = after.split_once("</item>").unwrap_or((after, ""));
            // An item without a page goes where the one before it does.
            if let Some(n) = tag.split("page=\"").nth(1).and_then(|v| v.split('"').next()).and_then(|v| v.parse::<usize>().ok()) {
                page = n.saturating_sub(1);
            }
            let mut name = String::new();
            let mut in_tag = false;
            for c in text.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    c if !in_tag => name.push(c),
                    _ => {}
                }
            }
            let name = unescape(name.split_whitespace().collect::<Vec<_>>().join(" ").as_str());
            if depth > 0 && !name.is_empty() {
                out.push(Symbol { line: page, name, kind: "heading", level: depth });
            }
            rest = after;
        } else {
            rest = &rest[1..];
        }
    }
    out
}

fn unescape(t: &str) -> String {
    t.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&")
}

// ---------------------------------------------------------------- a PDF as a document

/// The pages one under another, or two a row (side by side), centered.
fn layout(sizes: &[(f32, f32)], two: bool) -> Layout {
    let rows: Vec<&[(f32, f32)]> = sizes.chunks(if two { 2 } else { 1 }).collect();
    let row_w = |r: &[(f32, f32)]| r.iter().map(|p| p.0).sum::<f32>() + GAP * r.len().saturating_sub(1) as f32;
    let width = rows.iter().map(|r| row_w(r)).fold(0.0, f32::max);
    let (mut pos, mut y) = (Vec::with_capacity(sizes.len()), 0.0);
    for r in rows {
        let mut x = (width - row_w(r)) / 2.0;
        for (w, _) in r {
            pos.push((x, y));
            x += w + GAP;
        }
        y += r.iter().map(|p| p.1).fold(0.0, f32::max) + GAP;
    }
    (pos, (width, (y - GAP).max(0.0)))
}

/// The first page of the row at `y` (points from the top).
fn page_at(pos: &[(f32, f32)], y: f32) -> usize {
    let mut i = pos.iter().rposition(|p| p.1 <= y).unwrap_or(0);
    while i > 0 && pos[i - 1].1 == pos[i].1 {
        i -= 1;
    }
    i
}

/// The document's x at the canvas's left: centered when narrower, panned when wider.
fn left_of(doc_w: f32, scale: f32, width: u32, center_x: f32) -> f32 {
    let view = width as f32 / scale;
    if doc_w <= view {
        -(view - doc_w) / 2.0
    } else {
        center_x * (doc_w - view)
    }
}

/// The scale showing two pages a row whole on a `w`×`h` canvas.
fn spread_fit(sizes: &[(f32, f32)], (w, h): (u32, u32)) -> f32 {
    let (_, (dw, _)) = layout(sizes, true);
    let row_h = sizes.chunks(2).map(|r| r.iter().map(|p| p.1).fold(0.0, f32::max)).fold(0.0, f32::max);
    (w as f32 / dw.max(1.0)).min(h as f32 / row_h.max(1.0))
}

/// Two pages a row asked for and whole pages fitting at `scale` (fitted when None): the
/// scale fitting them, and whether they show so.
fn spread_state(two: bool, sizes: &[(f32, f32)], canvas: (u32, u32), scale: Option<f32>) -> (Option<f32>, bool) {
    let fit = (two && sizes.len() > 1).then(|| spread_fit(sizes, canvas));
    (fit, fit.is_some_and(|t| scale.is_none_or(|s| s <= t * 1.001)))
}

/// The most a PDF zooms: its largest page MAX_PAGE_PX big.
fn max_scale(sizes: &[(f32, f32)]) -> f32 {
    (MAX_PAGE_PX / sizes.iter().map(|(w, h)| w * h).fold(1.0, f32::max)).sqrt()
}

fn ordered((a, b): (At, At)) -> (At, At) {
    if a <= b { (a, b) } else { (b, a) }
}

/// The selection's text: words joined by spaces, lines by newlines, pages by a blank line.
fn selected_text(v: &View) -> String {
    let Some(sel) = v.sel else { return String::new() };
    let (a, b) = ordered(sel);
    let mut out = String::new();
    for page in a.0..=b.0 {
        let Some(Ok(words)) = v.words.get(&page) else { continue };
        let from = if page == a.0 { a.1 } else { 0 };
        let to = if page == b.0 { b.1.min(words.len().saturating_sub(1)) } else { words.len().saturating_sub(1) };
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        let mut last = None;
        for w in words.get(from..=to).unwrap_or_default() {
            match last {
                Some(l) if l == w.line => out.push(' '),
                Some(_) => out.push('\n'),
                None => {}
            }
            out.push_str(&w.text);
            last = Some(w.line);
        }
    }
    out
}

/// The word under (or nearest) screen cell `p`.
fn word_at(v: &View, p: Position) -> Option<At> {
    let geo = v.geo?;
    let sizes = v.info.as_ref()?.as_ref().ok()?;
    let (pos, _) = layout(sizes, v.spread);
    let px = f32::from(p.x.saturating_sub(geo.area.x)) * f32::from(geo.cell.0) + f32::from(geo.cell.0) / 2.0;
    let py = f32::from(p.y.saturating_sub(geo.area.y)) * f32::from(geo.cell.1) + f32::from(geo.cell.1) / 2.0;
    let (x, y) = (geo.left + px / geo.scale, geo.scroll + py / geo.scale);
    let gap = |lo: f32, hi: f32, at: f32| (lo - at).max(at - hi).max(0.0);
    // The page under it, or the nearest.
    let page = (0..pos.len()).min_by(|&a, &b| {
        let d = |i: usize| gap(pos[i].0, pos[i].0 + sizes[i].0, x) + gap(pos[i].1, pos[i].1 + sizes[i].1, y);
        d(a).total_cmp(&d(b))
    })?;
    let words = v.words.get(&page)?.as_ref().ok()?;
    let (xu, yu) = (x - pos[page].0, y - pos[page].1);
    let best = words.iter().enumerate().min_by(|(_, a), (_, b)| {
        let d = |w: &Word| gap(w.b[1], w.b[3], yu) * 4.0 + gap(w.b[0], w.b[2], xu);
        d(a).total_cmp(&d(b))
    })?;
    Some((page, best.0))
}

// ---------------------------------------------------------------- the jobs

/// What came from the jobs (tick).
pub fn tick(app: &mut App) -> bool {
    if !app.media.views.is_empty() {
        gc(app);
    }
    let mut changed = false;
    while let Ok(m) = app.media.rx.try_recv() {
        changed = true;
        match m {
            Msg::Image(key, r) => {
                if let Some(v) = app.media.views.get_mut(&key) {
                    v.img = Some(r.map(|(i, s)| (Arc::new(i), s)));
                }
            }
            Msg::Info(key, r) => {
                if let Some(v) = app.media.views.get_mut(&key) {
                    v.info = Some(r);
                }
            }
            Msg::Page(key, page, dpi, r) => {
                if let Some(v) = app.media.views.get_mut(&key) {
                    v.asked.retain(|a| *a != (page, dpi));
                    v.pages.retain(|(k, _)| *k != (page, dpi));
                    v.pages.push(((page, dpi), r.map(Arc::new)));
                    // The pages near the view stay; the rest go.
                    let at = v.now;
                    v.pages.sort_by_key(|((p, _), _)| p.abs_diff(at));
                    v.pages.truncate(PAGES_KEPT);
                }
            }
            Msg::Toc(key, toc) => {
                if let Some(v) = app.media.views.get_mut(&key) {
                    v.toc = toc.into();
                }
            }
            Msg::Words(key, page, r) => {
                if let Some(v) = app.media.views.get_mut(&key) {
                    v.words_asked.remove(&page);
                    v.words.insert(page, r);
                    v.words_ver += 1;
                }
            }
            Msg::AllWords(key, r) => {
                if let Some(v) = app.media.views.get_mut(&key) {
                    match r {
                        Ok(pages) => {
                            for (i, w) in pages.into_iter().enumerate() {
                                v.words.insert(i, Ok(w));
                            }
                        }
                        Err(e) => app.msg = format!("Find: {e}"),
                    }
                    v.words_ver += 1;
                }
                // Find's matches, over the whole document now; typed while they came: the
                // first from the view shown.
                app.find_refresh();
                if app.focus == Focus::Find && app.find.as_ref().is_some_and(|f| f.cur.is_none()) {
                    find_step(app, 0);
                }
            }
        }
    }
    changed
}

/// The view of file tab `key` (its file `path`), started (and started again when the
/// file changed).
fn view<'a>(app: &'a mut App, key: &str, path: &Path, kind: Kind) -> &'a mut View {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let tx = app.media.tx.clone();
    let (info_tool, toc_tool) = (app.media.tool("pdfinfo"), app.media.tool("pdftohtml"));
    let v = app.media.views.entry(key.to_string()).or_insert_with(|| View { center: (0.5, 0.5), ..Default::default() });
    if v.mtime != mtime || (v.img.is_none() && v.info.is_none()) {
        let keep = (v.scale, v.center, v.scroll, v.two);
        *v = View { mtime, scale: keep.0, center: keep.1, scroll: keep.2, two: keep.3, ..Default::default() };
        let (k, p) = (key.to_string(), path.to_path_buf());
        match kind {
            Kind::Image => {
                v.img = Some(Err("Loading…".into()));
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Image(k, decode(&p)));
                });
            }
            Kind::Pdf => {
                v.info = Some(Err("Loading…".into()));
                let (k2, p2, tx2) = (k.clone(), p.clone(), tx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Info(k, pdf_info(&info_tool, &p)));
                });
                std::thread::spawn(move || {
                    let _ = tx2.send(Msg::Toc(k2, pdf_toc(&toc_tool, &p2)));
                });
            }
        }
    }
    v
}

/// Page `page` asked for at `dpi` (unless drawn or coming), its words too; the nearest
/// drawing of it so far, meanwhile.
fn want_page(app: &mut App, key: &str, path: &Path, page: usize, dpi: u32) -> Option<(Arc<RgbaImage>, f32)> {
    let (render, text, tx) = (app.media.tool("pdftoppm"), app.media.tool("pdftotext"), app.media.tx.clone());
    let v = app.media.views.get_mut(key)?;
    if !v.pages.iter().any(|(k, _)| *k == (page, dpi)) && !v.asked.contains(&(page, dpi)) {
        v.asked.push((page, dpi));
        let (k, p, tx) = (key.to_string(), path.to_path_buf(), tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Page(k, page, dpi, render_page(&render, &p, page, dpi)));
        });
    }
    if !v.words.contains_key(&page) && v.words_asked.insert(page) {
        let (k, p) = (key.to_string(), path.to_path_buf());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Words(k, page, page_words(&text, &p, page)));
        });
    }
    let best = v.pages.iter().filter(|((p, _), r)| *p == page && r.is_ok()).min_by_key(|((_, d), _)| d.abs_diff(dpi))?;
    match best {
        ((_, d), Ok(img)) => Some((img.clone(), *d as f32 / 72.0)),
        _ => None,
    }
}

/// Page `i`'s drawing `src` (its pixels a point) at `scale`, made once and kept.
fn scaled(v: &mut View, i: usize, (src, ppu): &(Arc<RgbaImage>, f32), size: (f32, f32), scale: f32) -> Arc<RgbaImage> {
    let k = (i, Arc::as_ptr(src) as usize, scale.to_bits());
    if let Some((_, img)) = v.scaled.iter().find(|(kk, _)| *kk == k) {
        return img.clone();
    }
    let (w, h) = (((size.0 * scale).round() as u32).max(1), ((size.1 * scale).round() as u32).max(1));
    // Enlarged a lot: crisp pixels; otherwise a sharp filter (text stays legible).
    let filter = if scale > ppu * 2.0 { imageops::FilterType::Nearest } else { imageops::FilterType::CatmullRom };
    let img = Arc::new(imageops::resize(&**src, w, h, filter));
    v.scaled.retain(|((p, ..), _)| *p != i);
    v.scaled.push((k, img.clone()));
    // ponytail: the 8 latest; a view shows a few pages at most.
    if v.scaled.len() > 8 {
        v.scaled.remove(0);
    }
    img
}

// ---------------------------------------------------------------- drawing

/// The picture of `src` (`ppu` of its pixels a unit, `units` big) at `scale` screen
/// pixels a unit around `center`, on a `w`×`h` canvas of `bg`.
fn compose(src: &RgbaImage, ppu: f32, units: (f32, f32), scale: f32, center: (f32, f32), (w, h): (u32, u32), bg: Rgba<u8>) -> RgbaImage {
    let mut canvas = RgbaImage::from_pixel(w, h, bg);
    let (vw, vh) = (w as f32 / scale, h as f32 / scale);
    let start = |c: f32, total: f32, view: f32| if view >= total { 0.0 } else { (c * total - view / 2.0).clamp(0.0, total - view) };
    let (ux, uy) = (start(center.0, units.0, vw), start(center.1, units.1, vh));
    let (uw, uh) = (vw.min(units.0 - ux), vh.min(units.1 - uy));
    let (tw, th) = (((uw * scale).round() as u32).clamp(1, w), ((uh * scale).round() as u32).clamp(1, h));
    paste(&mut canvas, src, ppu, (ux, uy, uw, uh), ((w - tw) / 2, (h - th) / 2, tw, th));
    canvas
}

/// Units `(ux, uy, uw, uh)` of `src` (`ppu` pixels a unit) scaled onto `to` of `canvas`.
fn paste(canvas: &mut RgbaImage, src: &RgbaImage, ppu: f32, (ux, uy, uw, uh): (f32, f32, f32, f32), (tx, ty, tw, th): (u32, u32, u32, u32)) {
    if tw == 0 || th == 0 || src.width() == 0 || src.height() == 0 {
        return;
    }
    let (sx, sy) = (((ux * ppu) as u32).min(src.width() - 1), ((uy * ppu) as u32).min(src.height() - 1));
    let (sw, sh) = (((uw * ppu) as u32).clamp(1, src.width() - sx), ((uh * ppu) as u32).clamp(1, src.height() - sy));
    let part = imageops::crop_imm(src, sx, sy, sw, sh).to_image();
    // Pixel art enlarged stays crisp; otherwise a sharp filter (text on a page stays legible).
    let filter = if tw > sw * 2 { imageops::FilterType::Nearest } else { imageops::FilterType::CatmullRom };
    imageops::overlay(canvas, &imageops::resize(&part, tw, th, filter), i64::from(tx), i64::from(ty));
}

/// The pages (scaled; None: a blank page while it comes) on a `w`×`h` canvas whose top
/// left is the document's pixel `origin`, the boxes in `lit` tinted.
#[allow(clippy::too_many_arguments)]
fn compose_pages(pages: &[Shown], sizes: &[(f32, f32)], pos: &[(f32, f32)], scale: f32, origin: (i64, i64), (w, h): (u32, u32), bg: Rgba<u8>, lit: &[Lit]) -> RgbaImage {
    let mut canvas = RgbaImage::from_pixel(w, h, bg);
    let px = |u: f32| (u * scale).round() as i64;
    for (i, img) in pages {
        let (x0, y0) = (px(pos[*i].0) - origin.0, px(pos[*i].1) - origin.1);
        match img {
            Some(img) => imageops::replace(&mut canvas, &**img, x0, y0),
            None => tint_rect(&mut canvas, (x0, y0, px(sizes[*i].0), px(sizes[*i].1)), |p| *p = Rgba([255, 255, 255, 255])),
        }
        // The selection and Find's matches over their words.
        for (_, b, tint) in lit.iter().filter(|(p, ..)| p == i) {
            let r = (x0 + px(b[0]), y0 + px(b[1]), px(b[2]) - px(b[0]), px(b[3]) - px(b[1]));
            tint_rect(&mut canvas, r, |p| {
                for c in 0..3 {
                    p[c] = ((u16::from(p[c]) * 3 + u16::from(tint[c]) * 2) / 5) as u8;
                }
            });
        }
    }
    canvas
}

/// `f` over the pixels of `(x, y, w, h)` on `canvas` (clipped to it).
fn tint_rect(canvas: &mut RgbaImage, (x, y, w, h): (i64, i64, i64, i64), f: impl Fn(&mut Rgba<u8>)) {
    let (cw, ch) = (i64::from(canvas.width()), i64::from(canvas.height()));
    for yy in y.max(0)..(y + h).min(ch) {
        for xx in x.max(0)..(x + w).min(cw) {
            f(canvas.get_pixel_mut(xx as u32, yy as u32));
        }
    }
}

fn kb(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

fn digest(parts: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// The tab: a bar (what it is, the zoom, a PDF's page, the views), then the picture or
/// the pages (with their scrollbar).
#[allow(clippy::too_many_arguments)]
pub fn draw(app: &mut App, f: &mut Frame, g: usize, key: &str, path: &Path, kind: Kind, body: Rect) {
    fill(f, body, P.editor_bg);
    if body.height < 3 || body.width < 20 {
        return;
    }
    let bar = Rect::new(body.x, body.y, body.width, 1);
    // A PDF keeps the last column for its scrollbar.
    let sb_w = u16::from(kind == Kind::Pdf);
    let area = Rect::new(body.x, body.y + 1, body.width - sb_w, body.height - 1);
    let gfx = app.media.gfx.get_or_insert_with(crate::settingsgraphics::Graphics::new);
    gfx.refresh_size();
    let cell = gfx.cell();
    let (pw, ph) = (u32::from(area.width) * u32::from(cell.0), u32::from(area.height) * u32::from(cell.1));
    let tiles = kind == Kind::Pdf && gfx.tiles();
    let v = view(app, key, path, kind);
    let sizes = v.info.as_ref().and_then(|i| i.as_ref().ok()).cloned();
    let units = match (&v.img, &sizes) {
        (Some(Ok((_, s))), _) => Some((s.0 as f32, s.1 as f32)),
        (_, Some(s)) => Some(layout(s, false).1),
        _ => None,
    };
    // Fitted: a picture whole; a PDF's pages to the width (read by scrolling), or two
    // whole pages a row. Zoomed past that, two pages go back to one.
    let (two_fit, spread) = sizes.as_deref().map_or((None, false), |s| spread_state(v.two, s, (pw, ph), v.scale));
    let fit_scale = match (spread, units) {
        (true, _) => two_fit.unwrap_or(1.0),
        (_, Some((w, h))) => if kind == Kind::Pdf { pw as f32 / w } else { (pw as f32 / w).min(ph as f32 / h) }.max(0.001),
        _ => 1.0,
    };
    v.fit = fit_scale;
    let scale = v.scale.unwrap_or(fit_scale);
    let center = v.center;
    let (pos, (doc_w, total)) = sizes.as_deref().map_or((Vec::new(), (0.0, 0.0)), |s| layout(s, spread));
    // One page or two a row: the view stays at its page (unless moved since).
    if spread != v.spread && !pos.is_empty() && v.geo.is_none_or(|g| g.scroll == v.scroll) {
        v.scroll = pos[v.now.min(pos.len() - 1)].1;
    }
    v.spread = spread;
    let view_h = ph as f32 / scale;
    let max_scroll = (total - view_h).max(0.0);
    v.scroll = v.scroll.clamp(0.0, max_scroll);
    let mut scroll = v.scroll;
    let mut left = left_of(doc_w, scale, pw, center.0);
    // The terminal's tiles move by whole cells.
    if tiles {
        let snap = |u: f32, c: u16| (u * scale / f32::from(c)).round() * f32::from(c) / scale;
        (left, scroll) = (snap(left, cell.0), snap(scroll, cell.1));
    }
    let n_pages = sizes.as_ref().map_or(0, Vec::len);
    let page = if pos.is_empty() { 0 } else { page_at(&pos, scroll + view_h * 0.3) };
    v.now = page;
    v.geo = Some(Geo { area, cell, scale, left, scroll });
    // The bar: name, size, kind, bytes, zoom (100%: a picture's own pixels, a page at 96 dpi).
    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let bytes = std::fs::metadata(path).map_or(String::new(), |m| kb(m.len()));
    let pct = (scale * if kind == Kind::Pdf { 72.0 / 96.0 } else { 1.0 } * 100.0).round() as u32;
    let what = match (kind, units, &sizes) {
        (Kind::Image, Some((w, h)), _) => format!("{name} · {w}×{h} · {} · {bytes}", path.extension().map_or(String::new(), |e| e.to_string_lossy().to_uppercase())),
        (Kind::Pdf, _, Some(s)) if spread && page + 1 < n_pages => format!("{name} · pages {}–{} of {n_pages} · {bytes}", page + 1, page + 2),
        (Kind::Pdf, _, Some(s)) => format!("{name} · page {} of {n_pages} · {}×{} pt · {bytes}", page + 1, s[page].0, s[page].1),
        _ => format!("{name} · {bytes}"),
    };
    use crate::ui::kit::{self, Look};
    let mut buttons: Vec<(String, Option<Hit>)> = vec![("−".into(), Some(Hit::ZoomOut)), (format!(" {pct}% "), None), ("+".into(), Some(Hit::ZoomIn)), ("Fit".into(), Some(Hit::Fit)), ("1:1".into(), Some(Hit::Actual))];
    if kind == Kind::Pdf {
        buttons.extend([("‹".into(), Some(Hit::Prev)), ("›".into(), Some(Hit::Next)), ("2 pages".into(), Some(Hit::Two))]);
    }
    row(f, bar, P.widget_bg, vec![]);
    // How to show the file (the one shown in the main colours), then the tools left of it.
    let views = buttons_for(path);
    let items: Vec<(&str, Look)> = views.iter().map(|(l, how)| (*l, if *how == As::Main { Look::Primary } else { Look::Secondary })).collect();
    let (rects, mut x) = kit::button_row_end(f, Rect::new(bar.x, bar.y, bar.width.saturating_sub(1), 1), &items, P.widget_bg);
    app.ui.view_btns.extend(rects.into_iter().zip(views.iter()).filter(|(r, _)| !r.is_empty()).map(|(r, (_, how))| (r, *how)));
    x = x.saturating_sub(1);
    for (label, hit) in buttons.iter().rev() {
        let w = if hit.is_some() { kit::tool_w(label) } else { label.width() as u16 };
        if x < bar.x + w {
            break;
        }
        x -= w;
        let r = Rect::new(x, bar.y, w, 1);
        match hit {
            Some(h) => {
                let on = *h == Hit::Two && app.media.views.get(key).is_some_and(|v| v.two);
                kit::button(f, r, label, if on { Look::Primary } else { Look::Secondary }, P.widget_bg, false);
                app.media.hits.push((r, *h));
            }
            None => f.render_widget(Span::styled(label.clone(), Style::new().fg(P.editor_fg).bg(P.widget_bg)), r),
        }
        x = x.saturating_sub(1);
    }
    let room = x.saturating_sub(bar.x + 1) as usize;
    f.render_widget(Line::from(Span::styled(format!(" {}", fit(&what, room)), Style::new().fg(P.widget_fg))), Rect::new(bar.x, bar.y, (room + 1) as u16, 1));
    let bg = crate::settingsgraphics::pixel(P.editor_bg);
    let slot = 10 + g % 4;
    match (kind, units, sizes) {
        (Kind::Image, Some(u), _) => {
            let Some(Some(Ok((src, size)))) = app.media.views.get(key).map(|v| v.img.clone()) else { return };
            let ppu = src.width() as f32 / size.0 as f32;
            let key_d = digest((key, Arc::as_ptr(&src) as usize, scale.to_bits(), center.0.to_bits(), center.1.to_bits(), (pw, ph), bg.0));
            picture(app, f, area, slot, key_d, |w, h| compose(&src, ppu, u, scale, center, (w, h), bg));
        }
        (Kind::Pdf, Some(_), Some(sizes)) => {
            // The pages asked for at the resolution the view needs (in steps of 24 dpi,
            // 36 to 300), each as drawn so far meanwhile.
            let dpi = ((72.0 * scale / 24.0).ceil() as u32 * 24).clamp(36, 300);
            let mut lit = find_hits(app, g, key);
            let v = app.media.views.get(key).expect("started");
            if let Some(sel) = v.sel {
                let (a, b) = ordered(sel);
                let tint = crate::settingsgraphics::pixel(P.focus_border);
                for p in a.0..=b.0 {
                    let words = v.words.get(&p).and_then(|w| w.as_ref().ok()).map_or(&[][..], Vec::as_slice);
                    let (lo, hi) = (if p == a.0 { a.1 } else { 0 }, if p == b.0 { b.1 } else { usize::MAX });
                    lit.extend(words.iter().enumerate().filter(|(i, _)| *i >= lo && *i <= hi).map(|(_, w)| (p, w.b, tint)));
                }
            }
            let origin = ((left * scale).round() as i64, (scroll * scale).round() as i64);
            if tiles {
                let doc_px = ((doc_w * scale).ceil() as i64, (total * scale).ceil() as i64);
                let made_for = digest((scale.to_bits(), spread, cell, bg.0));
                draw_tiles(app, f, area, (key, path), (&sizes, &pos), scale, (origin, doc_px), dpi, (&lit, bg), made_for);
            } else {
                // One picture of the view, made again as it moves (half-blocks are cheap).
                let seen: Vec<usize> = (0..n_pages).filter(|&i| pos[i].1 < scroll + view_h && pos[i].1 + sizes[i].1 > scroll).collect();
                let srcs: Vec<Source> = seen.iter().map(|&i| (i, want_page(app, key, path, i, dpi))).collect();
                let after = seen.last().map_or(0, |l| l + 1);
                for i in after..(after + if spread { 2 } else { 1 }).min(n_pages) {
                    want_page(app, key, path, i, dpi);
                }
                let v = app.media.views.get_mut(key).expect("started");
                let shown: Vec<Shown> = srcs.iter().map(|(i, s)| (*i, s.as_ref().map(|s| scaled(v, *i, s, sizes[*i], scale)))).collect();
                let ptrs: Vec<(usize, usize)> = shown.iter().map(|(i, s)| (*i, s.as_ref().map_or(0, |a| Arc::as_ptr(a) as usize))).collect();
                let key_d = digest((key, &ptrs, scale.to_bits(), origin, spread, (pw, ph), bg.0, lit.iter().map(|(p, b, t)| (*p, b.map(f32::to_bits), t.0)).collect::<Vec<_>>()));
                picture(app, f, area, slot, key_d, |w, h| compose_pages(&shown, &sizes, &pos, scale, origin, (w, h), bg, &lit));
            }
            // The scrollbar, in points.
            let track = Rect::new(area.right(), area.y, 1, area.height);
            crate::ui::scrollbar(app, f, track, ScrollTarget::Media(g), total.ceil() as usize, view_h as usize, scroll as usize, max_scroll as usize);
        }
        _ => {
            let e = match app.media.views.get(key) {
                Some(View { img: Some(Err(e)), .. }) | Some(View { info: Some(Err(e)), .. }) => e.clone(),
                _ => "Loading…".into(),
            };
            let w = e.width() as u16;
            f.render_widget(Span::styled(fit(&e, area.width as usize), Style::new().fg(P.dim)), Rect::new(area.x + area.width.saturating_sub(w) / 2, area.y + area.height / 2, w.min(area.width), 1));
        }
    }
}

/// A PDF as tiles the terminal keeps (Kitty): each made and sent once (PNG), the view
/// then placeholder cells pointing into them, so scrolling sends cells, not pixels. The
/// tiles over the view first, then a row above and below ahead (two a frame).
#[allow(clippy::too_many_arguments)]
fn draw_tiles(app: &mut App, f: &mut Frame, area: Rect, (key, path): (&str, &Path), (sizes, pos): Pages, scale: f32, (origin, doc_px): ((i64, i64), (i64, i64)), dpi: u32, (lit, bg): (&[Lit], Rgba<u8>), made_for: u64) {
    let Some(cell) = app.media.gfx.as_ref().map(|g| g.cell()) else { return };
    let (cw, ch) = (i64::from(cell.0), i64::from(cell.1));
    let (tw, th) = (i64::from(TILE.0) * cw, i64::from(TILE.1) * ch);
    let (x0, x1) = (origin.0.max(0), (origin.0 + i64::from(area.width) * cw).min(doc_px.0));
    let (y0, y1) = (origin.1.max(0), (origin.1 + i64::from(area.height) * ch).min(doc_px.1));
    if let Some(v) = app.media.views.get_mut(key) {
        if v.tiles_of != made_for {
            app.media.trash.extend(v.tiles.drain().map(|(_, (id, _))| id));
            (v.tiles_of, v.scaled) = (made_for, Vec::new());
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (cols, rows) = (x0 / tw..=(x1 - 1) / tw, y0 / th..=(y1 - 1) / th);
    let mut want: Vec<(i64, i64, bool)> = rows.clone().flat_map(|ty| cols.clone().map(move |tx| (tx, ty, true))).collect();
    for ty in [rows.end() + 1, rows.start() - 1] {
        if ty >= 0 && ty * th < doc_px.1 {
            want.extend(cols.clone().map(|tx| (tx, ty, false)));
        }
    }
    let px = |u: f32| (u * scale).round() as i64;
    let meets = |a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)| a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3;
    let (mut seq, mut ahead) = (String::new(), 0);
    for (tx, ty, needed) in want {
        let r = (tx * tw, ty * th, tw, th);
        let pages: Vec<usize> = (0..pos.len()).filter(|&i| meets(r, (px(pos[i].0), px(pos[i].1), px(sizes[i].0), px(sizes[i].1)))).collect();
        let srcs: Vec<Source> = pages.iter().map(|&i| (i, want_page(app, key, path, i, dpi))).collect();
        // Ahead only what is ready (a blank page made now would be made again soon).
        if !needed && (ahead >= 2 || srcs.iter().any(|(_, s)| s.is_none())) {
            continue;
        }
        let Some(v) = app.media.views.get_mut(key) else { return };
        let shown: Vec<Shown> = srcs.iter().map(|(i, s)| (*i, s.as_ref().map(|s| scaled(v, *i, s, sizes[*i], scale)))).collect();
        let here: Vec<Lit> = lit
            .iter()
            .filter(|(p, b, _)| pages.contains(p) && meets(r, (px(pos[*p].0) + px(b[0]), px(pos[*p].1) + px(b[1]), px(b[2]) - px(b[0]) + 1, px(b[3]) - px(b[1]) + 1)))
            .copied()
            .collect();
        let k = digest((shown.iter().map(|(i, s)| (*i, s.as_ref().map_or(0, |a| Arc::as_ptr(a) as usize))).collect::<Vec<_>>(), here.iter().map(|(p, b, t)| (*p, b.map(f32::to_bits), t.0)).collect::<Vec<_>>()));
        if v.tiles.get(&(tx, ty)).is_some_and(|t| t.1 == k) {
            continue;
        }
        let img = compose_pages(&shown, sizes, pos, scale, (r.0, r.1), (tw as u32, th as u32), bg, &here);
        let id = app.media.new_id();
        let Some(gfx) = app.media.gfx.as_ref() else { return };
        seq.push_str(&gfx.transmit(id, &img, TILE.0, TILE.1));
        let Some(v) = app.media.views.get_mut(key) else { return };
        if let Some((old, _)) = v.tiles.insert((tx, ty), (id, k)) {
            app.media.trash.push(old);
        }
        ahead += usize::from(!needed);
    }
    let Some(v) = app.media.views.get_mut(key) else { return };
    // The farthest go past TILES_KEPT.
    if v.tiles.len() > TILES_KEPT {
        let mid = ((x0 + x1) / 2 / tw, (y0 + y1) / 2 / th);
        let mut far: Vec<((i64, i64), u32)> = v.tiles.iter().map(|(k, t)| (*k, t.0)).collect();
        far.sort_by_key(|(k, _)| std::cmp::Reverse((k.0 - mid.0).abs() + (k.1 - mid.1).abs()));
        for (k, id) in far.into_iter().take(v.tiles.len() - TILES_KEPT) {
            v.tiles.remove(&k);
            app.media.trash.push(id);
        }
    }
    // The cells: each a placeholder for its part of its tile.
    let buf = f.buffer_mut();
    for y in 0..area.height {
        for x in 0..area.width {
            let (dx, dy) = (origin.0 + i64::from(x) * cw, origin.1 + i64::from(y) * ch);
            if dx < 0 || dy < 0 || dx >= doc_px.0 || dy >= doc_px.1 {
                continue;
            }
            let Some(&(id, _)) = v.tiles.get(&(dx / tw, dy / th)) else { continue };
            let (sym, fg) = crate::settingsgraphics::placeholder(id, ((dy % th) / ch) as u16, ((dx % tw) / cw) as u16);
            buf[(area.x + x, area.y + y)].set_symbol(&sym).set_fg(fg);
        }
    }
    // The new tiles go out ahead of the cells showing them.
    if !seq.is_empty() {
        let c = &mut buf[(area.x, area.y)];
        let sym = format!("{seq}{}", c.symbol());
        c.set_symbol(&sym);
    }
    crate::settingsgraphics::mark_drawn(area);
}

/// A cell's size in pixels where pictures go to the terminal as Kitty images; None
/// elsewhere (their stand-ins are drawn in cells).
pub fn pixel_cell(app: &mut App) -> Option<(u16, u16)> {
    let gfx = app.media.gfx.get_or_insert_with(crate::settingsgraphics::Graphics::new);
    gfx.refresh_size();
    gfx.tiles().then(|| gfx.cell())
}

/// `img` (`area`'s cells' pixels) shown in `area` as slot `slot`'s picture: sent when its
/// pixels changed (a new image id; the one before goes a frame later, so nothing blinks),
/// then placeholder cells over `row_bg` (what its clear pixels show).
pub fn show_pixels(app: &mut App, f: &mut Frame, slot: &str, area: Rect, img: &RgbaImage, row_bg: &dyn Fn(u16) -> ratatui::style::Color) {
    let d = digest(img.as_raw());
    let (id, seq) = match app.media.swaps.get(slot) {
        Some(&(id, had)) if had == d => (id, String::new()),
        old => {
            let old = old.map(|o| o.0);
            let id = app.media.new_id();
            let Some(gfx) = app.media.gfx.as_ref() else { return };
            let seq = gfx.transmit(id, img, area.width, area.height);
            app.media.trash.extend(old);
            app.media.swaps.insert(slot.to_string(), (id, d));
            (id, seq)
        }
    };
    let buf = f.buffer_mut();
    for y in 0..area.height {
        for x in 0..area.width {
            let (sym, fg) = crate::settingsgraphics::placeholder(id, y, x);
            buf[(area.x + x, area.y + y)].set_symbol(&sym).set_fg(fg).set_bg(row_bg(y));
        }
    }
    if !seq.is_empty() {
        let c = &mut buf[(area.x, area.y)];
        let sym = format!("{seq}{}", c.symbol());
        c.set_symbol(&sym);
    }
    crate::settingsgraphics::mark_drawn(area);
}

/// A cell's size in pixels (pictures are sized by it).
pub fn cell(app: &mut App) -> (u16, u16) {
    let gfx = app.media.gfx.get_or_insert_with(crate::settingsgraphics::Graphics::new);
    gfx.refresh_size();
    gfx.cell()
}

/// A picture kept in the terminal whole by `key` (Kitty), `cols`×`rows` cells: its image
/// id, and the sequence sending it when new (to go out with this frame); its pixels from
/// `make(w, h)`, once. None: no Kitty graphics here.
pub fn kept(app: &mut App, key: u64, (cols, rows): (u16, u16), make: impl FnOnce((u32, u32)) -> Option<RgbaImage>) -> Option<(u32, String)> {
    let (tiles, cell) = app.media.gfx.as_ref().map(|g| (g.tiles(), g.cell()))?;
    if !tiles {
        return None;
    }
    if let Some(&(id, _)) = app.media.kept.get(&key) {
        return Some((id, String::new()));
    }
    let img = make((u32::from(cols) * u32::from(cell.0), u32::from(rows) * u32::from(cell.1)))?;
    // ponytail: all go at once past 128 pictures or 8M pixels (32 MB in the terminal); the
    // chrome's icons and caps are a few dozen small ones, a README has a few big ones.
    let px = img.width() * img.height();
    let held: u64 = app.media.kept.values().map(|&(_, n)| u64::from(n)).sum();
    if app.media.kept.len() >= 128 || held + u64::from(px) > 8 << 20 {
        let old: Vec<u32> = app.media.kept.drain().map(|(_, (id, _))| id).collect();
        app.media.trash.extend(old);
    }
    let id = app.media.new_id();
    let seq = app.media.gfx.as_ref()?.transmit(id, &img, cols, rows);
    app.media.kept.insert(key, (id, px));
    Some((id, seq))
}

/// The tiles replaced a frame ago leave the terminal (ui::draw, each frame).
pub fn flush(app: &mut App, f: &mut Frame) {
    let ids = std::mem::replace(&mut app.media.trash_old, std::mem::take(&mut app.media.trash));
    let Some(gfx) = app.media.gfx.as_ref().filter(|_| !ids.is_empty()) else { return };
    let seq: String = ids.iter().map(|&id| gfx.delete(id)).collect();
    let area = f.area();
    let c = &mut f.buffer_mut()[(area.x, area.y)];
    let sym = format!("{seq}{}", c.symbol());
    c.set_symbol(&sym);
    crate::settingsgraphics::mark_drawn(Rect::new(area.x, area.y, 1, 1));
}

/// `make(w, h)` drawn into `area` (a graphics slot), remade only when `key` changes.
fn picture(app: &mut App, f: &mut Frame, area: Rect, slot: usize, key: u64, make: impl FnOnce(u32, u32) -> RgbaImage) {
    let gfx = app.media.gfx.as_mut().expect("made in draw");
    if gfx.enabled() {
        gfx.raw(f, area, slot, key, make);
    } else {
        f.render_widget(Line::styled(" Pictures do not show in this terminal (TERMCODE_GRAPHICS=text).", Style::new().fg(P.dim)), area);
    }
}

fn buttons_for(path: &Path) -> Vec<(&'static str, As)> {
    buttons(path, false)
}

// ---------------------------------------------------------------- input

/// The active tab's media view: its key and file, when the tab shows one.
fn active(app: &App) -> Option<(String, PathBuf, Kind)> {
    let g = app.groups.get(app.ag)?;
    let key = g.tabs.get(g.active)?.key.clone();
    match app.docs.get(&key)? {
        Doc::Text(b) => Some((key, b.path.clone()?, b.media?)),
        Doc::Diff(_) => None,
    }
}

/// The key of the media view group `g` shows.
fn of_group(app: &App, g: usize) -> Option<String> {
    let grp = app.groups.get(g)?;
    let key = grp.tabs.get(grp.active)?.key.clone();
    app.media.views.contains_key(&key).then_some(key)
}

/// The PDF scrollbar's position (points) and its drag.
pub fn scroll_of(app: &App, g: usize) -> usize {
    of_group(app, g).and_then(|k| app.media.views.get(&k)).map_or(0, |v| v.scroll as usize)
}

pub fn set_scroll(app: &mut App, g: usize, off: usize) {
    if let Some(v) = of_group(app, g).and_then(|k| app.media.views.get_mut(&k)) {
        v.scroll = off as f32; // the frame clamps it
    }
}

/// Where the pages of a PDF sit as last drawn, and the view's height in points.
fn doc(v: &View) -> Option<(Vec<(f32, f32)>, f32)> {
    let sizes = v.info.as_ref()?.as_ref().ok()?;
    let geo = v.geo?;
    Some((layout(sizes, v.spread).0, f32::from(geo.area.height) * f32::from(geo.cell.1) / geo.scale))
}

/// The table of contents of the PDF in tab `key` (the Outline shows it).
pub fn toc(state: &State, key: &str) -> Option<Rc<[Symbol]>> {
    state.views.get(key).map(|v| v.toc.clone())
}

/// The page in view in tab `key`.
pub fn page_now(state: &State, key: &str) -> usize {
    state.views.get(key).map_or(0, |v| v.now)
}

/// The active PDF at page `page` (the Outline's rows go there); false: not a PDF.
pub fn goto_page(app: &mut App, page: usize) -> bool {
    let Some((key, _, Kind::Pdf)) = active(app) else { return false };
    let Some(v) = app.media.views.get_mut(&key) else { return false };
    if let Some((pos, _)) = doc(v).filter(|(pos, _)| page < pos.len()) {
        v.scroll = pos[page].1;
    }
    true
}

/// Zoomed by `by`; a PDF keeps the point under `at` (the mouse; else the view's middle)
/// where it is.
fn zoom(app: &mut App, by: f32, at: Option<Position>) {
    let Some((key, _, kind)) = active(app) else { return };
    let Some(v) = app.media.views.get_mut(&key) else { return };
    let sizes = v.info.as_ref().and_then(|i| i.as_ref().ok()).cloned();
    let max = match &sizes {
        Some(s) if kind == Kind::Pdf => max_scale(s).max(v.fit),
        _ => v.fit.max(1.0) * 16.0,
    };
    // Each way stops at its end (a step never jumps across the other end).
    let cur = v.scale.unwrap_or(v.fit);
    let new = if by > 1.0 { (cur * by).min(max.max(cur)) } else { (cur * by).max((v.fit * 0.1).min(cur)) };
    let (Some(geo), Some(sizes), Kind::Pdf) = (v.geo, sizes, kind) else {
        v.scale = Some(new);
        return;
    };
    let (cw, ch) = (f32::from(geo.cell.0), f32::from(geo.cell.1));
    let p = at.filter(|p| geo.area.contains(*p)).unwrap_or(Position::new(geo.area.x + geo.area.width / 2, geo.area.y + geo.area.height / 2));
    let (px, py) = (f32::from(p.x - geo.area.x) * cw + cw / 2.0, f32::from(p.y - geo.area.y) * ch + ch / 2.0);
    let (x, y) = (geo.left + px / geo.scale, geo.scroll + py / geo.scale);
    // The page under it and the point on that page, found again in the new layout (two
    // pages a row may come or go).
    let (pos, _) = layout(&sizes, v.spread);
    let gap = |lo: f32, hi: f32, at: f32| (lo - at).max(at - hi).max(0.0);
    let i = (0..pos.len()).min_by(|&a, &b| {
        let d = |i: usize| gap(pos[i].0, pos[i].0 + sizes[i].0, x) + gap(pos[i].1, pos[i].1 + sizes[i].1, y);
        d(a).total_cmp(&d(b))
    });
    let Some(i) = i else { return };
    let (xu, yu) = (x - pos[i].0, y - pos[i].1);
    let canvas = (u32::from(geo.area.width) * u32::from(geo.cell.0), u32::from(geo.area.height) * u32::from(geo.cell.1));
    let spread = spread_state(v.two, &sizes, canvas, Some(new)).1;
    let (pos, (doc_w, _)) = layout(&sizes, spread);
    (v.scale, v.spread) = (Some(new), spread);
    v.scroll = pos[i].1 + yu - py / new;
    let view_w = canvas.0 as f32 / new;
    if doc_w > view_w {
        v.center.0 = ((pos[i].0 + xu - px / new) / (doc_w - view_w)).clamp(0.0, 1.0);
    }
}

fn act(app: &mut App, hit: Hit) {
    match hit {
        Hit::ZoomIn => return zoom(app, 1.25, None),
        Hit::ZoomOut => return zoom(app, 1.0 / 1.25, None),
        _ => {}
    }
    let Some((key, _, kind)) = active(app) else { return };
    let Some(v) = app.media.views.get_mut(&key) else { return };
    match hit {
        Hit::ZoomIn | Hit::ZoomOut => {}
        Hit::Fit => (v.scale, v.center) = (None, (0.5, 0.5)),
        Hit::Actual => v.scale = Some(if kind == Kind::Pdf { 96.0 / 72.0 } else { 1.0 }),
        Hit::Two => v.two = !v.two,
        Hit::Prev | Hit::Next => {
            // A row's top: the next, or this one's (the one before when at it).
            let Some((pos, view_h)) = doc(v) else { return };
            let at = pos[page_at(&pos, v.scroll + view_h * 0.3)].1;
            let to = if hit == Hit::Next {
                pos.iter().map(|p| p.1).find(|&y| y > at + 0.5)
            } else if v.scroll > at + 1.0 {
                Some(at)
            } else {
                pos.iter().map(|p| p.1).rfind(|&y| y < at - 0.5)
            };
            if let Some(y) = to {
                v.scroll = y;
            }
        }
    }
}

/// Moves the view: a PDF up and down by `dy` (a share of the view's height); a zoomed
/// picture or a page wider than the view by a share of it.
fn pan(app: &mut App, dx: f32, dy: f32) -> bool {
    let Some((key, _, kind)) = active(app) else { return false };
    let Some(v) = app.media.views.get_mut(&key) else { return false };
    let zoomed = v.scale.is_some_and(|s| s > v.fit * 1.001);
    if kind == Kind::Pdf {
        if let Some((_, view_h)) = doc(v) {
            v.scroll += dy * view_h;
        }
        if zoomed {
            v.center.0 = (v.center.0 + dx).clamp(0.0, 1.0);
        }
        return true;
    }
    if !zoomed {
        return false;
    }
    let share = v.fit / v.scale.unwrap_or(v.fit);
    v.center = ((v.center.0 + dx * share).clamp(0.0, 1.0), (v.center.1 + dy * share).clamp(0.0, 1.0));
    true
}

// ---------------------------------------------------------------- Find

/// The PDF in the active tab, for Find: its key, its text's version and its lines (a line
/// of words a line). The first time, the whole document's words are asked for.
pub fn find_source(app: &mut App) -> Option<(String, u64, Rc<[String]>)> {
    let Some((key, path, Kind::Pdf)) = active(app) else { return None };
    let (tool, tx) = (app.media.tool("pdftotext"), app.media.tx.clone());
    let v = app.media.views.get_mut(&key)?;
    if !v.all_asked {
        v.all_asked = true;
        let k = key.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::AllWords(k, all_words(&tool, &path)));
        });
    }
    if v.find.as_ref().is_none_or(|t| t.ver != v.words_ver) {
        let mut pages: Vec<&usize> = v.words.keys().collect();
        pages.sort();
        let (mut lines, mut map) = (Vec::new(), Vec::new());
        for &page in pages {
            let Some(Ok(words)) = v.words.get(&page) else { continue };
            let mut i = 0;
            while i < words.len() {
                let (first, mut text, mut spans) = (i, String::new(), Vec::new());
                while i < words.len() && words[i].line == words[first].line {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    spans.push((text.len(), text.len() + words[i].text.len()));
                    text.push_str(&words[i].text);
                    i += 1;
                }
                lines.push(text);
                map.push((page, first, spans));
            }
        }
        v.find = Some(FindText { ver: v.words_ver, lines: lines.into(), map });
    }
    v.find.as_ref().map(|t| (key, t.ver, t.lines.clone()))
}

pub fn find_cur(app: &App, key: &str) -> Option<usize> {
    app.media.views.get(key)?.find_cur
}

pub fn set_find_cur(app: &mut App, key: &str, cur: Option<usize>) {
    if let Some(v) = app.media.views.get_mut(key) {
        v.find_cur = cur;
    }
}

/// The words a match covers: (page, word).
fn match_words(t: &FindText, (a, e): (crate::editor::Pos, crate::editor::Pos)) -> Vec<At> {
    let Some((page, first, spans)) = t.map.get(a.line) else { return Vec::new() };
    let to = if e.line == a.line { e.col } else { usize::MAX };
    spans.iter().enumerate().filter(|(_, (s, end))| *s < to && *end > a.col).map(|(j, _)| (*page, first + j)).collect()
}

/// Find's matches on the PDF of group `g` (when Find is on it): page, box, tint (the one
/// shown brighter).
fn find_hits(app: &App, g: usize, key: &str) -> Vec<Lit> {
    let (Some(f), Some(v)) = (app.find.as_ref(), app.media.views.get(key)) else { return Vec::new() };
    let Some(t) = v.find.as_ref().filter(|t| g == app.ag && f.of() == (key, t.ver)) else { return Vec::new() };
    let (cur, other) = (crate::settingsgraphics::pixel(P.find_match), crate::settingsgraphics::pixel(P.find_other));
    let mut out = Vec::new();
    for (n, m) in f.matches.iter().enumerate() {
        let tint = if f.cur == Some(n) { cur } else { other };
        for (page, w) in match_words(t, *m) {
            if let Some(word) = v.words.get(&page).and_then(|w| w.as_ref().ok()).and_then(|ws| ws.get(w)) {
                out.push((page, word.b, tint));
            }
        }
    }
    out
}

/// Find's next (`dir` 1), previous (-1) or first from the view (0, as the query is typed)
/// match on the active PDF, scrolled into view; false: the active tab is no PDF.
pub fn find_step(app: &mut App, dir: isize) -> bool {
    let Some((key, _, Kind::Pdf)) = active(app) else { return false };
    app.find_refresh();
    let Some(f) = app.find.as_ref() else { return true };
    if f.matches.is_empty() {
        if dir != 0 && !f.query.is_empty() {
            app.msg = "No results".into();
        }
        return true;
    }
    let (matches, cur, n) = (f.matches.clone(), f.cur, f.matches.len());
    let Some(v) = app.media.views.get_mut(&key) else { return true };
    let Some(t) = v.find.as_ref() else { return true };
    let page = |m: &(crate::editor::Pos, crate::editor::Pos)| t.map.get(m.0.line).map_or(0, |l| l.0);
    let i = match (cur, dir) {
        (Some(i), 1..) => (i + 1) % n,
        (Some(i), ..0) => (i + n - 1) % n,
        (Some(i), 0) => i,
        (None, d) if d >= 0 => matches.iter().position(|m| page(m) >= v.now).unwrap_or(0),
        (None, _) => matches.iter().rposition(|m| page(m) <= v.now).unwrap_or(n - 1),
    };
    // Into view when out of it: near the view's top.
    let words = match_words(t, matches[i]);
    if let (Some(&(page, w)), Some((pos, view_h))) = (words.first(), doc(v)) {
        if let Some(b) = v.words.get(&page).and_then(|ws| ws.as_ref().ok()).and_then(|ws| ws.get(w)).map(|w| w.b) {
            let (top, bottom) = (pos[page].1 + b[1], pos[page].1 + b[3]);
            if top < v.scroll || bottom > v.scroll + view_h {
                v.scroll = top - view_h / 10.0;
            }
        }
    }
    v.find_cur = Some(i);
    app.find_refresh();
    true
}

/// The selected text, one line of it (Find starts from it).
pub fn selected_line(app: &App) -> Option<String> {
    let (key, _, Kind::Pdf) = active(app)? else { return None };
    let text = selected_text(app.media.views.get(&key)?);
    (!text.is_empty() && !text.contains('\n')).then_some(text)
}

fn copy(app: &mut App, key: &str) {
    let Some(v) = app.media.views.get(key) else { return };
    let text = selected_text(v);
    if text.is_empty() {
        app.msg = "nothing selected: drag over the text first".into();
        return;
    }
    app.msg = format!("copied {} words via {}", text.split_whitespace().count(), crate::panel::clipboard_set(&text));
    app.clip = text;
}

/// Keys on a picture or a PDF; false: not ours.
pub fn key(app: &mut App, k: KeyEvent) -> bool {
    let Some((key, _, kind)) = active(app) else { return false };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && kind == Kind::Pdf {
        match k.code {
            KeyCode::Char('c') => copy(app, &key),
            KeyCode::Char('a') => {
                // The page in view, all of it.
                let Some(v) = app.media.views.get_mut(&key) else { return false };
                let page = v.now;
                let n = v.words.get(&page).and_then(|w| w.as_ref().ok()).map_or(0, Vec::len);
                v.sel = (n > 0).then_some(((page, 0), (page, n - 1)));
            }
            KeyCode::Char('v' | 'x') => app.msg = "a PDF is read-only".into(),
            _ => return false,
        }
        return true;
    }
    if k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return false;
    }
    let pdf = kind == Kind::Pdf;
    match k.code {
        KeyCode::Char('+' | '=') => act(app, Hit::ZoomIn),
        KeyCode::Char('-') => act(app, Hit::ZoomOut),
        KeyCode::Char('0') => act(app, Hit::Fit),
        KeyCode::Char('1') => act(app, Hit::Actual),
        KeyCode::Esc if pdf => {
            if let Some(v) = app.media.views.get_mut(&key) {
                v.sel = None;
            }
        }
        KeyCode::PageDown | KeyCode::Char(' ') if pdf => return pan(app, 0.0, 0.9),
        KeyCode::PageUp if pdf => return pan(app, 0.0, -0.9),
        KeyCode::Home | KeyCode::End if pdf => {
            if let Some(v) = app.media.views.get_mut(&key) {
                v.scroll = if k.code == KeyCode::Home { 0.0 } else { f32::MAX / 4.0 }; // the frame clamps it
            }
        }
        KeyCode::Left => return pan(app, -0.2, 0.0),
        KeyCode::Right => return pan(app, 0.2, 0.0),
        KeyCode::Up => return pan(app, 0.0, if pdf { -0.1 } else { -0.2 }),
        KeyCode::Down => return pan(app, 0.0, if pdf { 0.1 } else { 0.2 }),
        _ => return false,
    }
    true
}

/// A click: the bar's buttons; on a PDF's pages, the start of a selection.
pub fn click(app: &mut App, p: Position) -> bool {
    if let Some(h) = app.media.hits.iter().find(|(r, _)| r.contains(p)).map(|(_, h)| *h) {
        act(app, h);
        return true;
    }
    let Some(g) = app.ui.groups.iter().position(|gh| gh.text.contains(p)) else { return false };
    let Some(key) = of_group(app, g) else { return false };
    let Some(v) = app.media.views.get_mut(&key) else { return false };
    if !v.geo.is_some_and(|geo| geo.area.contains(p)) || !v.info.as_ref().is_some_and(Result::is_ok) {
        return false;
    }
    v.sel = word_at(v, p).map(|w| (w, w));
    (app.ag, app.focus, app.drag) = (g, Focus::Editor, Drag::Media);
    true
}

/// The selection dragged to `p`.
pub fn drag_to(app: &mut App, p: Position) {
    let Some((key, ..)) = active(app) else { return };
    let Some(v) = app.media.views.get_mut(&key) else { return };
    // Past the top or the bottom, the pages scroll along.
    if let Some(geo) = v.geo {
        if p.y < geo.area.y {
            v.scroll -= f32::from(geo.cell.1) * 2.0 / geo.scale;
        } else if p.y >= geo.area.bottom() {
            v.scroll += f32::from(geo.cell.1) * 2.0 / geo.scale;
        }
    }
    let at = Position::new(p.x, p.y.clamp(v.geo.map_or(0, |g| g.area.y), v.geo.map_or(0, |g| g.area.bottom().saturating_sub(1))));
    if let (Some(w), Some((from, _))) = (word_at(v, at), v.sel) {
        v.sel = Some((from, w));
    }
}

/// The wheel over a picture or a PDF: a PDF scrolls; a zoomed picture pans; with Ctrl,
/// both zoom.
pub fn wheel(app: &mut App, p: Position, d: isize, ctrl: bool) -> bool {
    let Some(g) = app.ui.groups.iter().position(|gh| gh.text.contains(p)) else { return false };
    if g != app.ag || active(app).is_none() {
        return false;
    }
    if ctrl {
        zoom(app, if d < 0 { 1.1 } else { 1.0 / 1.1 }, Some(p));
        return true;
    }
    // A PDF by rows of cells (as text scrolls); a picture by a share of the view.
    if let Some(v) = active(app).filter(|a| a.2 == Kind::Pdf).and_then(|a| app.media.views.get_mut(&a.0)) {
        if let Some(geo) = v.geo {
            v.scroll += d as f32 * f32::from(geo.cell.1) / geo.scale;
        }
        return true;
    }
    pan(app, 0.0, d as f32 * 0.06);
    true
}

/// The views of tabs no longer open go (their pixels with them).
pub fn gc(app: &mut App) {
    let open: Vec<String> = app.groups.iter().flat_map(|g| g.tabs.iter().map(|t| t.key.clone())).collect();
    let trash = &mut app.media.trash;
    app.media.views.retain(|k, v| {
        let keep = open.contains(k);
        if !keep {
            trash.extend(v.tiles.values().map(|t| t.0));
        }
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_buttons_go_by_the_extension() {
        assert_eq!((kind(Path::new("a/b.PNG")), kind(Path::new("x.jpeg")), kind(Path::new("m.pdf")), kind(Path::new("x.svg"))), (Some(Kind::Image), Some(Kind::Image), Some(Kind::Pdf), None));
        assert_eq!(buttons(Path::new("m.pdf"), false).iter().map(|b| b.0).collect::<Vec<_>>(), ["pages", "text", "hexdump"]);
        assert_eq!(buttons(Path::new("t.so"), true).iter().map(|b| b.0).collect::<Vec<_>>(), ["size", "readelf", "hexdump"]);
    }

    #[test]
    fn a_picture_is_fitted_centered_and_zoomed_around_its_middle() {
        // 4x2: left half red, right half blue.
        let src = RgbaImage::from_fn(4, 2, |x, _| if x < 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 255, 255]) });
        let bg = Rgba([0, 0, 0, 255]);
        // Fitted into 8x8: 8x4 in the middle, black above and below.
        let c = compose(&src, 1.0, (4.0, 2.0), 2.0, (0.5, 0.5), (8, 8), bg);
        assert_eq!((c.get_pixel(0, 0), c.get_pixel(0, 3)[0], c.get_pixel(7, 3)[2]), (&bg, 255, 255));
        // Zoomed 4x around the right half's middle: all blue.
        let c = compose(&src, 1.0, (4.0, 2.0), 4.0, (0.75, 0.5), (8, 8), bg);
        assert!(c.pixels().all(|p| p[2] == 255 && p[0] == 0), "only the right half in view");
    }

    #[test]
    fn pages_stack_with_gaps_and_words_read_with_their_boxes() {
        let sizes = parse_sizes("Pages:          3\nPage    1 size: 612 x 792 pts (letter)\nPage    2 size: 612 x 792 pts\nPage    3 size: 300 x 400 pts\n");
        assert_eq!(sizes, [(612.0, 792.0), (612.0, 792.0), (300.0, 400.0)]);
        assert_eq!(parse_sizes("Pages: 2\nPage size: 100 x 200 pts\n"), [(100.0, 200.0); 2], "one size for all");
        // One a row, centered on the widest; two a row, side by side.
        let (pos, size) = layout(&sizes, false);
        assert_eq!((pos, size), (vec![(0.0, 0.0), (0.0, 800.0), (156.0, 1600.0)], (612.0, 2000.0)));
        let (pos, size) = layout(&sizes, true);
        assert_eq!((&pos[..], size), (&[(0.0, 0.0), (620.0, 0.0), (466.0, 800.0)][..], (1232.0, 1200.0)));
        assert_eq!((page_at(&pos, 799.0), page_at(&pos, 100.0), page_at(&pos, 800.0)), (0, 0, 2), "a row's first page");
        // Two whole pages on a 2464×800 canvas: the height decides.
        assert_eq!(spread_fit(&sizes[..2], (2464, 792)), 1.0);
        let toc = parse_toc("<pdf2xml><page><text>x</text></page>\n<outline>\n<item page=\"3\">Intro &amp; more</item>\n<outline>\n<item page=\"5\">Part <b>A</b></item>\n<item>No page</item>\n</outline>\n<item page=\"9\">End</item>\n</outline>");
        assert_eq!(toc.iter().map(|s| (s.line, s.level, s.name.as_str())).collect::<Vec<_>>(), [(2, 1, "Intro & more"), (4, 2, "Part A"), (4, 2, "No page"), (8, 1, "End")]);
        let pages = parse_pages("<page width=\"1\">\n<line>\n<word xMin=\"1\">a</word>\n<page width=\"1\">\n<page width=\"1\">\n<line>\n<word xMin=\"1\">b</word>\n");
        assert_eq!(pages.iter().map(|p| p.iter().map(|w| w.text.as_str()).collect::<Vec<_>>()).collect::<Vec<_>>(), [vec!["a"], vec![], vec!["b"]], "an empty page keeps its place");
        let words = parse_words("<line xMin=\"1\">\n<word xMin=\"10.5\" yMin=\"20\" xMax=\"30\" yMax=\"28\">Tom&amp;Jerry</word>\n<word xMin=\"32\" yMin=\"20\" xMax=\"40\" yMax=\"28\">said</word>\n</line>\n<line xMin=\"1\">\n<word xMin=\"10\" yMin=\"40\" xMax=\"20\" yMax=\"48\">&lt;hi&gt;</word>\n");
        assert_eq!(words.iter().map(|w| (w.line, w.text.as_str())).collect::<Vec<_>>(), [(1, "Tom&Jerry"), (1, "said"), (2, "<hi>")]);
        assert_eq!(words[0].b, [10.5, 20.0, 30.0, 28.0]);
        let mut v = View::default();
        v.words.insert(0, Ok(words.clone()));
        v.words.insert(1, Ok(words));
        v.sel = Some(((1, 0), (0, 1))); // dragged backwards, over a page break
        assert_eq!(selected_text(&v), "said\n<hi>\n\nTom&Jerry");
    }
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::panel::App;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// The jobs (decoding, pdfinfo, pdftoppm) given time to come.
    fn settle(app: &mut App, t: &mut Terminal<TestBackend>, until: &str) -> String {
        for _ in 0..300 {
            tick(app);
            let s = screen(app, t);
            if s.contains(until) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no {until:?}\n{}", screen(app, t));
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
    }

    fn click(app: &mut App, r: Rect) {
        mouse(app, MouseEventKind::Down(MouseButton::Left), r.x + 1, r.y);
        mouse(app, MouseEventKind::Up(MouseButton::Left), r.x + 1, r.y);
    }

    fn press(app: &mut App, c: char) {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn a_picture_opens_as_one_zooms_and_shows_its_bytes_on_asking() {
        let dir = std::env::temp_dir().join(format!("termcode-media-png-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        RgbaImage::from_pixel(40, 20, Rgba([200, 30, 30, 255])).save(dir.join("red.png")).unwrap();
        let mut app = App::new(dir.clone(), None);
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        app.open_file(0, "red.png", false);
        app.focus = Focus::Editor;
        app.side_visible = false; // the bar whole in 120 columns
        let s = settle(&mut app, &mut t, "red.png · 40×20 · PNG");
        assert!(s.contains(&crate::ui::kit::btn_text("Fit")) && s.contains(&crate::ui::kit::btn_text("1:1")) && s.contains(&crate::ui::kit::btn_text("picture")) && s.contains(&crate::ui::kit::btn_text("hexdump")), "{s}");
        assert!(matches!(app.docs.get("file:red.png"), Some(Doc::Text(b)) if b.media == Some(Kind::Image) && b.lines.len() == 1), "no lines read");
        let status = s.lines().last().unwrap_or("");
        assert!(status.contains("Image") && !status.contains("Ln 1"), "{status}");
        // 1: its own pixels (100%); +: larger; 0: fitted again.
        press(&mut app, '1');
        assert!(screen(&mut app, &mut t).contains(" 100% "));
        press(&mut app, '+');
        assert!(screen(&mut app, &mut t).contains(" 125% "));
        press(&mut app, '0');
        assert!(!screen(&mut app, &mut t).contains(" 125% "));
        // hexdump, then the picture again.
        let btn = |app: &App, how: As| app.ui.view_btns.iter().find(|(_, h)| *h == how).unwrap().0;
        let r = btn(&app, As::Hex);
        click(&mut app, r);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("00000000  89 50 4e 47"), "its bytes\n{s}");
        let r = btn(&app, As::Main);
        click(&mut app, r);
        settle(&mut app, &mut t, "40×20");
        fs::remove_dir_all(&dir).unwrap();
    }

    fn script(path: &Path, body: &str) {
        fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A PDF in `dir` and poppler's tools, as far as termcode asks them: three letter
    /// pages, each with a line of two words at its top left.
    fn fake_pdf(dir: &Path) -> PathBuf {
        let _ = fs::remove_dir_all(dir);
        let tools = dir.join("tools");
        fs::create_dir_all(&tools).unwrap();
        fs::write(dir.join("doc.pdf"), "%PDF-1.4 fake\n").unwrap();
        RgbaImage::from_pixel(17, 22, Rgba([255, 255, 255, 255])).save(tools.join("page.png")).unwrap();
        script(&tools.join("pdfinfo"), "printf 'Pages:          3\\n'; for n in 1 2 3; do printf 'Page    %s size: 612 x 792 pts\\n' $n; done\n");
        script(&tools.join("pdftoppm"), "echo \"$*\" >> \"$(dirname \"$0\")/ppm.log\"\ncat \"$(dirname \"$0\")/page.png\"\n");
        script(
            &tools.join("pdftotext"),
            concat!(
                "line() { printf '<line xMin=\"0\">\\n<word xMin=\"0\" yMin=\"0\" xMax=\"300\" yMax=\"100\">%s</word>\\n<word xMin=\"300\" yMin=\"0\" xMax=\"612\" yMax=\"100\">world</word>\\n</line>\\n' \"$1\"; }\n",
                "case \"$*\" in\n",
                "  *'-bbox-layout -f '*) line Hello ;;\n",
                "  *-bbox-layout*) for w in Hello Hi Third; do printf '<page width=\"612\">\\n'; line $w; done ;;\n",
                "  *) printf 'Intro\\nfirst page\\fSecond\\fThird\\n' ;;\n",
                "esac\n"
            ),
        );
        script(&tools.join("pdftohtml"), "printf '<outline>\\n<item page=\"1\">Intro</item>\\n<item page=\"3\">The end</item>\\n</outline>\\n'\n");
        tools
    }

    /// The Kitty graphics commands a frame sends (in its cells' text).
    fn sent(t: &Terminal<TestBackend>) -> String {
        t.backend().buffer().content.iter().map(|c| c.symbol()).filter(|s| s.contains("\x1b_G")).collect()
    }

    #[test]
    fn a_pdf_in_kitty_is_tiles_sent_once_then_scrolled_by_cells() {
        let dir = std::env::temp_dir().join(format!("termcode-media-tiles-{}", std::process::id()));
        let tools = fake_pdf(&dir);
        let mut app = App::new(dir.clone(), None);
        (app.media.tools, app.media.gfx) = (Some(tools), Some(crate::settingsgraphics::Graphics::kitty()));
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        app.open_file(0, "doc.pdf", false);
        (app.focus, app.side_visible) = (Focus::Editor, false);
        // The pages come; their tiles go out (PNG, as placeholders' images), then nothing.
        let mut first = String::new();
        for _ in 0..300 {
            tick(&mut app);
            screen(&mut app, &mut t);
            first.push_str(&sent(&t));
            if app.media.views["file:doc.pdf"].tiles.len() >= 6 && sent(&t).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(first.contains("a=T,U=1,f=100") && first.contains(&format!("c={},r={}", TILE.0, TILE.1)), "tiles sent");
        let geo = app.media.views["file:doc.pdf"].geo.unwrap();
        let top = |t: &Terminal<TestBackend>| t.backend().buffer()[(geo.area.x + 30, geo.area.y)].symbol().to_string();
        let before = top(&t);
        assert!(before.starts_with('\u{10EEEE}'), "a placeholder: {before:?}");
        // A notch of the wheel: three rows on, the same tiles, no pixels sent.
        mouse(&mut app, MouseEventKind::ScrollDown, geo.area.x + 30, geo.area.y + 5);
        screen(&mut app, &mut t);
        assert!(!sent(&t).contains(&format!("c={},r={}", TILE.0, TILE.1)), "scrolling sends no tiles, cells only (the slider's own picture aside)");
        assert_ne!(top(&t), before, "the cells point further down");
        // A selection remakes the tiles under it; the old ones leave the frame after.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), geo.area.x + 2, geo.area.y + 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), geo.area.x + 2, geo.area.y + 1);
        screen(&mut app, &mut t);
        assert!(sent(&t).contains("a=T"), "the lit tile sent");
        screen(&mut app, &mut t);
        assert!(sent(&t).contains("a=d,d=I"), "the one it replaced deleted");
        // Ctrl+wheel zooms around the mouse.
        app.on_mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: geo.area.x + 30, row: geo.area.y + 5, modifiers: KeyModifiers::CONTROL });
        assert!(app.media.views["file:doc.pdf"].scale.is_some_and(|s| s > geo.scale), "zoomed in");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_pdf_finds_its_text_across_its_pages() {
        let dir = std::env::temp_dir().join(format!("termcode-media-find-{}", std::process::id()));
        let tools = fake_pdf(&dir);
        let mut app = App::new(dir.clone(), None);
        app.media.tools = Some(tools);
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        app.open_file(0, "doc.pdf", false);
        (app.focus, app.side_visible) = (Focus::Editor, false);
        settle(&mut app, &mut t, "page 1 of 3");
        // Ctrl+F, "world": one a page, in the whole document (its words asked for).
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(app.focus, Focus::Find);
        for c in "world".chars() {
            press(&mut app, c);
        }
        for _ in 0..300 {
            tick(&mut app);
            if app.find.as_ref().is_some_and(|f| f.matches.len() == 3) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let s = screen(&mut app, &mut t);
        assert_eq!(app.find.as_ref().map(|f| (f.matches.len(), f.cur)), Some((3, Some(0))), "{s}");
        assert!(s.contains("1 of 3"), "the Find box counts them\n{s}");
        // Enter: the next, on page 2, then page 3; Shift+Enter back.
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        settle(&mut app, &mut t, "2 of 3");
        assert_eq!(app.media.views["file:doc.pdf"].now, 1, "at page 2");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        settle(&mut app, &mut t, "page 3 of 3");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        settle(&mut app, &mut t, "2 of 3");
        // A phrase across words; nothing found says so.
        app.find.as_mut().unwrap().query = "third world".into();
        app.find_refresh();
        assert_eq!(app.find.as_ref().unwrap().matches.len(), 1);
        app.find.as_mut().unwrap().query = "nowhere".into();
        app.find_step(1);
        assert_eq!(app.msg, "No results");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_pdf_scrolls_selects_and_copies_its_text() {
        let dir = std::env::temp_dir().join(format!("termcode-media-pdf-{}", std::process::id()));
        let tools = fake_pdf(&dir);
        let mut app = App::new(dir.clone(), None);
        app.media.tools = Some(tools.clone());
        let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
        app.open_file(0, "doc.pdf", false);
        app.focus = Focus::Editor;
        app.side_visible = false;
        let s = settle(&mut app, &mut t, "page 1 of 3");
        assert!(s.contains("612×792 pt") && s.contains(&crate::ui::kit::btn_text("pages")) && s.contains(&crate::ui::kit::btn_text("text")) && s.contains(&crate::ui::kit::btn_text("‹")), "{s}");
        assert!(app.ui.scrollbars.iter().any(|sb| sb.target == ScrollTarget::Media(0)), "a scrollbar");
        // Down the document: PgDn moves a screen, › a page, End to the end.
        let top = app.media.views["file:doc.pdf"].scroll;
        app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        screen(&mut app, &mut t);
        assert!(app.media.views["file:doc.pdf"].scroll > top + 100.0, "a screen down");
        let next = app.media.hits.iter().find(|(_, h)| *h == Hit::Next).unwrap().0;
        click(&mut app, next);
        settle(&mut app, &mut t, "page 2 of 3");
        app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        settle(&mut app, &mut t, "page 3 of 3");
        let drawn = (0..300).any(|_| {
            tick(&mut app);
            std::thread::sleep(Duration::from_millis(10));
            fs::read_to_string(tools.join("ppm.log")).unwrap_or_default().contains("-f 3 -l 3")
        });
        assert!(drawn, "drawn as they came into view");
        // The scrollbar dragged to its top: page 1 again.
        set_scroll(&mut app, 0, 0);
        let s = settle(&mut app, &mut t, "page 1 of 3");
        // A drag from the first word to the second selects both; Ctrl+C copies them.
        settle(&mut app, &mut t, "page 1 of 3");
        for _ in 0..100 {
            tick(&mut app);
            if app.media.views.values().any(|v| v.words.contains_key(&0)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let geo = app.media.views.get("file:doc.pdf").and_then(|v| v.geo).unwrap();
        let (y, x0, x1) = (geo.area.y + 1, geo.area.x + 2, geo.area.right() - 3);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x0, y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x1, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x1, y);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.clip, "Hello world", "{s}\n{}", app.msg);
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.msg, "a PDF is read-only");
        // The table of contents in the Outline: its rows go to their pages.
        app.outline_open = true;
        let s = settle(&mut app, &mut t, "The end");
        assert!(s.contains("Intro"), "{s}");
        let row = app.ui.outline_rows[1].0;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), row.x + 4, row.y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), row.x + 4, row.y);
        screen(&mut app, &mut t);
        assert_eq!(app.media.views["file:doc.pdf"].now, 2, "at page 3");
        (app.outline_open, app.focus) = (false, Focus::Editor);
        // Two pages a row while whole pages fit; zoomed in, one again.
        let two = app.media.hits.iter().find(|(_, h)| *h == Hit::Two).unwrap().0;
        click(&mut app, two);
        app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        settle(&mut app, &mut t, "pages 1–2 of 3");
        press(&mut app, '1');
        settle(&mut app, &mut t, "page 1 of 3");
        press(&mut app, '0');
        settle(&mut app, &mut t, "pages 1–2 of 3");
        // Its text: the pages as headings.
        let btn = app.ui.view_btns.iter().find(|(_, h)| *h == As::Text).unwrap().0;
        click(&mut app, btn);
        let s = screen(&mut app, &mut t);
        assert!(s.contains("## Page 2") && s.contains("first page") && s.contains("Third"), "{s}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
