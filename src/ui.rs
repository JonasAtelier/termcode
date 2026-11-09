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
    #[test]
    fn word_span_is_bounded_and_utf8_safe() {
        let (old, new) = super::diff_word_spans("same old 中 end", "same new 文 end");
        assert_eq!(old, Some(5..12));
        assert_eq!(new, Some(5..12));
        assert!(super::diff_word_spans(&"x".repeat(2049), "new").0.is_none());
        assert!(super::diff_word_spans("same", "same").1.is_none());
    }
}

/// `workbench.density` plus `herdr-code.spacing`: comfortable (default) frames
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
    app.ui = Hits::default();
    app.gitui.hits = Default::default();
    let lay = lay(app);
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(P.editor_bg).fg(P.editor_fg)), area);
    if area.height < 5 || area.width < 30 {
        return;
    }
    // This frame's share of first-time highlighting (a far jump catches up over frames).
    let _hl = editor::FrameBudget::start();
    let title = Rect::new(area.x, area.y, area.width, 1);
    let status = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    title_bar(app, f, title, lay);
    status_bar(app, f, status, lay);
    // Between blocks: a gutter (cards), a shared line (lines), or only the sidebar's
    // divider (none). Each block draws inside what `blocks::card` leaves of its rect.
    let st = app.settings.blocks;
    crate::blocks::begin();
    // The spacing widths are the content's; a card adds its frame.
    let fr = crate::blocks::frame(st);
    let aw = (lay.s.activity_width + fr).min(body.width / 4);
    let act = Rect::new(body.x, body.y, aw, body.height);
    let inner = crate::blocks::card(st, f, act, P.activity_bg, false);
    activity_bar(app, f, inner);
    let mut x = act.right();
    if st != BlockStyle::None {
        if st == BlockStyle::Lines {
            crate::blocks::vline(x, body.y, body.bottom() - 1);
        }
        x += 1;
    }
    if app.side_visible {
        let max = body.width.saturating_sub(x - body.x + 1 + 20).max(12);
        let w = app.side_w.map_or(lay.s.sidebar_width + fr, |w| w + if app.side_w_legacy { fr } else { 0 }).clamp(12, max);
        app.ui.sidebar = Rect::new(x, body.y, w, body.height);
        sidebar(app, f, app.ui.sidebar, lay);
        x += w;
        // The resize handle: the line, or the gutter between the cards.
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
        x += 1;
    }
    let ed = Rect::new(x, body.y, body.right().saturating_sub(x), body.height);
    app.ui.editor_area = ed;
    editor_area(app, f, ed, lay);
    if let Some((g, zone)) = app.drop {
        if let Some(gh) = app.ui.groups.get(g) {
            let a = gh.area;
            let r = match zone {
                Zone::Left => Rect::new(a.x, a.y, a.width / 2, a.height),
                Zone::Right => Rect::new(a.x + a.width / 2, a.y, a.width - a.width / 2, a.height),
                Zone::Center => a,
                Zone::Tab(_) => Rect::default(), // the tab bar draws its insertion marker
            };
            fill(f, r, P.drop_bg);
        }
    }
    crate::blocks::finish(f);
    crate::navui::draw(app, f);
    crate::scm::draw_popups(app, f, area);
    if app.quick.is_some() {
        quick(app, f, area, lay);
    }
    if app.prompt.is_some() {
        prompt(app, f, area, lay);
    }
}

fn title_bar(app: &mut App, f: &mut Frame, r: Rect, lay: Lay) {
    let active = app
        .groups
        .get(app.ag)
        .and_then(|g| g.tabs.get(g.active))
        .map(|t| app.tab_title(&t.key));
    let text = match active {
        Some(t) => format!("{t} — {} — herdr-code", app.repo),
        None => format!("{} — herdr-code", app.repo),
    };
    let st = Style::new().fg(P.title_fg).bg(P.title_bg);
    fill(f, r, P.title_bg);
    let (tp, bp) = (lay.s.title_padding, lay.s.button_padding);
    let close = format!("{}{}{}Close{}", sp(bp), icons().close, sp(lay.s.icon_gap), sp(bp));
    let w = close.width() as u16;
    let t = fit(&text, r.width.saturating_sub(2 * (w + tp) + 2) as usize);
    let x = r.x + (r.width.saturating_sub(t.width() as u16)) / 2;
    f.render_widget(Span::styled(t, st), Rect::new(x, r.y, r.right().saturating_sub(x), 1));
    app.ui.close = Rect::new(r.right().saturating_sub(w + tp), r.y, w, 1);
    f.render_widget(Span::styled(close, Style::new().fg(P.status_err_fg).bg(P.status_err_bg)), app.ui.close);
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
        let enc = if b.readonly.is_some() && !b.binary {
            format!("{}{}Read-only", icons().lock, sp(lay.s.icon_gap))
        } else {
            "UTF-8".into()
        };
        let eol = if b.crlf { "CRLF" } else { "LF" };
        items = vec![s, indent, enc, eol.to_string(), b.syntax_name().to_string()];
    }
    // The left items stay whole. Short of room the message goes first (below), then the
    // right items from their left end: Ln/Col first, the language last.
    let right_text = |items: &[String]| if items.is_empty() { String::new() } else { format!("{}{edge}", items.join(&sep)) };
    while !items.is_empty() && lx as usize + gap as usize + right_text(&items).width() > r.right() as usize {
        items.remove(0);
    }
    let right = right_text(&items);
    let w = (right.width() as u16).min(r.width);
    let rx = r.right().saturating_sub(w);
    f.render_widget(Span::styled(right, st), Rect::new(rx, r.y, w, 1));
    // Blame left of them; the message (a settings error first) in what is left, cut once
    // with "…" and `gap` cells before what follows it.
    let end = crate::gitcmds::status_blame(app, f, Rect::new(lx, r.y, rx.saturating_sub(lx), 1), gap);
    let mut room = end.saturating_sub(lx + 2 * gap) as usize;
    let mut left = vec![Span::styled(sep.clone(), st)];
    if let Some(e) = &app.settings_err {
        let t = fit(&format!(" settings.json: {e} "), room);
        room = room.saturating_sub(t.width() + gap as usize);
        left.push(Span::styled(t, Style::new().fg(P.status_err_fg).bg(P.status_err_bg)));
        left.push(Span::styled(sep.clone(), st));
    }
    // Under 8 cells a cut message is noise ("op…"): it goes.
    left.push(Span::styled(if room < 8 { String::new() } else { fit(&app.msg, room) }, st));
    f.render_widget(Line::from(left), Rect::new(lx, r.y, end.saturating_sub(lx), 1));
}

/// VS Code's activity bar: each item a 3-row cell (2 in compact) with its icon centred,
/// the icon in a 2-cell slot (room for a non-Mono Nerd Font glyph). The active item gets
/// `activityBar.activeBorder` down its left edge over a subtle `activeBackground`; a
/// hovered one lights up from `inactiveForeground` to `foreground`.
fn activity_bar(app: &mut App, f: &mut Frame, r: Rect) {
    fill(f, r, P.activity_bg);
    let ic = icons();
    let empty = serde_json::Map::new();
    let theme = app.settings.theme.json.get("colors").and_then(serde_json::Value::as_object).unwrap_or(&empty);
    let active_bg = crate::blocks::active_bg(theme);
    let ih: u16 = if app.settings.compact { 2 } else { 3 };
    for (i, s) in SIDES.iter().enumerate() {
        let y = r.y + i as u16 * ih;
        if y + ih > r.bottom() || r.width == 0 {
            break;
        }
        let item = Rect::new(r.x, y, r.width, ih);
        let active = *s == app.side && app.side_visible;
        let hot = app.hover.is_some_and(|p| item.contains(p));
        let icon = match s {
            Side::Explorer => ic.files,
            Side::Search => ic.search,
            Side::Scm => ic.scm,
        };
        if active {
            fill(f, item, active_bg);
            for k in 0..ih {
                f.render_widget(Span::styled("▎", Style::new().fg(P.activity_border)), Rect::new(r.x, y + k, 1, 1));
            }
        }
        let fg = if active || hot { P.activity_fg } else { P.activity_inactive };
        let (ix, iy) = (r.x + 1 + r.width.saturating_sub(3) / 2, y + (ih - 1) / 2);
        f.render_widget(Span::styled(icon, Style::new().fg(fg)), Rect::new(ix, iy, 2.min(r.right().saturating_sub(ix)), 1));
        if *s == Side::Scm && !app.scm_files.is_empty() {
            activity_badge(f, r, item, (ix, iy), app.scm_files.len(), if active { active_bg } else { P.activity_bg });
        }
        app.ui.activity.push((item, *s));
    }
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

/// Sidebar title row with right-aligned action icons.
fn side_title(app: &mut App, f: &mut Frame, r: Rect, title: &str, lay: Lay, bg: Color) {
    fill(f, r, bg);
    let frame = crate::blocks::frame(app.settings.blocks) / 2;
    let ir = inset(r, (lay.pad + frame).min(r.width / 2));
    let rr = Rect::new(ir.right().saturating_sub(2).max(ir.x), r.y, ir.width.min(2), 1);
    // The title starts on the content's left edge: the section headers' chevron column.
    let x = (ir.x + 1).min(rr.x);
    let reserve = if app.side == Side::Scm { 4 } else { 1 };
    let text = fit(title, rr.x.saturating_sub(x + reserve) as usize);
    row(f, Rect::new(x, r.y, rr.x.saturating_sub(x), 1), bg, vec![Span::styled(text, Style::new().fg(P.sidebar_title))]);
    f.render_widget(Span::styled(icons().refresh, Style::new().fg(P.icon_fg).bg(bg)), rr);
    app.ui.find_btns.push((rr, FindBtn::Refresh));
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

fn paint_slider(f: &mut Frame, track: Rect, (start, len): (u16, u16), c: Rgba) {
    let track = track.intersection(f.area());
    let buf = f.buffer_mut();
    for y in track.y + start..(track.y + start + len).min(track.bottom()) {
        let cell = &mut buf[(track.x, y)];
        let under = cell.bg;
        cell.set_bg(c.over(under));
    }
}

fn sidebar(app: &mut App, f: &mut Frame, col: Rect, mut lay: Lay) {
    if app.side == Side::Explorer {
        // Halve the padding plus the row's leading blank, keeping whole cells.
        lay.pad = lay.pad.saturating_sub(1) / 2;
    }
    // Cards: the view's title sits on the workbench background, the sections are cards.
    let st = app.settings.blocks;
    let bg = if st == BlockStyle::Cards { crate::blocks::gutter() } else { P.sidebar_bg };
    fill(f, col, bg);
    let title = match app.side {
        Side::Explorer => "EXPLORER",
        Side::Search => "SEARCH",
        Side::Scm => "SOURCE CONTROL",
    };
    side_title(app, f, Rect::new(col.x, col.y, col.width, 1), title, lay, bg);
    // A card starts right under the title: no sectionGap rows against its border.
    let top = col.y + 1 + if st == BlockStyle::Cards { 0 } else { lay.s.section_gap };
    if app.side == Side::Scm {
        return crate::scm::draw_scm(app, f, Rect::new(col.x, top, col.width, col.bottom().saturating_sub(top)));
    }
    // The Explorer's TIMELINE pane takes the bottom of the sidebar.
    let (main, timeline) = if app.side == Side::Explorer { crate::scm::split_timeline(app, col) } else { (col, Rect::default()) };
    let main = Rect::new(col.x, top, col.width, main.bottom().saturating_sub(top));
    // A line between sections runs from the vertical line left of the sidebar to the right one.
    let (main, timeline) = crate::blocks::stack(st, main, timeline, col.x.saturating_sub(1), col.right());
    let focused = matches!(app.focus, Focus::Side | Focus::SearchInput);
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
                let files = app.search_rows().len() - app.hits.len();
                let summary = format!(" {} results in {files} files", app.hits.len());
                row(f, inset(Rect::new(r.x, y, r.width, 1), lay.pad), P.sidebar_bg, vec![Span::styled(summary, Style::new().fg(P.dim))]);
                y += 1;
            }
        }
        Side::Scm => {}
    }
    let list = Rect::new(r.x, y, r.width, r.bottom().saturating_sub(y));
    app.ui.side_list = list;
    let s = app.side as usize;
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
        let bg = if i == app.sel[s] && app.focus == Focus::Side {
            P.list_active_bg
        } else if i == app.sel[s] {
            P.list_inactive_bg
        } else if hover_row == Some(i) {
            P.list_hover_bg
        } else {
            P.sidebar_bg
        };
        fill(f, rr, bg);
        let ir = inset(rr, lay.pad);
        match app.side {
            Side::Explorer => explorer_row(app, f, ir, bg, i),
            Side::Search => search_row(app, f, ir, bg, &search_rows[i]),
            Side::Scm => {}
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
        crate::scm::draw_timeline(app, f, inner);
    }
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
    let s = app.side as usize;
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
            Side::Explorer => explorer_row(app, f, ir, bg, i),
            Side::Search => search_row(app, f, ir, bg, &search_rows[i]),
            Side::Scm => scm_row(app, f, ir, bg, &scm_rows[i], hover == Some(i)),
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
/// background over `under`, the surface behind. With ASCII icons a space takes each
/// cap's place: the square look. Either way one cell a side.
pub(crate) fn pill(text: String, st: Style, under: Color) -> Vec<Span<'static>> {
    if crate::theme::ascii() {
        return vec![Span::styled(format!(" {text} "), st)];
    }
    let cap = Style::new().fg(st.bg.unwrap_or(under)).bg(under);
    vec![Span::styled("\u{e0b6}", cap), Span::styled(text, st), Span::styled("\u{e0b4}", cap)]
}

/// Rounds a filled run's ends (see `pill`): `r`'s first and last cells become the caps,
/// `fill` over `under`. Nothing with ASCII icons, or in a run too short for both.
pub(crate) fn round_ends(f: &mut Frame, r: Rect, fill: Color, under: Color) {
    let r = r.intersection(f.area());
    if crate::theme::ascii() || r.width < 3 || r.height == 0 {
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

fn explorer_row(app: &App, f: &mut Frame, r: Rect, bg: Color, i: usize) {
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
    let name = fit(&n.name, (r.width as usize).saturating_sub(lead.width() + 1 + gap.len() + rw + 1));
    row(
        f,
        r,
        bg,
        vec![
            Span::styled(lead, Style::new().fg(P.icon_fg)),
            Span::styled(glyph, Style::new().fg(gfg)),
            Span::raw(gap),
            Span::styled(name, Style::new().fg(fg)),
        ],
    );
    if !right_spans.is_empty() {
        right(f, r, right_spans);
    }
}

fn search_row(app: &App, f: &mut Frame, r: Rect, bg: Color, hr: &HRow) {
    let ic = icons();
    let g = crate::blocks::icon_gap(app.settings.spacing.icon_gap);
    match hr {
        HRow::File(p, n) => {
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
                    Span::styled(format!(" {}{}", ic.chev_down, sp(g)), Style::new().fg(P.icon_fg)),
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
            let t = &h.text;
            let s = h.col.saturating_sub(1).min(t.len());
            let e = (s + app.query.len()).min(t.len());
            let (a, m, z) = if t.is_char_boundary(s) && t.is_char_boundary(e) { (&t[..s], &t[s..e], &t[e..]) } else { (t.as_str(), "", "") };
            let a = a.trim_start();
            let keep = a.chars().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect::<String>();
            let clean = |s: &str| s.replace('\t', " ").chars().map(|c| if c.is_control() { '·' } else { c }).collect::<String>();
            row(
                f,
                r,
                bg,
                vec![
                    Span::raw(sp(3 + 2 * g)), // under the file name
                    Span::styled(clean(&keep), Style::new().fg(P.sidebar_fg)),
                    Span::styled(clean(m), Style::new().fg(P.sidebar_fg).bg(P.find_other)),
                    Span::styled(clean(z), Style::new().fg(P.sidebar_fg)),
                ],
            );
        }
    }
}

pub(crate) fn scm_row(app: &mut App, f: &mut Frame, r: Rect, bg: Color, sr: &SRow, hot: bool) {
    let ic = icons();
    let g = crate::blocks::icon_gap(app.settings.spacing.icon_gap);
    match sr {
        SRow::Gap => {}
        SRow::MergeHead(n) => {
            row(f, r, bg, vec![Span::styled(format!(" {}{}Merge Changes", ic.chev_down, sp(g)), Style::new().fg(P.sidebar_fg).bold())]);
            let mut spans = vec![Span::styled(format!("{} ", ic.add), Style::new().fg(P.icon_fg))];
            spans.extend(badge(*n, bg));
            let x0 = right(f, r, spans);
            app.ui.scm_btns.push((Rect::new(x0, r.y, 1, 1), ScmAct::StageMerge));
        }
        SRow::Head(sec, n) => {
            let label = if *sec == Section::Staged { "Staged Changes" } else { "Changes" };
            row(f, r, bg, vec![Span::styled(format!(" {}{}{label}", ic.chev_down, sp(g)), Style::new().fg(P.sidebar_fg).bold())]);
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
    if r.is_empty() { return; }
    fill(f, r, P.input_bg);
    let (t, st) = if text.is_empty() {
        (placeholder.to_string(), Style::new().fg(P.placeholder))
    } else {
        (text.to_string(), Style::new().fg(P.input_fg))
    };
    let w = r.width.saturating_sub(1) as usize;
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
    if focused {
        f.buffer_mut().set_style(Rect::new(r.x, r.y, 1, 1), Style::new().bg(P.focus_border));
    }
}

fn editor_area(app: &mut App, f: &mut Frame, r: Rect, lay: Lay) {
    let n = app.groups.len().max(1) as u16;
    let gw = r.width.saturating_sub(n - 1) / n;
    let mut x = r.x;
    let st = app.settings.blocks;
    for g in 0..app.groups.len() {
        let w = if g + 1 == app.groups.len() { r.right().saturating_sub(x) } else { gw };
        let focused = g == app.ag && app.focus == Focus::Editor;
        let area = crate::blocks::card(st, f, Rect::new(x, r.y, w, r.height), P.editor_bg, focused);
        group(app, f, g, area, lay, (area.y > r.y).then_some(r.y));
        x += w;
        if g + 1 < app.groups.len() {
            match st {
                BlockStyle::None => {
                    let lines: Vec<Line> = (0..r.height).map(|_| Line::raw("│")).collect();
                    f.render_widget(Paragraph::new(lines).style(Style::new().fg(P.group_border).bg(P.editor_bg)), Rect::new(x, r.y, 1, r.height));
                }
                BlockStyle::Lines => crate::blocks::vline(x, r.y, r.bottom() - 1),
                BlockStyle::Cards => {}
            }
            x += 1;
        }
    }
}

/// The file icon a tab shows for its document key.
fn tab_icon(app: &App, key: &str) -> (String, Color) {
    let path = match key.split_once(':') {
        Some(("file", p)) => p,
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
    // Split Editor button at the right end of the tab bar.
    let split = Rect::new(tabbar.right().saturating_sub(3), tabbar.y, 2, 1);
    let mut x = tabbar.x;
    let hover = app.hover;
    let edge = sp(lay.s.tab_padding);
    let gap = sp(lay.s.icon_gap);
    // A dragged tab's slot in this bar: the cell before tab n, or after the last one.
    let slot = match app.drop {
        Some((dg, Zone::Tab(n))) if dg == g => Some(n),
        _ => None,
    };
    let mut marker = None;
    for (i, t) in app.groups[g].tabs.iter().enumerate() {
        if slot == Some(i) {
            marker = Some(x.saturating_sub(u16::from(i > 0)));
        }
        let title = app.tab_title(&t.key);
        let (glyph, gfg) = tab_icon(app, &t.key);
        let dirty = matches!(app.docs.get(&t.key), Some(Doc::Text(b)) if b.dirty());
        let active = i == app.groups[g].active;
        // Dirty state and close have separate fixed slots, so hover never hides an edit.
        let label = format!("{edge}{glyph}{gap}{}{gap}", fit(&title, 30));
        let tail_w = 2 + edge.len() as u16;
        let w = label.width() as u16 + tail_w;
        if x + w > split.x {
            break;
        }
        let (bg, fg) = if active {
            (P.tab_active_bg, if focused_group { P.tab_active_fg } else { P.tab_unfocused_fg })
        } else {
            (P.tab_inactive_bg, P.tab_inactive_fg)
        };
        let mut st = Style::new().fg(fg).bg(bg);
        if t.preview {
            st = st.add_modifier(Modifier::ITALIC);
        }
        let tr = Rect::new(x, tabbar.y, w, 1);
        let xr = Rect::new(x + w - tail_w + 1, tabbar.y, 1, 1);
        let hovered = hover.is_some_and(|p| tr.contains(p));
        let dot = if dirty { ic.dirty } else { " " };
        let tail = if active || hovered { ic.close } else { " " };
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
                Span::styled(format!("{dot}{tail}{edge}"), Style::new().fg(fg).bg(bg)),
            ],
        );
        if active {
            if let Some(y) = border_y {
                let color = if focused_group && app.focus == Focus::Editor { P.tab_active_border } else { P.tab_border };
                // The border keeps its glyph and foreground colour. Its underline
                // is a separate blue line along the selected tab's top edge.
                f.buffer_mut().set_style(Rect::new(x, y, w, 1),
                    Style::new().underline_color(color).add_modifier(Modifier::UNDERLINED));
            } else if focused_group && app.focus == Focus::Editor && !edge.is_empty() {
                f.render_widget(Span::styled(if crate::theme::ascii() { "|" } else { "▎" }, Style::new().fg(P.tab_active_border).bg(bg)), Rect::new(x, tabbar.y, 1, 1));
            }
        }
        gh.tabs.push((Rect::new(x, tabbar.y, w - tail_w + 1, 1), xr));
        x += w;
        if x < split.x {
            fill(f, Rect::new(x, tabbar.y, 1, 1), P.tab_border);
        }
        x += 1;
    }
    if let Some(n) = slot.filter(|&n| n >= gh.tabs.len()) {
        marker = Some(if n == 0 { tabbar.x } else { x - 1 });
    }
    if let Some(mx) = marker.filter(|&mx| mx < tabbar.right()) {
        let st = Style::new().fg(P.tab_dnd_border).bg(P.tabs_bg);
        f.render_widget(Span::styled(if crate::theme::ascii() { "|" } else { "▌" }, st), Rect::new(mx, tabbar.y, 1, 1));
    }
    if app.groups.len() < crate::panel::MAX_GROUPS && !app.groups[g].tabs.is_empty() {
        f.render_widget(Span::styled(ic.split, Style::new().fg(P.icon_fg)), split);
        app.ui.split_btns.push((split, g));
    }
    let top = 1;
    let body = Rect::new(area.x, area.y + top, area.width, area.height.saturating_sub(top));
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
        app.ui.groups.push(gh);
        return;
    };
    let editor_focus = focused_group && app.focus == Focus::Editor;
    let target = ScrollTarget::Editor(g);
    let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
    // editor.rulers with their colours resolved (none while toggled off).
    let rulers: Vec<(usize, Color)> = if app.rulers_on {
        app.settings.rulers.iter().map(|(c, col)| (*c, col.map_or(P.ruler, |x| x.over(P.editor_bg)))).collect()
    } else {
        Vec::new()
    };
    let slider_c = slider_color(app, track, target);
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
            });
            let view = &mut app.groups[g].tabs[active].view;
            let (sb, mm, texts) = diff_view(f, d, view, body, slider_c, u16::from(lay.comfy), lay.s.editor_padding, &rulers, mini, focused_group);
            if let Some((area, mini)) = mm {
                app.ui.minimaps.push(MiniHit { area, group: g, mini, scroll: view.row_of(view.scroll) });
            }
            gh.text = body;
            gh.diff_text = texts;
            sb
        }
        Some(Doc::Text(b)) => {
            let view = &mut app.groups[g].tabs[active].view;
            let lang = lang_id(b.syntax_name());
            let rb = &app.settings.rainbow;
            let qd = app.qdiff.get(&key);
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
                peek,
                rulers: &rulers,
            };
            let out = text_editor(f, b, view, body, focused_group, editor_focus, &deco);
            gh.text = out.text;
            gh.marks_x = out.marks_x;
            gh.folds_x = out.folds_x;
            gh.zone = out.zone.map(|z| (z.y, z.height));
            if let Some((area, mini)) = out.mini {
                app.ui.minimaps.push(MiniHit { area, group: g, mini, scroll: view.row_of(view.scroll) });
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
    app.ui.groups.push(gh);
    // Conflict colours and actions, blame ghost text.
    crate::gitcmds::editor_overlay(app, f, g);
    if focused_group && app.find.is_some() {
        let text = app.ui.groups[g].text;
        find_widget(app, f, text, lay);
    }
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
    peek: Option<PeekDraw<'a>>,
    rulers: &'a [(usize, Color)],
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

/// What `text_editor` laid out, for hit-testing.
struct EdOut {
    text: Rect,
    sb: Option<ScrollHit>,
    mini: Option<(Rect, Mini)>,
    marks_x: Option<u16>,
    folds_x: Option<u16>,
    zone: Option<Rect>,
    peek_btns: Vec<(Rect, PeekBtn)>,
    peek_body: Rect,
    peek_max: usize,
}

/// Put `glyph` in a cell that is still blank (indent guides never cover text).
fn mark_blank(f: &mut Frame, x: u16, y: u16, glyph: &str, fg: Color) {
    let cell = &mut f.buffer_mut()[(x, y)];
    if cell.symbol() == " " {
        cell.set_symbol(glyph).set_fg(fg);
    }
}

/// Lays out [minimap?][gutter][text][minimap?][blank][scrollbar] and draws the
/// text, its decorations and the peek zone below its hunk.
fn text_editor(
    f: &mut Frame,
    b: &mut Buffer,
    v: &mut View,
    area: Rect,
    group_focus: bool,
    editor_focus: bool,
    d: &Deco,
) -> EdOut {
    b.clamp_view(v);
    let mut area = area;
    let mut out = EdOut { text: area, sb: None, mini: None, marks_x: None, folds_x: None, zone: None, peek_btns: Vec::new(), peek_body: Rect::default(), peek_max: 0 };
    if let Some(ro) = &b.readonly {
        let banner = Rect::new(area.x, area.y, area.width, 1);
        let banner_text = format!(" {}{}{ro}", icons().lock, sp(d.icon_gap));
        row(f, banner, P.widget_bg, vec![Span::styled(banner_text, Style::new().fg(P.git_modified))]);
        area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
        out.text = area;
        if b.binary {
            return out;
        }
    }
    let ic = icons();
    let digits = b.lines.len().to_string().len().max(2) as u16;
    // Gutter: [pad][number][Git marker][fold button?]text [blank][scrollbar].
    // The chevron follows the Git lane directly; its trailing blank is clickable.
    let (pad, blank) = (d.gap, u16::from(d.comfy));
    let nums = pad + digits;
    let fold_width = if v.fold_ends.iter().any(Option::is_some) { 2 } else { 0 };
    let gutter = nums + 1 + fold_width;
    let base_text = area.width.saturating_sub(gutter + 1 + blank);
    let mw = d.minimap.map_or(0, |m| minimap::width(base_text, area.width, m.max_column, m.min_text));
    let left = d.minimap.is_some_and(|m| m.left);
    let gx = area.x + if left { mw } else { 0 };
    let text = Rect::new(gx + gutter, area.y, area.width.saturating_sub(gutter + 1 + blank + mw), area.height);
    out.text = text;
    out.marks_x = Some(gx + nums);
    let h = area.height as usize;
    let end = v.shift_line(v.scroll, h as isize).min(b.lines.len() - 1);
    b.highlight_view(v.scroll, end + 1);
    out.folds_x = (fold_width > 0).then_some(gx + nums + 1);
    let c = b.clamp(v.cursor);
    let sel = v.selection();
    let sel_bg = if group_focus { P.selection } else { P.sel_inactive };
    let tab = editor::tab();
    let guides = d.guides.then(|| Guides::new(&b.lines, tab));
    let active = guides.as_ref().and_then(|g| g.active(c.line));
    let visible = |col: usize| (col >= v.hscroll && col < v.hscroll + text.width as usize).then(|| text.x + (col - v.hscroll) as u16);
    // The peek zone sits before line `before` when that place is on screen.
    let zone = d.peek.as_ref().filter(|p| p.hunk.peek_before() >= v.scroll && p.hunk.peek_before() <= v.scroll + h).map(|p| {
        let body = p.hunk.base.len() + p.hunk.cur.len();
        (p.hunk.peek_before(), (2 + body as u16).min((area.height / 3).max(4)).min(area.height))
    });
    let (mut y, mut i) = (area.y, v.scroll);
    let mut cursor_y = None;
    while y < area.bottom() {
        if let Some((before, zh)) = zone {
            if out.zone.is_none() && i == before {
                // The gutter and the text only: a left minimap stays beside it.
                let zr = Rect::new(gx, y, text.right().saturating_sub(gx), zh.min(area.bottom() - y)).intersection(area);
                out.zone = Some(zr);
                y += zr.height;
                continue;
            }
        }
        if i >= b.lines.len() {
            break;
        }
        if i == c.line {
            cursor_y = Some(y);
        }
        let cur_line = i == c.line && sel.is_none();
        let bg = if cur_line { P.cur_line } else { P.editor_bg };
        let nr_st = Style::new().fg(if i == c.line { P.line_nr_active } else { P.line_nr }).bg(P.editor_bg);
        f.render_widget(Span::styled(format!("{}{:>w$}", sp(pad), i + 1, w = digits as usize), nr_st), Rect::new(gx, y, nums, 1));
        let (mark, mfg) = match d.marks.get(i).copied().flatten() {
            Some(Mark::Added) => (ic.gutter_bar, P.gutter_added),
            Some(Mark::Modified) => (ic.gutter_bar, P.gutter_modified),
            Some(Mark::Deleted) => (ic.gutter_del, P.gutter_deleted),
            None => (" ", P.editor_fg),
        };
        f.render_widget(Span::styled(mark, Style::new().fg(mfg).bg(P.editor_bg)), Rect::new(gx + nums, y, 1, 1));
        if let Some(x) = out.folds_x {
            let button = Rect::new(x, y, fold_width, 1).intersection(area);
            let foldable = v.fold_ends[i].is_some();
            let hot = foldable && d.hover.is_some_and(|p| button.contains(p));
            let glyph = if !foldable { " " } else if v.collapsed.contains(&i) { ic.chev_right } else { ic.chev_down };
            let st = Style::new().fg(if hot { P.icon_fg } else { P.line_nr_active }).bg(if hot { P.widget_bg } else { P.editor_bg });
            f.render_widget(Span::styled(format!("{glyph} "), st), button);
        }
        let line = &b.lines[i];
        let mut overlays: Vec<(usize, usize, Color)> = Vec::new();
        let mut thin: Vec<(usize, Color)> = Vec::new();
        if let Some((rb, skip)) = d.rainbow {
            for (s, e, paint) in rb.line(line, tab, skip) {
                match paint {
                    Paint::Level(n) if rb.light => thin.push((s, rb.colors[n % rb.colors.len()].over(bg))),
                    Paint::Level(n) => overlays.push((s, e, rb.colors[n % rb.colors.len()].over(bg))),
                    Paint::Error => overlays.push((s, e, rb.error.over(bg))),
                    Paint::Tabmix => overlays.push((s, e, rb.tabmix.unwrap_or(rb.error).over(bg))),
                }
            }
        }
        if let Some(fd) = d.find {
            for (n, (a, e)) in fd.matches.iter().enumerate().filter(|(_, (a, _))| a.line == i) {
                overlays.push((a.col, e.col, if fd.cur == Some(n) { P.find_match } else { P.find_other }));
            }
        }
        let mut eol = None;
        if let Some((a, e)) = sel {
            if a.line <= i && i <= e.line {
                let s = if a.line == i { a.col } else { 0 };
                let t = if e.line == i { e.col } else { line.len() };
                overlays.push((s, t, sel_bg));
                if e.line > i {
                    eol = Some(sel_bg);
                }
            }
        }
        let tr = Rect::new(text.x, y, text.width, 1);
        fill(f, tr, bg);
        let l = render_line(line, b.segs(i), &overlays, eol, v.hscroll, text.width as usize, Style::new().fg(P.editor_fg).bg(bg));
        f.render_widget(l, tr);
        if v.collapsed.contains(&i) {
            let end = editor::display_col(line, line.len()).saturating_sub(v.hscroll);
            if end + 2 < text.width as usize {
                f.render_widget(Span::styled(" …", Style::new().fg(P.line_nr).bg(bg)), Rect::new(text.x + end as u16, y, 2, 1));
            }
        }
        if let Some(g) = &guides {
            for lvl in 0..g.level(i) {
                let on = active.is_some_and(|(ak, a, e)| ak == lvl && (a..=e).contains(&i));
                if let Some(x) = visible(lvl * tab) {
                    mark_blank(f, x, y, ic.guide, if on { P.guide_active } else { P.guide });
                }
            }
        }
        for (s, col) in thin {
            if let Some(x) = visible(editor::display_col(line, s)) {
                mark_blank(f, x, y, ic.guide, col);
            }
        }
        draw_rulers(f, tr, v.hscroll, d.rulers, bg);
        let next = v.shift_line(i, 1);
        i = if next > i { next } else { b.lines.len() };
        y += 1;
    }
    // Rulers run the full height, below the last line too (the peek zone clears its rows).
    for ry in y..area.bottom() {
        draw_rulers(f, Rect::new(text.x, ry, text.width, 1), v.hscroll, d.rulers, P.editor_bg);
    }
    if let (Some(zr), Some(pk)) = (out.zone, d.peek.as_ref()) {
        draw_peek(f, zr, gx, gutter, text.x, pk, &b.lines, d, &mut out);
    }
    if editor_focus {
        let x = editor::display_col(&b.lines[c.line], c.col);
        if let Some(cy) = cursor_y.filter(|_| x >= v.hscroll && x < v.hscroll + text.width as usize) {
            f.set_cursor_position((text.x + (x - v.hscroll) as u16, cy));
        }
    }
    if let Some(cfg) = d.minimap.filter(|_| mw > 0) {
        let mr = Rect::new(if left { area.x } else { text.right() }, area.y, mw, area.height);
        let hovered = d.hover.is_some_and(|p| mr.contains(p));
        let slider = (cfg.slider_always || hovered || d.mini_drag).then(|| {
            if d.mini_drag {
                P.mini_slider_active
            } else if hovered {
                P.mini_slider_hover
            } else {
                P.mini_slider
            }
        });
        let mini = draw_minimap(f, b, v, mr, &cfg, d, slider);
        out.mini = Some((mr, mini));
    }
    // Vertical scrollbar with VS Code's overview-ruler marks under the slider.
    let track = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
    if track.height == 0 || area.width < gutter + 2 {
        return out;
    }
    let n = b.lines.len();
    let total = v.row_of(n - 1) + 1;
    let max_off = total.saturating_sub(1);
    let s = decor::slider(track.height, total + h.saturating_sub(1), h, v.row_of(v.scroll), max_off);
    paint_slider(f, track, s, d.slider);
    if d.ruler {
        let th = track.height as usize;
        let row_of = |line: usize| (v.row_of(line) * th / total.max(1)).min(th - 1);
        let mut lanes: Vec<Option<(u8, &str, Color)>> = vec![None; th];
        let mut put = |r: usize, prio: u8, g: &'static str, c: Color| {
            if lanes[r].is_none_or(|(p, _, _)| p <= prio) {
                lanes[r] = Some((prio, g, c));
            }
        };
        for (i, m) in d.marks.iter().enumerate().take(n) {
            let c = match m {
                Some(Mark::Added) => P.ruler_added,
                Some(Mark::Modified) => P.ruler_modified,
                Some(Mark::Deleted) => P.ruler_deleted,
                None => continue,
            };
            put(row_of(i), 1, ic.lane_left, c);
        }
        for (a, _) in d.find.map_or(&[][..], |fd| &fd.matches[..]) {
            put(row_of(a.line), 2, ic.lane_right, P.ruler_find);
        }
        put(row_of(c.line), 3, ic.lane_cursor, P.cursor);
        for (r, lane) in lanes.into_iter().enumerate() {
            if let Some((_, g, col)) = lane {
                f.buffer_mut()[(track.x, track.y + r as u16)].set_symbol(g).set_fg(col);
            }
        }
    }
    out.sb = Some(ScrollHit { track, target: ScrollTarget::Editor(0), slider: s, max_off, page: h.max(1) });
    out
}

/// The minimap in `r`: a git lane, then braille cells (a dot row per line, a dot
/// where its run of columns has code) or half blocks (a half per line), in the
/// cell's most frequent token colour muted towards the background (VS Code's
/// silhouette); find matches and the selection as cell backgrounds, and (when
/// shown) the slider as a translucent overlay.
fn draw_minimap(f: &mut Frame, b: &mut Buffer, v: &View, r: Rect, cfg: &minimap::Cfg, d: &Deco, slider: Option<Rgba>) -> Mini {
    // Editor rows, not buffer lines: a folded block is not drawn here either.
    let n = v.row_of(b.lines.len().saturating_sub(1)) + 1;
    let scroll = v.row_of(v.scroll);
    let view = r.height as usize; // the editor shows as many rows as the minimap is tall
    let braille = cfg.render_characters;
    let slots = if braille { 4 } else { 2 };
    let mini = Mini::new(r.height, n, view, slots, cfg.size);
    let end = v.line_of(mini.slot_rows(scroll, r.height.saturating_sub(1), slots - 1).end.saturating_sub(1)) + 1;
    // Only lines near the viewport are tokenised for the minimap; farther ones (fill and
    // fit on a long file) are sampled in the editor colour, not tokenised on every edit.
    b.highlight_upto(end.min(v.scroll + 2 * view));
    let cols = r.width.saturating_sub(1) as usize;
    let dcols = if braille { cols * 2 } else { cols };
    let dcpc = minimap::chars_per_col(dcols, cfg.max_column);
    let sel = v.selection();
    let finds: Vec<usize> = d.find.map_or(Vec::new(), |fd| fd.matches.iter().map(|(a, _)| a.line).collect());
    let tab = editor::tab();
    let ascii = crate::theme::ascii();
    // At most 4 sampled lines per slot: a squeezed huge file stays cheap.
    let slot_lines = |rows: std::ops::Range<usize>| -> Vec<usize> {
        let step = rows.len().div_ceil(4).max(1);
        rows.step_by(step).map(|row| v.line_of(row)).collect()
    };
    // The whole span, not the sampled lines: a match between them still shows.
    let slot_bg = |rows: &std::ops::Range<usize>| {
        if rows.is_empty() {
            return P.mini_bg;
        }
        let (l0, l1) = (v.line_of(rows.start), v.line_of(rows.end - 1));
        if finds.get(finds.partition_point(|&l| l < l0)).is_some_and(|&l| l <= l1) {
            P.mini_find
        } else if sel.is_some_and(|(a, e)| a.line <= l1 && e.line >= l0) {
            P.mini_sel
        } else {
            P.mini_bg
        }
    };
    let ink = |c: Option<Color>, under: Color| c.map_or(under, |c| minimap::ink(c, P.mini_fg_opacity, under));
    for row in 0..r.height {
        let y = r.y + row;
        let span = mini.slot_rows(scroll, row, 0).start..mini.slot_rows(scroll, row, slots - 1).end;
        // Git lane: the strongest mark among the row's lines.
        let lane = span
            .clone()
            .filter_map(|row| d.marks.get(v.line_of(row)).copied().flatten())
            .max_by_key(|m| match m {
                Mark::Modified => 3,
                Mark::Added => 2,
                Mark::Deleted => 1,
            });
        let (lg, lc) = match lane {
            Some(Mark::Added) => (ic_lane(ascii), P.mini_added),
            Some(Mark::Modified) => (ic_lane(ascii), P.mini_modified),
            Some(Mark::Deleted) => (ic_lane(ascii), P.mini_deleted),
            None => (" ", P.mini_bg),
        };
        let cell = &mut f.buffer_mut()[(r.x, y)];
        cell.set_symbol(lg).set_fg(lc).set_bg(P.mini_bg);
        let slot_samples: Vec<(std::ops::Range<usize>, Vec<Option<Color>>)> = (0..slots)
            .map(|k| {
                let rows = mini.slot_rows(scroll, row, k);
                let ls = slot_lines(rows.clone());
                let samples = minimap::sample(ls.iter().map(|&l| (b.lines[l].as_str(), b.segs(l))), dcols, dcpc, cfg.max_column, P.editor_fg, tab);
                (rows, samples)
            })
            .collect();
        // Slot backgrounds (find, selection) are per row, not per column.
        let bgs: Vec<Color> = slot_samples.iter().map(|(l, _)| slot_bg(l)).collect();
        let cell_bg = slot_bg(&span);
        for col in 0..cols {
            let x = r.x + 1 + col as u16;
            let cell = &mut f.buffer_mut()[(x, y)];
            if braille {
                let dots: Vec<[Option<Color>; 2]> = slot_samples.iter().map(|(_, s)| [s[col * 2], s[col * 2 + 1]]).collect();
                let (ch, c) = minimap::braille(&dots);
                cell.set_char(ch).set_fg(ink(c, cell_bg)).set_bg(cell_bg);
            } else {
                let (s0, s1) = (slot_samples[0].1[col], slot_samples[1].1[col]);
                let (up, down) = (ink(s0, bgs[0]), ink(s1, bgs[1]));
                if ascii {
                    // No half blocks (ambiguous width): the upper line if it has code, else the lower.
                    cell.set_symbol(" ").set_bg(if s0.is_some() { up } else { down });
                } else {
                    cell.set_symbol("▀").set_fg(up).set_bg(down);
                }
            }
        }
    }
    // Translucent, like VS Code's slider layer: over the background, and over a
    // half block's upper half (it is area too), but not over braille dots.
    if let Some(sc) = slider {
        let (top, len) = mini.slider(scroll);
        let buf = f.buffer_mut();
        for y in r.y + top..(r.y + top + len).min(r.bottom()) {
            for x in r.x..r.right() {
                let cell = &mut buf[(x, y)];
                let (fg, bg) = (cell.fg, cell.bg);
                if !braille && x > r.x {
                    cell.set_fg(sc.over(fg));
                }
                cell.set_bg(sc.over(bg));
            }
        }
    }
    mini
}

fn ic_lane(ascii: bool) -> &'static str {
    if ascii {
        "|"
    } else {
        "▎"
    }
}

/// The peek zone: a title row (file, N of M changes, Stage / Revert / ↑ / ↓ / ×),
/// the hunk's index lines on removed-line red and buffer lines on inserted-line
/// green with word highlights, and a bottom border in peekView.border.
#[allow(clippy::too_many_arguments)]
fn draw_peek(f: &mut Frame, zr: Rect, gx: u16, gutter: u16, tx: u16, pk: &PeekDraw, lines: &[String], d: &Deco, out: &mut EdOut) {
    let zr = zr.intersection(f.area());
    let tx = tx.min(zr.right());
    f.render_widget(Clear, zr);
    fill(f, zr, P.editor_bg);
    let ic = icons();
    let title = Rect::new(zr.x, zr.y, zr.width, 1);
    fill(f, title, P.peek_title_bg);
    let label = Style::new().fg(P.peek_title_fg).bg(P.peek_title_bg).bold();
    let desc = Style::new().fg(P.peek_desc_fg).bg(P.peek_title_bg);
    // Laid out right to left by priority: ↑ ↓ × always, then Revert, Stage, "N of M
    // changes", and the file name (cut from the left) gets what is left.
    let gap = d.icon_gap;
    let mut btns: Vec<(String, PeekBtn, bool)> = vec![
        (ic.up.into(), PeekBtn::Prev, true),
        (ic.down.into(), PeekBtn::Next, true),
        (ic.close.into(), PeekBtn::Close, true),
    ];
    let mut room = zr.width.saturating_sub(1 + btns.iter().map(|(t, _, _)| t.width() as u16 + gap).sum::<u16>());
    // Stage waits for the git backend (drawn dimmed).
    for (t, b, on) in [(" Revert ", PeekBtn::Revert, true), (" Stage ", PeekBtn::Stage, false)] {
        let w = t.width() as u16 + gap;
        if room < w {
            break; // never a lower-priority button without the higher one
        }
        room -= w;
        btns.insert(0, (t.into(), b, on));
    }
    let count = format!("  —  {} of {} changes", pk.index + 1, pk.count);
    let count = if (count.width() as u16) <= room { count } else { String::new() };
    room = room.saturating_sub(count.width() as u16);
    let name = if room >= 2 { fit_left(&pk.file, room as usize) } else { String::new() };
    f.render_widget(Line::from(vec![Span::styled(format!(" {name}"), label), Span::styled(count, desc)]), title);
    let total: u16 = btns.iter().map(|(t, _, _)| t.width() as u16 + gap).sum();
    let mut x = zr.right().saturating_sub(total);
    for (t, b, enabled) in btns {
        let w = t.width() as u16;
        let r = Rect::new(x, zr.y, w, 1).intersection(zr);
        let st = if !enabled {
            desc.add_modifier(Modifier::DIM)
        } else if matches!(b, PeekBtn::Stage | PeekBtn::Revert) {
            Style::new().fg(P.button_fg).bg(P.button_bg)
        } else {
            Style::new().fg(P.peek_title_fg).bg(P.peek_title_bg)
        };
        if !r.is_empty() {
            f.render_widget(Span::styled(t, st), r);
            out.peek_btns.push((r, b));
        }
        x += w + gap;
    }
    // Body: removed (index) rows, then added (buffer) rows.
    let h = pk.hunk;
    let (nb, nc) = (h.base.len(), h.cur.len());
    let body = Rect::new(zr.x, zr.y + 1, zr.width, zr.height.saturating_sub(2));
    out.peek_body = body;
    out.peek_max = (nb + nc).saturating_sub(body.height as usize);
    let scroll = pk.scroll.min(out.peek_max);
    let pairs = nb.min(nc);
    for k in 0..body.height as usize {
        let r = scroll + k;
        if r >= nb + nc {
            break;
        }
        let y = body.y + k as u16;
        let (removed, idx) = if r < nb { (true, r) } else { (false, r - nb) };
        let (text, no, line_bg, word) = if removed {
            (pk.base[h.base.start + idx].as_str(), h.base.start + idx + 1, P.diff_del_line, P.diff_del_text)
        } else {
            (lines.get(h.cur.start + idx).map_or("", String::as_str), h.cur.start + idx + 1, P.diff_ins_line, P.diff_ins_text)
        };
        let rr = Rect::new(zr.x, y, zr.width, 1);
        fill(f, rr, line_bg);
        let sign = if removed { "-" } else { "+" };
        // The editor's gutter: the number right before its marker cell, the sign in it.
        let num = format!("{:>w$}{sign}", no, w = gutter.saturating_sub(1) as usize);
        f.render_widget(Span::styled(num, Style::new().fg(P.line_nr).bg(line_bg)), Rect::new(gx, y, gutter, 1).intersection(rr));
        let words = if idx < pairs {
            let (o, n) = decor::word_changes(&pk.base[h.base.start + idx], lines.get(h.cur.start + idx).map_or("", String::as_str));
            if removed { o } else { n }
        } else {
            Vec::new()
        };
        let overlays: Vec<(usize, usize, Color)> = words.into_iter().map(|w| (w.start, w.end, word.over(line_bg))).collect();
        let tr = Rect::new(tx, y, zr.right().saturating_sub(tx), 1);
        f.render_widget(render_line(text, None, &overlays, None, 0, tr.width as usize, Style::new().fg(P.editor_fg).bg(line_bg)), tr);
    }
    if zr.height >= 2 {
        let by = zr.bottom() - 1;
        let border = "─".repeat(zr.width as usize);
        f.render_widget(Span::styled(border, Style::new().fg(P.peek_border).bg(P.editor_bg)), Rect::new(zr.x, by, zr.width, 1));
    }
}

/// A diff's minimap: the modified side as a buffer and each row's mark, the
/// settings, and the mouse over or dragging it.
pub(crate) struct DiffMini<'a> {
    pub buf: &'a mut Buffer,
    pub marks: &'a [Option<Mark>],
    pub cfg: minimap::Cfg,
    pub hover: Option<Position>,
    pub drag: bool,
}

/// The minimap buffer and marks of diff tab `key`, built once per doc: the
/// modified side, one buffer line per row (empty on a filler row).
pub(crate) fn diff_mini<'a>(cache: &'a mut std::collections::HashMap<String, (DiffId, Buffer, Vec<Option<Mark>>)>, key: &str, d: &DiffDoc) -> Option<(&'a mut Buffer, &'a [Option<Mark>])> {
    if cache.get(key).is_none_or(|e| e.0 != d.id) {
        let lines = d.rows.iter().map(|r| r.right.as_ref().map_or(String::new(), |(_, t)| t.clone())).collect();
        let marks = d
            .rows
            .iter()
            .map(|r| match r.kind {
                Kind::Same => None,
                Kind::Ins => Some(Mark::Added),
                Kind::Del => Some(Mark::Deleted),
                Kind::Change => Some(Mark::Modified),
            })
            .collect();
        cache.insert(key.to_string(), (d.id, Buffer::from_lines(&d.file.path, lines, "diff"), marks));
    }
    cache.get_mut(key).map(|e| (&mut e.1, e.2.as_slice()))
}

/// VS Code's side-by-side diff editor: [original][│][modified][minimap][blank]
/// [scrollbar with the overview ruler]. Both sides share `v` (scroll, hscroll),
/// so they always move together; only the visible rows are drawn. `blank`:
/// columns between the text and the scrollbar. Returns the scrollbar and the
/// minimap, for hit-testing.
/// What a diff view laid out: its scrollbar, its minimap, each side's text area.
pub(crate) type DiffHits = (Option<ScrollHit>, Option<(Rect, Mini)>, Option<[Rect; 2]>);

#[allow(clippy::too_many_arguments)]
pub(crate) fn diff_view(
    f: &mut Frame,
    d: &DiffDoc,
    v: &mut View,
    area: Rect,
    slider: Rgba,
    blank: u16,
    gap: u16,
    rulers: &[(usize, Color)],
    mini: Option<DiffMini>,
    focus: bool,
) -> DiffHits {
    if let Some(note) = &d.note {
        f.render_widget(Paragraph::new(note.as_str()).style(Style::new().fg(P.dim)), area);
        return (None, None, None);
    }
    let n = d.rows.len();
    v.scroll = v.scroll.min(n.saturating_sub(1));
    let track = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
    let area = Rect::new(area.x, area.y, area.width.saturating_sub(1 + blank), area.height);
    // As the editor's gutter: [pad][number][sign]code, pad = `gap` (editorPadding).
    let digits = n.to_string().len().max(2) as u16;
    let gw = gap + digits + 1;
    // The minimap takes its columns from the modified side, as in the editor.
    let right_w = area.width.saturating_sub(area.width.saturating_sub(1) / 2 + 1);
    let mw = mini.as_ref().map_or(0, |m| minimap::width(right_w.saturating_sub(gw), area.width, m.cfg.max_column, m.cfg.min_text));
    let text_area = Rect::new(area.x, area.y, area.width - mw, area.height);
    let half = text_area.width.saturating_sub(1) / 2;
    let sides = [Rect::new(area.x, area.y, half, area.height), Rect::new(area.x + half + 1, area.y, text_area.width.saturating_sub(half + 1), area.height)];
    // Sideways no further than the longest visible line needs, on the narrower side.
    let tw = sides[0].width.min(sides[1].width).saturating_sub(gw) as usize;
    let visible = d.rows.iter().skip(v.scroll).take(area.height as usize);
    let longest = visible.flat_map(|r| [&r.left, &r.right]).flatten().map(|(_, t)| editor::display_col(t, t.len())).max().unwrap_or(0);
    v.hscroll = v.hscroll.min(longest.saturating_sub(tw));
    let sep: Vec<Line> = (0..area.height).map(|_| Line::raw("│")).collect();
    f.render_widget(Paragraph::new(sep).style(Style::new().fg(P.group_border)), Rect::new(area.x + half, area.y, 1, area.height));
    // Each side's text, right of its gutter: where clicks and drags select (`panel::diff_pos`).
    let texts = sides.map(|r| Rect::new(r.x + gw.min(r.width), r.y, r.width.saturating_sub(gw), r.height));
    // The selection, on one side (`v.side`), in the editor's selection colour.
    let (sel, sel_bg) = (v.selection(), if focus { P.selection } else { P.sel_inactive });
    for (k, r) in d.rows.iter().skip(v.scroll).take(area.height as usize).enumerate() {
        let y = area.y + k as u16;
        let words = if r.kind == Kind::Change {
            match (&r.left, &r.right) {
                (Some((_, old)), Some((_, new))) => diff_word_spans(old, new),
                _ => (None, None),
            }
        } else { (None, None) };
        for (s, cell) in [&r.left, &r.right].into_iter().enumerate() {
            let sr = Rect::new(sides[s].x, y, sides[s].width, 1);
            let changed = match (s, r.kind) {
                (_, Kind::Same) => false,
                (0, Kind::Del | Kind::Change) | (1, Kind::Ins | Kind::Change) => true,
                _ => false,
            };
            match cell {
                None => {
                    let hatch = "╱".repeat(sr.width as usize);
                    row(f, sr, P.editor_bg, vec![Span::styled(hatch, Style::new().fg(P.diff_filler))]);
                }
                Some((n, text)) => {
                    let bg = match (changed, s) {
                        (false, _) => P.editor_bg,
                        (true, 0) => P.diff_del_line,
                        _ => P.diff_ins_line,
                    };
                    let sign = match (changed, s) {
                        (false, _) => " ",
                        (true, 0) => "-",
                        _ => "+",
                    };
                    fill(f, sr, bg);
                    let g = Span::styled(format!("{}{:>w$}{sign}", sp(gap), n + 1, w = digits as usize), Style::new().fg(P.line_nr).bg(bg));
                    f.render_widget(g, Rect::new(sr.x, y, gw.min(sr.width), 1));
                    let tr = Rect::new(sr.x + gw.min(sr.width), y, sr.width.saturating_sub(gw), 1);
                    let word_bg = if s == 0 { P.diff_del_text } else { P.diff_ins_text };
                    let ranges = if s == 0 { &words.0 } else { &words.1 };
                    let mut overlays: Vec<_> = ranges.iter().map(|w| (w.start, w.end, word_bg.over(bg))).collect();
                    // Last, so it wins over the diff's line and word colours.
                    let mut eol = None;
                    let i = v.scroll + k;
                    if let Some((a, b)) = sel.filter(|(a, b)| s as u8 == v.side && (a.line..=b.line).contains(&i)) {
                        let start = if i == a.line { a.col.min(text.len()) } else { 0 };
                        let end = if i == b.line { b.col.min(text.len()) } else { text.len() };
                        overlays.push((start, end, sel_bg));
                        eol = (i < b.line).then_some(sel_bg);
                    }
                    f.render_widget(render_line(text, None, &overlays, eol, v.hscroll, tr.width as usize, Style::new().fg(P.editor_fg).bg(bg)), tr);
                    draw_rulers(f, tr, v.hscroll, rulers, bg);
                }
            }
        }
    }
    let mut out_mini = None;
    if let Some(m) = mini.filter(|_| mw > 0) {
        let mr = Rect::new(text_area.right(), area.y, mw, area.height);
        let hovered = m.hover.is_some_and(|p| mr.contains(p));
        let ms = (m.cfg.slider_always || hovered || m.drag).then_some(if m.drag {
            P.mini_slider_active
        } else if hovered {
            P.mini_slider_hover
        } else {
            P.mini_slider
        });
        let deco = Deco {
            comfy: false,
            gap: 0,
            icon_gap: 1,
            marks: m.marks,
            rainbow: None,
            guides: false,
            ruler: false,
            find: None,
            slider,
            minimap: Some(m.cfg),
            hover: m.hover,
            mini_drag: m.drag,
            peek: None,
            rulers: &[],
        };
        // App::hl_pending counts this buffer (by its MiniHit) while it catches up.
        out_mini = Some((mr, draw_minimap(f, m.buf, v, mr, &m.cfg, &deco, ms)));
    }
    let h = track.height as usize;
    if h == 0 {
        return (None, out_mini, Some(texts));
    }
    let s = decor::slider(track.height, n + h - 1, h, v.scroll, n.saturating_sub(1));
    paint_slider(f, track, s, slider);
    diff_overview(f, &d.rows, v, track);
    (Some(ScrollHit { track, target: ScrollTarget::Editor(0), slider: s, max_off: n.saturating_sub(1), page: h }), out_mini, Some(texts))
}

/// A stable word span without diff work on the render path. Multiple edits on
/// one line share a span; long lines retain their line highlight only.
fn diff_word_spans(old: &str, new: &str) -> (Option<std::ops::Range<usize>>, Option<std::ops::Range<usize>>) {
    if old == new || old.len() > 2048 || new.len() > 2048 {
        return (None, None);
    }
    let prefix: usize = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    let suffix: usize = old[prefix..].chars().rev().zip(new[prefix..].chars().rev()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum();
    let span = |text: &str| {
        let mut start = prefix;
        let mut end = text.len() - suffix;
        if start == end { return None; }
        while let Some(c) = text[..start].chars().next_back().filter(|c| !c.is_whitespace()) {
            start -= c.len_utf8();
        }
        while let Some(c) = text[end..].chars().next().filter(|c| !c.is_whitespace()) {
            end += c.len_utf8();
        }
        Some(start..end)
    };
    (span(old), span(new))
}

/// The diff's overview ruler on its scrollbar track, as VS Code's two lanes:
/// removed rows in the cell's left half (`▌`), inserted in its right (`▐`), both
/// splitting the cell; the change F7 last jumped to as the cursor.
fn diff_overview(f: &mut Frame, rows: &[crate::diff::Row], v: &View, track: Rect) {
    let (n, th) = (rows.len(), track.height as usize);
    if n == 0 || th == 0 {
        return;
    }
    let row_of = |i: usize| (i * th / n).min(th - 1);
    let mut lanes = vec![(false, false); th];
    for (i, r) in rows.iter().enumerate() {
        let (del, ins) = match r.kind {
            Kind::Same => continue,
            Kind::Del => (true, false),
            Kind::Ins => (false, true),
            Kind::Change => (r.left.is_some(), r.right.is_some()),
        };
        let l = &mut lanes[row_of(i)];
        (l.0, l.1) = (l.0 || del, l.1 || ins);
    }
    let ic = icons();
    let buf = f.buffer_mut();
    for (k, lane) in lanes.into_iter().enumerate() {
        let cell = &mut buf[(track.x, track.y + k as u16)];
        let under = cell.bg;
        match lane {
            (true, true) => cell.set_symbol(ic.lane_left).set_fg(P.diff_ruler_del.over(under)).set_bg(P.diff_ruler_ins.over(under)),
            (true, false) => cell.set_symbol(ic.lane_left).set_fg(P.diff_ruler_del.over(under)),
            (false, true) => cell.set_symbol(ic.lane_right).set_fg(P.diff_ruler_ins.over(under)),
            (false, false) => continue,
        };
    }
    if v.want_x.is_some() {
        buf[(track.x, track.y + row_of(v.cursor.line) as u16)].set_symbol(ic.lane_cursor).set_fg(P.cursor);
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
    let ig = if lay.comfy { lay.s.icon_gap } else { 1 };
    let w = 52.min(text.width.saturating_sub(bw + off));
    if w < 30 || text.height < rows + bh + 1 {
        return;
    }
    let outer = Rect::new(text.right() - off - w - bw, text.y, w + bw, rows + bh).intersection(f.area());
    if lay.comfy {
        f.render_widget(Clear, outer);
        let frame = popup_block(P.widget_bg, P.widget_fg, (fp, 0));
        f.render_widget(frame, outer);
    }
    let r = Rect::new(outer.x + bw / 2, outer.y + bh / 2, w, rows).intersection(outer);
    fill(f, r, P.widget_bg);
    // [toggle][gap][input…][count][up][ ][down][ ][close][ ]
    let iw = w - 25 - ig;
    // Row 1: [>] [query  Aa ab] "N of M" [↑][↓][×]
    let toggle = Rect::new(r.x, r.y, 1, rows);
    f.render_widget(
        Span::styled(if fd.replace.is_some() { ic.chev_down } else { ic.chev_right }, Style::new().fg(P.icon_fg)),
        Rect::new(toggle.x, r.y, 1, 1),
    );
    let q = Rect::new(r.x + 1 + ig, r.y, iw, 1);
    input(f, q, &fd.query, "Find", app.focus == Focus::Find && fd.field == 0);
    let mut btns = vec![(toggle, FindBtn::ToggleReplace)];
    for (k, (icon, on, b)) in [(ic.case, fd.case, FindBtn::Case), (ic.word, fd.word, FindBtn::Word)].into_iter().enumerate() {
        let tr = Rect::new(q.right() - 4 + 2 * k as u16, r.y, 1, 1);
        let st = if on { Style::new().fg(P.option_fg).bg(P.option_bg) } else { Style::new().fg(P.input_fg).bg(P.input_bg) };
        f.render_widget(Span::styled(icon, st), tr);
        btns.push((tr, b));
    }
    let count = match (fd.matches.len(), fd.cur) {
        (0, _) if !fd.query.is_empty() => "No results".to_string(),
        (0, _) => String::new(),
        (n, Some(c)) => format!("{} of {n}", c + 1),
        (n, None) => format!("? of {n}"),
    };
    let count_st = Style::new().fg(if count == "No results" { P.error_fg } else { P.widget_fg });
    f.render_widget(Span::styled(format!(" {count}"), count_st), Rect::new(q.right(), r.y, 13, 1));
    let bx = r.right() - 7;
    for (k, (icon, b)) in [(ic.up, FindBtn::Prev), (ic.down, FindBtn::Next), (ic.close, FindBtn::Close)].into_iter().enumerate() {
        let br = Rect::new(bx + 2 * k as u16, r.y, 1, 1);
        f.render_widget(Span::styled(icon, Style::new().fg(P.icon_fg)), br);
        btns.push((br, b));
    }
    app.ui.find_inputs.push((q, 0));
    if let Some(rep) = &fd.replace {
        let rq = Rect::new(q.x, r.y + 1, iw, 1);
        input(f, rq, rep, "Replace", app.focus == Focus::Find && fd.field == 1);
        app.ui.find_inputs.push((rq, 1));
        for (k, (icon, b)) in [(ic.replace, FindBtn::ReplaceOne), (ic.replace_all, FindBtn::ReplaceAll)].into_iter().enumerate() {
            let br = Rect::new(rq.right() + 2 + 2 * k as u16, rq.y, 1, 1);
            f.render_widget(Span::styled(icon, Style::new().fg(P.icon_fg)), br);
            btns.push((br, b));
        }
    }
    if app.focus == Focus::Find {
        let (field, s) = if fd.field == 1 { (1, fd.replace.as_deref().unwrap_or("")) } else { (0, fd.query.as_str()) };
        let cx = q.x + 1 + (s.width() as u16).min(iw.saturating_sub(6));
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
    let ph = if q.commands { "Type the name of a command to run" } else { "Search files by name" };
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
        if q.commands {
            row(f, rr, bg, vec![Span::styled(format!(" {item}"), Style::new().fg(fg))]);
            if let Some(c) = COMMANDS.iter().find(|c| c.0 == item).filter(|c| !c.2.is_empty()) {
                right(f, rr, vec![Span::styled(format!("{} ", c.2), Style::new().fg(P.dim))]);
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
        let st = if i == p.sel { Style::new().fg(P.button_fg).bg(P.button_bg) } else { Style::new().fg(P.button2_fg).bg(P.button2_bg) };
        // Too small a terminal: a button cut by the edge is drawn (and hit) only where visible.
        let br = Rect::new(x, by, bw, 1).intersection(r);
        if br.is_empty() {
            continue;
        }
        f.render_widget(Span::styled(t, st), br);
        btns.push((br, i));
    }
    app.ui.prompt_btns = btns;
}
