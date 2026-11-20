//! Markdown preview, drawn as GitHub draws a README: headings over a rule, paragraphs and
//! list items reflowed to the pane with hanging indents (ordered lists numbered), quotes
//! and alerts (`> [!NOTE]`) with their bar, fenced code highlighted like the editor,
//! tables in a grid (aligned, cells wrapped, rows striped), rules, pictures (a local
//! image as half-blocks here; ui.rs puts the real pixels over them where the terminal
//! has Kitty graphics), and the HTML READMEs use (`<p align="center">`, `<img>`, `<br>`,
//! `<details>`, `<b>`, `<kbd>`…). Entities and `:emoji:` read as their characters. Links
//! are never followed and nothing is fetched: a picture from the web shows as its name.
//! ui.rs keeps the result until the source, the width or the colours change.

use crate::editor::Buffer;
use crate::theme::{self, P};
use image::{imageops, Rgba, RgbaImage};
use ratatui::prelude::*;
use std::path::{Path, PathBuf};
use syntect::highlighting::Highlighter;
use syntect::parsing::Scope;
use unicode_width::UnicodeWidthStr;

struct Look {
    text: Style,
    heading: Style,
    bullet: Style,
    quote: Style,
    bar: Style,
    code: Style,
    block: Style,
    link: Style,
    rule: Style,
    chip: Style,
    stripe: Color,
}

/// The editor's Markdown token colours, so the preview matches the theme.
fn look(link: Color) -> Look {
    let syn = theme::syntax();
    let h = Highlighter::new(&syn.theme);
    let fg = |scope: &str| {
        let c = h.style_for_stack(&[Scope::new(scope).unwrap()]).foreground;
        Color::Rgb(c.r, c.g, c.b)
    };
    Look {
        text: Style::new().fg(P.editor_fg),
        heading: Style::new().fg(fg("markup.heading.markdown")).bold(),
        bullet: Style::new().fg(fg("punctuation.definition.list_item.markdown")),
        quote: Style::new().fg(P.dim),
        bar: Style::new().fg(P.line_nr),
        code: Style::new().fg(fg("markup.raw.inline.markdown")).bg(P.widget_bg),
        block: Style::new().fg(P.editor_fg).bg(P.widget_bg),
        link: Style::new().fg(link),
        rule: Style::new().fg(P.line_nr),
        chip: Style::new().fg(P.widget_fg).bg(P.widget_bg),
        stripe: P.widget_bg,
    }
}

/// A picture in the preview: its first row and column, its size in cells, its file.
#[derive(Clone, Debug, PartialEq)]
pub struct Pic {
    pub row: usize,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
    pub path: PathBuf,
}

/// The preview: its lines and the pictures in them.
#[derive(Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub pics: Vec<Pic>,
}

/// Where pictures come from: the file's folder and a cell's size in pixels. None: none
/// drawn (the GitHub view's README, from the web).
pub type Pics<'a> = Option<(&'a Path, (u16, u16))>;

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

/// A block being gathered: its first-row prefix, the indent of its other rows, its text.
struct Para {
    lead: Vec<Span<'static>>,
    hang: usize,
    text: String,
    base: Style,
    /// List item indent in the source: deeper lines continue it.
    indent: Option<usize>,
    /// A quote's bar (every row keeps it).
    bar: Option<Style>,
}

/// The alerts GitHub draws from `> [!KIND]`: their title, mark and colour.
const ALERTS: [(&str, &str, &str, Color); 5] = [
    ("NOTE", "Note", "ⓘ", Color::Rgb(0x44, 0x93, 0xf8)),
    ("TIP", "Tip", "✦", Color::Rgb(0x3f, 0xb9, 0x50)),
    ("IMPORTANT", "Important", "❢", Color::Rgb(0xab, 0x7d, 0xf8)),
    ("WARNING", "Warning", "⚠", Color::Rgb(0xd2, 0x99, 0x22)),
    ("CAUTION", "Caution", "⊘", Color::Rgb(0xf8, 0x51, 0x49)),
];

struct R<'a> {
    lk: Look,
    width: usize,
    out: Vec<Line<'static>>,
    pics: Vec<Pic>,
    env: Pics<'a>,
}

impl R<'_> {
    /// GitHub's margin between blocks: one blank row.
    fn gap(&mut self) {
        if self.out.last().is_some_and(|l| l.width() > 0) {
            self.out.push(Line::default());
        }
    }

    fn flush(&mut self, cur: &mut Option<Para>) {
        let Some(p) = cur.take() else { return };
        // A paragraph that is only a picture (or a link around one): the picture.
        if p.lead.is_empty() && p.bar.is_none() && self.lone_picture(&p.text) {
            return;
        }
        let body = inline(p.text.trim_end_matches("<br>"), p.base, &self.lk);
        let rows = wrap(p.lead, body, p.hang, self.width);
        self.out.extend(rows);
    }

    /// `![alt](src)`, `[![alt](src)](href)` or `<img …>` alone: drawn when it can be.
    fn lone_picture(&mut self, text: &str) -> bool {
        let t = text.trim();
        let t = t.strip_prefix('[').and_then(|r| r.rsplit_once("](")).filter(|(_, href)| href.ends_with(')') && !href.contains(' ')).map_or(t, |(inner, _)| inner);
        let (src, width) = if let Some(r) = t.strip_prefix("![") {
            match r.split_once("](") {
                Some((_, rest)) if rest.ends_with(')') && !rest[..rest.len() - 1].contains(')') => (rest[..rest.len() - 1].split_whitespace().next().unwrap_or("").to_string(), None),
                _ => return false,
            }
        } else if t.starts_with("<img") && t.ends_with('>') && t.matches('<').count() == 1 {
            (attr(t, "src").unwrap_or_default(), attr(t, "width").and_then(|w| w.trim_end_matches("px").parse().ok()))
        } else {
            return false;
        };
        self.picture(&src, width, Align::Left)
    }

    /// The picture at `src` (a file beside the Markdown), `width` pixels wide (else its
    /// own), placed `align`; false: not one to draw (from the web, not a picture).
    fn picture(&mut self, src: &str, width: Option<u32>, align: Align) -> bool {
        let Some((dir, cell)) = self.env else { return false };
        if src.contains("://") || src.starts_with("data:") || src.is_empty() {
            return false;
        }
        let path = dir.join(src.split(['?', '#']).next().unwrap_or(src));
        let Some((cols, rows)) = pic_size(&path, width, self.width, cell) else { return false };
        let Some(cells) = half_blocks(&path, cols, rows) else { return false };
        let col = match align {
            Align::Left => 0,
            Align::Center => (self.width.saturating_sub(cols as usize) / 2) as u16,
            Align::Right => self.width.saturating_sub(cols as usize) as u16,
        };
        self.gap();
        self.pics.push(Pic { row: self.out.len(), col, cols, rows, path });
        for mut l in cells {
            l.spans.insert(0, Span::raw(" ".repeat(col as usize)));
            self.out.push(l);
        }
        true
    }

    fn heading(&mut self, level: usize, text: &str, align: Align) {
        self.gap();
        let st = if level == 6 { self.lk.heading.fg(P.dim) } else { self.lk.heading };
        let rows = wrap(Vec::new(), inline(text, st, &self.lk), 0, self.width);
        self.out.extend(rows.into_iter().map(|l| aligned(l, align, self.width)));
        // GitHub draws a line under the two largest.
        if level <= 2 {
            self.out.push(Line::styled("─".repeat(self.width), self.lk.rule));
        }
    }
}

/// The preview of `src` as lines, `width` wide (no pictures).
pub fn render(src: &[String], width: usize, link: Color) -> Vec<Line<'static>> {
    render_doc(src, width, link, None).lines
}

pub fn render_doc(src: &[String], width: usize, link: Color, env: Pics) -> Rendered {
    let mut r = R { lk: look(link), width: width.max(20), out: Vec::new(), pics: Vec::new(), env };
    let mut cur: Option<Para> = None;
    // Ordered lists' next numbers, by depth; the last block a list item.
    let mut numbers: Vec<Option<usize>> = Vec::new();
    let mut in_list = false;
    let mut i = 0;
    while i < src.len() {
        let line = src[i].replace('\t', "    ");
        let t = line.trim_start();
        let indent = line.len() - t.len();
        i += 1;
        if t.is_empty() {
            r.flush(&mut cur);
            r.gap();
            continue;
        }
        if t.starts_with("<!--") {
            // An HTML comment, to its end.
            let mut l = t.to_string();
            while !l.contains("-->") && i < src.len() {
                l = src[i].clone();
                i += 1;
            }
            continue;
        }
        let list_now = list_item(t).is_some() || (in_list && cur.as_ref().is_some_and(|p| p.indent.is_some_and(|n| indent > n)));
        if !list_now && indent < 2 {
            numbers.clear();
        }
        in_list = list_now;
        if let Some(fence) = ["```", "~~~"].into_iter().find(|f| t.starts_with(f)) {
            r.flush(&mut cur);
            let lang = t[3..].split_whitespace().next().unwrap_or("").to_string();
            let mut code = Vec::new();
            while i < src.len() && !src[i].trim_start().starts_with(fence) {
                code.push(src[i].clone());
                i += 1;
            }
            i += 1; // the closing fence
            r.gap();
            let block = code_block(&lang, code, r.width, &r.lk);
            r.out.extend(block);
            continue;
        }
        // HTML, to the next blank line (as GitHub takes a block of it).
        if html_start(t) && cur.is_none() {
            let mut block = vec![t.to_string()];
            while i < src.len() && !src[i].trim().is_empty() {
                block.push(src[i].trim().to_string());
                i += 1;
            }
            r.gap();
            html(&mut r, &block.join("\n"));
            continue;
        }
        let hashes = t.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ') {
            r.flush(&mut cur);
            let text = t[hashes..].trim().trim_end_matches('#').trim_end();
            r.heading(hashes, text, Align::Left);
            continue;
        }
        // Setext: a paragraph's line over `===` or `---`.
        let setext = (!t.is_empty() && t.trim_end().chars().all(|c| c == '=')).then_some(1).or((t.trim_end().len() >= 2 && t.trim_end().chars().all(|c| c == '-')).then_some(2));
        if let (Some(level), Some(p)) = (setext, cur.as_ref().filter(|p| p.lead.is_empty() && p.bar.is_none() && p.indent.is_none())) {
            let text = p.text.clone();
            cur = None;
            r.heading(level, &text, Align::Left);
            continue;
        }
        if t.len() >= 3 && t.chars().all(|c| matches!(c, '-' | ' ')) && t.contains("---")
            || t.chars().all(|c| matches!(c, '*' | ' ')) && t.matches('*').count() >= 3
            || t.chars().all(|c| matches!(c, '_' | ' ')) && t.matches('_').count() >= 3
        {
            r.flush(&mut cur);
            r.gap();
            r.out.push(Line::styled("─".repeat(r.width), r.lk.rule));
            continue;
        }
        if t.starts_with('|') {
            r.flush(&mut cur);
            let mut rows = vec![t.to_string()];
            while i < src.len() && src[i].trim_start().starts_with('|') {
                rows.push(src[i].trim().to_string());
                i += 1;
            }
            r.gap();
            let grid = table(&rows, r.width, &r.lk);
            r.out.extend(grid);
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            let q = q.strip_prefix(' ').unwrap_or(q).trim_end();
            let bar = cur.as_ref().and_then(|p| p.bar);
            if let Some(alert) = ALERTS.iter().find(|a| q.eq_ignore_ascii_case(&format!("[!{}]", a.0))).filter(|_| bar.is_none()) {
                // An alert: its bar and title in its colour.
                r.flush(&mut cur);
                r.gap();
                let st = Style::new().fg(alert.3);
                r.out.push(Line::from(vec![Span::styled("▎ ", st), Span::styled(format!("{} {}", alert.2, alert.1), st.bold())]));
                cur = Some(Para { lead: vec![Span::styled("▎ ", st)], hang: 2, text: String::new(), base: r.lk.text, indent: None, bar: Some(st) });
                continue;
            }
            match cur.as_mut().filter(|p| p.bar.is_some()) {
                // A blank quote line: the next paragraph in the quote.
                Some(p) if q.is_empty() => {
                    let (st, base) = (p.bar.unwrap_or(r.lk.bar), p.base);
                    r.flush(&mut cur);
                    r.out.push(Line::from(Span::styled("▎", st)));
                    cur = Some(Para { lead: vec![Span::styled("▎ ", st)], hang: 2, text: String::new(), base, indent: None, bar: Some(st) });
                }
                Some(p) => push_text(&mut p.text, q),
                None => {
                    r.flush(&mut cur);
                    r.gap();
                    cur = Some(Para { lead: vec![Span::styled("▎ ", r.lk.bar)], hang: 2, text: q.to_string(), base: r.lk.quote, indent: None, bar: Some(r.lk.bar) });
                }
            }
            continue;
        }
        if let Some((marker, rest)) = list_item(t) {
            let was_list = cur.as_ref().is_some_and(|p| p.indent.is_some()) || r.out.last().is_some_and(|l| l.width() > 0) && numbers.iter().any(Option::is_some);
            r.flush(&mut cur);
            if !was_list && !r.out.last().is_some_and(|l| l.spans.first().is_some_and(|s| s.content.starts_with("  "))) {
                r.gap();
            }
            let depth = indent / 2;
            numbers.truncate(depth + 1);
            numbers.resize(depth + 1, None);
            let (glyph, rest) = match rest.strip_prefix("[ ] ").map(|r| ("☐", r)).or_else(|| rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")).map(|r| ("☑", r))) {
                Some((g, r)) => (g.to_string(), r),
                None if marker.ends_with(['.', ')']) => {
                    // GitHub counts on from the first item's number.
                    let n = numbers[depth].unwrap_or_else(|| marker[..marker.len() - 1].parse().unwrap_or(1));
                    numbers[depth] = Some(n + 1);
                    (format!("{n}."), rest)
                }
                None => {
                    numbers[depth] = None;
                    (["•", "◦", "▪"][depth % 3].to_string(), rest)
                }
            };
            let pad = "  ".repeat(depth + 1);
            let hang = pad.width() + glyph.width() + 1;
            cur = Some(Para {
                lead: vec![Span::raw(pad), Span::styled(format!("{glyph} "), r.lk.bullet)],
                hang,
                text: rest.to_string(),
                base: r.lk.text,
                indent: Some(indent),
                bar: None,
            });
            continue;
        }
        if indent >= 4 && cur.is_none() {
            // An indented code block.
            let mut code = vec![line[4.min(line.len())..].to_string()];
            while i < src.len() && (src[i].starts_with("    ") || src[i].starts_with('\t')) {
                code.push(src[i].replace('\t', "    ")[4..].to_string());
                i += 1;
            }
            r.gap();
            let block = code_block("", code, r.width, &r.lk);
            r.out.extend(block);
            continue;
        }
        match cur.as_mut() {
            // Continuation: a paragraph's next line, or deeper than its list item.
            Some(p) if p.indent.is_none_or(|n| indent > n) && p.bar.is_none() => push_text(&mut p.text, t),
            _ => {
                r.flush(&mut cur);
                r.gap();
                cur = Some(Para { lead: Vec::new(), hang: 0, text: t.to_string(), base: r.lk.text, indent: None, bar: None });
            }
        }
        // Two trailing spaces or a backslash end the row here.
        if line.ends_with("  ") || line.ends_with('\\') {
            if let Some(p) = cur.as_mut() {
                p.text = format!("{}<br>", p.text.trim_end_matches('\\').trim_end());
            }
        }
    }
    r.flush(&mut cur);
    while r.out.last().is_some_and(|l| l.width() == 0) {
        r.out.pop();
    }
    Rendered { lines: r.out, pics: r.pics }
}

fn push_text(text: &mut String, more: &str) {
    if !text.is_empty() && !text.ends_with("<br>") {
        text.push(' ');
    }
    text.push_str(more.trim());
}

/// `- x`, `* x`, `+ x`, `1. x`, `1) x`: the marker and the text.
fn list_item(t: &str) -> Option<(&str, &str)> {
    if let Some(rest) = t.strip_prefix(['-', '*', '+']).and_then(|r| r.strip_prefix(' ')) {
        return Some((&t[..1], rest));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let after = t.get(digits..digits + 2)?;
    (digits > 0 && digits <= 9 && (after == ". " || after == ") ")).then(|| (&t[..digits + 1], &t[digits + 2..]))
}

/// A line starting an HTML block: a tag (not an autolink).
fn html_start(t: &str) -> bool {
    let r = t.strip_prefix("</").or_else(|| t.strip_prefix('<'));
    r.is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && !r.starts_with("http") && r.chars().take_while(char::is_ascii_alphanumeric).count() > 0 && t.contains('>'))
}

/// An HTML attribute's value: `name="v"`, `name='v'` or `name=v`.
fn attr(tag: &str, name: &str) -> Option<String> {
    let low = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = low[from..].find(name).map(|a| a + from) {
        from = at + name.len();
        let before_ok = at == 0 || !low.as_bytes()[at - 1].is_ascii_alphanumeric() && low.as_bytes()[at - 1] != b'-';
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=').filter(|_| before_ok) else { continue };
        let rest = rest.trim_start();
        return Some(match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or("").to_string(),
            _ => rest.split(|c: char| c.is_whitespace() || c == '>').next().unwrap_or("").trim_end_matches('/').to_string(),
        });
    }
    None
}

/// The alignment an HTML tag asks for.
fn align_of(tag: &str) -> Option<Align> {
    match attr(tag, "align")?.to_ascii_lowercase().as_str() {
        "center" | "middle" => Some(Align::Center),
        "right" => Some(Align::Right),
        "left" => Some(Align::Left),
        _ => None,
    }
}

fn aligned(mut l: Line<'static>, align: Align, width: usize) -> Line<'static> {
    let pad = match align {
        Align::Left => 0,
        Align::Center => width.saturating_sub(l.width()) / 2,
        Align::Right => width.saturating_sub(l.width()),
    };
    if pad > 0 {
        l.spans.insert(0, Span::raw(" ".repeat(pad)));
    }
    l
}

/// A block of HTML, as GitHub shows the tags READMEs use: paragraphs and headings
/// (aligned), pictures, line breaks, `<details>` open under its summary, lists; the
/// inline tags inside them; anything else as its text.
fn html(r: &mut R, block: &str) {
    // The paragraph being gathered (as inline Markdown-with-HTML), its alignment, its
    // heading level, its lead.
    let mut text = String::new();
    let mut aligns: Vec<Align> = Vec::new();
    let mut level = 0usize;
    let mut lead = String::new();
    fn flush(r: &mut R, text: &mut String, aligns: &[Align], level: usize, lead: &mut String) {
        // HTML's white space: any run of it is one space.
        let t = text.split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches("<br>").to_string();
        text.clear();
        let lead = std::mem::take(lead);
        if t.is_empty() {
            return;
        }
        let align = aligns.last().copied().unwrap_or(Align::Left);
        if level > 0 {
            return r.heading(level, &t, align);
        }
        let hang = lead.width();
        let rows = wrap(if lead.is_empty() { Vec::new() } else { vec![Span::styled(lead, r.lk.bullet)] }, inline(&t, r.lk.text, &r.lk), hang, r.width);
        r.out.extend(rows.into_iter().map(|l| aligned(l, align, r.width)));
    }
    let mut rest = block;
    while !rest.is_empty() {
        if let Some(c) = rest.strip_prefix("<!--") {
            rest = c.split_once("-->").map_or("", |(_, a)| a);
            continue;
        }
        let tag_at = rest.find('<').filter(|&at| html_start(&rest[at..]));
        let Some(at) = tag_at else {
            text.push_str(rest);
            break;
        };
        text.push_str(&rest[..at]);
        let Some(end) = rest[at..].find('>') else {
            text.push_str(&rest[at..]);
            break;
        };
        let tag = &rest[at..at + end + 1];
        rest = &rest[at + end + 1..];
        let closing = tag.starts_with("</");
        let name: String = tag.trim_start_matches(['<', '/']).chars().take_while(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
        match name.as_str() {
            "p" | "div" | "center" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "details" | "summary" | "ul" | "ol" | "li" | "table" | "tr" | "picture" | "blockquote" | "hr" => {
                flush(r, &mut text, &aligns, level, &mut lead);
                if closing {
                    if matches!(name.as_str(), "p" | "div" | "center" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
                        aligns.pop();
                        level = 0;
                        r.gap();
                    }
                    continue;
                }
                match name.as_str() {
                    "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        aligns.push(align_of(tag).unwrap_or(aligns.last().copied().unwrap_or(Align::Left)));
                        level = name.strip_prefix('h').and_then(|n| n.parse().ok()).unwrap_or(0);
                    }
                    "center" => aligns.push(Align::Center),
                    "summary" => lead = "▾ ".into(),
                    "li" => lead = "  • ".into(),
                    "hr" => r.out.push(Line::styled("─".repeat(r.width), r.lk.rule)),
                    _ => {}
                }
            }
            // A picture of its own (in its paragraph, alone so far): drawn; else inline.
            "img" if !closing => {
                let align = aligns.last().copied().unwrap_or(Align::Left);
                let src = attr(tag, "src").unwrap_or_default();
                let width = attr(tag, "width").and_then(|w| w.trim_end_matches("px").parse().ok());
                let alone = strip_tags(&text).trim().is_empty();
                if alone && r.env.is_some() && !src.contains("://") {
                    flush(r, &mut text, &aligns, level, &mut lead);
                    if r.picture(&src, width, align) {
                        continue;
                    }
                }
                text.push_str(tag);
            }
            // Its own `<source>`s are for other screens; its `<img>` is the picture.
            "source" => {}
            "br" => text.push_str("<br>"),
            _ => text.push_str(tag),
        }
    }
    flush(r, &mut text, &aligns, level, &mut lead);
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// HTML entities as their characters.
fn entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10);
        let named = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => "&",
                "lt" => "<",
                "gt" => ">",
                "quot" => "\"",
                "apos" | "#39" => "'",
                "nbsp" => " ",
                "copy" => "©",
                "reg" => "®",
                "trade" => "™",
                "mdash" => "—",
                "ndash" => "–",
                "hellip" => "…",
                "middot" => "·",
                "times" => "×",
                "rarr" => "→",
                "larr" => "←",
                _ => "",
            };
            let num = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")).and_then(|h| u32::from_str_radix(h, 16).ok()).or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok())).and_then(char::from_u32);
            match (c, num) {
                ("", Some(ch)) => Some((ch.to_string(), e)),
                ("", None) => None,
                (c, _) => Some((c.to_string(), e)),
            }
        });
        match named {
            Some((c, e)) => {
                out.push_str(&c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// GitHub's `:shortcodes:` READMEs use most.
fn emoji(code: &str) -> Option<&'static str> {
    Some(match code {
        "rocket" => "🚀",
        "tada" => "🎉",
        "sparkles" => "✨",
        "white_check_mark" | "heavy_check_mark" => "✅",
        "x" => "❌",
        "warning" => "⚠️",
        "bulb" => "💡",
        "memo" => "📝",
        "bug" => "🐛",
        "fire" => "🔥",
        "star" => "⭐",
        "heart" => "❤️",
        "+1" | "thumbsup" => "👍",
        "-1" | "thumbsdown" => "👎",
        "construction" => "🚧",
        "zap" => "⚡",
        "package" => "📦",
        "wrench" => "🔧",
        "hammer" => "🔨",
        "lock" => "🔒",
        "books" => "📚",
        "book" => "📖",
        "link" => "🔗",
        "point_right" => "👉",
        "information_source" => "ℹ️",
        "exclamation" => "❗",
        "question" => "❓",
        "smile" => "😄",
        "eyes" => "👀",
        "computer" => "💻",
        "gear" => "⚙️",
        "art" => "🎨",
        "heavy_plus_sign" => "➕",
        "arrow_right" => "➡️",
        "new" => "🆕",
        "pushpin" => "📌",
        "mag" => "🔍",
        "clipboard" => "📋",
        "rotating_light" => "🚨",
        "recycle" => "♻️",
        "globe_with_meridians" => "🌐",
        "penguin" => "🐧",
        "crab" => "🦀",
        _ => return None,
    })
}

/// Inline Markdown and HTML: code, bold, italic, strikethrough, links (and bare URLs),
/// pictures (as their name in a chip), line breaks, `:emoji:`, entities.
fn inline(text: &str, base: Style, lk: &Look) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    let mut prev: Option<char> = None;
    let flush = |plain: &mut String, out: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            out.push(Span::styled(entities(&std::mem::take(plain)), base));
        }
    };
    while let Some(c) = rest.chars().next() {
        let boundary = prev.is_none_or(|p| !p.is_alphanumeric());
        if c == '\\' && rest.len() > 1 {
            let n = rest[1..].chars().next().map_or(1, char::len_utf8);
            plain.push_str(&rest[1..1 + n]);
            prev = rest[1..].chars().next();
            rest = &rest[1 + n..];
            continue;
        }
        if let Some(tail) = rest.strip_prefix('`') {
            let ticks = 1 + tail.bytes().take_while(|&b| b == b'`').count();
            let mark = "`".repeat(ticks);
            if let Some((body, after)) = rest[ticks..].split_once(mark.as_str()).filter(|(b, _)| !b.is_empty()) {
                flush(&mut plain, &mut out);
                out.push(Span::styled(format!(" {} ", body.trim()), base.patch(lk.code)));
                rest = after;
                prev = Some('`');
                continue;
            }
        }
        // `_` and `__` only at a word's edges (snake_case stays as it is).
        let emphasis = [("***", Style::new().bold().italic()), ("**", Style::new().bold()), ("__", Style::new().bold()), ("~~", Style::new().crossed_out()), ("*", Style::new().italic()), ("_", Style::new().italic())];
        if let Some((m, st, body, after)) = emphasis.iter().find_map(|&(m, st)| {
            if m.starts_with('_') && !boundary {
                return None;
            }
            let (body, after) = rest.strip_prefix(m)?.split_once(m)?;
            let edge_ok = !m.starts_with('_') || after.chars().next().is_none_or(|c| !c.is_alphanumeric());
            (!body.is_empty() && !body.starts_with(' ') && !body.ends_with(' ') && edge_ok).then_some((m, st, body, after))
        }) {
            let _ = m;
            flush(&mut plain, &mut out);
            out.extend(inline(body, base.patch(st), lk));
            rest = after;
            prev = Some('*');
            continue;
        }
        // HTML: the tags READMEs use inline.
        if c == '<' && html_start(rest) {
            if let Some(end) = rest.find('>') {
                let tag = &rest[..=end];
                let name: String = tag.trim_start_matches(['<', '/']).chars().take_while(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
                let after = &rest[end + 1..];
                flush(&mut plain, &mut out);
                let closing = tag.starts_with("</");
                let style = match name.as_str() {
                    "b" | "strong" => Some(Style::new().bold()),
                    "i" | "em" => Some(Style::new().italic()),
                    "s" | "del" | "strike" => Some(Style::new().crossed_out()),
                    "u" | "ins" => Some(Style::new().underlined()),
                    "code" | "kbd" | "samp" | "tt" => Some(lk.code),
                    "a" => Some(lk.link),
                    "sub" | "sup" | "span" | "small" | "mark" | "abbr" | "font" => Some(Style::new()),
                    _ => None,
                };
                rest = after;
                prev = Some('>');
                match (name.as_str(), style, closing) {
                    ("br", ..) => out.push(Span::raw("\n")),
                    ("img", ..) => out.push(chip(&attr(tag, "alt").filter(|a| !a.is_empty()).unwrap_or_else(|| "picture".into()), lk)),
                    (_, Some(st), false) => {
                        let close = format!("</{name}>");
                        let (body, after) = match rest.to_ascii_lowercase().find(&close) {
                            Some(at) => (&rest[..at], &rest[at + close.len()..]),
                            None => (rest, ""),
                        };
                        let pad = if matches!(name.as_str(), "code" | "kbd" | "samp" | "tt") { " " } else { "" };
                        let mut inner = inline(&format!("{pad}{body}{pad}"), base.patch(st), lk);
                        out.append(&mut inner);
                        rest = after;
                    }
                    // A tag termcode does not draw: dropped, its text kept.
                    _ => {}
                }
                continue;
            }
        }
        let image = rest.starts_with("![");
        if let Some(tail) = rest.strip_prefix('!').unwrap_or(rest).strip_prefix('[') {
            if let Some((label, tail)) = split_label(tail) {
                if let Some((_, after)) = tail.split_once(')') {
                    flush(&mut plain, &mut out);
                    if image {
                        out.push(chip(if label.is_empty() { "picture" } else { label }, lk));
                    } else {
                        out.extend(inline(label, base.patch(lk.link), lk));
                    }
                    rest = after;
                    prev = Some(')');
                    continue;
                }
            }
        }
        if let Some((url, after)) = rest.strip_prefix('<').and_then(|t| t.split_once('>')).filter(|(u, _)| u.starts_with("http")) {
            flush(&mut plain, &mut out);
            out.push(Span::styled(url.to_string(), base.patch(lk.link)));
            rest = after;
            continue;
        }
        // A bare URL is a link too.
        if boundary && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let end = rest.find(|c: char| c.is_whitespace() || c == '<').unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
            flush(&mut plain, &mut out);
            out.push(Span::styled(url.to_string(), base.patch(lk.link)));
            rest = &rest[url.len()..];
            prev = Some('/');
            continue;
        }
        if c == ':' {
            if let Some(code) = rest[1..].split(':').next().filter(|c| !c.is_empty() && c.len() < 32 && c.chars().all(|ch| ch.is_ascii_alphanumeric() || "_+-".contains(ch))) {
                if let Some(e) = emoji(code) {
                    plain.push_str(e);
                    rest = &rest[code.len() + 2..];
                    prev = Some(':');
                    continue;
                }
            }
        }
        plain.push(c);
        prev = Some(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut plain, &mut out);
    out
}

/// A link's label up to its `](`, nested brackets (a picture in a link) included.
fn split_label(tail: &str) -> Option<(&str, &str)> {
    let mut depth = 0;
    for (i, c) in tail.char_indices() {
        match c {
            '[' => depth += 1,
            ']' if depth == 0 => return tail[i..].strip_prefix("](").map(|t| (&tail[..i], t)),
            ']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// A picture's name in a chip (a badge, a picture from the web).
fn chip(name: &str, lk: &Look) -> Span<'static> {
    Span::styled(format!(" {} ", entities(name)), lk.chip)
}

/// `lead` then `body`, word-wrapped to `width`; rows after the first start with `hang`
/// blanks (a quote's: its bar). A word longer than a row is cut; "\n" breaks the row.
fn wrap(lead: Vec<Span<'static>>, body: Vec<Span<'static>>, hang: usize, width: usize) -> Vec<Line<'static>> {
    let width = width.max(hang + if hang > 0 { 8 } else { 1 });
    let indent = |lead: &[Span<'static>]| {
        // A quote's bar continues down its rows.
        match lead.first() {
            Some(s) if s.content.starts_with('▎') => vec![s.clone()],
            _ => vec![Span::raw(" ".repeat(hang))],
        }
    };
    let next = indent(&lead);
    let mut rows = vec![lead];
    let mut x: usize = rows[0].iter().map(Span::width).sum();
    for span in body {
        if span.content == "\n" {
            rows.push(next.clone());
            x = hang;
            continue;
        }
        for word in span.content.split_inclusive(' ') {
            let mut word = word.to_string();
            if x + word.trim_end().width() > width && x > hang && word.trim_end().width() <= width - hang {
                rows.push(next.clone());
                x = hang;
                word = word.trim_start().to_string();
            }
            loop {
                let room = width.saturating_sub(x);
                if word.trim_end().width() <= room {
                    break;
                }
                let mut cut = 0;
                let mut w = 0;
                for (i, c) in word.char_indices() {
                    let cw = c.to_string().width();
                    if w + cw > room {
                        break;
                    }
                    w += cw;
                    cut = i + c.len_utf8();
                }
                if cut == 0 && x > hang {
                    rows.push(next.clone());
                    x = hang;
                    continue;
                }
                let cut = if cut == 0 { word.chars().next().map_or(word.len(), char::len_utf8) } else { cut };
                rows.last_mut().unwrap().push(Span::styled(word[..cut].to_string(), span.style));
                rows.push(next.clone());
                x = hang;
                word = word[cut..].to_string();
            }
            if !word.is_empty() {
                x += word.width();
                rows.last_mut().unwrap().push(Span::styled(word, span.style));
            }
        }
    }
    rows.into_iter().map(Line::from).collect()
}

/// A fence's info word as the extension the editor's syntaxes know.
fn ext(lang: &str) -> &str {
    match lang.to_ascii_lowercase().as_str() {
        "rust" => "rs",
        "python" | "python3" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "shell" | "console" | "shell-session" | "zsh" | "sh" | "bash" => "sh",
        "golang" => "go",
        "ruby" => "rb",
        "yml" => "yaml",
        "c++" => "cpp",
        "jsonc" | "json5" => "json",
        _ => lang,
    }
}

/// Fenced or indented code on the code background, highlighted as its language.
fn code_block(lang: &str, code: Vec<String>, width: usize, lk: &Look) -> Vec<Line<'static>> {
    let n = code.len();
    let mut b = Buffer::from_lines(&format!("code.{}", ext(lang)), code, "preview");
    b.highlight_upto(n);
    let pad = |l: &mut Line<'static>| {
        let w = l.width();
        if w < width {
            l.spans.push(Span::styled(" ".repeat(width - w), lk.block));
        }
    };
    let mut out = vec![Line::default()];
    for i in 0..n {
        let mut l = crate::ui::render_line(&b.lines[i], b.segs(i), &[], None, 0, width.saturating_sub(4), lk.block);
        l.spans.insert(0, Span::styled("  ", lk.block));
        pad(&mut l);
        out.push(l);
    }
    pad(&mut out[0]);
    let mut last = Line::default();
    pad(&mut last);
    out.push(last);
    out
}

/// A pipe table as GitHub's grid: every cell framed, the header bold, columns aligned as
/// the delimiter row says, cells wrapped to fit the pane, body rows striped.
fn table(rows: &[String], width: usize, lk: &Look) -> Vec<Line<'static>> {
    let cells = |r: &str| -> Vec<String> {
        let r = r.trim();
        let r = r.strip_prefix('|').unwrap_or(r);
        let r = r.strip_suffix('|').unwrap_or(r);
        // `\|` is a pipe in a cell.
        let mut out = vec![String::new()];
        let mut esc = false;
        for c in r.chars() {
            match c {
                '|' if !esc => out.push(String::new()),
                '\\' if !esc => esc = true,
                c => {
                    if esc && c != '|' {
                        out.last_mut().unwrap().push('\\');
                    }
                    esc = false;
                    out.last_mut().unwrap().push(c);
                }
            }
        }
        out.into_iter().map(|c| c.trim().to_string()).collect()
    };
    let mut grid: Vec<Vec<String>> = rows.iter().map(|r| cells(r)).collect();
    let rule = |r: &[String]| r.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
    let header = grid.len() > 1 && rule(&grid[1]);
    let aligns: Vec<Align> = if header {
        grid.remove(1)
            .iter()
            .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            })
            .collect()
    } else {
        Vec::new()
    };
    let spans: Vec<Vec<Vec<Span<'static>>>> = grid
        .iter()
        .enumerate()
        .map(|(r, row)| row.iter().map(|c| inline(c, if header && r == 0 { lk.text.bold() } else { lk.text }, lk)).collect())
        .collect();
    let cols = spans.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let natural: Vec<usize> = (0..cols).map(|c| spans.iter().filter_map(|r| r.get(c)).map(|s| s.iter().map(Span::width).sum::<usize>()).max().unwrap_or(0).max(1)).collect();
    // Too wide: the widest columns give way first.
    let room = width.saturating_sub(3 * cols + 1).max(cols);
    let mut widths = natural.clone();
    if natural.iter().sum::<usize>() > room {
        let mut order: Vec<usize> = (0..cols).collect();
        order.sort_by_key(|&c| natural[c]);
        let mut left = room;
        for (k, &c) in order.iter().enumerate() {
            let share = left / (cols - k);
            widths[c] = natural[c].min(share).max(1);
            left -= widths[c];
        }
    }
    let line = |l: char, m: char, r: char| {
        let mut s = String::from(l);
        for (c, w) in widths.iter().enumerate() {
            s.push_str(&"─".repeat(w + 2));
            s.push(if c + 1 == cols { r } else { m });
        }
        Line::styled(s, lk.rule)
    };
    let mut out = vec![line('┌', '┬', '┐')];
    for (r, row) in spans.into_iter().enumerate() {
        let bg = if header && r > 0 && r % 2 == 0 || !header && r % 2 == 1 { Some(lk.stripe) } else { None };
        let wrapped: Vec<Vec<Line<'static>>> = (0..cols).map(|c| wrap(Vec::new(), row.get(c).cloned().unwrap_or_default(), 0, widths[c])).collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for k in 0..height {
            let mut l = vec![Span::styled("│", lk.rule)];
            for (c, cell) in wrapped.iter().enumerate() {
                let content = cell.get(k).cloned().unwrap_or_default();
                let w = content.width();
                let free = widths[c].saturating_sub(w);
                let (before, after) = match aligns.get(c).copied().unwrap_or(Align::Left) {
                    Align::Left => (0, free),
                    Align::Center => (free / 2, free - free / 2),
                    Align::Right => (free, 0),
                };
                let fill = |n: usize| Span::styled(" ".repeat(n), bg.map_or(Style::new(), |b| Style::new().bg(b)));
                l.push(fill(1 + before));
                l.extend(content.spans.into_iter().map(|s| match bg {
                    Some(b) if s.style.bg.is_none() => Span::styled(s.content, s.style.bg(b)),
                    _ => s,
                }));
                l.push(fill(after + 1));
                l.push(Span::styled("│", lk.rule));
            }
            out.push(Line::from(l));
        }
        if header && r == 0 {
            out.push(line('├', '┼', '┤'));
        }
    }
    out.push(line('└', '┴', '┘'));
    out
}

/// A picture's size in cells: its own pixels (or `width` wide), at most the pane's
/// width and 60 rows.
fn pic_size(path: &Path, width: Option<u32>, max_cols: usize, (cw, ch): (u16, u16)) -> Option<(u16, u16)> {
    let (w, h) = image::image_dimensions(path).ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    let (cw, ch) = (f64::from(cw.max(1)), f64::from(ch.max(1)));
    let want = f64::from(width.unwrap_or(w).max(1));
    let mut cols = (want / cw).round().clamp(1.0, max_cols as f64);
    let mut rows = (cols * cw * f64::from(h) / f64::from(w) / ch).round().max(1.0);
    if rows > 60.0 {
        rows = 60.0;
        cols = (rows * ch * f64::from(w) / f64::from(h) / cw).round().max(1.0);
    }
    Some((cols as u16, rows as u16))
}

/// A picture as half-blocks (two pixels a cell): `rows` lines `cols` wide. Kept by file,
/// time and size: a preview is drawn again at each edit of its source.
pub(crate) fn half_blocks(path: &Path, cols: u16, rows: u16) -> Option<Vec<Line<'static>>> {
    type Key = (PathBuf, Option<std::time::SystemTime>, u16, u16, Color);
    static KEPT: std::sync::Mutex<Vec<(Key, Vec<Line<'static>>)>> = std::sync::Mutex::new(Vec::new());
    let key: Key = (path.to_path_buf(), std::fs::metadata(path).and_then(|m| m.modified()).ok(), cols, rows, P.editor_bg);
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, lines)) = kept.iter().find(|(k, _)| *k == key) {
        return Some(lines.clone());
    }
    let img = image::open(path).ok()?.to_rgba8();
    let small = imageops::resize(&img, u32::from(cols), u32::from(rows) * 2, imageops::FilterType::Triangle);
    let bg = crate::settingsgraphics::pixel(P.editor_bg);
    let over = |p: &Rgba<u8>| {
        let a = u16::from(p[3]);
        let mix = |c: usize| ((u16::from(p[c]) * a + u16::from(bg[c]) * (255 - a)) / 255) as u8;
        Color::Rgb(mix(0), mix(1), mix(2))
    };
    let lines: Vec<Line<'static>> = (0..u32::from(rows))
        .map(|y| Line::from((0..u32::from(cols)).map(|x| Span::styled("▀", Style::new().fg(over(small.get_pixel(x, 2 * y))).bg(over(small.get_pixel(x, 2 * y + 1))))).collect::<Vec<_>>()))
        .collect();
    // ponytail: the 16 latest; a README has a few pictures.
    if kept.len() >= 16 {
        kept.remove(0);
    }
    kept.push((key, lines.clone()));
    Some(lines)
}

/// A picture's pixels for the terminal (Kitty): `w`×`h`, over the editor's background.
pub fn pixels(path: &Path, (w, h): (u32, u32)) -> Option<RgbaImage> {
    let img = image::open(path).ok()?.to_rgba8();
    let img = imageops::resize(&img, w, h, imageops::FilterType::CatmullRom);
    let mut canvas = RgbaImage::from_pixel(w, h, crate::settingsgraphics::pixel(P.editor_bg));
    imageops::overlay(&mut canvas, &img, 0, 0);
    Some(canvas)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(src: &str, width: usize) -> Vec<String> {
        let lines: Vec<String> = src.lines().map(str::to_string).collect();
        render(&lines, width, Color::Blue).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn paragraphs_reflow_and_lists_hang_under_their_text() {
        let out = text("# Title\n\nOne two\nthree four five six seven.\n\n- an item that wraps onto\n  a second source line\n  1. nested\n  1. counted\n", 24);
        assert_eq!(
            out,
            [
                "Title",
                "────────────────────────",
                "",
                "One two three four five",
                "six seven.",
                "",
                "  • an item that wraps",
                "    onto a second source",
                "    line",
                "    1. nested",
                "    2. counted",
            ]
        );
        // A heading right under text still gets its margin; setext headings too.
        let rule = "─".repeat(20);
        assert_eq!(text("para\n## Next\nSub\n---\nend", 20), ["para", "", "Next", &rule, "", "Sub", &rule, "", "end"]);
    }

    #[test]
    fn inline_marks_become_styles_not_text() {
        let lk = look(Color::Blue);
        let spans = inline("a `code` **bold** _it_ snake_case [link](http://x) ![pic](p.png) \\* &amp; :rocket: <kbd>Ctrl</kbd>", lk.text, &lk);
        let s: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(s, "a  code  bold it snake_case link  pic  * & 🚀  Ctrl ");
        assert!(spans.iter().any(|s| s.content == "bold" && s.style.add_modifier.contains(Modifier::BOLD)));
        assert!(spans.iter().any(|s| s.content == "it" && s.style.add_modifier.contains(Modifier::ITALIC)));
        assert!(spans.iter().any(|s| s.content == "link" && s.style.fg == Some(Color::Blue)));
        let url: Vec<String> = inline("see https://x.org/a.", lk.text, &lk).iter().map(|s| s.content.to_string()).collect();
        assert_eq!(url, ["see ", "https://x.org/a", "."], "a bare URL is a link, its full stop is not");
    }

    #[test]
    fn code_tables_quotes_alerts_rules_and_comments() {
        let out = text("```sh\necho hi\n```\n| Key | Action |\n|:-:|---|\n| F1 | palette |\n\n> quoted\n> more\n\n> [!WARNING]\n> careful\n\n---\n<!-- hidden -->\nend\n", 30);
        assert_eq!(
            out,
            [
                "",
                "  echo hi",
                "",
                "",
                "┌─────┬─────────┐",
                "│ Key │ Action  │",
                "├─────┼─────────┤",
                "│ F1  │ palette │",
                "└─────┴─────────┘",
                "",
                "▎ quoted more",
                "",
                "▎ ⚠ Warning",
                "▎ careful",
                "",
                "──────────────────────────────",
                "",
                "end"
            ]
        );
        // Too wide: cells wrap inside the grid.
        let out = text("| a | b |\n|---|---|\n| one two three four | x |\n", 20);
        assert!(out.iter().all(|l| l.width() <= 20), "{out:#?}");
        assert!(out.iter().filter(|l| l.contains("│ x")).count() == 1 && out.len() == 6, "{out:#?}");
    }

    #[test]
    fn html_readme_headers_center_and_pictures_draw() {
        let dir = std::env::temp_dir().join(format!("termcode-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        RgbaImage::from_pixel(80, 40, Rgba([255, 0, 0, 255])).save(dir.join("logo.png")).unwrap();
        let src: Vec<String> = "<p align=\"center\">\n  <img src=\"logo.png\" width=\"80\">\n</p>\n<h1 align=\"center\">Name</h1>\n<p align=\"center\">\n  <a href=\"x\"><img src=\"https://badge\" alt=\"CI\"></a> fast &amp; small<br>two\n</p>\n\n<details>\n<summary>More</summary>\n\nhidden text\n</details>\n\n![shot](logo.png)\n".lines().map(str::to_string).collect();
        let doc = render_doc(&src, 40, Color::Blue, Some((&dir, (8, 16))));
        let rows: Vec<String> = doc.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect();
        // The logo: 80 px = 10 cells at 8 px, centered; 40 px tall = 20 px a 10-cell row → 3 rows (of 16 px).
        assert_eq!(doc.pics[0], Pic { row: 0, col: 15, cols: 10, rows: 3, path: dir.join("logo.png") });
        assert!(rows[0].starts_with(&format!("{}▀", " ".repeat(15))), "{rows:#?}");
        let name = rows.iter().find(|r| r.contains("Name")).unwrap();
        assert_eq!(name.trim_start().len(), 4);
        assert_eq!(name.len() - 4, (40 - 4) / 2, "centered");
        assert!(rows.iter().any(|r| r.contains(" CI  fast & small")), "a badge from the web: its name\n{rows:#?}");
        let two: Vec<String> = render_doc(&["<p>".into(), "<img src=\"https://a\" alt=\"A\">".into(), "<img src=\"https://b\" alt=\"B\">".into(), "</p>".into()], 40, Color::Blue, None).lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect();
        assert_eq!(two, [" A   B"], "badges side by side");
        assert!(rows.iter().any(|r| r.trim() == "two"), "<br>");
        assert!(rows.iter().any(|r| r.starts_with("▾ More")) && rows.iter().any(|r| r == "hidden text"), "{rows:#?}");
        assert_eq!(doc.pics.len(), 2, "the Markdown picture too");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
