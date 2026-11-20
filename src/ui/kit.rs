//! termcode's UI kit: every button, icon button, check box, input field, section header,
//! card, popup frame, splitter, scrollbar, pill, badge and progress bar is drawn here, so
//! each looks and lights up the same in every view (an extension's rows too). A view says
//! where and what; the kit says how.
//!
//! While a frame is drawn the kit knows the pointer (`begin`): a button lights under it
//! and lights more while held down, and every button drawn is noted (`end`), so moving
//! onto or off one redraws and a press shows, with no view keeping its own list.

use super::*;
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy)]
struct Ptr {
    hover: Option<Position>,
    held: Option<Rect>,
    /// The density (a button's padding, a header's indent), and the Cards block style.
    s: Spacing,
    cards: bool,
}

thread_local! {
    static PTR: Cell<Ptr> = const { Cell::new(Ptr { hover: None, held: None, s: Spacing::COMFORTABLE, cards: false }) };
    static BTNS: RefCell<Vec<Rect>> = const { RefCell::new(Vec::new()) };
}

/// A frame starts: where the pointer is, the button held down, the density and block style.
pub(crate) fn begin(app: &App) {
    let cards = app.settings.blocks == crate::blocks::BlockStyle::Cards;
    PTR.set(Ptr { hover: app.hover, held: app.pressed, s: app.settings.spacing, cards });
    BTNS.with_borrow_mut(Vec::clear);
}

/// The frame drawn: the buttons it drew (`app.ui.btns`).
pub(crate) fn end() -> Vec<Rect> {
    BTNS.take()
}

/// The pointer is on `r`.
pub(crate) fn hot(r: Rect) -> bool {
    PTR.get().hover.is_some_and(|p| r.contains(p))
}

/// `r` is the button held down.
fn held(r: Rect) -> bool {
    PTR.get().held.is_some_and(|q| q.intersects(r))
}

fn note(r: Rect) {
    if !r.is_empty() {
        BTNS.with_borrow_mut(|b| b.push(r));
    }
}

/// A button's kind: the main action, another, one that destroys, or one that cannot be
/// used now (greyed, no click).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Look {
    Primary,
    Secondary,
    Danger,
    Off,
}

/// The light a lit icon takes: white on a dark theme, black on a light one.
pub(crate) fn lit_fg() -> Color {
    match P.editor_bg {
        Color::Rgb(r, g, b) if (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 >= 128 => Color::Rgb(0, 0, 0),
        _ => Color::Rgb(255, 255, 255),
    }
}

/// `bg` lit a step further (a button held down): toward white on a dark theme, toward
/// black on a light one.
fn lighter(bg: Color) -> Color {
    let Color::Rgb(r, g, b) = lit_fg() else { return bg };
    Rgba(r, g, b, 255).alpha(0.22).over(bg)
}

/// A button's colours: its kind's, lit under the pointer, lighter still while held down.
pub(crate) fn style(look: Look, hot: bool, held: bool) -> Style {
    let (fg, bg, hover) = match look {
        Look::Primary => (P.button_fg, P.button_bg, P.button_hover),
        Look::Secondary => (P.button2_fg, P.button2_bg, P.button2_hover),
        Look::Danger => (P.button_fg, P.error_fg, lighter(P.error_fg)),
        Look::Off => return Style::new().fg(P.dim).bg(P.button2_bg),
    };
    let bg = match (held, hot) {
        (true, _) => lighter(hover),
        (false, true) => hover,
        _ => bg,
    };
    Style::new().fg(fg).bg(bg)
}

/// Blank cells each side of a button's label (`spacing.buttonPadding`, at least one).
fn pad() -> u16 {
    PTR.get().s.button_padding.max(1)
}

/// Buttons come in two sizes, drawn alike: an action button (a dialog's, a page's) with
/// `spacing.buttonPadding` blanks each side of its label, and a tool button (a toolbar's,
/// one inside a line of text) with one. The cells an action button with `label` takes.
pub(crate) fn button_w(label: &str) -> u16 {
    label.width() as u16 + 2 * pad()
}

/// The cells a tool button with `label` takes.
pub(crate) fn tool_w(label: &str) -> u16 {
    label.width() as u16 + 2
}

/// A tool button inside a line of text (an extension's row, a tooltip): its spans on
/// `under`, `r` where they land (`tool_w` wide). Lit and noted as `button`.
pub(crate) fn button_spans(label: &str, look: Look, r: Rect, under: Color) -> Vec<Span<'static>> {
    let st = style(look, look != Look::Off && hot(r), look != Look::Off && held(r));
    if look != Look::Off {
        note(r);
    }
    pill(label.to_string(), st, under)
}

/// A button filling `r`: its label centred, lit under the pointer or while `lit` (the
/// keyboard's choice), lighter while held; with a Nerd Font its end cells round over
/// `under`. Noted for hover and the pressed look (`Look::Off` is not: no click).
pub(crate) fn button(f: &mut Frame, r: Rect, label: &str, look: Look, under: Color, lit: bool) {
    let r = r.intersection(f.area());
    if r.is_empty() {
        return;
    }
    let st = style(look, look != Look::Off && (lit || hot(r)), look != Look::Off && held(r));
    let bg = st.bg.unwrap_or(P.button_bg);
    fill(f, r, bg);
    let t = fit(label, r.width.saturating_sub(2) as usize);
    let x = r.x + r.width.saturating_sub(t.width() as u16) / 2;
    f.render_widget(Span::styled(t, st), Rect::new(x, r.y, r.right() - x, 1));
    round_ends(f, r, bg, under);
    if look != Look::Off {
        note(r);
    }
}

/// A button at (x, y) in `b`, as wide as its label needs: drawn whole or not at all. Its
/// rect (empty when it did not fit).
pub(crate) fn button_at(f: &mut Frame, b: Rect, (x, y): (u16, u16), label: &str, look: Look, under: Color) -> Rect {
    let r = Rect::new(x, y, button_w(label), 1);
    if y < b.y || y >= b.bottom() || x < b.x || r.right() > b.right() {
        return Rect::default();
    }
    button(f, r, label, look, under, false);
    r
}

/// Buttons left to right from `b`'s first cell, a blank between: each drawn whole or not
/// at all. Their rects, in order (empty where one did not fit).
pub(crate) fn button_row(f: &mut Frame, b: Rect, items: &[(&str, Look)], under: Color) -> Vec<Rect> {
    let mut x = b.x;
    items
        .iter()
        .map(|&(label, look)| {
            let r = button_at(f, b, (x, b.y), label, look, under);
            if !r.is_empty() {
                x = r.right() + 1;
            }
            r
        })
        .collect()
}

/// The colours of a button at `r` whose content the caller draws (a coloured dot, a struck
/// label): lit and pressed as `button`, and noted the same.
pub(crate) fn look_at(look: Look, r: Rect) -> Style {
    if look != Look::Off {
        note(r);
    }
    style(look, look != Look::Off && hot(r), look != Look::Off && held(r))
}

/// Tool buttons right to left from `b`'s last cell, a blank between (they read left to
/// right, the last given rightmost). Their rects in the order given (empty where one did
/// not fit), and the column left of them all.
pub(crate) fn button_row_end(f: &mut Frame, b: Rect, items: &[(&str, Look)], under: Color) -> (Vec<Rect>, u16) {
    let mut x = b.right();
    let mut out = vec![Rect::default(); items.len()];
    for (i, &(label, look)) in items.iter().enumerate().rev() {
        let w = tool_w(label);
        if x < b.x + w {
            break;
        }
        x -= w;
        let r = Rect::new(x, b.y, w, 1);
        button(f, r, label, look, under, false);
        out[i] = r;
        x = x.saturating_sub(1);
    }
    (out, x)
}

/// A segmented control at (x, y) in `b`: the choices side by side in one pill on `under`,
/// the chosen one in a button's colours, one under the pointer lit. Each choice's rect
/// (none when it did not all fit), and the x after it.
pub(crate) fn segmented(f: &mut Frame, b: Rect, (x, y): (u16, u16), opts: &[String], sel: usize, under: Color) -> (Vec<Rect>, u16) {
    if y < b.y || y >= b.bottom() {
        return (Vec::new(), x);
    }
    let nerd = crate::theme::nerd();
    let mut cx = x + u16::from(nerd);
    let parts: Vec<(Rect, Style, String)> = opts
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let r = Rect::new(cx, y, o.width() as u16 + 2, 1);
            cx += r.width;
            let st = if i == sel {
                Style::new().fg(P.button_fg).bg(P.button_bg).bold()
            } else if held(r) {
                Style::new().fg(P.button_fg).bg(P.button_hover)
            } else if hot(r) {
                Style::new().fg(P.editor_fg).bg(P.list_hover_bg)
            } else {
                Style::new().fg(P.input_fg).bg(P.input_bg)
            };
            (r, st, format!(" {o} "))
        })
        .collect();
    if cx > b.right() || parts.is_empty() {
        return (Vec::new(), cx + 2);
    }
    for (r, st, t) in &parts {
        f.render_widget(Span::styled(t.clone(), *st), *r);
        note(*r);
    }
    if nerd {
        let cap = |c: &'static str, st: Style| Span::styled(c, Style::new().fg(st.bg.unwrap_or(P.input_bg)).bg(under));
        f.render_widget(cap("\u{e0b6}", parts[0].1), Rect::new(x, y, 1, 1));
        f.render_widget(cap("\u{e0b4}", parts[parts.len() - 1].1), Rect::new(cx, y, 1, 1));
    }
    (parts.into_iter().map(|p| p.0).collect(), cx + 2)
}

/// An icon button (a toolbar's, a tab bar's): `glyph` in the icon colour on `bg`, lit
/// while `on` (a panel it toggles is open); lit with a backdrop under the pointer, in a
/// button's colours while held down.
pub(crate) fn icon_button(f: &mut Frame, r: Rect, glyph: &str, bg: Color, on: bool) {
    let r = r.intersection(f.area());
    if r.is_empty() {
        return;
    }
    let st = if held(r) {
        Style::new().fg(P.button_fg).bg(P.button_bg)
    } else if hot(r) {
        Style::new().fg(lit_fg()).bg(P.list_hover_bg)
    } else {
        Style::new().fg(if on { lit_fg() } else { P.icon_fg }).bg(bg)
    };
    fill(f, r, st.bg.unwrap_or(bg));
    let x = r.x + r.width.saturating_sub(glyph.width() as u16) / 2;
    f.render_widget(Span::styled(glyph.to_string(), st.bold()), Rect::new(x, r.y, r.right() - x, 1));
    note(r);
}

/// A check box and its label at (x, y) in `b`: ticked in a button's colours, empty as an
/// input field; lit under the pointer. Its rect (empty when it did not fit).
pub(crate) fn check(f: &mut Frame, b: Rect, (x, y): (u16, u16), on: bool, label: &str) -> Rect {
    let w = 4 + label.width() as u16;
    let r = Rect::new(x, y, w, 1).intersection(b);
    if r.is_empty() || y < b.y {
        return Rect::default();
    }
    let lit = hot(r);
    let box_r = Rect::new(r.x, r.y, 3.min(r.width), 1);
    let under: Vec<Color> = box_r.positions().map(|p| f.buffer_mut()[p].bg).collect();
    let mark = if crate::theme::ascii() {
        Span::styled(if on { "[x]" } else { "[ ]" }, Style::new().fg(if lit { lit_fg() } else { P.editor_fg }))
    } else if on {
        Span::styled(format!(" {} ", icons().check), Style::new().fg(P.button_fg).bg(if lit { P.button_hover } else { P.button_bg }))
    } else {
        Span::styled("   ", Style::new().bg(if lit { P.list_hover_bg } else { P.input_bg }))
    };
    f.render_widget(Line::from(vec![mark, Span::raw(" "), Span::styled(label.to_string(), Style::new().fg(P.editor_fg))]), r);
    if !crate::theme::ascii() {
        let fill = match (on, lit) {
            (true, true) => P.button_hover,
            (true, false) => P.button_bg,
            (false, true) => P.list_hover_bg,
            (false, false) => P.input_bg,
        };
        super::pixels::ask(f, box_r, super::pixels::Shape::Check { on, fill, mark: P.button_fg, under: P.editor_bg }, under);
    }
    note(r);
    r
}

/// A search field: the search icon, the query (its end in view) or the placeholder, `tail`
/// at the right in `tail_fg` (a count), the focus mark at its left edge while `focused`.
/// The caret's column.
pub(crate) fn search_input(f: &mut Frame, r: Rect, text: &str, placeholder: &str, focused: bool, tail: &str, tail_fg: Color) -> u16 {
    let r = r.intersection(f.area());
    if r.is_empty() {
        return r.x;
    }
    fill(f, r, P.input_bg);
    let icon = format!(" {} ", icons().search);
    let (iw, tw) = (icon.width() as u16, (tail.width() as u16).min(r.width / 2));
    f.render_widget(Span::styled(icon, Style::new().fg(P.icon_fg)), Rect::new(r.x, r.y, iw.min(r.width), 1));
    let tr = Rect::new(r.x + iw.min(r.width), r.y, r.width.saturating_sub(iw + tw), 1);
    let room = tr.width as usize;
    let (shown, fg) = if text.is_empty() { (fit(placeholder, room), P.placeholder) } else { (fit_left(text, room), P.input_fg) };
    f.render_widget(Span::styled(shown, Style::new().fg(fg)), tr);
    f.render_widget(Span::styled(fit(tail, tw as usize), Style::new().fg(tail_fg)), Rect::new(r.right() - tw, r.y, tw, 1));
    if focused {
        f.buffer_mut()[(r.x, r.y)].set_symbol(if crate::theme::ascii() { "|" } else { "▎" }).set_fg(P.focus_border);
    }
    tr.x + (text.width() as u16).min(tr.width)
}

/// An icon button that cannot be used now: dim, no click.
pub(crate) fn icon_off(f: &mut Frame, r: Rect, glyph: &str, bg: Color) {
    let r = r.intersection(f.area());
    let x = r.x + r.width.saturating_sub(glyph.width() as u16) / 2;
    fill(f, r, bg);
    f.render_widget(Span::styled(glyph.to_string(), Style::new().fg(P.dim)), Rect::new(x, r.y, r.right().saturating_sub(x), 1));
}

/// `p` percent as a bar of `cells` cells.
pub(crate) fn progress_bar(p: u8, cells: usize) -> String {
    let n = usize::from(p.min(100)) * cells / 100;
    let (on, off) = if crate::theme::ascii() { ("#", "-") } else { ("█", "░") };
    on.repeat(n) + &off.repeat(cells - n)
}

/// A titled card with rounded corners in `border`; the inside (a cell of padding each
/// side), or empty when `r` is too small for one.
pub(crate) fn card(f: &mut Frame, r: Rect, title: &str, border: Color) -> Rect {
    if r.height < 3 || r.width < 8 {
        return Rect::default();
    }
    let blk = popup_block(P.editor_bg, P.editor_fg, (1, 0))
        .border_style(Style::new().fg(border).bg(P.editor_bg))
        .title(Line::from(Span::styled(format!(" {title} "), Style::new().fg(P.editor_fg).bold())));
    let inner = blk.inner(r);
    f.render_widget(blk, r);
    inner
}

/// A scrollbar's look only, on `track` (where the view scrolls itself some other way).
pub(crate) fn slider(f: &mut Frame, track: Rect, total: usize, view: usize, off: usize) {
    if total > view && !track.is_empty() {
        let s = decor::slider(track.height, total, view, off, total - view);
        draw_slider(f, track, s, (total, view, off, total - view), if hot(track) { P.slider_hover } else { P.slider });
    }
}

/// A scrollbar's slider: `s` (its cells), or where pictures show a rounded one placed to
/// the pixel (`total`, `view`, `off`, `max_off` say where).
fn draw_slider(f: &mut Frame, track: Rect, s: (u16, u16), (total, view, off, max_off): (usize, usize, usize, usize), c: Rgba) {
    let Some((_, ch)) = super::pixels::cell().filter(|_| track.width == 1 && track.height > 1) else {
        paint_slider(f, track, s, c);
        return;
    };
    let ch = f32::from(ch);
    let span = f32::from(track.height) * ch;
    let len = (view as f32 / total.max(1) as f32 * span).clamp(ch, span);
    let start = if max_off == 0 { 0.0 } else { off.min(max_off) as f32 / max_off as f32 * (span - len) };
    let (c0, c1) = ((start / ch).floor() as u16, ((start + len) / ch).ceil().min(f32::from(track.height)) as u16);
    let cells = Rect::new(track.x, track.y + c0, 1, c1.saturating_sub(c0)).intersection(f.area());
    let under: Vec<Color> = cells.positions().map(|p| f.buffer_mut()[p].bg).collect();
    paint_slider(f, track, (c0, c1.saturating_sub(c0)), c);
    super::pixels::ask(f, cells, super::pixels::Shape::Slider { start: start - f32::from(c0) * ch, len, color: c }, under);
}

/// A side bar section's header (Source Control's, an extension's): its chevron, its title
/// bold on the section background, `extra` dim after it.
pub(crate) fn section_head(_app: &App, f: &mut Frame, r: Rect, title: &str, open: bool, extra: &str, focused: bool) {
    head(f, r, title, open, extra, focused);
}

/// `section_head` where there is no `App` at hand (an extension's rows).
pub(crate) fn head(f: &mut Frame, r: Rect, title: &str, open: bool, extra: &str, focused: bool) {
    let Ptr { s, cards, .. } = PTR.get();
    let ic = icons();
    fill(f, r, P.section_bg);
    let chev = if open { ic.chev_down } else { ic.chev_right };
    let fg = if focused && !cards { P.focus_border } else { P.section_fg };
    let pad = s.sidebar_padding.saturating_sub(1) / 2;
    let gap = crate::blocks::icon_gap(s.icon_gap) / 2;
    let ir = inset(r, pad);
    // As in the rows: cut once with "…"; the dim file name only while the title is whole.
    let head = format!(" {chev}{}{title}", sp(gap));
    let extra = if extra.is_empty() { String::new() } else { format!(" {extra}") };
    let (head, extra) = fit_pair(&head, &extra, ir.width as usize);
    row(f, ir, P.section_bg, vec![Span::styled(head, Style::new().fg(fg).bold()), Span::styled(extra, Style::new().fg(P.dim))]);
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


/// A slim scrollbar slider over `track`, composited over what is drawn there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scrollbar(app: &mut App, f: &mut Frame, track: Rect, target: ScrollTarget, total: usize, view: usize, off: usize, max_off: usize) {
    let s = decor::slider(track.height, total, view, off, max_off);
    draw_slider(f, track, s, (total, view, off, max_off), slider_color(app, track, target));
    app.ui.scrollbars.push(ScrollHit { track, target, slider: s, max_off, page: view.max(1) });
}

pub(crate) fn slider_color(app: &App, track: Rect, target: ScrollTarget) -> Rgba {
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
    if r.width < 3 || r.height == 0 {
        return;
    }
    if crate::theme::nerd() {
        let st = Style::new().fg(fill).bg(under);
        let buf = f.buffer_mut();
        buf[(r.x, r.y)].set_symbol("\u{e0b6}").set_style(st);
        buf[(r.right() - 1, r.y)].set_symbol("\u{e0b4}").set_style(st);
    }
    // Where pictures show, rounded in pixels (with any font): a one-line run's.
    for (right, x) in [(false, r.x), (true, r.right() - 1)].into_iter().filter(|_| r.height == 1) {
        super::pixels::ask(f, Rect::new(x, r.y, 1, 1), super::pixels::Shape::Cap { right, fill, under }, vec![under]);
    }
}

/// A count badge: a pill on `under` (the row's background).
pub(crate) fn badge(n: usize, under: Color) -> Vec<Span<'static>> {
    pill(n.to_string(), Style::new().fg(P.badge_fg).bg(P.badge_bg), under)
}

pub(crate) fn input(f: &mut Frame, r: Rect, text: &str, placeholder: &str, focused: bool) {
    input_marked(f, r, text, placeholder, focused.then_some(P.focus_border));
}

/// An `input` field's whole text drawn selected (Ctrl+A, a double-click).
pub(crate) fn select_all(f: &mut Frame, r: Rect, text: &str) {
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

/// How a tool button with `label` reads on screen (between its round caps, or blanks).
#[cfg(test)]
pub(crate) fn btn_text(label: &str) -> String {
    if crate::theme::nerd() { format!("\u{e0b6}{label}\u{e0b4}") } else { format!(" {label} ") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::{PAct, Prompt};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::Terminal;

    #[test]
    fn a_button_lights_under_the_pointer_lighter_while_held_and_back_when_let_go() {
        let root = std::env::temp_dir().join(format!("termcode-kit-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(root.clone(), None);
        app.prompt = Some(Prompt { text: vec!["Close?".into()], buttons: vec![("Keep".into(), PAct::Cancel), ("Close".into(), PAct::Cancel)], sel: 1 });
        let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut draw = |app: &mut App| t.draw(|f| crate::ui::draw(app, f)).unwrap().buffer.clone();
        let buf = draw(&mut app);
        let (keep, close) = (app.ui.prompt_btns[1].0, app.ui.prompt_btns[0].0);
        assert!(app.ui.btns.contains(&keep) && app.ui.btns.contains(&close), "noted for hover and the pressed look");
        let mid = |r: Rect| (r.x + r.width / 2, r.y);
        assert_eq!((buf[mid(keep)].bg, buf[mid(close)].bg), (P.button2_bg, P.button_bg), "the chosen one is the main one");
        assert_eq!(buf[mid(keep)].symbol(), "e", "the label centred, padded each side");
        let ev = |kind, (x, y): (u16, u16)| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
        assert!(app.on_mouse(ev(MouseEventKind::Moved, mid(keep))), "onto a button: a redraw");
        assert_eq!(draw(&mut app)[mid(keep)].bg, P.button2_hover, "lit");
        // Held down on the other one (a prompt takes the click at once: put it back to see).
        app.on_mouse(ev(MouseEventKind::Moved, mid(close)));
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), mid(close)));
        assert_eq!(app.pressed, Some(close));
        app.prompt = Some(Prompt { text: vec!["Close?".into()], buttons: vec![("Keep".into(), PAct::Cancel), ("Close".into(), PAct::Cancel)], sel: 1 });
        let held = draw(&mut app)[mid(close)].bg;
        assert!(held != P.button_bg && held != P.button_hover, "lighter while held: {held:?}");
        app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), mid(close)));
        assert_eq!((app.pressed, draw(&mut app)[mid(close)].bg), (None, P.button_hover), "let go: lit only");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn widgets_size_and_draw_the_same_way_everywhere() {
        assert_eq!(progress_bar(40, 10), if crate::theme::ascii() { "####------" } else { "████░░░░░░" });
        assert_eq!(button_w("Save"), 4 + 2 * pad());
        let mut t = Terminal::new(TestBackend::new(40, 3)).unwrap();
        t.draw(|f| {
            let b = Rect::new(0, 0, 40, 3);
            let r = button_row(f, b, &[("One", Look::Primary), ("Two", Look::Secondary), (&"x".repeat(40), Look::Primary)], P.editor_bg);
            assert_eq!(r[1].x, r[0].right() + 1, "a blank between");
            assert!(r[2].is_empty(), "one that does not fit is not drawn");
            let (r, left) = button_row_end(f, b, &[("A", Look::Secondary), ("B", Look::Primary)], P.editor_bg);
            assert_eq!((r[1].right(), r[0].right() + 1, left + 1), (b.right(), r[1].x, r[0].x));
            assert!(check(f, b, (0, 2), true, "On").width == 6);
        })
        .unwrap();
    }

    /// The kit is the only place that draws a button or a scrollbar from the theme's
    /// colours: a view that draws its own drifts out of step (padding, hover, the pressed
    /// look, a scrollbar that cannot be clicked).
    #[test]
    fn only_the_kit_draws_buttons_and_scrollbars() {
        let mut bad = Vec::new();
        let mut stack = vec![std::path::PathBuf::from("src")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let name = p.to_string_lossy().into_owned();
                // The kit, the palette, tests, and the settings' pixel switch and option list.
                if !name.ends_with(".rs") || ["kit.rs", "theme.rs", "tests.rs", "ui_tests.rs", "settingsui.rs"].iter().any(|n| name.ends_with(n)) {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                let code = text.split("#[cfg(test)]").next().unwrap_or("");
                for (i, l) in code.lines().enumerate() {
                    if ["P.button_bg", "P.button2_bg", "P.button_hover", "P.button2_hover", "\"┃\"", "\"▐\""].iter().any(|k| l.contains(k)) {
                        bad.push(format!("{name}:{}: {}", i + 1, l.trim()));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "draw these with ui/kit.rs:\n{}", bad.join("\n"));
    }
}
