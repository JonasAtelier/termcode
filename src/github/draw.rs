//! The GitHub view's drawing: the account pane (avatar, contribution graph, lists) and
//! the repository pane (status, stats, lists). Split from `github` (data, fetching,
//! keys and clicks), no behaviour change.

use crate::ui::kit::{self, Look};
use super::*;
use crate::ui::{fit, inset, row};
use image::RgbaImage;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

pub(super) fn ic(i: usize) -> &'static str {
    icons().gh[i]
}

/// A button (the kit's) at `at`'s start; its width.
pub(super) fn button(app: &mut App, f: &mut Frame, at: Rect, label: &str, primary: bool, b: Btn) -> u16 {
    let label = label.trim();
    let r = Rect::new(at.x, at.y, kit::button_w(label).min(at.width), 1);
    kit::button(f, r, label, if primary { Look::Primary } else { Look::Secondary }, P.sidebar_bg, false);
    app.gh.hits.btns.push((r, b));
    r.width
}

/// An icon button (the kit's) filling `r`.
fn icon(app: &mut App, f: &mut Frame, r: Rect, glyph: &str, b: Btn) {
    kit::icon_button(f, r, glyph, P.sidebar_bg, false);
    app.gh.hits.btns.push((r, b));
}

/// A message (with buttons under it, one a row, the first the main one) in the view;
/// the row under it all.
pub(super) fn message(app: &mut App, f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, btns: &[(&str, Btn)]) -> u16 {
    let p = Paragraph::new(lines).wrap(Wrap { trim: true }).style(Style::new().fg(P.sidebar_fg));
    let h = p.line_count(area.width) as u16;
    f.render_widget(p, area);
    let mut y = area.y + h;
    for (i, (label, b)) in btns.iter().enumerate() {
        y += if i == 0 { 1 } else { 0 };
        if y < area.bottom() {
            button(app, f, Rect::new(area.x, y, area.width, 1), label, i == 0, *b);
        }
        y += 1;
    }
    y
}

/// `text` as a QR code in half blocks (two modules a cell, a quiet zone of two), black
/// on white whatever the theme, so a phone's camera reads it; its rows, or None when it
/// does not fit `at` (or half blocks can't be drawn).
pub(super) fn qr(f: &mut Frame, at: Rect, text: &str) -> Option<u16> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::L).ok()?;
    let (n, quiet) = (code.width(), 2usize);
    let size = n + 2 * quiet;
    let rows = size.div_ceil(2);
    if crate::theme::ascii() || usize::from(at.width) < size || usize::from(at.height) < rows {
        return None;
    }
    let dots = code.to_colors();
    let dark = |x: usize, y: usize| (quiet..quiet + n).contains(&x) && (quiet..quiet + n).contains(&y) && dots[(y - quiet) * n + x - quiet] == qrcode::Color::Dark;
    let ink = |d: bool| if d { Color::Rgb(0, 0, 0) } else { Color::Rgb(255, 255, 255) };
    let x0 = at.x + (at.width - size as u16) / 2;
    let buf = f.buffer_mut();
    for row in 0..rows {
        for col in 0..size {
            let cell = &mut buf[(x0 + col as u16, at.y + row as u16)];
            cell.set_symbol("▀").set_fg(ink(dark(col, row * 2))).set_bg(ink(dark(col, row * 2 + 1)));
        }
    }
    Some(rows as u16)
}

/// The view below its title: what needs doing first (gh, signing in), else the two panes.
pub fn draw(app: &mut App, f: &mut Frame, r: Rect) {
    app.gh.hits.area = r;
    let focused = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let pad = (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1);
    let area = inset(card, pad);
    if area.width < 8 || area.height < 3 {
        return;
    }
    app.ui.side_list = Rect::default();
    let dim = Style::new().fg(P.dim);
    match app.gh.state.clone() {
        State::Loading if app.gh.data.is_none() => {
            message(app, f, area, vec![Line::styled("Loading…", dim)], &[]);
        }
        State::NoGh => {
            let lines = vec![
                Line::styled("GitHub CLI (gh) is not installed.", Style::new().bold()),
                Line::raw(""),
                Line::raw("termcode reaches GitHub through it, so it never holds a password or token."),
                Line::raw(""),
                Line::styled("Install it from https://cli.github.com (Debian, Ubuntu: sudo apt install gh), then:", dim),
            ];
            message(app, f, area, lines, &[(" Try Again ", Btn::Retry)]);
        }
        State::SignedOut => {
            let mut lines = vec![
                Line::styled("Sign in to GitHub", Style::new().bold()),
                Line::raw(""),
                Line::raw("to see your repositories, pull requests, review requests, issues and notifications."),
                Line::raw(""),
            ];
            let busy = Style::new().fg(P.git_modified);
            match (app.gh.signing_in, app.gh.qr.clone()) {
                (true, Some(q)) => {
                    let Some(code) = q.code else {
                        lines.push(Line::styled("Answer gh in the terminal below; its code then shows here, with a QR code for your phone.", busy));
                        message(app, f, area, lines, &[]);
                        return;
                    };
                    lines.push(Line::styled("Scan this with your phone (camera or GitHub Mobile):", busy));
                    let y = message(app, f, area, lines, &[]) + 1;
                    let at = Rect::new(area.x, y.min(area.bottom()), area.width, area.bottom().saturating_sub(y));
                    let h = qr(f, at, crate::github::DEVICE_PAGE).unwrap_or(0);
                    let mut rest = vec![
                        Line::from(vec![Span::raw("then enter "), Span::styled(code, Style::new().bold().fg(P.sidebar_fg)), Span::raw(" and approve.")]),
                    ];
                    if h == 0 {
                        rest.push(Line::styled(format!("(On any device: {})", crate::github::DEVICE_PAGE), dim));
                    }
                    rest.push(Line::styled("gh keeps the sign-in; termcode never sees a password or token.", dim));
                    let y = at.y + h + u16::from(h > 0);
                    if y < area.bottom() {
                        message(app, f, Rect::new(area.x, y, area.width, area.bottom() - y), rest, &[]);
                    }
                }
                (true, None) => {
                    lines.push(Line::styled("Finish signing in in the terminal below: it shows a code to enter in your browser.", busy));
                    message(app, f, area, lines, &[]);
                }
                _ => {
                    lines.push(Line::styled("gh asks for your account in the terminal below; termcode never sees a password or token.", dim));
                    message(app, f, area, lines, &[(" Sign in with GitHub ", Btn::SignIn), (" Sign in with a QR code (phone) ", Btn::SignInQr)]);
                }
            }
        }
        State::Failed(e) if app.gh.data.is_none() => {
            let lines = vec![Line::styled("Couldn't reach GitHub.", Style::new().bold()), Line::raw(""), Line::styled(e, Style::new().fg(P.error_fg))];
            message(app, f, area, lines, &[(" Try Again ", Btn::Retry)]);
        }
        _ => ready(app, f, area, focused),
    }
}

/// The signed-in view: the account, the top pane, the line between, the bottom pane.
pub(super) fn ready(app: &mut App, f: &mut Frame, area: Rect, focused: bool) {
    let Some(d) = app.gh.data.clone() else { return };
    let dim = Style::new().fg(P.dim);
    // The account: its round picture (two rows) and name; Refresh at the right.
    let pic = Rect::new(area.x, area.y, 4.min(area.width), 2.min(area.height));
    let drew = match (&app.gh.avatar, crate::theme::ascii()) {
        (Some((login, img)), false) if *login == d.login => {
            let g = app.gh.graphics.get_or_insert_with(crate::settingsgraphics::Graphics::new);
            g.picture(f, pic, img, P.sidebar_bg, P.icon_fg)
        }
        _ => false,
    };
    if !drew {
        f.render_widget(Span::styled(ic(GITHUB), Style::new().fg(P.sidebar_fg)), Rect::new(pic.x + 1, pic.y, 2, 1));
        // In pixels: the mark as big as the picture would be.
        crate::ui::pixels::icon(f, pic, ic(GITHUB), P.sidebar_fg, 0.62);
    }
    let tx = pic.right() + 1;
    let refresh = format!(" {} ", if app.gh.loading || app.gh.repo_loading { ic(RUNNING) } else { ic(REFRESH) });
    let rw = refresh.width() as u16;
    let text_w = area.right().saturating_sub(tx + rw + 1);
    f.render_widget(Span::styled(fit(&d.login, text_w as usize), Style::new().fg(P.sidebar_fg).bold()), Rect::new(tx, area.y, text_w, 1));
    let second = match &app.gh.state {
        State::Failed(e) => (format!("Couldn't refresh: {e}"), Style::new().fg(P.error_fg)),
        _ => (format!("{} repositories", d.repos.len()), dim),
    };
    if area.height > 1 {
        f.render_widget(Span::styled(fit(&second.0, (area.right() - tx) as usize), second.1), Rect::new(tx, area.y + 1, area.right() - tx, 1));
    }
    icon(app, f, Rect::new(area.right() - rw, area.y, rw, 1), &refresh, Btn::Refresh);
    // The panes share what is left: the top one's height dragged (half by default), each
    // keeping a few rows.
    let y0 = area.y + 3;
    let body = area.bottom().saturating_sub(y0);
    if body < 6 {
        return;
    }
    // Half each by default: both have a graph.
    let top_h = app.gh.split.unwrap_or(body / 2).clamp(3, body - 4);
    let top = Rect::new(area.x, y0, area.width, top_h);
    let sash = Rect::new(area.x, y0 + top_h, area.width, 1);
    let bottom = Rect::new(area.x, sash.y + 1, area.width, area.bottom() - sash.y - 1);
    app.gh.hits.sash = sash;
    app.gh.hits.panes = [top, bottom];
    let top = activity(app, f, top, &d);
    list(app, f, top, TOP, focused);
    // The line between: the chosen repository's name in it, lit while dragged or hovered.
    let hot = matches!(app.drag, Drag::View(crate::ext::GITHUB)) || app.hover.is_some_and(|p| sash.contains(p));
    let line_fg = if hot { P.sash_hover } else { P.group_border };
    let name = app.gh.chosen.as_deref().map(|n| format!(" {} {} ", ic(REPO), n)).unwrap_or_default();
    let h = if crate::theme::ascii() { "-" } else { "─" };
    let label_w = (name.width() as u16).min(sash.width.saturating_sub(2));
    row(f, sash, P.sidebar_bg, vec![
        Span::styled(h.repeat(2), Style::new().fg(line_fg)),
        Span::styled(fit(&name, label_w as usize), Style::new().fg(P.sidebar_title).bold()),
        Span::styled(h.repeat(sash.width.saturating_sub(2 + label_w) as usize), Style::new().fg(line_fg)),
    ]);
    repo_pane(app, f, bottom, focused);
}

/// `a` blended `t` of the way to `b` (theme colours, opaque).
pub(super) fn mix(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
            Color::Rgb(m(r1, r2), m(g1, g2), m(b1, b2))
        }
        _ => b,
    }
}

/// A contribution level's colour: a faint block for none, then four greens.
pub(super) fn level_color(level: u8) -> Color {
    match level {
        0 => mix(P.sidebar_bg, P.sidebar_fg, 0.12),
        l => mix(P.sidebar_bg, P.gh_green, [0.3, 0.55, 0.8, 1.0][(l as usize - 1).min(3)]),
    }
}

/// Cells a week takes in the year graph, and a day in the month's: the blocks' size, the
/// same in both.
pub(super) const YEAR_PITCH: u16 = 2;
pub(super) const MONTH_PITCH: u16 = YEAR_PITCH;

/// A day's block in its level's colour (`#` in ASCII).
pub(super) fn block(level: u8) -> Span<'static> {
    let glyph = if crate::theme::ascii() { "#" } else { "■" };
    Span::styled(glyph, Style::new().fg(level_color(level)))
}

/// Today, as the calendar writes a date ("2026-11-13", UTC as GitHub's).
pub(super) fn today() -> String {
    let (y, m, d) = ymd(crate::scm::now());
    format!("{y}-{m:02}-{d:02}")
}

/// "Nov 8" for "2026-11-08".
pub(super) fn short_date(date: &str) -> String {
    let m: usize = date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1).clamp(1, 12);
    let d: u32 = date.get(8..10).and_then(|d| d.parse().ok()).unwrap_or(0);
    format!("{} {d}", MONTHS[m - 1])
}

pub(super) const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// (year, month 1-12, day) of unix time `t` (UTC).
pub fn ymd(t: i64) -> (i32, u32, u32) {
    // Howard Hinnant's civil_from_days.
    let z = t.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

/// A calendar year's contributions, fetched once (the month view and an older year).
pub(super) fn want_year(app: &mut App, y: i32) {
    if app.gh.years.contains_key(&y) || app.gh.year_asked.is_some() {
        return;
    }
    app.gh.year_asked = Some(y);
    let (bin, tx, root) = (app.gh.bin.clone(), app.gh.tx.clone(), cwd(app));
    let query = ACTIVITY.replace("contributionsCollection", &format!("contributionsCollection(from: \"{y}-01-01T00:00:00Z\", to: \"{y}-12-31T23:59:59Z\")"));
    std::thread::spawn(move || {
        let a = gh_json(&bin, &root, &["api", "graphql", "-f", &query]).ok().and_then(|v| activity_of(&v));
        let _ = tx.send(Msg::Year(y, a));
    });
}

/// ‹ or ›: an older (`d` -1) or newer year (the last 12 months newest), or month, no
/// further back than the account and on to this month.
pub(super) fn act_step(app: &mut App, d: i32) {
    let (now_y, now_m, _) = ymd(crate::scm::now());
    let since = app.gh.data.as_ref().map_or(now_y, |x| x.since.min(now_y));
    let gh = &mut app.gh;
    gh.act_scroll[ACCOUNT] = 0;
    if gh.act_month {
        let (y, m) = gh.act_ym;
        let i = (y * 12 + m as i32 - 1 + d).clamp(since * 12, now_y * 12 + now_m as i32 - 1);
        gh.act_ym = (i.div_euclid(12), (i.rem_euclid(12) + 1) as u32);
    } else {
        // Newest first: the last 12 months, then each year back.
        let at = gh.act_year.map_or(0, |y| now_y - y + 1);
        let at = (at - d).clamp(0, now_y - since + 1);
        gh.act_year = (at > 0).then(|| now_y - at + 1);
    }
}

/// The calendar the graph shows now: the last 12 months, or a fetched year (asked for
/// when missing: None meanwhile).
pub(super) fn calendar(app: &mut App) -> Option<Activity> {
    let year = if app.gh.act_month { Some(app.gh.act_ym.0) } else { app.gh.act_year };
    match year {
        None => app.gh.data.as_ref()?.activity.clone(),
        Some(y) => {
            want_year(app, y);
            app.gh.years.get(&y).cloned()
        }
    }
}

/// The account's activity at the top of the upper pane, as GitHub draws it: a header
/// (which folds it) with the contributions shown, or the day under the pointer; Year and
/// Month with ‹ › to go back; a block a day in green; the legend and the account's
/// numbers. Returns what is left for the list (which keeps a few rows, or the graph does
/// not show).
pub(super) fn activity(app: &mut App, f: &mut Frame, r: Rect, d: &Data) -> Rect {
    if d.activity.is_none() {
        return r;
    }
    let folded = app.gh.activity_folded;
    let month = app.gh.act_month;
    // The grid's rows: text blocks a row a weekday (a week of the month); pixels as tall
    // as their squares (a cell wide a day in the year, two in the month).
    let px = pixels(app);
    let grid = match (month, px) {
        (false, Some((cw, ch))) => (7 * u32::from(YEAR_PITCH * cw)).div_ceil(u32::from(ch)) as u16,
        (false, None) => 7,
        (true, Some((cw, ch))) => (6 * u32::from(MONTH_PITCH * cw)).div_ceil(u32::from(ch)) as u16,
        (true, None) => 6,
    };
    // Header and Year | Month, the labels and the grid, the legend, the numbers, a gap. The
    // list below keeps three rows; short of the whole graph, its header alone shows.
    if r.height < 2 + 3 || r.width < 18 {
        return r;
    }
    let folded = folded || r.height < 2 + 1 + grid + 3 + 3;
    let used = if folded { 2 } else { 2 + 1 + grid + 3 };
    let cal = calendar(app);
    let (when, total) = match (month, app.gh.act_year, &cal) {
        (true, ..) => {
            let (y, m) = app.gh.act_ym;
            let key = format!("{y}-{m:02}");
            let n: u64 = cal.iter().flat_map(|c| c.weeks.iter().flatten()).filter(|x| x.date.starts_with(&key)).map(|x| x.count).sum();
            (format!("{} {y}", MONTHS[m as usize - 1]), n)
        }
        (false, None, c) => ("the past year".to_string(), c.as_ref().map_or(0, |c| c.total)),
        (false, Some(y), c) => (y.to_string(), c.as_ref().map_or(0, |c| c.total)),
    };
    // The header (drawn last: it names the block under the pointer).
    let chev = if folded { icons().chev_right } else { icons().chev_down };
    let hr = Rect::new(r.x, r.y, r.width, 1);
    let header = |app: &App, f: &mut Frame| {
        let hovered = app.hover.and_then(|p| app.gh.hits.act_cells.iter().find(|(c, .., w)| *w == ACCOUNT && c.contains(p)));
        let what = match hovered {
            Some((_, date, n, _)) => format!("{}: {n} contribution{}", short_date(date), if *n == 1 { "" } else { "s" }),
            None => format!("{total} in {when}"),
        };
        row(f, hr, P.section_bg, vec![Span::styled(fit(&format!("{chev} ACTIVITY · {what}"), r.width as usize), Style::new().fg(P.section_fg).bold())]);
    };
    app.gh.hits.btns.push((hr, Btn::Activity));
    if folded {
        header(app, f);
        return Rect::new(r.x, r.y + 2, r.width, r.height - 2);
    }
    // Year | Month at the left, ‹ what › at the right.
    let cy = r.y + 1;
    let mut x = r.x;
    for (label, m) in [(" Year ", false), (" Month ", true)] {
        x += button(app, f, Rect::new(x, cy, r.right() - x, 1), label, month == m, Btn::ActMode(m)) + 1;
    }
    let step = format!(" {} ", when.strip_prefix("the ").unwrap_or(&when));
    let sx = r.right().saturating_sub(step.width() as u16 + 4);
    if sx > x {
        icon(app, f, Rect::new(sx, cy, 2, 1), "‹ ", Btn::ActStep(-1));
        f.render_widget(Span::styled(step.clone(), Style::new().fg(P.sidebar_fg)), Rect::new(sx + 2, cy, step.width() as u16, 1));
        icon(app, f, Rect::new(r.right() - 2, cy, 2, 1), " ›", Btn::ActStep(1));
    }
    let g = Rect::new(r.x, r.y + 2, r.width, 1 + grid);
    let (start, end) = match &cal {
        None => {
            f.render_widget(Span::styled("Loading…", Style::new().fg(P.dim)), Rect::new(g.x + 2, g.y + 2, g.width.saturating_sub(2), 1));
            (r.x, r.right())
        }
        Some(c) if month => month_view(app, f, g, c, px),
        Some(c) => year_view(app, f, g, c, px, ACCOUNT),
    };
    header(app, f);
    // Less ■■■■■ More centred under the graph, then the account's numbers.
    let ly = r.y + used - 3;
    let mut legend = vec![Span::styled("Less ", Style::new().fg(P.dim))];
    legend.extend((0..5).flat_map(|l| [block(l), Span::raw(" ")]));
    legend.push(Span::styled("More", Style::new().fg(P.dim)));
    let lw = legend.iter().map(|s| s.width() as u16).sum::<u16>();
    let lx = ((start + end) / 2).saturating_sub(lw / 2).clamp(r.x, r.right().saturating_sub(lw));
    f.render_widget(Line::from(legend), Rect::new(lx, ly, lw.min(r.width), 1));
    let earned: u64 = d.repos.iter().map(|r| r.stars).sum();
    let a = d.activity.as_ref().expect("checked above");
    let s = |n: u64| if n == 1 { "" } else { "s" };
    let numbers = format!("{} repos · {} {earned} · {} follower{} · {} following", d.repos.len(), ic(STAR), a.followers, s(a.followers), a.following);
    f.render_widget(Span::styled(fit(&numbers, r.width as usize), Style::new().fg(P.dim)), Rect::new(r.x, ly + 1, r.width, 1));
    Rect::new(r.x, r.y + used, r.width, r.height - used)
}

/// Pixels for the graphs (Kitty graphics, not ASCII): a cell's size; None for text.
pub(super) fn pixels(app: &mut App) -> Option<(u16, u16)> {
    let g = app.gh.graphics.get_or_insert_with(crate::settingsgraphics::Graphics::new);
    (g.pixel_controls() && !crate::theme::ascii()).then(|| g.cell())
}

/// The graph as pixels: each day a square (rounded off at its corners) `pitch` px apart at
/// its (column, row), a gap round it, on `bg`.
pub fn grid_image(w: u32, h: u32, pitch: u32, days: &[(u32, u32, Color)], bg: Color) -> RgbaImage {
    use crate::settingsgraphics::pixel;
    let gap = (pitch / 5).max(1);
    let side = pitch.saturating_sub(gap).max(1);
    let mut img = RgbaImage::from_pixel(w, h, pixel(bg));
    for &(col, row, color) in days {
        let (x0, y0) = (col * pitch + gap / 2, row * pitch + gap / 2);
        for y in y0..(y0 + side).min(h) {
            for x in x0..(x0 + side).min(w) {
                let corner = side >= 6 && (x == x0 || x == x0 + side - 1) && (y == y0 || y == y0 + side - 1);
                if !corner {
                    img.put_pixel(x, y, pixel(color));
                }
            }
        }
    }
    img
}

/// A digest of a graph's squares and colours: its image is made again only when it changes.
pub(super) fn digest(days: &[(u32, u32, Color)], size: (u16, u16)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    size.hash(&mut h);
    format!("{days:?}{:?}", (P.gh_green, P.sidebar_bg, P.sidebar_fg)).hash(&mut h);
    h.finish()
}

/// GitHub's year graph in `g`: month names over a column a week, a row a weekday (Sunday
/// first; Mon, Wed and Fri named), newest week at the right; the wheel scrolls it back. A
/// week takes YEAR_PITCH cells: with pixels (`px`) a square that wide, else a block.
/// Returns the graph's left and right edges.
pub(super) fn year_view(app: &mut App, f: &mut Frame, g: Rect, c: &Activity, px: Option<(u16, u16)>, which: usize) -> (u16, u16) {
    let cols = (g.width.saturating_sub(2) / YEAR_PITCH) as usize;
    let n = c.weeks.len();
    let shown = cols.min(n);
    app.gh.act_scroll[which] = app.gh.act_scroll[which].min(n - shown);
    let start = n - shown - app.gh.act_scroll[which];
    // Centred: the weekday names, then the weeks.
    let g = Rect { x: g.x + (g.width - (2 + shown as u16 * YEAR_PITCH)) / 2, ..g };
    let gx = g.x + 2;
    // A weekday's text row: its own, or (pixels) where its square's middle falls.
    let row_of = |wd: u32| match px {
        Some((cw, ch)) => {
            let pitch = u32::from(YEAR_PITCH * cw);
            g.y + 1 + ((wd * pitch + pitch / 2) / u32::from(ch)) as u16
        }
        None => g.y + 1 + wd as u16,
    };
    let mut taken = Vec::new();
    for (k, label) in [(1, "M"), (3, "W"), (5, "F")] {
        let y = row_of(k);
        if !taken.contains(&y) && y < g.bottom() {
            taken.push(y);
            f.render_widget(Span::styled(label, Style::new().fg(P.dim)), Rect::new(g.x, y, 1, 1));
        }
    }
    let mut squares = Vec::new();
    let mut free = gx; // month names never overlap
    for (i, w) in c.weeks[start..start + shown].iter().enumerate() {
        let x = gx + i as u16 * YEAR_PITCH;
        // A month's name over the week its first day falls in.
        if let Some(first) = w.iter().find(|d| d.date.ends_with("-01")) {
            let m: usize = first.date.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(1);
            if x >= free && x + 3 <= g.right() {
                f.render_widget(Span::styled(MONTHS[m - 1], Style::new().fg(P.dim)), Rect::new(x, g.y, 3, 1));
                free = x + 4;
            }
        }
        for day in w.iter().filter(|d| d.date <= today()) {
            let cell = Rect::new(x, row_of(u32::from(day.weekday)), YEAR_PITCH, 1);
            if px.is_some() {
                squares.push((i as u32, u32::from(day.weekday), level_color(day.level)));
            } else {
                f.render_widget(block(day.level), Rect::new(x, cell.y, 1, 1));
            }
            app.gh.hits.act_cells.push((cell, day.date.clone(), day.count, which));
        }
    }
    let area = Rect::new(gx, g.y + 1, shown as u16 * YEAR_PITCH, g.height - 1);
    if let (Some((cw, _)), Some(gr)) = (px, app.gh.graphics.as_mut()) {
        let key = digest(&squares, (area.width, area.height));
        // Slot 7 the account's, 9 the repository's.
        gr.raw(f, area, 7 + 2 * which, key, |w, h| grid_image(w, h, u32::from(YEAR_PITCH * cw), &squares, P.sidebar_bg));
    }
    app.gh.hits.act_graph[which] = area;
    (g.x, area.right())
}

/// One month as a calendar in `g`: weekday initials over a block a day, MONTH_PITCH cells
/// apart (with pixels, squares that wide).
/// Returns the graph's left and right edges.
pub(super) fn month_view(app: &mut App, f: &mut Frame, g: Rect, c: &Activity, px: Option<(u16, u16)>) -> (u16, u16) {
    let (y, m) = app.gh.act_ym;
    let key = format!("{y}-{m:02}-");
    let days: Vec<&Day> = c.weeks.iter().flatten().filter(|d| d.date.starts_with(&key)).collect();
    let today = today();
    // Centred: seven days across.
    let width = (7 * MONTH_PITCH).min(g.width);
    let gx = g.x + (g.width - width) / 2;
    let step = MONTH_PITCH;
    for (k, label) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
        f.render_widget(Span::styled(*label, Style::new().fg(P.dim)), Rect::new(gx + step * k as u16, g.y, 1, 1));
    }
    let Some(first) = days.first() else {
        let note = "No contributions this month.";
        let x = g.x + g.width.saturating_sub(note.width() as u16) / 2;
        f.render_widget(Span::styled(note, Style::new().fg(P.dim)), Rect::new(x, g.y + 2, g.right() - x, 1));
        return (g.x, g.right());
    };
    let lead = first.weekday as usize;
    let mut squares = Vec::new();
    for (i, day) in days.iter().enumerate() {
        let slot = lead + i;
        let (col, week) = ((slot % 7) as u32, (slot / 7) as u32);
        let cell = match px {
            Some((cw, ch)) => {
                let pitch = u32::from(MONTH_PITCH * cw);
                Rect::new(gx + MONTH_PITCH * col as u16, g.y + 1 + ((week * pitch + pitch / 2) / u32::from(ch)) as u16, MONTH_PITCH, 1)
            }
            None => Rect::new(gx + MONTH_PITCH * col as u16, g.y + 1 + week as u16, 1, 1),
        };
        // Days to come have no block, as on GitHub.
        if cell.bottom() > g.bottom() || day.date > today {
            break;
        }
        if px.is_some() {
            squares.push((col, week, level_color(day.level)));
        } else {
            f.render_widget(block(day.level), cell);
        }
        app.gh.hits.act_cells.push((cell, day.date.clone(), day.count, ACCOUNT));
    }
    if let (Some((cw, _)), Some(gr)) = (px, app.gh.graphics.as_mut()) {
        let area = Rect::new(gx, g.y + 1, width, g.height - 1);
        let key = digest(&squares, (area.width, area.height));
        gr.raw(f, area, 8, key, |w, h| grid_image(w, h, u32::from(MONTH_PITCH * cw), &squares, P.sidebar_bg));
    }
    app.gh.hits.act_graph[ACCOUNT] = Rect::default();
    (gx, gx + width)
}

/// The bottom pane: the chosen repository's description and status, then its lists.
pub(super) fn repo_pane(app: &mut App, f: &mut Frame, r: Rect, focused: bool) {
    let dim = Style::new().fg(P.dim);
    let Some(name) = app.gh.chosen.clone() else {
        let lines = vec![Line::styled("Choose a repository above to see its Actions, pull requests and issues here.", dim)];
        message(app, f, r, lines, &[]);
        return;
    };
    let info = app.gh.chosen_info().cloned().unwrap_or_else(|| RepoInfo { name: name.clone(), ..RepoInfo::default() });
    // Its description, then what it is: private, default branch, language, stars, forks,
    // last push; its page at the right.
    let now = crate::scm::now();
    let ow = 3;
    icon(app, f, Rect::new(r.right().saturating_sub(ow), r.y, ow, 1), ic(OPEN), Btn::OpenRepo);
    let desc = if info.description.is_empty() { "No description.".to_string() } else { info.description.clone() };
    f.render_widget(Span::styled(fit(&desc, r.width.saturating_sub(ow + 1) as usize), Style::new().fg(P.sidebar_fg)), Rect::new(r.x, r.y, r.width.saturating_sub(ow + 1), 1));
    let mut facts: Vec<String> = Vec::new();
    if info.private {
        facts.push(format!("{} private", icons().lock));
    }
    if info.archived {
        facts.push("archived".into());
    }
    if info.fork {
        facts.push("fork".into());
    }
    facts.extend([info.branch.clone(), info.language.clone()].into_iter().filter(|s| !s.is_empty()));
    facts.push(format!("{} {}", ic(STAR), info.stars));
    facts.push(format!("{} {}", ic(FORK), info.forks));
    if info.pushed > 0 {
        facts.push(format!("pushed {}", crate::scm::ago_short(info.pushed, now)));
    }
    if r.height > 1 {
        f.render_widget(Span::styled(fit(&facts.join(" · "), r.width as usize), dim), Rect::new(r.x, r.y + 1, r.width, 1));
    }
    let lists = Rect::new(r.x, r.y + 3, r.width, r.height.saturating_sub(3));
    match &app.gh.repo {
        Some(Err(e)) => {
            let lines = vec![Line::styled(format!("Couldn't read it: {e}"), Style::new().fg(P.error_fg))];
            message(app, f, lists, lines, &[(" Try Again ", Btn::Retry)]);
        }
        None => {
            message(app, f, lists, vec![Line::styled("Loading…", dim)], &[]);
        }
        Some(Ok(_)) => {
            let lists = stats(app, f, lists);
            list(app, f, lists, BOTTOM, focused)
        }
    }
}

/// Rows the repository's lists keep under its stats (a header and three).
pub const LIST_ROWS: u16 = 4;

/// The repository's numbers above its lists: a header (which folds them) with its commits in
/// the past year, or the day under the pointer; its commit graph, as the account's; its
/// languages as a bar in their colours with their shares; its top contributors. Returns
/// what is left for the lists (which keep a few rows, or the stats do not show).
pub(super) fn stats(app: &mut App, f: &mut Frame, r: Rect) -> Rect {
    let Some(Ok(rd)) = app.gh.repo.clone() else { return r };
    let folded = app.gh.stats_folded;
    let px = pixels(app);
    let grid = match px {
        Some((cw, ch)) => (7 * u32::from(YEAR_PITCH * cw)).div_ceil(u32::from(ch)) as u16,
        None => 7,
    };
    // The lists below keep LIST_ROWS rows. Of the rest: the header always, then by turns
    // what fits: the graph, the language bar, the contributors, the key.
    let mut room = r.height.saturating_sub(LIST_ROWS);
    if room == 0 || r.width < 18 {
        return r;
    }
    room -= 1;
    let mut take = |want: bool, rows: u16| {
        let fits = want && !folded && room >= rows;
        if fits {
            room -= rows;
        }
        fits
    };
    let show_graph = take(true, 1 + grid);
    let show_bar = take(!rd.languages.is_empty(), 1);
    let show_people = take(!rd.contributors.is_empty(), 1);
    let show_key = take(show_bar, 1);
    let used = if folded {
        2
    } else {
        1 + if show_graph { 1 + grid } else { 0 } + u16::from(show_bar) + u16::from(show_people) + u16::from(show_key)
    };
    let dim = Style::new().fg(P.dim);
    let g = Rect::new(r.x, r.y + 1, r.width, if show_graph { 1 + grid } else { 0 });
    if show_graph {
        match &rd.commits {
            Some(c) => {
                year_view(app, f, g, c, px, REPO_GRAPH);
            }
            None => {
                let note = "GitHub is counting its commits…";
                let x = g.x + g.width.saturating_sub(note.width() as u16) / 2;
                f.render_widget(Span::styled(note, dim), Rect::new(x, g.y + 1 + grid / 2, g.right() - x, 1));
            }
        }
    }
    // The header last: it names the day under the pointer.
    let hovered = app.hover.and_then(|p| app.gh.hits.act_cells.iter().find(|(c, .., w)| *w == REPO_GRAPH && c.contains(p)));
    let what = match (hovered, &rd.commits) {
        (Some((_, date, n, _)), _) => format!("{}: {n} commit{}", short_date(date), if *n == 1 { "" } else { "s" }),
        (None, Some(c)) => format!("{} commits in the past year", c.total),
        (None, None) => "counting…".to_string(),
    };
    let chev = if folded { icons().chev_right } else { icons().chev_down };
    let hr = Rect::new(r.x, r.y, r.width, 1);
    row(f, hr, P.section_bg, vec![Span::styled(fit(&format!("{chev} STATS · {what}"), r.width as usize), Style::new().fg(P.section_fg).bold())]);
    app.gh.hits.btns.push((hr, Btn::Stats));
    if folded {
        return Rect::new(r.x, r.y + 2, r.width, r.height - 2);
    }
    let mut y = g.bottom();
    if show_bar {
        // The bar: a cell per share (at least one), the last language to the end.
        let total: u64 = rd.languages.iter().map(|l| l.1).sum::<u64>().max(1);
        let ascii = crate::theme::ascii();
        let mut x = r.x;
        for (i, (lang, bytes)) in rd.languages.iter().enumerate() {
            if x >= r.right() {
                break;
            }
            let cells = if i + 1 == rd.languages.len() { r.right() - x } else { ((bytes * u64::from(r.width) + total / 2) / total).max(1).min(u64::from(r.right() - x)) as u16 };
            let glyph = if ascii { "#" } else { "█" };
            f.render_widget(Span::styled(glyph.repeat(cells as usize), Style::new().fg(lang_color(lang))), Rect::new(x, y, cells, 1));
            x += cells;
        }
        let dot = if ascii { "*" } else { "●" };
        let mut key: Vec<Span> = Vec::new();
        for (lang, bytes) in &rd.languages {
            let share = *bytes as f64 * 100.0 / total as f64;
            key.push(Span::styled(dot, Style::new().fg(lang_color(lang))));
            key.push(Span::styled(format!(" {lang} {share:.1}%  "), dim));
        }
        if show_key {
            f.render_widget(Line::from(key), Rect::new(r.x, y + 1, r.width, 1));
            y += 1;
        }
        y += 1;
    }
    if show_people {
        let who = rd.contributors.iter().map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(" · ");
        f.render_widget(Span::styled(fit(&format!("Contributors: {who}"), r.width as usize), dim), Rect::new(r.x, y, r.width, 1));
    }
    Rect::new(r.x, r.y + used, r.width, r.height - used)
}

/// A pane's list, scrolled to keep its selection in view.
pub(super) fn list(app: &mut App, f: &mut Frame, r: Rect, pane: usize, focused: bool) {
    app.gh.hits.lists[pane] = r;
    let all = rows(&app.gh, pane);
    let h = r.height as usize;
    if all.is_empty() || h == 0 {
        return;
    }
    let keys = focused && app.gh.pane == pane;
    let gh = &mut app.gh;
    gh.sel[pane] = gh.sel[pane].min(all.len() - 1);
    if std::mem::take(&mut gh.follow[pane]) {
        if gh.sel[pane] < gh.off[pane] {
            gh.off[pane] = gh.sel[pane];
        } else if gh.sel[pane] >= gh.off[pane] + h {
            gh.off[pane] = gh.sel[pane] + 1 - h;
        }
    }
    gh.off[pane] = gh.off[pane].min(all.len().saturating_sub(h));
    let (off, sel) = (gh.off[pane], gh.sel[pane]);
    let now = crate::scm::now();
    // A scrolling list keeps its last column for the scrollbar.
    let w = r.width.saturating_sub(u16::from(all.len() > h));
    for (i, &rw) in all.iter().enumerate().skip(off).take(h) {
        let rr = Rect::new(r.x, r.y + (i - off) as u16, w, 1);
        app.gh.hits.rows.push((rr, pane, i));
        let hot = app.hover.is_some_and(|p| rr.contains(p));
        if let Row::Head(s) = rw {
            head(app, f, rr, s, i == sel && keys);
            continue;
        }
        let bg = if i == sel && keys {
            P.list_active_bg
        } else if i == sel {
            P.list_inactive_bg
        } else if hot {
            P.list_hover_bg
        } else {
            P.sidebar_bg
        };
        item(app, f, rr, bg, (pane, i, rw), now, hot || i == sel);
    }
    if all.len() > h {
        let track = Rect::new(r.right().saturating_sub(1), r.y, 1, r.height);
        crate::ui::scrollbar(app, f, track, crate::panel::ScrollTarget::Gh(pane), all.len(), h, off, all.len() - h);
    }
}

/// A section's header: its chevron, name and count, in the section colours.
pub(super) fn head(app: &App, f: &mut Frame, r: Rect, s: Sec, focused: bool) {
    let gh = &app.gh;
    let d = gh.data.as_ref();
    let (name, count) = match s {
        Sec::Repos => ("REPOSITORIES", d.map_or(0, |d| d.repos.len())),
        Sec::Reviews => ("REVIEW REQUESTS", gh.prs(s).len()),
        Sec::Mine => ("MY PULL REQUESTS", gh.prs(s).len()),
        Sec::Assigned => ("ASSIGNED TO ME", gh.issues(s).len()),
        Sec::Notes => ("NOTIFICATIONS", d.map_or(0, |d| d.notes.len())),
        Sec::Runs => ("ACTIONS", gh.runs().len()),
        Sec::RepoPrs => ("PULL REQUESTS", gh.prs(s).len()),
        Sec::RepoIssues => ("ISSUES", gh.issues(s).len()),
    };
    crate::ui::section_head(app, f, r, name, !gh.folded[s as usize], &count.to_string(), focused);
}

/// A repository's name as the account sees it: its own without the owner.
pub(super) fn repo_label(gh: &GitHub, name: &str) -> String {
    let login = gh.data.as_ref().map_or("", |d| d.login.as_str());
    match name.split_once('/') {
        Some((owner, n)) if owner == login => n.to_string(),
        _ => name.to_string(),
    }
}

/// A row: its icon and title, then who (or which repository) and when; its buttons
/// instead while selected or under the pointer.
pub(super) fn item(app: &mut App, f: &mut Frame, r: Rect, bg: Color, (pane, i, rw): (usize, usize, Row), now: i64, buttons: bool) {
    let gh = &app.gh;
    let green = P.gh_green;
    let ago = |t: i64| crate::scm::ago_short(t, now);
    let (icon, color, label, meta) = match rw {
        Row::Repo(k) => {
            let Some(rp) = gh.data.as_ref().map(|d| &d.repos[k]) else { return };
            // The chosen one's icon is lit.
            let color = if gh.chosen.as_deref() == Some(rp.name.as_str()) { P.focus_border } else { P.icon_fg };
            let lock = if rp.private { format!(" {}", icons().lock) } else { String::new() };
            (ic(REPO), color, format!("{}{lock}", repo_label(gh, &rp.name)), ago(rp.pushed))
        }
        Row::Pr(s, k) => {
            let p = &gh.prs(s)[k];
            let (icon, color) = if p.draft { (ic(PR_DRAFT), P.dim) } else { (ic(PR), green) };
            let checks = match p.checks {
                Checks::Pass => format!("{} ", ic(PASS)),
                Checks::Fail => format!("{} ", ic(FAIL)),
                Checks::Pending => format!("{} ", ic(RUNNING)),
                Checks::None => String::new(),
            };
            // The account's lists span repositories: they say which.
            let who = if s == Sec::RepoPrs { p.author.clone() } else { repo_label(gh, &p.repo) };
            (icon, color, format!("#{} {}", p.number, p.title), format!("{checks}{who} · {}", ago(p.updated)))
        }
        Row::Issue(s, k) => {
            let is = &gh.issues(s)[k];
            let who = if s == Sec::RepoIssues { is.author.clone() } else { repo_label(gh, &is.repo) };
            (ic(ISSUE), green, format!("#{} {}", is.number, is.title), format!("{who} · {}", ago(is.updated)))
        }
        Row::Run(k) => {
            let rn = &gh.runs()[k];
            let (icon, color) = run_icon(rn.state);
            (icon, color, rn.title.clone(), format!("{} · {}", rn.workflow, ago(rn.updated)))
        }
        Row::Note(k) => {
            let Some(n) = gh.data.as_ref().map(|d| &d.notes[k]) else { return };
            let icon = match n.kind.as_str() {
                "PullRequest" => ic(PR),
                "Issue" => ic(ISSUE),
                _ => ic(GITHUB),
            };
            (icon, P.sidebar_fg, n.title.clone(), format!("{} · {}", repo_label(gh, &n.repo), ago(n.updated)))
        }
        Row::Empty(s) => {
            let what = match s {
                Sec::Repos => "No repositories.",
                Sec::Reviews => "No pull requests wait for your review.",
                Sec::Mine => "You have no open pull requests.",
                Sec::Assigned => "No open issues are assigned to you.",
                Sec::Notes => "No unread notifications.",
                Sec::Runs => "No workflow runs.",
                Sec::RepoPrs => "No open pull requests.",
                Sec::RepoIssues => "No open issues.",
            };
            row(f, r, bg, vec![Span::styled(fit(&format!("  {what}"), r.width as usize), Style::new().fg(P.dim).italic())]);
            return;
        }
        Row::Head(_) => return,
    };
    // The tail (who and when, or the buttons) ends a cell before the edge; two cells part
    // it from the title, which is cut first.
    let keys = if buttons { keys_of(gh, rw) } else { Vec::new() };
    let lead = format!("  {icon} ");
    let tail_w = if keys.is_empty() { meta.width() as u16 } else { keys.len() as u16 * 2 }.min(r.width * 2 / 5);
    let title_w = r.width.saturating_sub(lead.width() as u16 + tail_w + 3) as usize;
    row(f, r, bg, vec![Span::styled(lead.clone(), Style::new().fg(color)), Span::styled(fit(&label, title_w), Style::new().fg(P.sidebar_fg))]);
    let tx = r.right().saturating_sub(tail_w + 1);
    if keys.is_empty() {
        f.render_widget(Span::styled(fit(&meta, tail_w as usize), Style::new().fg(P.dim).bg(bg)), Rect::new(tx, r.y, tail_w, 1));
    } else {
        for (k, key) in keys.iter().enumerate() {
            let glyph = match key {
                Key::Edit => ic(EDIT),
                Key::Checkout => icons().branch,
                Key::Close | Key::Cancel => ic(STOP),
                Key::Rerun => ic(RERUN),
                Key::Open => ic(OPEN),
            };
            let br = Rect::new(tx + k as u16 * 2, r.y, 2, 1).intersection(r);
            kit::icon_button(f, br, &format!("{glyph} "), bg, false);
            app.gh.hits.btns.push((br, Btn::Act(pane, i, *key)));
        }
    }
    // The title field over the row's title while it is edited.
    let gh = &app.gh;
    let (pr, repo, n) = match rw {
        Row::Pr(s, k) => (true, gh.prs(s)[k].repo.clone(), gh.prs(s)[k].number),
        Row::Issue(s, k) => (false, gh.issues(s)[k].repo.clone(), gh.issues(s)[k].number),
        _ => return,
    };
    if let Some(e) = gh.edit.as_ref().filter(|e| e.pr == pr && e.n == n && e.repo == repo) {
        let x = r.x + lead.width() as u16;
        let field = Rect::new(x, r.y, r.right().saturating_sub(x + 1), 1);
        let skip = crate::ui::rename_field(f, field, &e.field, false);
        app.gh.hits.field = (field, skip);
    }
}

pub(super) fn run_icon(s: RunState) -> (&'static str, Color) {
    match s {
        RunState::Pass => (ic(PASS), P.gh_green),
        RunState::Fail => (ic(FAIL), P.error_fg),
        RunState::Running => (ic(RUNNING), P.gh_yellow),
        RunState::Queued => (ic(QUEUED), P.gh_yellow),
        RunState::Skipped => (ic(SKIPPED), P.dim),
    }
}

