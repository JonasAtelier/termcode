//! Drawing, VS Code layout: title bar, activity bar, resizable sidebar, editor
//! groups (tabs, text or side-by-side diff, find widget), status bar, popups.
//! Every clickable area is recorded in `app.ui` from the rect actually drawn,
//! so mouse targets stay exact in both densities.

use crate::decor::{self, Guides, Hunk, Mark, Paint, Rainbow};
use crate::minimap::{self, Mini};
use crate::diff::Kind;
use crate::editor::{self, Buffer, View};
use crate::git::Section;
use crate::icons::{file_icon, folder_icon};
use crate::settings::Spacing;
use crate::panel::{
    base, App, DiffDoc, DiffId, Doc, Drag, Find, FindBtn, Focus, GroupHit, HRow, Hits, MiniHit, PeekBtn, ScmAct, ScrollHit,
    ScrollTarget, Side, SRow, Zone, COMMANDS, SIDES,
};
use crate::blocks::BlockStyle;
use crate::theme::{git_color, icons, Rgba, P};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use syntect::highlighting::{FontStyle, Style as SStyle};
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
mod visual_tests {
    use ratatui::layout::Rect;

    #[test]
    fn control_characters_go_but_a_pictures_escapes_stay() {
        let mut b = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 6, 2));
        b[(1, 0)].set_symbol("\t");
        b[(2, 0)].set_symbol("\x1b[31m");
        b[(4, 1)].set_symbol("\x1b_Gq=2;\x1b\\");
        super::scrub(&mut b, &[Rect::new(3, 1, 3, 1)]);
        assert_eq!((b[(1, 0)].symbol(), b[(2, 0)].symbol()), (" ", " "));
        assert_eq!(b[(4, 1)].symbol(), "\x1b_Gq=2;\x1b\\", "a picture's cell is left as it is");
    }

    #[test]
    fn word_span_is_bounded_and_utf8_safe() {
        let (old, new) = super::diff_word_spans("same old 中 end", "same new 文 end");
        assert_eq!(old, Some(5..12));
        assert_eq!(new, Some(5..12));
        assert!(super::diff_word_spans(&"x".repeat(2049), "new").0.is_none());
        assert!(super::diff_word_spans("same", "same").1.is_none());
    }
}

mod diffs;
mod panels;
mod text;
pub(crate) use diffs::*;
use panels::*;
use text::*;

/// `workbench.density` plus `termcode.spacing`: comfortable (default) frames
/// popups and pads the gutter; compact is the original tight layout.
#[derive(Clone, Copy)]
struct Lay {
    comfy: bool,
    s: Spacing,
    /// Inner horizontal padding of sidebar rows and inputs.
    pad: u16,
}

fn lay(app: &App) -> Lay {
    let s = app.settings.spacing;
    Lay { comfy: !app.settings.compact, s, pad: s.sidebar_padding }
}

/// Truncate to `w` display cells with an ellipsis (none in 0 cells; no blank before it).
pub(crate) fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// A row's text in `room` cells: `main` cut with one "…", then `extra` (the dim author or
/// folder, with its leading blank) only while `main` is whole and a few cells of it fit.
/// Never "name… …" or "subject……".
pub(crate) fn fit_pair(main: &str, extra: &str, room: usize) -> (String, String) {
    let m = fit(main, room);
    let left = room.saturating_sub(m.width());
    let x = if m == main && left >= 4 { fit(extra, left) } else { String::new() };
    (m, x)
}

/// Cells a row's text keeps in `w` left of its right-aligned `meta` (time, badges, +N):
/// two blank cells between them at least. The meta's own leading blanks count, a
/// badge's padding (it has a background) does not.
pub(crate) fn room_before(meta: &[Span], w: usize) -> usize {
    let mut blank = 0;
    for s in meta {
        let t = s.content.as_ref();
        let lead = if s.style.bg.is_some() { 0 } else { t.len() - t.trim_start().len() };
        blank += lead;
        if lead < t.len() {
            break;
        }
    }
    let gap = if meta.is_empty() { 0 } else { 2usize.saturating_sub(blank) };
    w.saturating_sub(width(meta) as usize + gap)
}

pub(crate) fn fill(f: &mut Frame, r: Rect, bg: Color) {
    f.buffer_mut().set_style(r, Style::new().bg(bg));
}

/// Draw `spans` left-aligned in a one-row rect over `bg`.
pub(crate) fn row(f: &mut Frame, r: Rect, bg: Color, spans: Vec<Span<'static>>) {
    fill(f, r, bg);
    f.render_widget(Line::from(spans), r);
}

pub(crate) fn width(spans: &[Span]) -> u16 {
    spans.iter().map(|s| s.width() as u16).sum()
}

/// Keep the end of `s` in `w` cells: "…/net/sample.c".
pub fn fit_left(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let keep: String = s
        .chars()
        .rev()
        .scan(0, |n, c| {
            *n += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            (*n < w).then_some(c)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{keep}")
}

/// `n` spaces: the blank cells after an icon, inside a button, at an edge.
/// A side bar section's header (Source Control's, an extension's): its chevron, its title
/// bold on the section background, `extra` dim after it.
pub(crate) fn section_head(app: &App, f: &mut Frame, r: Rect, title: &str, open: bool, extra: &str, focused: bool) {
    let s = app.settings.spacing;
    let ic = icons();
    fill(f, r, P.section_bg);
    let chev = if open { ic.chev_down } else { ic.chev_right };
    let fg = if focused && app.settings.blocks != crate::blocks::BlockStyle::Cards { P.focus_border } else { P.section_fg };
    let pad = s.sidebar_padding.saturating_sub(1) / 2;
    let gap = crate::blocks::icon_gap(s.icon_gap) / 2;
    let ir = inset(r, pad);
    // As in the rows: cut once with "…"; the dim file name only while the title is whole.
    let head = format!(" {chev}{}{title}", sp(gap));
    let extra = if extra.is_empty() { String::new() } else { format!(" {extra}") };
    let (head, extra) = fit_pair(&head, &extra, ir.width as usize);
    row(f, ir, P.section_bg, vec![Span::styled(head, Style::new().fg(fg).bold()), Span::styled(extra, Style::new().fg(P.dim))]);
}

pub(crate) fn sp(n: u16) -> String {
    " ".repeat(n as usize)
}

pub(crate) fn inset(r: Rect, pad: u16) -> Rect {
    Rect::new(r.x + pad, r.y, r.width.saturating_sub(2 * pad), r.height)
}

/// A popup frame: rounded border with 2x1 padding (comfortable only).
pub(crate) fn popup_block(bg: Color, fg: Color, (h, v): (u16, u16)) -> Block<'static> {
    let b = Block::bordered()
        .border_style(Style::new().fg(P.widget_border).bg(bg))
        .style(Style::new().bg(bg).fg(fg))
        .padding(Padding::new(h, h, v, v));
    if crate::theme::ascii() {
        let s = icons().border;
        b.border_set(symbols::border::Set {
            top_left: s[0],
            top_right: s[1],
            bottom_left: s[2],
            bottom_right: s[3],
            horizontal_top: s[4],
            horizontal_bottom: s[4],
            vertical_left: s[5],
            vertical_right: s[5],
        })
    } else {
        b.border_type(BorderType::Rounded)
    }
}

/// A splitter (VS Code's sash) filling `r`: `│` down a 1-column rect, else `─`,
/// in `border` on `bg`, or in `sash.hoverBorder` while `hot` (hovered or dragged;
/// a terminal cursor cannot change shape).
pub(crate) fn sash(f: &mut Frame, r: Rect, hot: bool, border: Color, bg: Color) {
    let (h, v) = if crate::theme::ascii() { (icons().border[4], icons().border[5]) } else { ("─", "│") };
    let glyph = if r.width == 1 && r.height > 1 { v } else { h };
    let fg = if hot { P.sash_hover } else { border };
    let buf = f.buffer_mut();
    for p in r.positions() {
        buf[p].set_symbol(glyph).set_fg(fg).set_bg(bg);
    }
}

/// VS Code language id for a syntect syntax name (indent-rainbow's language lists).
pub fn lang_id(name: &str) -> String {
    match name {
        "Plain Text" => "plaintext",
        "C++" => "cpp",
        "C#" => "csharp",
        "Objective-C" => "objective-c",
        "Objective-C++" => "objective-cpp",
        "Markdown" | "MultiMarkdown" => "markdown",
        "Bourne Again Shell (bash)" | "Shell-Unix-Generic" => "shellscript",
        "Git Commit" => "git-commit",
        "JavaScript" | "JavaScript (Babel)" => "javascript",
        n => return n.to_lowercase().replace(' ', ""),
    }
    .into()
}

pub fn draw(app: &mut App, f: &mut Frame) {
    app.exts.begin();
    draw_frame(app, f);
    crate::ext::overlay(app, f);
    if app.prefix_armed {
        prefix_hint(app, f);
    }
    crate::media::flush(app, f);
    scrub(f.buffer_mut(), &crate::settingsgraphics::take_drawn());
    // A terminal without 24-bit colour gets the nearest it has (`colors`).
    crate::colors::fit(f.buffer_mut());
}

/// Text from outside (commit messages, titles, file names) never reaches the terminal with
/// a control character in it: a tab would move its cursor, an escape would be a command to
/// it. Cells in `pictures` are escape sequences on purpose (a graphics protocol's) and stay.
pub(crate) fn scrub(buf: &mut ratatui::buffer::Buffer, pictures: &[Rect]) {
    let (area, width) = (buf.area, buf.area.width.max(1) as usize);
    for (i, cell) in buf.content.iter_mut().enumerate() {
        if cell.symbol().chars().any(char::is_control) {
            let p = Position::new(area.x + (i % width) as u16, area.y + (i / width) as u16);
            if !pictures.iter().any(|r| r.contains(p)) {
                cell.set_symbol(" ");
            } else {
                // A picture's escape is one cell on screen, whatever its text's width:
                // otherwise the diff skips that many cells after it.
                cell.diff_option = ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN);
            }
        }
    }
}

/// After the prefix: its quick keys and what they do, at the bottom right (which-key).
fn prefix_hint(app: &App, f: &mut Frame) {
    let Some(p) = app.settings.prefix else { return };
    let label = |c: crate::parse::Chord| match c {
        (ratatui::crossterm::event::KeyCode::Char(ch), m) if m.is_empty() => ch.to_string(),
        c => crate::parse::chord_label(c),
    };
    let mut rows: Vec<(String, String)> = app.settings.prefix_keys.iter().map(|(c, n)| (label(*c), n.clone())).collect();
    rows.sort_by_key(|r| r.0.to_lowercase());
    let p = crate::parse::chord_label(p);
    let foot = format!("{p} again: {p} itself · Esc: none");
    let kw = rows.iter().map(|r| r.0.width()).max().unwrap_or(1);
    let w = (rows.iter().map(|r| kw + 2 + r.1.width()).max().unwrap_or(0).max(foot.width()) + 4) as u16;
    let area = f.area();
    let h = (rows.len() + 4) as u16;
    if area.width < w + 2 || area.height < h + 3 {
        return;
    }
    let r = Rect::new(area.right() - w - 1, area.bottom() - h - 2, w, h);
    f.render_widget(Clear, r);
    let block = popup_block(P.widget_bg, P.widget_fg, (1, 0)).title(Span::styled(format!(" {p} then "), Style::new().fg(P.sidebar_title).bold()));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines: Vec<Line> = rows
        .into_iter()
        .map(|(k, n)| Line::from(vec![Span::styled(format!("{k:>kw$}  "), Style::new().fg(P.focus_border).bold()), Span::styled(n, Style::new().fg(P.widget_fg))]))
        .collect();
    lines.push(Line::default());
    lines.push(Line::styled(foot, Style::new().fg(P.dim)));
    f.render_widget(Paragraph::new(lines).style(Style::new().bg(P.widget_bg)), inner);
}

fn draw_frame(app: &mut App, f: &mut Frame) {
    app.ui = Hits::default();
    app.gitui.hits = Default::default();
    app.gh.hits = Default::default();
    app.prog.clear_hits();
    app.media.clear_hits();
    let lay = lay(app);
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(P.editor_bg).fg(P.editor_fg)), area);
    if area.height < 5 || area.width < 30 {
        crate::settingsui::draw(app, f);
        return;
    }
    // This frame's share of first-time highlighting (a far jump catches up over frames).
    let _hl = editor::FrameBudget::start();
    let title = Rect::new(area.x, area.y, area.width, 1);
    let status = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    title_bar(app, f, title, lay);
    status_bar(app, f, status, lay);
    // Cards touch at their borders beside the sidebar. Lines reserve a shared line;
    // none keeps only the sidebar's divider. Each card draws inside its own frame.
    let st = app.settings.blocks;
    crate::blocks::begin();
    // The spacing widths are the content's; a card adds its frame.
    let fr = crate::blocks::frame(st);
    let aw = (lay.s.activity_width + fr).min(body.width / 4);
    let act = Rect::new(body.x, body.y, aw, body.height);
    let inner = crate::blocks::card(st, f, act, P.activity_bg, app.activity_lit);
    activity_bar(app, f, inner);
    let mut x = act.right();
    if st == BlockStyle::Lines {
        crate::blocks::vline(x, body.y, body.bottom() - 1);
        x += 1;
    }
    if app.side_visible {
        let max = body.width.saturating_sub(x - body.x + 1 + 20).max(12);
        let w = app.side_w.map_or(lay.s.sidebar_width + fr, |w| w + if app.side_w_legacy { fr } else { 0 }).clamp(12, max);
        app.ui.sidebar = Rect::new(x, body.y, w, body.height);
        sidebar(app, f, app.ui.sidebar, lay);
        x += w;
        // Cards use the editor's left border as the resize handle.
        let d = Rect::new(x, body.y, 1, body.height);
        app.ui.divider = d;
        match st {
            BlockStyle::None => {
                let lines: Vec<Line> = (0..d.height).map(|_| Line::raw("│")).collect();
                f.render_widget(Paragraph::new(lines).style(Style::new().fg(P.group_border).bg(P.sidebar_bg)), d);
            }
            BlockStyle::Lines => crate::blocks::vline(x, body.y, body.bottom() - 1),
            BlockStyle::Cards => {}
        }
        x += u16::from(st != BlockStyle::Cards);
    }
    let mut ed = Rect::new(x, body.y, body.right().saturating_sub(x), body.height);
    // The Outline right of the editors (and the terminal), its width dragged; the editors
    // keep 30 columns. Drawn after them: a restored tab's document is read as it shows.
    let mut outline = None;
    // (A repository page has its own outline beside its tree.)
    // An extension's right panel takes the Outline's place in its view.
    if (app.outline_open || crate::ext::right_here(app)) && ed.width >= 50 && !crate::ext::page(app) {
        let w = app.outline_w.unwrap_or(30).clamp(16, ed.width - 30);
        ed.width -= w;
        outline = Some(Rect::new(ed.right(), ed.y, w, ed.height));
    }
    // The terminal panel under the editor groups, their width; its height dragged, the
    // groups keeping at least a tab bar and a few lines.
    let panel_here = crate::ext::terminal_here(app);
    // An extension's bottom panel takes the terminal's place in its view.
    let ext_bottom = crate::ext::bottom_here(app);
    if ((app.term_open && panel_here) || ext_bottom) && ed.height >= 12 {
        let h = app.term_h.unwrap_or(ed.height / 3).clamp(5, ed.height - 7);
        ed.height -= h;
        let pr = Rect::new(ed.x, ed.bottom(), ed.width, h);
        if !(ext_bottom && crate::ext::draw_panel(app, f, pr, crate::ext::Slot::Bottom)) {
            terminal_panel(app, f, pr);
        }
    }
    app.ui.editor_area = ed;
    editor_area(app, f, ed, lay);
    if let Some(r) = outline {
        // A serial channel's tab: its settings there instead (the same button).
        // A driver's right panel there instead (a serial channel's settings, an extension's).
        if !crate::ext::right(app, f, r) {
            outline_panel(app, f, r);
        }
    }
    if let Some((g, zone)) = app.drop {
        if let Some(gh) = app.ui.groups.get(g) {
            let a = gh.area;
            let r = match zone {
                Zone::Left => Rect::new(a.x, a.y, a.width / 2, a.height),
                Zone::Right => Rect::new(a.x + a.width / 2, a.y, a.width - a.width / 2, a.height),
                Zone::Up => Rect::new(a.x, a.y, a.width, a.height / 2),
                Zone::Down => Rect::new(a.x, a.y + a.height / 2, a.width, a.height - a.height / 2),
                Zone::Center => a,
                Zone::Tab(_) => Rect::default(), // the tab bar draws its insertion marker
            };
            fill(f, r, P.drop_bg);
        }
    }
    crate::blocks::finish(f);
    explorer_overlay(app, f, area);
    crate::navui::draw(app, f);
    settings_notice(app, f, area);
    crate::scm::draw_popups(app, f, area);
    if app.quick.is_some() {
        quick(app, f, area, lay);
    }
    if app.prompt.is_some() {
        prompt(app, f, area, lay);
    }
    crate::settingsui::draw(app, f);
}

fn title_bar(app: &mut App, f: &mut Frame, r: Rect, lay: Lay) {
    let active = app
        .groups
        .get(app.ag)
        .and_then(|g| g.tabs.get(g.active))
        .map(|t| app.tab_title(&t.key));
    // file — folder — app, a part equal to the one before it once (termcode's own repo).
    let mut parts: Vec<String> = active.into_iter().chain([app.repo.clone(), "termcode".into()]).collect();
    parts.dedup();
    let text = parts.join(" — ");
    let st = Style::new().fg(P.title_fg).bg(P.title_bg);
    fill(f, r, P.title_bg);
    let (tp, bp) = (lay.s.title_padding.div_ceil(2), lay.s.button_padding.div_ceil(2));
    // The same cross as an editor tab's close button, bold.
    let close = format!("{}{}{}Close{}", sp(bp), icons().tab_close, sp(lay.s.icon_gap.div_ceil(2)), sp(bp));
    let w = close.width() as u16;
    let t = fit(&text, r.width.saturating_sub(2 * (w + tp) + 2) as usize);
    let x = r.x + (r.width.saturating_sub(t.width() as u16)) / 2;
    f.render_widget(Span::styled(t, st), Rect::new(x, r.y, r.right().saturating_sub(x), 1));
    app.ui.close = Rect::new(r.right().saturating_sub(w + tp), r.y, w, 1);
    let hot = app.hover.is_some_and(|p| app.ui.close.contains(p)) && !matches!(app.drag, Drag::Close);
    let tint = Rgba(255, 255, 255, 255);
    let bg = if hot { tint.alpha(0.12).over(P.status_err_bg) } else { P.status_err_bg };
    f.render_widget(Span::styled(close, Style::new().fg(P.status_err_fg).bg(bg).add_modifier(Modifier::BOLD)), app.ui.close);
}

/// A button's colours, lit while the pointer is on it.
pub(crate) fn button_style(primary: bool, hot: bool) -> Style {
    match (primary, hot) {
        (true, false) => Style::new().fg(P.button_fg).bg(P.button_bg),
        (true, true) => Style::new().fg(P.button_fg).bg(P.button_hover),
        (false, false) => Style::new().fg(P.button2_fg).bg(P.button2_bg),
        (false, true) => Style::new().fg(P.button2_fg).bg(P.button2_hover),
    }
}

/// A broken settings.json, as a notification above the status bar's right end (the
/// last good settings stay in use). Close hides it until the error changes.
fn settings_notice(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(e) = app.settings_err.clone().filter(|e| app.settings_err_closed.as_ref() != Some(e)) else { return };
    let w = 60.min(area.width.saturating_sub(2));
    if w < 24 {
        return;
    }
    let body = ratatui::widgets::Paragraph::new(e.as_str()).wrap(ratatui::widgets::Wrap { trim: true });
    let lines = body.line_count(w - 4).min(4) as u16;
    let h = lines + 5;
    if area.height < h + 2 {
        return;
    }
    let r = Rect::new(area.right() - w - 1, area.bottom() - 1 - h, w, h);
    f.render_widget(Clear, r);
    let block = popup_block(P.widget_bg, P.widget_fg, (1, 0));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let icon = icons().problem[2];
    f.render_widget(Line::from(vec![Span::styled(format!("{icon} "), Style::new().fg(P.error_fg)), Span::styled("settings.json has an error", Style::new().fg(P.widget_fg).bold())]), Rect::new(inner.x, inner.y, inner.width, 1));
    f.render_widget(body.style(Style::new().fg(P.widget_fg)), Rect::new(inner.x, inner.y + 1, inner.width, lines));
    let label = format!("{0}Close{0}", sp(app.settings.spacing.button_padding.min(1)));
    let br = Rect::new(inner.x, inner.bottom() - 1, label.width() as u16, 1).intersection(inner);
    f.render_widget(Span::styled(label, button_style(false, app.hover.is_some_and(|p| br.contains(p)))), br);
    app.ui.notice_close = br;
}

fn status_bar(app: &mut App, f: &mut Frame, r: Rect, lay: Lay) {
    let st = Style::new().fg(P.status_fg).bg(P.status_bg);
    fill(f, r, P.status_bg);
    let edge = sp(lay.s.status_padding);
    let sep = sp(lay.s.status_gap);
    // Branch, sync / Publish Branch or the running git op (clickable).
    let lx = crate::gitcmds::status_left(app, f, r, lay.s.status_padding, lay.s.status_gap);
    let gap = lay.s.status_gap;
    let mut items = Vec::new();
    if let Some(Doc::Text(b)) = app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).and_then(|t| app.docs.get(&t.key)) {
        let v = &app.groups[app.ag].tabs[app.groups[app.ag].active].view;
        let c = b.clamp(v.cursor);
        let col = editor::display_col(&b.lines[c.line], c.col) + 1;
        let mut s = format!("Ln {}, Col {col}", c.line + 1);
        if let Some(sel) = b.selected_text(v) {
            s.push_str(&format!(" ({} selected)", sel.chars().count()));
        }
        let indent = if b.indent == "\t" { format!("Tab Size: {}", editor::tab()) } else { format!("Spaces: {}", b.indent.len()) };
        let enc = if b.binary {
            "Hex".into()
        } else if b.elf {
            "ELF".into()
        } else if b.readonly.is_some() {
            format!("{}{}Read-only", icons().lock, sp(lay.s.icon_gap))
        } else {
            "UTF-8".into()
        };
        let eol = if b.crlf { "CRLF" } else { "LF" };
        items = vec![s, indent, enc, eol.to_string(), b.syntax_name().to_string()];
        // A picture or a PDF's pages: no lines, columns or indent to tell.
        if let Some(kind) = b.media {
            items = vec![format!("{}{}Read-only", icons().lock, sp(lay.s.icon_gap)), if kind == crate::media::Kind::Pdf { "PDF".into() } else { "Image".into() }];
        }
    }
    // Only the branch, the blame, the message and these: problems and errors are shown
    // where they are (underlines, the settings notification), never here.
    app.ui.status_bar = r;
    // The left items stay whole. Short of room the message goes first (below), then the
    // right items from their left end: Ln/Col first, the language last.
    let width = |items: &[String]| if items.is_empty() { 0 } else { items.iter().map(|t| t.width()).sum::<usize>() + sep.len() * (items.len() - 1) + edge.len() };
    while !items.is_empty() && lx as usize + gap as usize + width(&items) > r.right() as usize {
        items.remove(0);
    }
    let all = items.len() == 5;
    let mut x = r.right().saturating_sub((width(&items) as u16).min(r.width));
    let rx = x;
    for (k, t) in items.iter().enumerate() {
        let ir = Rect::new(x, r.y, (t.width() as u16).min(r.right().saturating_sub(x)), 1);
        let lit = app.hover.is_some_and(|p| ir.contains(p));
        f.render_widget(Span::styled(t.clone(), if lit { st.bg(P.status_hover) } else { st }), ir);
        app.ui.status_items.push(ir);
        if k == 0 && all {
            app.ui.status_line = ir;
        }
        x += ir.width + gap;
    }
    // Blame left of them; the message in what is left, cut once with "…" and `gap` cells
    // before what follows it.
    let end = crate::gitcmds::status_blame(app, f, Rect::new(lx, r.y, rx.saturating_sub(lx), 1), gap);
    let room = end.saturating_sub(lx + 2 * gap) as usize;
    // Under 8 cells a cut message is noise ("op…"): it goes.
    let left = vec![Span::styled(sep.clone(), st), Span::styled(if room < 8 { String::new() } else { fit(&app.msg, room) }, st)];
    f.render_widget(Line::from(left), Rect::new(lx, r.y, end.saturating_sub(lx), 1));
}

/// VS Code's activity bar: the views at the top and Settings at the bottom, each item two
/// rows with its icon in a 2-cell slot (room for a non-Mono Nerd Font glyph). The active
/// item gets `activityBar.activeBorder` down its left edge over a subtle `activeBackground`;
/// a hovered one lights up from `inactiveForeground` to `foreground`.
fn activity_bar(app: &mut App, f: &mut Frame, r: Rect) {
    fill(f, r, P.activity_bg);
    let ic = icons();
    let empty = serde_json::Map::new();
    let theme = app.settings.theme.json.get("colors").and_then(serde_json::Value::as_object).unwrap_or(&empty);
    let active_bg = crate::blocks::active_bg(theme);
    // Two rows an item: comfortable puts the icon on the second, so one blank row lies
    // between icons and the active block takes half of it on each side.
    let (ih, off): (u16, u16) = if app.settings.compact { (2, 0) } else { (2, 1) };
    let ix = r.x + 1 + r.width.saturating_sub(3) / 2;
    // The lifted block hugs the icon on row `iy`: the blank row above (comfortable) and
    // below count half, a half block each (½ + 1 + ½ rows); rows from `end` on are not
    // its to draw.
    let item = |f: &mut Frame, iy: u16, active: bool, hot: bool, icon: &str, end: u16| {
        if active {
            for row in (iy.saturating_sub(off)..=iy + 1).filter(|&row| row < end && row >= r.y) {
                if row == iy {
                    fill(f, Rect::new(r.x, row, r.width, 1), active_bg);
                    f.render_widget(Span::styled("▎", Style::new().fg(P.activity_border)), Rect::new(r.x, row, 1, 1));
                } else {
                    let half = if row < iy { "▄" } else { "▀" };
                    let st = Style::new().fg(active_bg).bg(P.activity_bg);
                    f.render_widget(Span::styled(half.repeat(r.width as usize), st), Rect::new(r.x, row, r.width, 1));
                }
            }
        }
        let fg = if active || hot { P.activity_fg } else { P.activity_inactive };
        f.render_widget(Span::styled(icon.to_string(), Style::new().fg(fg)), Rect::new(ix, iy, 2.min(r.right().saturating_sub(ix)), 1));
    };
    // Settings: like a view's item, its icon above one blank row at the bottom; lifted
    // while its dialog is open (the view's item is not, meanwhile).
    let settings_open = app.settings_dlg.is_some();
    let siy = r.bottom().saturating_sub(2).max(r.y);
    let top = siy.saturating_sub(off).max(r.y);
    let bottom = Rect::new(r.x, top, r.width, r.bottom() - top);
    app.ui.settings_btn = bottom;
    if r.width > 0 && r.height > 0 {
        item(f, siy, settings_open, app.hover.is_some_and(|p| bottom.contains(p)), ic.settings, r.bottom());
    }
    // Upward from Settings, where the code views leave room: the Extensions view (+), the
    // installed extensions, GitHub, the Programmer; each lit down to the icon below it.
    let fits = |q: Rect| q.y >= r.y + ih * SIDES.len() as u16 && r.width > 0;
    let mut views_end = bottom.y;
    let mut below = siy;
    let mut low = Vec::new();
    for (n, icon) in crate::ext::bottom_items(app) {
        let q = Rect::new(r.x, views_end.saturating_sub(ih), r.width, ih);
        if !fits(q) || q.y >= views_end {
            break;
        }
        let active = app.side == Side::View(n) && app.side_visible && !settings_open;
        let hot = app.hover.is_some_and(|p| q.contains(p));
        item(f, q.y + off, active, hot, &icon, below);
        below = q.y + off;
        views_end = q.y;
        low.push((q, Side::View(n)));
    }
    for (i, s) in SIDES.iter().enumerate() {
        let y = r.y + i as u16 * ih;
        if y + ih > views_end || r.width == 0 {
            break;
        }
        let item_r = Rect::new(r.x, y, r.width, ih);
        let active = *s == app.side && app.side_visible && !settings_open;
        let hot = app.hover.is_some_and(|p| item_r.contains(p));
        let icon = match s {
            Side::Explorer => ic.files,
            Side::Search => ic.search,
            Side::Scm => ic.scm,
            Side::View(_) => continue,
        };
        let iy = y + off;
        item(f, iy, active, hot, icon, bottom.y);
        if *s == Side::Scm && !app.scm_files.is_empty() {
            activity_badge(f, r, item_r, (ix, iy), app.scm_files.len(), if active { active_bg } else { P.activity_bg });
        }
        app.ui.activity.push((item_r, *s));
    }
    app.ui.activity.extend(low);
}

/// The SCM count on its activity item, Dock / VS Code style: a pill (at most "99+") off
/// the icon's top-right corner, on the row above the icon (the icon's own row when the
/// item has none), one cell right of the icon column, shifted left to stay in the bar but
/// never onto the indicator column or the icon itself. Where no pill fits (ASCII icons,
/// a narrow bar): the bare count in the badge colours, else a dot.
fn activity_badge(f: &mut Frame, bar: Rect, item: Rect, (ix, iy): (u16, u16), n: usize, under: Color) {
    let label = if n > 99 { "99+".to_string() } else { n.to_string() };
    let st = Style::new().fg(P.activity_badge_fg).bg(P.activity_badge_bg);
    let above = iy > item.y;
    let (y, lo) = if above { (iy - 1, bar.x + 1) } else { (iy, ix + 1) };
    let lw = label.len() as u16;
    let ascii = crate::theme::ascii();
    let mut shapes = vec![(vec![Span::styled(label.clone(), st)], lw), (vec![Span::styled(if ascii { "*" } else { "●" }, Style::new().fg(P.activity_badge_bg).bg(under))], 1)];
    if !ascii {
        shapes.insert(0, (pill(label, st, under), lw + 2));
    }
    for (spans, w) in shapes {
        let x = (ix + 1).min(bar.right().saturating_sub(w)).max(lo);
        if x + w <= bar.right() {
            f.render_widget(Line::from(spans), Rect::new(x, y, w, 1));
            return;
        }
    }
}

/// The view name sits in the card's top border, or on its own row without cards.
fn side_title(app: &App, f: &mut Frame, r: Rect, title: &str, lay: Lay, bg: Color) {
    let frame = crate::blocks::frame(app.settings.blocks) / 2;
    if frame == 0 {
        fill(f, r, bg);
    }
    let x = r.x + (lay.pad + frame + 1).min(r.width);
    let end = r.right().saturating_sub(frame);
    let text = fit(title, end.saturating_sub(x + frame) as usize);
    let text = if frame > 0 { format!(" {text} ") } else { text };
    let rr = Rect::new(x.saturating_sub(frame), r.y, (text.width() as u16).min(end.saturating_sub(x.saturating_sub(frame))), 1);
    row(f, rr, bg, vec![Span::styled(text, Style::new().fg(P.sidebar_title).bold())]);
}

/// A slim scrollbar slider over `track`, composited over what is drawn there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scrollbar(app: &mut App, f: &mut Frame, track: Rect, target: ScrollTarget, total: usize, view: usize, off: usize, max_off: usize) {
    let s = decor::slider(track.height, total, view, off, max_off);
    paint_slider(f, track, s, slider_color(app, track, target));
    app.ui.scrollbars.push(ScrollHit { track, target, slider: s, max_off, page: view.max(1) });
}

fn slider_color(app: &App, track: Rect, target: ScrollTarget) -> Rgba {
    if matches!(app.drag, Drag::Scroll(sb, _) if sb.target == target) {
        P.slider_active
    } else if app.hover.is_some_and(|p| track.contains(p)) {
        P.slider_hover
    } else {
        P.slider
    }
}

pub(crate) fn paint_slider(f: &mut Frame, track: Rect, (start, len): (u16, u16), c: Rgba) {
    let track = track.intersection(f.area());
    let buf = f.buffer_mut();
    let horizontal = track.width > 1;
    let span = if horizontal { track.width } else { track.height };
    let end = start.saturating_add(len).min(span);
    for at in start..end {
        let cell = &mut buf[if horizontal { (track.x + at, track.y) } else { (track.x, track.y + at) }];
        let under = cell.bg;
        if horizontal {
            // Keep the row clickable, but draw a narrow stroke with softened ends.
            let glyph = if crate::theme::ascii() { "-" }
                else if end - start == 1 { "•" }
                else if at == start { "╶" }
                else if at + 1 == end { "╴" }
                else { "━" };
            cell.set_symbol(glyph).set_fg(c.over(under));
        } else {
            cell.set_bg(c.over(under));
        }
    }
}

fn horizontal_scrollbar(track: Rect, total: usize, page: usize, off: usize) -> ScrollHit {
    let max_off = total.saturating_sub(page);
    let slider = decor::slider(track.width, total, page, off, max_off);
    ScrollHit { track, target: ScrollTarget::EditorHorizontal(0), slider, max_off, page }
}

fn sidebar(app: &mut App, f: &mut Frame, col: Rect, mut lay: Lay) {
    // All sidebar views share the same edge, including Search inputs and actions.
    lay.pad = lay.pad.saturating_sub(1) / 2;
    // Cards start on the same row as the activity bar and editor frames.
    let st = app.settings.blocks;
    let bg = if st == BlockStyle::Cards { crate::blocks::gutter() } else { P.sidebar_bg };
    fill(f, col, bg);
    let title = match app.side {
        Side::Explorer => "EXPLORER",
        Side::Search => "SEARCH",
        Side::Scm => "SOURCE CONTROL",
        Side::View(_) => "",
    };
    let title_row = Rect::new(col.x, col.y, col.width, 1);
    let top = col.y + if st == BlockStyle::Cards { 0 } else { 1 + lay.s.section_gap };
    // A driver's view (ext.rs): its side bar, its title.
    if let Side::View(n) = app.side {
        crate::ext::side(app, f, Rect::new(col.x, top, col.width, col.bottom().saturating_sub(top)), n);
        let t = crate::ext::title(app, n);
        side_title(app, f, title_row, &t, lay, bg);
        return;
    }
    if app.side == Side::Scm {
        crate::scm::draw_scm(app, f, Rect::new(col.x, top, col.width, col.bottom().saturating_sub(top)));
        side_title(app, f, title_row, title, lay, bg);
        return;
    }
    // The Explorer's TIMELINE pane, Search's REFERENCES, takes the bottom of the sidebar.
    let (main, timeline) = match app.side {
        Side::Explorer => crate::scm::split_timeline(app, col),
        _ => crate::scm::split_refs(app, col),
    };
    let main = Rect::new(col.x, top, col.width, main.bottom().saturating_sub(top));
    // A line between sections runs from the vertical line left of the sidebar to the right one.
    let (main, timeline) = crate::blocks::stack(st, main, timeline, col.x.saturating_sub(1), col.right());
    let focused = matches!(app.focus, Focus::Side | Focus::SearchInput) && !app.activity_lit;
    let r = crate::blocks::card(st, f, main, P.sidebar_bg, focused);
    let mut y = r.y;
    let ic = icons();
    // Inputs and buttons line up with the rows' padding (at least one cell).
    let m = lay.pad.min(r.width / 2);
    let field = |y: u16, h: u16| Rect::new(r.x + m, y, r.width.saturating_sub(2 * m), h);
    match app.side {
        Side::Explorer => {
            let hr = Rect::new(r.x, y, r.width, 1);
            fill(f, hr, P.section_bg);
            let hdr = format!(" {}{}{}", ic.chev_down, sp(crate::blocks::icon_gap(lay.s.icon_gap) / 2), app.repo.to_uppercase());
            row(f, inset(hr, lay.pad), P.section_bg, vec![Span::styled(hdr, Style::new().fg(P.section_fg).bold())]);
            y += 1;
        }
        Side::Search => {
            let inp = field(y, 1);
            let focused = app.focus == crate::panel::Focus::SearchInput;
            let controls = if inp.width >= 8 { 6 } else { 0 };
            // The right cap needs its cell even with no toggles.
            let text = Rect::new(inp.x, inp.y, inp.width.saturating_sub(controls.max(1)), 1);
            fill(f, inp, P.input_bg);
            // A pill: its caps take the padding cells either end, so the text and the
            // toggles keep their columns; the card border and the caret show focus.
            input(f, text, &app.query, "Search", focused && crate::theme::ascii());
            if focused && app.query_all {
                select_all(f, text, &app.query);
            }
            round_ends(f, inp, P.input_bg, P.sidebar_bg);
            app.ui.side_input = inp;
            let (c, w, rx) = app.s_opts;
            let toggles = [(ic.case, c, FindBtn::SearchCase), (ic.word, w, FindBtn::SearchWord), (ic.regex, rx, FindBtn::SearchRegex)];
            let mut tx = inp.right().saturating_sub(controls);
            for (icon, on, b) in toggles.into_iter().take(if controls > 0 { 3 } else { 0 }) {
                let tr = Rect::new(tx, y, 1, 1);
                let st = if on { Style::new().fg(P.option_fg).bg(P.option_bg) } else { Style::new().fg(P.input_fg).bg(P.input_bg) };
                f.render_widget(Span::styled(icon, st), tr);
                app.ui.find_btns.push((tr, b));
                tx += 2;
            }
            if focused && text.width > 1 {
                let cx = text.x + 1 + (app.query.width() as u16).min(text.width.saturating_sub(2));
                f.set_cursor_position((cx, y));
            }
            // One sectionGap between the field and the results, which the summary heads.
            y += 1 + lay.s.section_gap;
            if !app.hits.is_empty() {
                let files = app.search_rows().iter().filter(|r| matches!(r, HRow::File(..))).count();
                let summary = format!(" {} results in {files} files", app.hits.len());
                row(f, inset(Rect::new(r.x, y, r.width, 1), lay.pad), P.sidebar_bg, vec![Span::styled(summary, Style::new().fg(P.dim))]);
                y += 1;
            }
        }
        Side::Scm => {}
        Side::View(_) => {}
    }
    let list = Rect::new(r.x, y, r.width, r.bottom().saturating_sub(y));
    app.ui.side_list = list;
    let s = app.side.idx();
    let n = app.side_count();
    // Item k sits on row k * step; the rows between are blank (listItemGap).
    let step = 1 + lay.s.list_gap;
    let h = (list.height as usize).div_ceil(step as usize);
    app.ui.side_rows = h;
    app.ui.side_gap = lay.s.list_gap;
    app.sel[s] = app.sel[s].min(n.saturating_sub(1));
    app.off[s] = app.off[s].min(n.saturating_sub(1));
    let hover_row = app.hover.filter(|p| list.contains(*p)).and_then(|p| app.ui.side_item(p.y, app.off[s]));
    let search_rows = if app.side == Side::Search { app.search_rows() } else { Vec::new() };
    for k in 0..h {
        let i = app.off[s] + k;
        if i >= n {
            if i == 0 && app.side == Side::Search && app.query.is_empty() {
                let hint = Span::styled("  Type and press Enter (F4/Alt+4)", Style::new().fg(P.dim));
                row(f, inset(Rect::new(list.x, list.y, list.width, 1), lay.pad), P.sidebar_bg, vec![hint]);
            }
            break;
        }
        let rr = Rect::new(list.x, list.y + k as u16 * step, list.width, 1);
        // A drag's target folder lights up with what shows inside it (the root: all).
        let dropping = app.side == Side::Explorer
            && app.tree_drop.as_deref().is_some_and(|t| t.is_empty() || app.tree.get(i).is_some_and(|n| n.path == t || n.path.starts_with(&format!("{t}/"))));
        // Ctrl+click / Shift+click selections light up as the cursor's row does.
        let marked = app.side == Side::Explorer && app.tree.get(i).is_some_and(|n| app.tree_marks.contains(&n.path));
        let bg = if dropping {
            P.drop_bg
        } else if (i == app.sel[s] || marked) && app.focus == Focus::Side {
            P.list_active_bg
        } else if i == app.sel[s] || marked {
            P.list_inactive_bg
        } else if hover_row == Some(i) {
            P.list_hover_bg
        } else {
            P.sidebar_bg
        };
        fill(f, rr, bg);
        let ir = inset(rr, lay.pad);
        match app.side {
            Side::Explorer => {
                let (tip, field) = explorer_row(app, f, ir, bg, i);
                if let Some((full, x, file, link)) = tip {
                    app.ui.tips.push((ir, full, x, file, link));
                }
                if let Some(field) = field {
                    (app.ui.rename, app.ui.rename_field) = (ir, field);
                }
            }
            Side::Search => search_row(app, f, ir, bg, &search_rows[i], i),
            Side::Scm => {}
            Side::View(_) => {}
        }
    }
    sticky_rows(app, f, list, lay.pad, lay.s.list_gap, &[], &search_rows);
    if n > h && h > 0 {
        let track = Rect::new(list.right() - 1, list.y, 1, list.height);
        scrollbar(app, f, track, ScrollTarget::Side, n, h, app.off[s], n - h);
    }
    if !timeline.is_empty() {
        let inner = crate::blocks::card(st, f, timeline, P.sidebar_bg, false);
        crate::blocks::seam(st, f, main, timeline, focused);
        if app.side == Side::Explorer {
            crate::scm::draw_timeline(app, f, inner);
        } else {
            crate::scm::draw_refs(app, f, inner);
        }
    }
    side_title(app, f, title_row, title, lay, bg);
}

/// VS Code's tree sticky scroll over the list just drawn: the folders (Search file
/// groups, the CHANGES sections) holding the top rows, pinned over them, then a thin
/// separator line in `sideBarStickyScroll.border` along the last pinned row's bottom
/// (or its configured listItemGap). One Explorer row (the default) is the breadcrumb of the
/// folder you are in. At most `stickyScrollMaxItemCount` rows, never over 40% of the
/// list. `pad`, `gap`: the list's row padding and listItemGap; `scm_rows` /
/// `search_rows`: the rows of that view (empty for the others).
pub(crate) fn sticky_rows(app: &mut App, f: &mut Frame, list: Rect, pad: u16, gap: u16, scm_rows: &[SRow], search_rows: &[HRow]) {
    let step = 1 + gap;
    let room = (list.height as usize * 2 / 5) / step as usize;
    let cap = if app.settings.sticky { app.settings.sticky_max.min(room) } else { 0 };
    let s = app.side.idx();
    let pinned = app.pinned(app.off[s], cap, search_rows, scm_rows);
    app.ui.sticky_cap = cap;
    app.ui.sticky = pinned.clone();
    if pinned.is_empty() {
        return;
    }
    let h = pinned.len() as u16 * step;
    app.ui.sticky_h = h;
    let area = Rect::new(list.x, list.y, list.width, h);
    // Rows hidden under the sticky rows keep none of their buttons.
    app.ui.scm_btns.retain(|(b, _)| !area.contains(b.as_position()));
    let hover = app.hover.filter(|p| area.contains(*p)).and_then(|p| app.ui.sticky_at(p.y));
    // CHANGES shares the sidebar's focus with GRAPH.
    let focused = app.focus == Focus::Side && !(app.side == Side::Scm && app.gitui.graph_focus);
    for (k, &i) in pinned.iter().enumerate() {
        let bg = if i == app.sel[s] && focused {
            P.list_active_bg
        } else if i == app.sel[s] {
            P.list_inactive_bg
        } else if hover == Some(i) {
            P.list_hover_bg
        } else {
            P.sticky_bg
        };
        let y = list.y + k as u16 * step;
        // Cleared first: `fill` keeps the symbols, and the row this one covers would
        // show through after a shorter name.
        f.render_widget(Clear, Rect::new(list.x, y, list.width, step));
        fill(f, Rect::new(list.x, y, list.width, step), bg);
        let ir = inset(Rect::new(list.x, y, list.width, 1), pad);
        match app.side {
            Side::Explorer if cap == 1 => breadcrumb_row(app, f, ir, bg, i),
            Side::Explorer => {
                let (tip, field) = explorer_row(app, f, ir, bg, i);
                if let Some((full, x, file, link)) = tip {
                    app.ui.tips.push((ir, full, x, file, link));
                }
                if let Some(field) = field {
                    (app.ui.rename, app.ui.rename_field) = (ir, field);
                }
            }
            Side::Search => search_row(app, f, ir, bg, &search_rows[i], i),
            Side::Scm => scm_row(app, f, ir, bg, &scm_rows[i], hover == Some(i)),
            Side::View(_) => {}
        }
    }
    // Share the last header's row rather than reserving an empty separator row.
    // With an explicit listItemGap, reuse that gap for the separator instead.
    if gap == 0 {
        f.buffer_mut().set_style(Rect::new(list.x, list.y + h - 1, list.width, 1),
            Style::new().underline_color(P.sticky_line).add_modifier(Modifier::UNDERLINED));
        return;
    }
    let line = if crate::theme::ascii() { "_" } else { "▁" };
    let buf = f.buffer_mut();
    for x in list.x..list.right() {
        let c = &mut buf[(x, list.y + h - 1)];
        c.reset();
        c.set_symbol(line).set_fg(P.sticky_line).set_bg(P.sidebar_bg);
    }
}

/// The one-row sticky Explorer: folder `p`'s path from its top-level folder, ` › `
/// between the names (each coloured as the tree colours it), cut from the left with
/// `…` to fit, the deepest names kept. Each name is hit-tested on its own.
fn breadcrumb_row(app: &mut App, f: &mut Frame, r: Rect, bg: Color, p: usize) {
    const SEP: &str = " › ";
    const MORE: &str = "… › ";
    let lead = format!(" {}{}", icons().chev_down, sp(crate::blocks::icon_gap(app.settings.spacing.icon_gap) / 2));
    let names: Vec<(usize, String, Color)> = app
        .tree_path(p)
        .into_iter()
        .map(|i| {
            let n = &app.tree[i];
            (i, n.name.clone(), app.dirty_dirs.get(&n.path).map_or(P.sidebar_fg, |&c| git_color(c)))
        })
        .collect();
    let room = (r.width as usize).saturating_sub(lead.width() + 1);
    let need = |k: usize| {
        let shown = &names[names.len() - k..];
        shown.iter().map(|(_, n, _)| n.width()).sum::<usize>() + SEP.width() * (k - 1) + if k < names.len() { MORE.width() } else { 0 }
    };
    let mut keep = names.len();
    while keep > 1 && need(keep) > room {
        keep -= 1;
    }
    let dim = Style::new().fg(P.dim);
    let mut spans = vec![Span::styled(lead.clone(), Style::new().fg(P.icon_fg))];
    let mut x = r.x + lead.width() as u16;
    if keep < names.len() {
        spans.push(Span::styled(MORE, dim));
        x += MORE.width() as u16;
    }
    for (k, (i, name, fg)) in names[names.len() - keep..].iter().enumerate() {
        if k > 0 {
            spans.push(Span::styled(SEP, dim));
            x += SEP.width() as u16;
        }
        // Only the deepest name alone can still be too wide: cut it.
        let name = fit(name, (r.right().saturating_sub(x + 1)) as usize);
        let w = name.width() as u16;
        app.ui.sticky_segs.push((Rect::new(x, r.y, w, 1).intersection(r), *i));
        spans.push(Span::styled(name, Style::new().fg(*fg)));
        x += w;
    }
    row(f, r, bg, spans);
}

/// Right-aligned spans inside a row.
pub(crate) fn right(f: &mut Frame, r: Rect, spans: Vec<Span<'static>>) -> u16 {
    let w = width(&spans).min(r.width);
    f.render_widget(Line::from(spans), Rect::new(r.right() - w, r.y, w, 1));
    r.right() - w
}

/// A pill: `text` in `st` between Nerd Font round caps (U+E0B6, U+E0B4) drawn in `st`'s
/// background over `under`, the surface behind. Without a Nerd Font a space takes each
/// cap's place: the square look. Either way one cell a side.
pub(crate) fn pill(text: String, st: Style, under: Color) -> Vec<Span<'static>> {
    if !crate::theme::nerd() {
        return vec![Span::styled(format!(" {text} "), st)];
    }
    let cap = Style::new().fg(st.bg.unwrap_or(under)).bg(under);
    vec![Span::styled("\u{e0b6}", cap), Span::styled(text, st), Span::styled("\u{e0b4}", cap)]
}

/// Rounds a filled run's ends (see `pill`): `r`'s first and last cells become the caps,
/// `fill` over `under`. Nothing without a Nerd Font, or in a run too short for both.
pub(crate) fn round_ends(f: &mut Frame, r: Rect, fill: Color, under: Color) {
    let r = r.intersection(f.area());
    if !crate::theme::nerd() || r.width < 3 || r.height == 0 {
        return;
    }
    let st = Style::new().fg(fill).bg(under);
    let buf = f.buffer_mut();
    buf[(r.x, r.y)].set_symbol("\u{e0b6}").set_style(st);
    buf[(r.right() - 1, r.y)].set_symbol("\u{e0b4}").set_style(st);
}

/// A count badge: a pill on `under` (the row's background).
pub(crate) fn badge(n: usize, under: Color) -> Vec<Span<'static>> {
    pill(n.to_string(), Style::new().fg(P.badge_fg).bg(P.badge_bg), under)
}

/// An Explorer row's tooltip (a cut name's full name, the name's column, a file's path)
/// and rename box (box, scrolled cols).
type RowExtras = (Option<(Option<String>, u16, Option<String>, Option<String>)>, Option<(Rect, usize)>);

/// One Explorer row; its tooltip when it has one (a cut name in full, a file's MD5) with
/// the column the name starts at, and the rename box with how far it is scrolled when this
/// row is being renamed (for the mouse).
fn explorer_row(app: &App, f: &mut Frame, r: Rect, bg: Color, i: usize) -> RowExtras {
    let n = &app.tree[i];
    let ic = icons();
    let fi = &app.settings.file_icons;
    let mark = if n.dir { app.dirty_dirs.get(&n.path).copied() } else { app.marks.get(&n.path).copied() };
    let fg = mark.map_or(P.sidebar_fg, git_color);
    let (chev, (glyph, gfg)) = if n.dir {
        (if n.open { ic.chev_down } else { ic.chev_right }, folder_icon(&n.name, n.open, fi))
    } else {
        (" ", file_icon(&n.name, fi))
    };
    let s = app.settings.spacing;
    let indent = sp(s.tree_indent * n.depth as u16);
    let gap = sp(crate::blocks::icon_gap(s.icon_gap) / 2);
    // VS Code: a folder holding changes gets the colour and a dot; a file its letter.
    let right_spans = match mark {
        Some(c) if n.dir => vec![Span::styled(format!("{} ", ic.folder_dot), Style::new().fg(git_color(c)))],
        Some(c) => vec![Span::styled(format!("{c} "), Style::new().fg(git_color(c)))],
        None => vec![],
    };
    let rw = width(&right_spans) as usize;
    // [indent][twistie][gap][icon][gap][name]: an icon is laid out as one cell.
    let lead = format!(" {indent}{chev}{gap}");
    // A symlink: an arrow after its name (red when what it points to is not there).
    let arrow = n.link.as_ref().map(|(_, there)| Span::styled(if crate::theme::ascii() { " ->" } else { " ↪" }, Style::new().fg(if *there { P.dim } else { P.error_fg })));
    let aw = arrow.as_ref().map_or(0, |a| a.width());
    let name = fit(&n.name, (r.width as usize).saturating_sub(lead.width() + 1 + gap.len() + rw + 1 + aw));
    let name_x = r.x + (lead.width() + 1 + gap.len()) as u16;
    let mut spans = vec![
        Span::styled(lead, Style::new().fg(P.icon_fg)),
        Span::styled(glyph, Style::new().fg(gfg)),
        Span::raw(gap),
        Span::styled(name.clone(), Style::new().fg(fg)),
    ];
    spans.extend(arrow);
    row(f, r, bg, spans);
    if !right_spans.is_empty() {
        right(f, r, right_spans);
    }
    if let Some(rn) = app.rename.as_ref().filter(|rn| rn.path == n.path) {
        // The name's place to the row's end, as VS Code's rename box.
        let field = Rect::new(name_x, r.y, r.right().saturating_sub(name_x), 1);
        let skip = rename_field(f, field, rn, app.rename_conflict());
        return (None, Some((field, skip)));
    }
    let full = (name != n.name).then(|| n.name.clone());
    let file = (!n.dir).then(|| n.path.clone());
    let link = n.link.as_ref().map(|_| n.path.clone());
    ((full.is_some() || file.is_some() || link.is_some()).then_some((full, name_x, file, link)), None)
}

/// The in-place rename box: the name on the input colours with its selection, scrolled
/// to keep the caret in view, and the terminal cursor there. A name already taken (or not
/// a name) is in `errorForeground` while it is typed.
pub(crate) fn rename_field(f: &mut Frame, r: Rect, rn: &crate::panel::Rename, conflict: bool) -> usize {
    if r.width < 2 {
        return 0;
    }
    // Over the row's name and git letter: nothing of them may show through.
    f.render_widget(Clear, r);
    fill(f, r, P.input_bg);
    let caret = rn.text[..rn.cur].width();
    let w = r.width as usize - 1;
    let skip = caret.saturating_sub(w);
    let sel: Vec<(usize, usize, Color)> = rn.selection().map(|(a, b)| (a, b, P.selection)).into_iter().collect();
    let fg = if conflict { P.error_fg } else { P.input_fg };
    let line = render_line(&rn.text, None, &sel, None, skip, w, Style::new().fg(fg).bg(P.input_bg));
    f.render_widget(line, r);
    f.set_cursor_position((r.x + (caret - skip) as u16, r.y));
    skip
}

/// Over everything but popups, an Explorer row whose name was cut grows to the right to
/// show all of it: the row the pointer rests on (a click or double-click there is a click
/// on the row), and the rename box while it is being edited.
fn explorer_overlay(app: &mut App, f: &mut Frame, area: Rect) {
    if let Some(rn) = app.rename.as_ref() {
        let (field, _) = app.ui.rename_field;
        let need = rn.text.width() as u16 + 2;
        if field.width > 0 && need > field.width {
            let wide = Rect::new(field.x, field.y, need.min(area.right().saturating_sub(field.x)), 1);
            let skip = rename_field(f, wide, rn, app.rename_conflict());
            app.ui.rename_field = (wide, skip);
        }
        return;
    }
    let Some(t) = app.tip.as_ref().filter(|t| t.shown) else { return };
    let ext = t.ext(area.right());
    // Lines under the row (over the rows above it at the list's foot): where a link points,
    // a file's MD5 with its Copy button.
    let mut info: Vec<(String, bool, bool)> = Vec::new(); // text, an error, a Copy button
    if let Some(to) = &t.link_to {
        info.push((format!(" Link {} {to} ", if crate::theme::ascii() { "->" } else { "→" }), to.ends_with("(missing)"), false));
    }
    if t.file.is_some() {
        info.push(match &t.md5 {
            None => (" MD5  computing… ".to_string(), false, false),
            Some(Ok(s)) => (format!(" MD5  {s} "), false, true),
            Some(Err(e)) => (format!(" MD5  {e} "), true, false),
        });
    }
    let n = info.len() as u16;
    let below = t.rect.y + n < app.ui.side_list.bottom();
    let y0 = if below { t.rect.y + 1 } else { t.rect.y.saturating_sub(n) };
    let lines: Vec<(Rect, String, bool, bool)> = info
        .into_iter()
        .enumerate()
        .map(|(k, (text, err, button))| {
            let w = (text.width() as u16 + if button { 7 } else { 0 }).min(area.right().saturating_sub(t.x));
            (Rect::new(t.x, y0 + k as u16, w, 1), text, err, button)
        })
        .collect();
    let block = lines.iter().fold(Rect::default(), |acc, (r, ..)| if acc.is_empty() { *r } else { acc.union(*r) });
    let on = |p: Position| t.rect.contains(p) || ext.contains(p) || block.contains(p);
    let shown = app.hover.is_some_and(on) && app.ui.tips.iter().any(|(r, s, _, file, _)| *r == t.rect && *s == t.text && *file == t.file);
    if !shown {
        return;
    }
    if let Some(text) = &t.text {
        // The row's own look (hover or selection background, git colour), carried on.
        let look = f.buffer_mut()[(t.x, t.rect.y)].style();
        f.render_widget(Clear, ext);
        fill(f, ext, look.bg.unwrap_or(P.sidebar_bg));
        f.render_widget(Span::styled(format!("{text} "), look), ext);
        app.ui.tip_ext = ext;
    }
    for (r, text, err, button) in lines {
        f.render_widget(Clear, r);
        fill(f, r, P.widget_bg);
        let mut spans = vec![Span::styled(text.clone(), Style::new().fg(if err { P.error_fg } else { P.widget_fg }))];
        if button {
            let br = Rect::new(r.x + text.width() as u16, r.y, 6, 1).intersection(r);
            let hot = app.hover.is_some_and(|p| br.contains(p));
            spans.push(Span::styled(" Copy ", button_style(false, hot)));
            app.ui.tip_copy = br;
        }
        f.render_widget(Line::from(spans), r);
    }
    app.ui.tip_md5 = block;
}

fn search_row(app: &mut App, f: &mut Frame, r: Rect, bg: Color, hr: &HRow, i: usize) {
    let ic = icons();
    let g = crate::blocks::icon_gap(app.settings.spacing.icon_gap);
    match hr {
        HRow::File(p, n) => {
            let chev = if app.search_closed.contains(p) { ic.chev_right } else { ic.chev_down };
            app.ui.search_folds.push((Rect::new(r.x, r.y, (2 + g).min(r.width), 1), i));
            let dir = p.rsplit_once('/').map_or(String::new(), |(d, _)| format!(" {d}"));
            let (glyph, gfg) = file_icon(base(p), &app.settings.file_icons);
            let count = badge(*n, bg);
            let room = room_before(&count, (r.width as usize).saturating_sub(3 + 2 * g as usize));
            let (name, dir) = fit_pair(base(p), &dir, room);
            row(
                f,
                r,
                bg,
                vec![
                    Span::styled(format!(" {chev}{}", sp(g)), Style::new().fg(P.icon_fg)),
                    Span::styled(glyph, Style::new().fg(gfg)),
                    Span::raw(sp(g)),
                    Span::styled(name, Style::new().fg(P.sidebar_fg)),
                    Span::styled(dir, Style::new().fg(P.dim)),
                ],
            );
            right(f, r, count);
        }
        HRow::Hit(h) => {
            let h = &app.hits[*h];
            let mut spans = vec![Span::raw(sp(3 + 2 * g))]; // under the file name
            spans.extend(match_spans(&h.text, h.col, app.query.len()));
            row(f, r, bg, spans);
        }
    }
}

/// A result line with its match (`len` bytes at 1-based byte column `col`) marked, and
/// at most 12 characters kept before it.
pub(crate) fn match_spans(t: &str, col: usize, len: usize) -> Vec<Span<'static>> {
    let s = col.saturating_sub(1).min(t.len());
    let e = (s + len).min(t.len());
    let (a, m, z) = if t.is_char_boundary(s) && t.is_char_boundary(e) { (&t[..s], &t[s..e], &t[e..]) } else { (t, "", "") };
    let a = a.trim_start();
    let keep = a.chars().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<String>();
    let clean = |s: &str| s.replace('\t', " ").chars().map(|c| if c.is_control() { '·' } else { c }).collect::<String>();
    vec![
        Span::styled(clean(&keep), Style::new().fg(P.sidebar_fg)),
        Span::styled(clean(m), Style::new().fg(P.sidebar_fg).bg(P.find_other)),
        Span::styled(clean(z), Style::new().fg(P.sidebar_fg)),
    ]
}

pub(crate) fn scm_row(app: &mut App, f: &mut Frame, r: Rect, bg: Color, sr: &SRow, hot: bool) {
    let ic = icons();
    let g = crate::blocks::icon_gap(app.settings.spacing.icon_gap) / 2;
    match sr {
        SRow::Gap => {}
        SRow::MergeHead(n) => {
            let chev = if app.scm_folded[0] { ic.chev_right } else { ic.chev_down };
            row(f, r, bg, vec![Span::styled(format!(" {chev}{}Merge Changes", sp(g)), Style::new().fg(P.sidebar_fg).bold())]);
            let mut spans = vec![Span::styled(format!("{} ", ic.add), Style::new().fg(P.icon_fg))];
            spans.extend(badge(*n, bg));
            let x0 = right(f, r, spans);
            app.ui.scm_btns.push((Rect::new(x0, r.y, 1, 1), ScmAct::StageMerge));
        }
        SRow::Head(sec, n) => {
            let label = if *sec == Section::Staged { "Staged Changes" } else { "Changes" };
            let chev = if app.scm_folded[usize::from(*sec != Section::Staged) + 1] { ic.chev_right } else { ic.chev_down };
            row(f, r, bg, vec![Span::styled(format!(" {chev}{}{label}", sp(g)), Style::new().fg(P.sidebar_fg).bold())]);
            let acts: Vec<(&str, ScmAct)> = if *sec == Section::Staged {
                vec![(ic.remove, ScmAct::UnstageAll)]
            } else {
                vec![(ic.discard, ScmAct::DiscardAll), (ic.add, ScmAct::StageAll)]
            };
            let mut spans = Vec::new();
            for (icon, _) in &acts {
                spans.push(Span::styled(format!("{icon} "), Style::new().fg(P.icon_fg)));
            }
            spans.extend(badge(*n, bg));
            let x0 = right(f, r, spans);
            for (k, (_, a)) in acts.into_iter().enumerate() {
                app.ui.scm_btns.push((Rect::new(x0 + 2 * k as u16, r.y, 1, 1), a));
            }
        }
        SRow::File(i) => {
            let file = app.scm_files[*i].clone();
            let dir = file.path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            let letter = file.letter;
            let mut acts: Vec<(&str, ScmAct)> = vec![(ic.open_file, ScmAct::OpenFile(*i))];
            if file.section == Section::Staged {
                acts.push((ic.remove, ScmAct::Unstage(*i)));
            } else {
                acts.push((ic.discard, ScmAct::Discard(*i)));
                acts.push((ic.add, ScmAct::Stage(*i)));
            }
            let mut spans = Vec::new();
            if hot {
                for (icon, _) in &acts {
                    spans.push(Span::styled(format!("{icon} "), Style::new().fg(P.icon_fg)));
                }
            }
            spans.push(Span::styled(format!(" {letter} "), Style::new().fg(git_color(letter))));
            let deleted = letter == 'D';
            let mut name_st = Style::new().fg(git_color(letter));
            if deleted {
                name_st = name_st.add_modifier(Modifier::CROSSED_OUT);
            }
            let (glyph, gfg) = file_icon(base(&file.path), &app.settings.file_icons);
            // The icon lines up under the section label, the name after the gap.
            let lead = 2 + g as usize;
            let avail = room_before(&spans, (r.width as usize).saturating_sub(lead + 1 + g as usize));
            let (name, dir) = fit_pair(base(&file.path), &if dir.is_empty() { dir } else { format!(" {dir}") }, avail);
            row(
                f,
                r,
                bg,
                vec![
                    Span::raw(" ".repeat(lead)),
                    Span::styled(glyph, Style::new().fg(gfg)),
                    Span::raw(sp(g)),
                    Span::styled(name, name_st),
                    Span::styled(dir, Style::new().fg(P.dim)),
                ],
            );
            let x0 = right(f, r, spans);
            if hot {
                for (k, (_, a)) in acts.into_iter().enumerate() {
                    app.ui.scm_btns.push((Rect::new(x0 + 2 * k as u16, r.y, 1, 1), a));
                }
            }
        }
    }
}

pub(crate) fn input(f: &mut Frame, r: Rect, text: &str, placeholder: &str, focused: bool) {
    input_marked(f, r, text, placeholder, focused.then_some(P.focus_border));
}

/// An `input` field's whole text drawn selected (Ctrl+A, a double-click).
fn select_all(f: &mut Frame, r: Rect, text: &str) {
    let w = (text.width() as u16).min(r.width.saturating_sub(2));
    for x in r.x + 1..r.x + 1 + w {
        f.buffer_mut()[(x, r.y)].set_bg(P.selection);
    }
}

/// `input` with the focus mark in `mark`: a thin bar at its left edge, as a tab's.
pub(crate) fn input_marked(f: &mut Frame, r: Rect, text: &str, placeholder: &str, mark: Option<Color>) {
    if r.is_empty() { return; }
    fill(f, r, P.input_bg);
    let (t, st) = if text.is_empty() {
        (placeholder.to_string(), Style::new().fg(P.placeholder))
    } else {
        (text.to_string(), Style::new().fg(P.input_fg))
    };
    // A blank before the text, and its last cell kept for the caret (the callers put it
    // at most there), so a long text never runs under it or into what follows.
    let w = r.width.saturating_sub(2) as usize;
    // Keep the end (where the caret is) visible.
    let shown = if t.width() > w && !text.is_empty() {
        t.chars().rev().scan(0, |acc, c| {
            *acc += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            (*acc <= w).then_some(c)
        }).collect::<Vec<_>>().into_iter().rev().collect()
    } else {
        fit(&t, w)
    };
    f.render_widget(Span::styled(format!(" {shown}"), st), r);
    if let Some(c) = mark {
        f.buffer_mut()[(r.x, r.y)].set_symbol(if crate::theme::ascii() { "|" } else { "▎" }).set_fg(c);
    }
}

/// Group `g`'s hit areas, at its index (groups of other spaces keep empty ones).
fn put_hit(app: &mut App, g: usize, gh: GroupHit) {
    if app.ui.groups.len() <= g {
        app.ui.groups.resize_with(g + 1, GroupHit::default);
    }
    app.ui.groups[g] = gh;
}

/// A tab bar's scroll arrow: three cells (" ‹ "), lit under the pointer, easy to hit.
const TAB_ARROW_W: u16 = 3;

/// The first tab a bar of `room` cells shows, tabs `widths` wide: the active one in
/// view when it changed since last time (else as scrolled), no room left empty at the
/// end while tabs are hidden on the left, a cell for each arrow shown.
pub(crate) fn tab_scroll(grp: &mut crate::panel::Group, widths: &[u16], room: u16) -> usize {
    let n = widths.len();
    if n == 0 || widths.iter().sum::<u16>() <= room {
        grp.tab_off = 0;
        grp.tab_follow = Some(grp.active);
        return 0;
    }
    // Tabs from `off` that fit (with the arrows they need).
    let fits = |off: usize, last: usize| {
        let arrows = TAB_ARROW_W * (u16::from(off > 0) + u16::from(last + 1 < n));
        widths[off..=last].iter().sum::<u16>() + arrows <= room
    };
    let mut off = grp.tab_off.min(n - 1);
    if grp.tab_follow != Some(grp.active) {
        let a = grp.active.min(n - 1);
        if a < off {
            off = a;
        }
        while off < a && !fits(off, a) {
            off += 1;
        }
        grp.tab_follow = Some(grp.active);
    }
    // No blank end: shown from as far left as still fits the rest.
    while off > 0 && fits(off - 1, n - 1) {
        off -= 1;
    }
    grp.tab_off = off;
    off
}

fn editor_area(app: &mut App, f: &mut Frame, r: Rect, lay: Lay) {
    app.ui.groups.clear();
    app.ui.groups.resize_with(app.groups.len(), GroupHit::default);
    let space = app.space();
    if !app.groups.iter().any(|g| g.space == space) {
        // A space with nothing open yet: one word on what opens something here.
        let hint = crate::ext::hint(space);
        fill(f, r, P.editor_bg);
        if r.height > 2 && !hint.is_empty() {
            let w = hint.chars().count() as u16;
            f.render_widget(Span::styled(hint, Style::new().fg(P.dim)), Rect::new(r.x + r.width.saturating_sub(w) / 2, r.y + r.height / 2, w.min(r.width), 1));
        }
        return;
    }
    let st = app.settings.blocks;
    // Cards sit border to border; the other styles draw a 1-cell line between groups.
    let sep = u16::from(st != BlockStyle::Cards);
    // Columns: a group `below` sits under the one before it, in its column.
    let cols = app.columns(space);
    let n = cols.len().max(1) as u16;
    let gw = r.width.saturating_sub((n - 1) * sep) / n;
    let mut x = r.x;
    for (ci, col) in cols.iter().enumerate() {
        let w = if ci + 1 == cols.len() { r.right().saturating_sub(x) } else { gw };
        let m = col.len() as u16;
        let gh = r.height.saturating_sub((m - 1) * sep) / m;
        let mut y = r.y;
        for (k, &g) in col.iter().enumerate() {
            let h = if k + 1 == col.len() { r.bottom().saturating_sub(y) } else { gh };
            // Find's box sits in the group: its card keeps the focus border.
            let focused = g == app.ag && matches!(app.focus, Focus::Editor | Focus::Find) && !app.activity_lit;
            let area = crate::blocks::card(st, f, Rect::new(x, y, w, h), P.editor_bg, focused);
            group(app, f, g, area, lay, (area.y > y).then_some(y));
            y += h;
            if k + 1 < col.len() {
                match st {
                    BlockStyle::None => f.render_widget(Paragraph::new("─".repeat(usize::from(w))).style(Style::new().fg(P.group_border).bg(P.editor_bg)), Rect::new(x, y, w, 1)),
                    BlockStyle::Lines => crate::blocks::hline(y, x, x + w - 1),
                    BlockStyle::Cards => {}
                }
                y += sep;
            }
        }
        x += w;
        if ci + 1 < cols.len() {
            match st {
                BlockStyle::None => {
                    let lines: Vec<Line> = (0..r.height).map(|_| Line::raw("│")).collect();
                    f.render_widget(Paragraph::new(lines).style(Style::new().fg(P.group_border).bg(P.editor_bg)), Rect::new(x, r.y, 1, r.height));
                }
                BlockStyle::Lines => crate::blocks::vline(x, r.y, r.bottom() - 1),
                BlockStyle::Cards => {}
            }
            x += sep;
        }
    }
}

/// The file icon a tab shows for its document key.
fn tab_icon(app: &App, key: &str) -> (String, Color) {
    let path = match key.split_once(':') {
        Some(("file" | "preview", p)) => p,
        Some(("diff" | "cdiff", rest)) => rest.split_once(':').map_or(rest, |(_, p)| p),
        _ => return (icons().history.to_string(), P.icon_fg),
    };
    file_icon(base(path), &app.settings.file_icons)
}

fn group(app: &mut App, f: &mut Frame, g: usize, area: Rect, lay: Lay, border_y: Option<u16>) {
    // A restored tab's document is read when the tab is first shown.
    crate::session::materialize(app, g);
    let mut gh = GroupHit { area, ..Default::default() };
    let tabbar = Rect::new(area.x, area.y, area.width, 1);
    gh.tabbar = tabbar;
    fill(f, tabbar, P.tabs_bg);
    let focused_group = g == app.ag;
    let ic = icons();
    // Split Editor (right, then down) at the right end of the tab bar, Toggle Terminal before them.
    let split = Rect::new(tabbar.right().saturating_sub(3), tabbar.y, 2, 1);
    let split_down = Rect::new(split.x.saturating_sub(2), tabbar.y, 2, 1);
    // Each shown while that split may be made (3 columns, 3 groups a column, at most).
    let has_tabs = !app.groups[g].tabs.is_empty();
    let (can_right, can_down) = (has_tabs && app.can_split(g, false), has_tabs && app.can_split(g, true));
    let show_split = can_right || can_down;
    let term_btn = Rect::new(if show_split { split_down.x.saturating_sub(2) } else { split.x }, tabbar.y, 2, 1);
    let outline_btn = Rect::new(term_btn.x.saturating_sub(2), tabbar.y, 2, 1);
    let mut x = tabbar.x;
    let hover = app.hover;
    let pad = lay.s.tab_padding.div_ceil(2);
    let edge = sp(pad.div_ceil(2).max(1));
    let tail_edge = sp(pad / 2 + 1);
    let gap = sp(lay.s.icon_gap.div_ceil(2));
    // The name keeps the icon's gap from the dirty dot after it.
    let close_gap = gap.clone();
    // A dragged tab's slot in this bar: the cell before tab n, or after the last one.
    let slot = match app.drop {
        Some((dg, Zone::Tab(n))) if dg == g => Some(n),
        _ => None,
    };
    let mut marker = None;
    // Then [dirty dot] [blank] [cross]: fixed slots, so editing never moves the cross.
    let tail_w = 3 + tail_edge.len() as u16;
    // Each tab's label and width; the bar scrolled to show the active tab when that
    // changed (Ctrl+PgDn, a file opened), else where the wheel or an arrow left it.
    let labels: Vec<(String, u16)> = app.groups[g]
        .tabs
        .iter()
        .map(|t| {
            let (glyph, _) = tab_icon(app, &t.key);
            // Dirty state and close have separate fixed slots, so hover never hides an edit.
            let label = format!("{edge}{glyph}{gap}{}{close_gap}", fit(&app.tab_title(&t.key), 30));
            let w = label.width() as u16 + tail_w;
            (label, w)
        })
        .collect();
    let room = outline_btn.x.saturating_sub(tabbar.x);
    let first = tab_scroll(&mut app.groups[g], &labels.iter().map(|l| l.1).collect::<Vec<_>>(), room);
    let n_tabs = labels.len();
    let arrow = |left: bool| if crate::theme::ascii() { if left { "<" } else { ">" } } else if left { "‹" } else { "›" };
    // An arrow, three cells wide: lit white under the pointer, pressed (a button's
    // colours) while the mouse holds it down.
    let pressed = app.pressed;
    let draw_arrow = |f: &mut Frame, r: Rect, left: bool| {
        let hot = hover.is_some_and(|p| r.contains(p));
        let st = match (pressed.is_some_and(|q| q.intersects(r)), hot) {
            (true, _) => Style::new().fg(P.button_fg).bg(P.button_bg),
            // Under the pointer: lit white.
            (false, true) => Style::new().fg(Color::Rgb(255, 255, 255)).bg(P.tab_hover),
            _ => Style::new().fg(P.icon_fg).bg(P.tabs_bg),
        }
        .bold();
        f.render_widget(Span::styled(format!(" {} ", arrow(left)), st), r);
    };
    // Hidden on the left: an arrow first.
    if first > 0 {
        let r = Rect::new(x, tabbar.y, TAB_ARROW_W, 1);
        draw_arrow(f, r, true);
        gh.tab_prev = r;
        x += TAB_ARROW_W;
    }
    gh.tab_first = first;
    let end = outline_btn.x.saturating_sub(TAB_ARROW_W);
    for (i, t) in app.groups[g].tabs.iter().enumerate().skip(first) {
        if slot == Some(i) {
            marker = Some(x.saturating_sub(u16::from(i > first)));
        }
        let (label, w) = (&labels[i].0, labels[i].1);
        let (glyph, gfg) = tab_icon(app, &t.key);
        let dirty = matches!(app.docs.get(&t.key), Some(Doc::Text(b)) if b.dirty());
        let active = i == app.groups[g].active;
        // The cells before the buttons kept for the right arrow while more follow.
        let limit = if i + 1 < n_tabs { end } else { outline_btn.x };
        if x + w > limit {
            let r = Rect::new(end, tabbar.y, TAB_ARROW_W, 1);
            draw_arrow(f, r, false);
            gh.tab_next = r;
            break;
        }
        let tr = Rect::new(x, tabbar.y, w, 1);
        let hovered = hover.is_some_and(|p| tr.contains(p));
        let (bg, fg) = if active {
            (P.tab_active_bg, if focused_group { P.tab_active_fg } else { P.tab_unfocused_fg })
        } else {
            (if hovered { P.tab_hover } else { P.tab_inactive_bg }, P.tab_inactive_fg)
        };
        let mut st = Style::new().fg(fg).bg(bg);
        if t.preview {
            st = st.add_modifier(Modifier::ITALIC);
        }
        let xr = Rect::new(x + w - tail_w + 2, tabbar.y, 1, 1);
        let dot = if dirty { ic.dirty } else { " " };
        // Every tab can be closed from its cross (in the tab's own colour).
        let tail = ic.tab_close;
        let lead = edge.to_string();
        let name = label[lead.len() + glyph.len()..].to_string();
        row(
            f,
            tr,
            bg,
            vec![
                Span::styled(lead, st),
                Span::styled(glyph, Style::new().fg(gfg).bg(bg)),
                Span::styled(name, st),
                // Unsaved changes: a white dot, whatever the tab's colours.
                Span::styled(dot, Style::new().fg(Color::Rgb(255, 255, 255)).bg(bg)),
                Span::styled(" ", Style::new().bg(bg)),
                Span::styled(tail, Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD)),
                Span::styled(tail_edge.clone(), Style::new().bg(bg)),
            ],
        );
        // Every tab has a left edge: the shown one's in the accent, the others' grey.
        let color = match (active, focused_group) {
            (true, true) => P.tab_active_border,
            (true, false) => P.tab_border,
            (false, _) => P.tab_inactive_border,
        };
        let st = if active { Style::new().fg(color).bg(bg).bold() } else { Style::new().fg(color).bg(bg) };
        f.render_widget(Span::styled(if crate::theme::ascii() { "|" } else { "▎" }, st), Rect::new(x, tabbar.y, 1, 1));
        gh.tabs.push((Rect::new(x, tabbar.y, w - tail_w + 2, 1), xr));
        x += w;
    }
    if let Some(n) = slot.filter(|&n| n >= first + gh.tabs.len()) {
        marker = Some(if n == 0 { tabbar.x } else { x - 1 });
    }
    if let Some(mx) = marker.filter(|&mx| mx < tabbar.right()) {
        let st = Style::new().fg(P.tab_dnd_border).bg(P.tabs_bg);
        f.render_widget(Span::styled(if crate::theme::ascii() { "|" } else { "▌" }, st), Rect::new(mx, tabbar.y, 1, 1));
    }
    if show_split {
        for (r, glyph, down) in [(split, ic.split, false), (split_down, ic.split_down, true)].into_iter().filter(|(_, _, d)| if *d { can_down } else { can_right }) {
            let hot = hover.is_some_and(|p| r.contains(p));
            f.render_widget(Span::styled(glyph, Style::new().fg(if hot { P.tab_active_fg } else { P.icon_fg })), r);
            app.ui.split_btns.push((r, g, down));
        }
    }
    let lit = if app.term_open { P.tab_active_fg } else { P.icon_fg };
    f.render_widget(Span::styled(ic.terminal, Style::new().fg(lit)), term_btn);
    app.ui.term_btns.push(term_btn);
    // Off on a repository page: it shows its own outline.
    let page = app.groups[g].tabs.get(app.groups[g].active).is_some_and(|t| crate::ext::page_key(&t.key));
    let lit = if page { P.dim } else if app.outline_open { P.tab_active_fg } else { P.icon_fg };
    f.render_widget(Span::styled(ic.outline, Style::new().fg(lit)), outline_btn);
    if !page {
        app.ui.outline_btns.push(outline_btn);
    }
    let top = 1;
    let mut body = Rect::new(area.x, area.y + top, area.width, area.height.saturating_sub(top));
    let active = app.groups[g].active;
    let Some(key) = app.groups[g].tabs.get(active).map(|t| t.key.clone()) else {
        let start = body.y + body.height.saturating_sub(9) / 2;
        if start < body.bottom() {
            f.render_widget(Paragraph::new("herdr").alignment(Alignment::Center).style(Style::new().fg(P.dim).bold()), Rect::new(body.x, start, body.width, 1));
        }
        for (i, (label, side)) in [("Open File       Ctrl+P", None), ("Search          F4", Some(Side::Search)), ("Source Control  F2", Some(Side::Scm))].into_iter().enumerate() {
            let y = start + 2 + i as u16 * 2;
            if y >= body.bottom() { break; }
            let label = fit(label, body.width as usize);
            let w = label.width() as u16;
            let r = Rect::new(body.x + body.width.saturating_sub(w) / 2, y, w, 1);
            row(f, r, P.editor_bg, vec![Span::styled(label, Style::new().fg(P.focus_border))]);
            app.ui.welcome.push((r, g, side));
        }
        gh.text = body;
        put_hit(app, g, gh);
        return;
    };
    // A Markdown file (a patch): its preview (diff) and source buttons, on the breadcrumbs'
    // row.
    let look = crate::panel::other_look(&key).map(|(_, l)| l);
    if app.breadcrumbs_on && (key.starts_with("file:") || look.is_some()) && body.height > 2 {
        let r = Rect::new(body.x, body.y, body.width, 1);
        breadcrumbs(app, f, r, g, &key);
        if let Some(look) = look {
            let rendered = !key.starts_with("file:");
            let shown = match look {
                "patch:" => " diff ",
                "usage:" => " usage ",
                "log:" => " log ",
                _ => " preview ",
            };
            let mut x = r.right().saturating_sub(1);
            for (label, on) in [(shown, true), (" source ", false)].into_iter().rev() {
                x = x.saturating_sub(label.width() as u16 + 1);
                let b = Rect::new(x, r.y, label.width() as u16, 1);
                let hot = app.hover.is_some_and(|p| b.contains(p));
                f.render_widget(Span::styled(label, button_style(on == rendered, hot)), b);
                app.ui.md_btns.push((b, g, on));
            }
        }
        gh.crumbs = r;
        body = Rect::new(body.x, body.y + 1, body.width, body.height - 1);
    }
    if let Some(rel) = key.strip_prefix("preview:") {
        markdown_preview(app, f, g, rel, body, &mut gh);
        put_hit(app, g, gh);
        return;
    }
    if let Some(rel) = key.strip_prefix("log:") {
        crate::logview::draw(app, f, g, &key, rel, body, &mut gh);
        put_hit(app, g, gh);
        return;
    }
    // A patch as a diff: made again as the patch changes.
    if let Some(rel) = key.strip_prefix("patch:") {
        crate::patch::ensure(app, &key, rel);
    }
    // A map file's memory use, as its source changes.
    if let Some(rel) = key.strip_prefix("usage:") {
        crate::size::ensure(app, &key, rel);
    }
    // A driver's tab (ext.rs): drawn whole (a serial stream, the Programmer's page, an
    // extension's), a page (a repository's README), or its frame around the tab's
    // document, drawn below as an editor (a repository's file).
    match crate::ext::tab(app, f, g, body, &key, focused_group && app.focus == Focus::Editor) {
        Some(crate::ext::Drawn::Whole) => {
            gh.text = body;
            put_hit(app, g, gh);
            return;
        }
        Some(crate::ext::Drawn::Page(text, rows)) => {
            gh.text = text;
            gh.preview_rows = rows;
            put_hit(app, g, gh);
            return;
        }
        Some(crate::ext::Drawn::Rest(rest)) => body = rest,
        None => {}
    }
    let wrap = app.wraps(&key);
    let editor_focus = focused_group && app.focus == Focus::Editor;
    // The terminal draws its cursor over any popup: no caret where the commit card goes.
    let card = crate::scm::card_rect(app, f.area());
    let target = ScrollTarget::Editor(g);
    let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
    // editor.rulers with their colours resolved (none while toggled off).
    let rulers: Vec<(usize, Color)> = if app.rulers_on {
        app.settings.rulers.iter().map(|(c, col)| (*c, col.map_or(P.ruler, |x| x.over(P.editor_bg)))).collect()
    } else {
        Vec::new()
    };
    let slider_c = slider_color(app, track, target);
    let hrow = body.bottom().saturating_sub(u16::from(border_y.is_none()));
    let hcolor = slider_color(app, Rect::new(body.x, hrow, body.width, 1), ScrollTarget::EditorHorizontal(g));
    // The minimap as pixels where the terminal has Kitty graphics.
    let mini_px = if app.minimap_on { crate::media::pixel_cell(app) } else { None };
    // A picture or a PDF's pages: media.rs draws the tab (its bar, its buttons).
    if let Some(Doc::Text(b)) = app.docs.get(&key) {
        if let (Some(kind), Some(path)) = (b.media, b.path.clone()) {
            gh.text = body;
            crate::media::draw(app, f, g, &key, &path, kind, body);
            put_hit(app, g, gh);
            // Find, under the bar (a PDF's text).
            if focused_group && app.find.is_some() && kind == crate::media::Kind::Pdf {
                find_widget(app, f, Rect::new(body.x, body.y + 1, body.width, body.height.saturating_sub(1)), lay);
            }
            return;
        }
    }
    let sb = match app.docs.get_mut(&key) {
        None => {
            f.render_widget(Paragraph::new("Loading…").style(Style::new().fg(P.dim)), body);
            gh.text = body;
            None
        }
        Some(Doc::Diff(d)) => {
            // The modified side's minimap, as VS Code's diff editor shows it.
            let mini = (app.minimap_on && d.note.is_none()).then_some(()).and_then(|_| diff_mini(&mut app.diff_minis, &key, d)).map(|(buf, marks)| DiffMini {
                buf,
                marks,
                cfg: app.settings.minimap,
                hover: app.hover,
                drag: matches!(app.drag, Drag::Minimap(m, _) if m.group == g),
                px: mini_px,
            });
            let view = &mut app.groups[g].tabs[active].view;
            let (sb, mm, texts, hsb, img) = diff_view(f, d, view, body, slider_c, u16::from(lay.comfy), lay.s.editor_padding, &rulers, mini, focused_group, app.diff_syntax.entry(key.clone()).or_default(), app.find.as_ref().filter(|_| focused_group), border_y.is_some());
            if let Some((area, mini)) = mm {
                view.mini_start = mini.first;
                app.ui.minimaps.push(MiniHit { area, group: g, mini, scroll: view.row_of(view.scroll) });
                if let Some((img, slider)) = img {
                    let bg = |row: u16| slider.filter(|(t, l, _)| (*t..t + l).contains(&row)).map_or(P.mini_bg, |s| s.2);
                    crate::media::show_pixels(app, f, &format!("minimap:{g}"), area, &img, &bg);
                }
            }
            gh.text = body;
            gh.text.height = texts.map_or(body.height, |r| r[0].height);
            gh.diff_text = texts;
            if let Some(mut hsb) = hsb {
                hsb.target = ScrollTarget::EditorHorizontal(g);
                paint_slider(f, hsb.track, hsb.slider, hcolor);
                app.ui.scrollbars.push(hsb);
            }
            sb
        }
        Some(Doc::Text(b)) => {
            let view = &mut app.groups[g].tabs[active].view;
            let lang = lang_id(b.syntax_name());
            let rb = &app.settings.rainbow;
            let qd = app.qdiff.get(&key);
            let mut problems = app.lints.shown(&key);
            problems.sort_by_key(|p| p.3);
            let peek = app.peek.as_ref().filter(|p| p.group == g && p.key == key).and_then(|p| {
                let q = qd?;
                Some(PeekDraw {
                    hunk: q.hunks.get(p.hunk)?,
                    base: q.base.as_deref()?,
                    index: p.hunk,
                    count: q.hunks.len(),
                    file: base(key.trim_start_matches("file:")).to_string(),
                    scroll: p.scroll,
                })
            });
            let deco = Deco {
                bottom_border: border_y.is_some(),
                comfy: lay.comfy,
                gap: lay.s.editor_padding,
                icon_gap: lay.s.icon_gap,
                marks: app.qdiff.get(&key).map_or(&[][..], |q| &q.marks),
                rainbow: (app.rainbow_on && rb.applies(&lang)).then(|| (rb, rb.skip_errors(&lang))),
                guides: app.guides_on,
                ruler: app.ruler_on,
                find: if focused_group { app.find.as_ref() } else { None },
                slider: slider_c,
                minimap: app.minimap_on.then_some(app.settings.minimap),
                hover: app.hover,
                mini_drag: matches!(app.drag, Drag::Minimap(m, _) if m.group == g),
                mini_px,
                mini_rows: false,
                whitespace: app.settings.render_whitespace,
                occurrences: app.settings.occurrences,
                match_brackets: app.settings.match_brackets,
                bracket_colors: app.settings.bracket_colors,
                wrap,
                peek,
                rulers: &rulers,
                problems: &problems,
            };
            let out = text_editor(f, b, view, body, focused_group, editor_focus, &deco);
            // A binary (or an ELF) file: how to show it, on the banner's right.
            if b.readonly.is_some() && (b.binary || b.elf_file || b.hex.is_some() || b.pdf_text) {
                let now = App::shown_as(b);
                let mut x = body.right().saturating_sub(2);
                for (label, how) in b.path.as_deref().map(|p| crate::media::buttons(p, b.elf_file)).unwrap_or_default().into_iter().rev() {
                    let t = format!(" {label} ");
                    x = x.saturating_sub(t.width() as u16 + 1);
                    let r = Rect::new(x, body.y, t.width() as u16, 1);
                    let hot = app.hover.is_some_and(|p| r.contains(p));
                    f.render_widget(Span::styled(t, button_style(now == how, hot)), r);
                    app.ui.view_btns.push((r, how));
                }
            }
            if let Some(p) = out.caret.filter(|p| !card.is_some_and(|r| r.contains(*p))) {
                f.set_cursor_position(p);
            }
            gh.text = out.text;
            gh.marks_x = out.marks_x;
            gh.folds_x = out.folds_x;
            gh.zone = out.zone.map(|z| (z.y, z.height));
            gh.rows = out.rows;
            if let Some(mut hsb) = out.hsb {
                hsb.target = ScrollTarget::EditorHorizontal(g);
                paint_slider(f, hsb.track, hsb.slider, hcolor);
                app.ui.scrollbars.push(hsb);
            }
            if let Some((area, mini)) = out.mini {
                view.mini_start = mini.first;
                app.ui.minimaps.push(MiniHit { area, group: g, mini, scroll: view.row_of(view.scroll) });
                if let Some((img, slider)) = out.mini_img {
                    // The slider's rows in its colour under the clear picture, the rest the minimap's.
                    let bg = |row: u16| slider.filter(|(t, l, _)| (*t..t + l).contains(&row)).map_or(P.mini_bg, |s| s.2);
                    crate::media::show_pixels(app, f, &format!("minimap:{g}"), area, &img, &bg);
                }
            }
            if let Some(z) = out.zone {
                app.ui.peek_zone = z;
                app.ui.peek_body = out.peek_body;
                app.ui.peek_max = out.peek_max;
                app.ui.peek_btns = out.peek_btns;
            }
            out.sb
        }
    };
    if let Some(mut sb) = sb {
        sb.target = target;
        app.ui.scrollbars.push(sb);
    }
    put_hit(app, g, gh);
    // Conflict colours and actions, blame ghost text.
    crate::gitcmds::editor_overlay(app, f, g);
    if focused_group && app.find.is_some() {
        let text = app.ui.groups[g].text;
        find_widget(app, f, text, lay);
    }
}

/// VS Code's breadcrumbs: the file's folders and name, then the symbols around the
/// cursor, outermost first; cut from the left when narrow.
fn breadcrumbs(app: &mut App, f: &mut Frame, r: Rect, g: usize, key: &str) {
    fill(f, r, P.breadcrumb_bg);
    let st = Style::new().fg(P.breadcrumb_fg).bg(P.breadcrumb_bg);
    let rel = crate::panel::other_look(key).map_or_else(|| key.strip_prefix("file:").unwrap_or(key), |(rel, _)| rel);
    let names: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let mut parts: Vec<Vec<Span<'static>>> = names
        .iter()
        .enumerate()
        .map(|(n, p)| {
            let mut v = Vec::new();
            if n + 1 == names.len() {
                let (glyph, gfg) = file_icon(p, &app.settings.file_icons);
                v.push(Span::styled(format!("{glyph} "), Style::new().fg(gfg).bg(P.breadcrumb_bg)));
            }
            v.push(Span::styled(p.to_string(), st));
            v
        })
        .collect();
    let grp = &mut app.groups[g];
    let active = grp.active;
    let view = &mut grp.tabs[active].view;
    if let Some(Doc::Text(b)) = app.docs.get_mut(key) {
        // The folds (a symbol's extent) are worked out as the text is drawn, after this
        // row: a tab's first frame needs them now.
        b.clamp_view(view);
        let syms = b.symbols();
        let line = b.clamp(view.cursor).line;
        for s in crate::symbols::path_at(&syms, &b.lines, &view.fold_ends, line) {
            parts.push(vec![Span::styled(s.name.clone(), st)]);
        }
    }
    if let Some(last) = parts.last_mut().and_then(|p| p.last_mut()) {
        last.style = last.style.fg(P.breadcrumb_focus);
    }
    let sep = if crate::theme::ascii() { " > " } else { " › " };
    let total = |parts: &[Vec<Span>]| parts.iter().map(|p| width(p) as usize).sum::<usize>() + parts.len().saturating_sub(1) * sep.width() + 1;
    let mut cut = false;
    while parts.len() > 1 && total(&parts) + if cut { 2 } else { 0 } > r.width as usize {
        parts.remove(0);
        cut = true;
    }
    let mut spans = vec![Span::styled(if cut { " …" } else { " " }, st)];
    for (n, p) in parts.into_iter().enumerate() {
        if n > 0 || cut {
            spans.push(Span::styled(sep, st));
        }
        spans.extend(p);
    }
    f.render_widget(Line::from(spans), r);
}

fn sstyle(base: Style, s: &SStyle) -> Style {
    let c = s.foreground;
    let mut st = if c.a == 0 { base } else { base.fg(Color::Rgb(c.r, c.g, c.b)) };
    if s.font_style.contains(FontStyle::BOLD) {
        st = st.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        st = st.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        st = st.add_modifier(Modifier::UNDERLINED);
    }
    st
}

/// One display line: highlight segments, background overlays (byte ranges),
/// tabs expanded, wide chars clipped cleanly at both scroll edges.
pub(crate) fn render_line(
    text: &str,
    segs: Option<&[(usize, SStyle)]>,
    overlays: &[(usize, usize, Color)],
    eol_bg: Option<Color>,
    hscroll: usize,
    width: usize,
    base: Style,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur = String::new();
    let mut cur_st = base;
    let mut push = |s: &str, st: Style, spans: &mut Vec<Span<'static>>, cur: &mut String| {
        if st != cur_st && !cur.is_empty() {
            spans.push(Span::styled(std::mem::take(cur), cur_st));
        }
        cur_st = st;
        cur.push_str(s);
    };
    let (mut x, mut si) = (0usize, 0usize);
    let end = hscroll + width;
    for (i, c) in text.char_indices() {
        let nx = editor::advance(x, c);
        if x >= end {
            break;
        }
        if nx > hscroll {
            let mut st = base;
            if let Some(segs) = segs {
                while si < segs.len() && segs[si].0 <= i {
                    si += 1;
                }
                if let Some(s) = segs.get(si) {
                    st = sstyle(base, &s.1);
                }
            }
            if let Some(&(_, _, bg)) = overlays.iter().rev().find(|(a, b, _)| *a <= i && i < *b) {
                st = st.bg(bg);
            }
            let vis = nx.min(end) - x.max(hscroll);
            let glyph = if c == '\t' || x < hscroll || nx > end {
                " ".repeat(vis)
            } else if c.is_control() {
                "·".to_string()
            } else {
                c.to_string()
            };
            push(&glyph, st, &mut spans, &mut cur);
        }
        x = nx;
    }
    if let Some(bg) = eol_bg {
        if x >= hscroll && x < end {
            push(" ", base.bg(bg), &mut spans, &mut cur);
        }
    }
    if !cur.is_empty() {
        spans.push(Span::styled(cur, cur_st));
    }
    Line::from(spans)
}

/// What an editor draws besides the text.
struct Deco<'a> {
    /// A card's bottom frame can carry the scrollbar without taking a text row.
    bottom_border: bool,
    comfy: bool,
    /// Cells before the line numbers (editorPadding); the marker cell follows the number.
    gap: u16,
    icon_gap: u16,
    marks: &'a [Option<Mark>],
    /// indent-rainbow settings, and whether this language skips its error colour.
    rainbow: Option<(&'a Rainbow, bool)>,
    guides: bool,
    ruler: bool,
    find: Option<&'a Find>,
    slider: Rgba,
    minimap: Option<minimap::Cfg>,
    hover: Option<Position>,
    /// This editor's minimap slider is being dragged.
    mini_drag: bool,
    /// A cell's pixels where the minimap is drawn as an image (Kitty); else braille.
    mini_px: Option<(u16, u16)>,
    /// The marks tint their lines' whole rows in the minimap (a diff's changes).
    mini_rows: bool,
    whitespace: editor::Whitespace,
    occurrences: bool,
    match_brackets: bool,
    bracket_colors: bool,
    /// editor.wordWrap: lines wrap at the text width, no horizontal scrolling.
    wrap: bool,
    peek: Option<PeekDraw<'a>>,
    rulers: &'a [(usize, Color)],
    /// Problems (`lint`): line, byte range, severity; mildest first, so the worst draws last.
    problems: &'a [(usize, usize, usize, crate::lint::Sev)],
}

/// editor.rulers on one row: the cell at display column N (0-based), so the
/// line marks "after N characters", moved with horizontal scroll and drawn only
/// in a blank cell on the row's own background (never over text, a selection or
/// a find match, as VS Code draws them behind the text).
fn draw_rulers(f: &mut Frame, tr: Rect, hscroll: usize, rulers: &[(usize, Color)], line_bg: Color) {
    for &(col, fg) in rulers {
        if col < hscroll || col >= hscroll + tr.width as usize {
            continue;
        }
        let x = tr.x + (col - hscroll) as u16;
        let cell = &mut f.buffer_mut()[(x, tr.y)];
        if cell.symbol() == " " && cell.bg == line_bg {
            cell.set_symbol(icons().guide).set_fg(fg);
        }
    }
}

/// The quick-diff peek of one hunk, drawn as a zone below it.
struct PeekDraw<'a> {
    hunk: &'a Hunk,
    base: &'a [String],
    index: usize,
    count: usize,
    file: String,
    scroll: usize,
}

/// A pixel minimap's slider: its first row, rows, colour (drawn as the cells' background).
type MiniSlider = (u16, u16, Color);

/// What `text_editor` laid out, for hit-testing.
struct EdOut {
    text: Rect,
    sb: Option<ScrollHit>,
    hsb: Option<ScrollHit>,
    mini: Option<(Rect, Mini)>,
    /// The minimap as pixels, for the terminal to show (Kitty: `media::show_pixels`).
    mini_img: Option<(image::RgbaImage, Option<MiniSlider>)>,
    marks_x: Option<u16>,
    folds_x: Option<u16>,
    zone: Option<Rect>,
    peek_btns: Vec<(Rect, PeekBtn)>,
    peek_body: Rect,
    peek_max: usize,
    /// With word wrap, each text row's (line, first display column); see `GroupHit::rows`.
    rows: Vec<Option<(usize, usize)>>,
    /// Where the terminal cursor goes, when focused and on screen.
    caret: Option<Position>,
}

/// Put `glyph` in a cell that is still blank (indent guides never cover text).
fn mark_blank(f: &mut Frame, x: u16, y: u16, glyph: &str, fg: Color) {
    let cell = &mut f.buffer_mut()[(x, y)];
    if cell.symbol() == " " {
        cell.set_symbol(glyph).set_fg(fg);
    }
}

fn find_widget(app: &mut App, f: &mut Frame, text: Rect, lay: Lay) {
    let Some(fd) = app.find.as_ref() else { return };
    let ic = icons();
    let rows = if fd.replace.is_some() { 2 } else { 1 };
    // Comfortable: a rounded frame with findWidgetPadding inside, one cell off the scrollbar.
    let fp = lay.s.find_padding;
    let (bw, bh) = if lay.comfy { (2 + 2 * fp, 2) } else { (0, 0) };
    let off = u16::from(lay.comfy);
    let w = 52.min(text.width.saturating_sub(bw + off));
    if w < 30 || text.height < rows + bh + 1 {
        return;
    }
    let outer = Rect::new(text.right() - off - w - bw, text.y, w + bw, rows + bh).intersection(f.area());
    app.ui.find_area = outer;
    if lay.comfy {
        f.render_widget(Clear, outer);
        // Named in its top border, as the sidebar's views are.
        let title = Span::styled(" SEARCH ", Style::new().fg(P.sidebar_title).bold());
        let frame = popup_block(P.widget_bg, P.widget_fg, (fp, 0)).title(Line::from(vec![Span::raw(" "), title]));
        f.render_widget(frame, outer);
    }
    let r = Rect::new(outer.x + bw / 2, outer.y + bh / 2, w, rows).intersection(outer);
    fill(f, r, P.widget_bg);
    // Row 1: [query…] then "N of M" against the right edge; no buttons: Enter / Shift+Enter
    // step, Esc closes, Alt+C / Alt+W / Alt+R toggle (shown in the box only while on).
    let count = match (fd.matches.len(), fd.cur) {
        _ if fd.invalid => "Bad regex".to_string(),
        (0, _) if !fd.query.is_empty() => "No results".to_string(),
        (0, _) => String::new(),
        (n, Some(c)) => format!("{} of {n}", c + 1),
        (n, None) => format!("? of {n}"),
    };
    let cw = 12; // "No results", "999 of 9999"
    let iw = w - cw;
    let q = Rect::new(r.x, r.y, iw, 1);
    // The focus mark turns red while the query finds nothing.
    let missed = fd.invalid || (fd.matches.is_empty() && !fd.query.is_empty());
    let mark = (app.focus == Focus::Find && fd.field == 0).then_some(if missed { P.error_fg } else { P.focus_border });
    // The query's text stops before the toggles that are on (drawn at its right end).
    let qt = Rect { width: iw.saturating_sub(2 * on_count(fd)), ..q };
    fill(f, q, P.input_bg);
    input_marked(f, qt, &fd.query, "Find", mark);
    if mark.is_some() && fd.all {
        select_all(f, qt, &fd.query);
    }
    let on: Vec<&str> = [(ic.case, fd.case), (ic.word, fd.word), (ic.regex, fd.regex)].into_iter().filter(|t| t.1).map(|t| t.0).collect();
    for (k, icon) in on.iter().rev().enumerate() {
        let tr = Rect::new(q.right().saturating_sub(2 + 2 * k as u16), r.y, 1, 1);
        f.render_widget(Span::styled(*icon, Style::new().fg(P.option_fg).bg(P.option_bg)), tr);
    }
    // A terminal has one font size: the count is dimmed to sit back instead.
    let count_st = Style::new().fg(if count == "No results" || fd.invalid { P.error_fg } else { P.dim });
    let shown = fit(&count, cw as usize - 1);
    let cx = r.right().saturating_sub(shown.width() as u16);
    f.render_widget(Span::styled(shown, count_st), Rect::new(cx, r.y, r.right() - cx, 1));
    let mut btns = Vec::new();
    app.ui.find_inputs.push((q, 0));
    if let Some(rep) = &fd.replace {
        let rq = Rect::new(q.x, r.y + 1, iw, 1);
        input(f, rq, rep, "Replace", app.focus == Focus::Find && fd.field == 1);
        if app.focus == Focus::Find && fd.field == 1 && fd.all {
            select_all(f, rq, rep);
        }
        app.ui.find_inputs.push((rq, 1));
        for (k, (icon, b)) in [(ic.replace, FindBtn::ReplaceOne), (ic.replace_all, FindBtn::ReplaceAll)].into_iter().enumerate() {
            let br = Rect::new(rq.right() + 2 + 2 * k as u16, rq.y, 1, 1);
            f.render_widget(Span::styled(icon, Style::new().fg(P.icon_fg)), br);
            btns.push((br, b));
        }
    }
    if app.focus == Focus::Find {
        let (field, s, w) = if fd.field == 1 { (1, fd.replace.as_deref().unwrap_or(""), iw) } else { (0, fd.query.as_str(), qt.width) };
        let cx = q.x + 1 + (s.width() as u16).min(w.saturating_sub(2));
        f.set_cursor_position((cx, r.y + field));
    }
    app.ui.find_btns.extend(btns);
}

/// A `w`x`h` rect centred in `area` (`top`: one row below its top), always inside it.
pub(crate) fn centered(area: Rect, w: u16, h: u16, top: bool) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height.saturating_sub(u16::from(top))));
    let y = if top { area.y + 1 } else { area.y + (area.height - h) / 2 };
    Rect::new(area.x + (area.width - w) / 2, y, w, h).intersection(area)
}

fn quick(app: &mut App, f: &mut Frame, area: Rect, lay: Lay) {
    let fi = app.settings.file_icons.clone();
    let drag = match app.drag {
        Drag::Scroll(sb, _) => Some(sb.target),
        _ => None,
    };
    let hover = app.hover;
    let Some(q) = app.quick.as_mut() else { return };
    let (ph, pv) = lay.s.popup_padding;
    let extra = if lay.comfy { 3 + 2 * pv } else { 0 };
    let pop = centered(area, (area.width * 6 / 10).max(40), (area.height * 6 / 10).max(6 + extra), true);
    f.render_widget(Clear, pop);
    let (inp, list) = if lay.comfy {
        let block = popup_block(P.quick_bg, P.quick_fg, (ph, pv));
        let inner = block.inner(pop);
        f.render_widget(block, pop);
        let inp = Rect::new(inner.x, inner.y, inner.width, 1).intersection(pop);
        (inp, Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2)).intersection(pop))
    } else {
        f.buffer_mut().set_style(pop, Style::new().bg(P.quick_bg).fg(P.quick_fg));
        let inp = Rect::new(pop.x + 1, pop.y, pop.width.saturating_sub(2), 1).intersection(pop);
        (inp, Rect::new(pop.x, pop.y + 1, pop.width, pop.height.saturating_sub(1)).intersection(pop))
    };
    let ph = if q.commands { "Type the name of a command to run" } else { "Search files by name (append : to go to a line or @ to go to a symbol)" };
    let goto = !q.commands && matches!(q.query.chars().next(), Some(':' | '@'));
    input(f, inp, &q.query, ph, true);
    f.set_cursor_position((inp.x + 1 + (q.query.width() as u16).min(inp.width.saturating_sub(2)), inp.y));
    let lh = list.height.max(1) as usize;
    if q.sel < q.off {
        q.off = q.sel;
    } else if q.sel >= q.off + lh {
        q.off = q.sel + 1 - lh;
    }
    for (k, item) in q.items.iter().skip(q.off).take(lh).enumerate() {
        let rr = Rect::new(list.x, list.y + k as u16, list.width, 1);
        let on = q.off + k == q.sel;
        let (bg, fg) = if on { (P.quick_sel_bg, P.quick_sel_fg) } else { (P.quick_bg, P.quick_fg) };
        if goto {
            row(f, rr, bg, vec![Span::styled(format!(" {item}"), Style::new().fg(fg))]);
            if let Some((_, kind)) = q.goto.get(q.off + k).filter(|(_, kind)| !kind.is_empty()) {
                right(f, rr, vec![Span::styled(format!("{kind} "), Style::new().fg(P.dim))]);
            }
            continue;
        }
        if q.commands {
            row(f, rr, bg, vec![Span::styled(format!(" {item}"), Style::new().fg(fg))]);
            // The user's own key for it first, else the default one.
            let user = app.settings.keybindings.iter().find(|(_, l)| l == item).map(|(c, _)| crate::parse::chord_label(*c));
            if let Some(hint) = user.or_else(|| COMMANDS.iter().find(|c| c.0 == item).filter(|c| !c.2.is_empty()).map(|c| c.2.to_string())) {
                right(f, rr, vec![Span::styled(format!("{hint} "), Style::new().fg(P.dim))]);
            }
            continue;
        }
        let dir = item.rsplit_once('/').map_or("", |(d, _)| d);
        let (glyph, gfg) = file_icon(base(item), &fi);
        row(
            f,
            rr,
            bg,
            vec![
                Span::raw(" "),
                Span::styled(glyph, Style::new().fg(gfg)),
                Span::raw(sp(lay.s.icon_gap)),
                Span::styled(base(item).to_string(), Style::new().fg(fg)),
                Span::styled(format!("  {dir}"), Style::new().fg(P.dim)),
            ],
        );
    }
    let (n, off) = (q.items.len(), q.off);
    app.ui.quick = pop;
    app.ui.quick_list = list;
    if n > lh {
        // In the padding, one blank column off the rows when there is room;
        // compact: over the rows' last column.
        let x = if lay.comfy { list.right() + u16::from(lay.s.popup_padding.0 >= 2) } else { list.right() - 1 };
        let track = Rect::new(x, list.y, 1, list.height);
        let s = decor::slider(track.height, n, lh, off, n - lh);
        let c = if drag == Some(ScrollTarget::Quick) {
            P.slider_active
        } else if hover.is_some_and(|p| track.contains(p)) {
            P.slider_hover
        } else {
            P.slider
        };
        paint_slider(f, track, s, c);
        app.ui.scrollbars.push(ScrollHit { track, target: ScrollTarget::Quick, slider: s, max_off: n - lh, page: lh });
    }
}

fn prompt(app: &mut App, f: &mut Frame, area: Rect, lay: Lay) {
    let Some(p) = app.prompt.as_ref() else { return };
    let bp = lay.s.button_padding;
    // Wide enough for the text and for the button row.
    let buttons: usize = p.buttons.iter().map(|(l, _)| l.width() + 2 * bp as usize + 2).sum();
    let cw = p.text.iter().map(|t| t.width()).max().unwrap_or(20).max(36).max(buttons) as u16 + 2;
    let (r, body, by) = if lay.comfy {
        // Border, popupPadding, the text, a blank row, the buttons.
        let (ph, pv) = lay.s.popup_padding;
        let r = centered(area, cw + 2 + 2 * ph, p.text.len() as u16 + 4 + 2 * pv, false);
        f.render_widget(Clear, r);
        let block = popup_block(P.widget_bg, P.widget_fg, (ph, pv));
        let inner = block.inner(r);
        f.render_widget(block, r);
        (r, inner, inner.bottom().saturating_sub(1))
    } else {
        let r = centered(area, cw + 2, p.text.len() as u16 + 4, false);
        f.render_widget(Clear, r);
        f.buffer_mut().set_style(r, Style::new().bg(P.widget_bg).fg(P.widget_fg));
        (r, Rect::new(r.x + 1, r.y + 1, r.width - 2, p.text.len() as u16), r.bottom() - 2)
    };
    let lines: Vec<Line> = p
        .text
        .iter()
        .enumerate()
        .map(|(k, t)| {
            let st = if k == 0 { Style::new().fg(P.widget_fg).bold() } else { Style::new().fg(P.widget_fg) };
            Line::styled(if lay.comfy { t.clone() } else { format!(" {t}") }, st)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), Rect::new(body.x, body.y, body.width, p.text.len() as u16).intersection(r));
    let gap = 1 + u16::from(lay.comfy);
    let mut x = if lay.comfy { body.right() + gap } else { r.right() - 1 };
    let mut btns = Vec::new();
    for (i, (label, _)) in p.buttons.iter().enumerate().rev() {
        let t = format!("{}{label}{}", sp(bp), sp(bp));
        let bw = t.width() as u16;
        x = x.saturating_sub(bw + gap);
        // Too small a terminal: a button cut by the edge is drawn (and hit) only where visible.
        let br = Rect::new(x, by, bw, 1).intersection(r);
        let st = button_style(i == p.sel, app.hover.is_some_and(|h| br.contains(h)));
        if br.is_empty() {
            continue;
        }
        f.render_widget(Span::styled(t, st), br);
        btns.push((br, i));
    }
    app.ui.prompt_btns = btns;
}

#[cfg(test)]
mod lane_tests {
    #[test]
    fn the_git_lane_marks_each_slot() {
        let g = |lit: &[bool], ascii| super::lane_glyph(lit, ascii);
        assert_eq!([g(&[true, false], false), g(&[false, true], false), g(&[true, true], false), g(&[false, false], false)], ['▘', '▖', '▌', ' ']);
        assert_eq!([g(&[true, false, false, false], false), g(&[false, true, true, false], false), g(&[true; 4], false)], ['⠉', '⠶', '⣿']);
        assert_eq!(g(&[false, false, true, false], true), '|');
    }
}
