//! The Programmer's drawing: its side bar (the features, the target), its page per
//! feature (Device, Memory, Program, Erase, Option Bytes, Serial), the job card, the
//! log. Its widgets are the kit's (ui/kit.rs); what they do is in programmer.rs.

use super::*;

/// The page's glyphs, as `icons().prog` holds them.
pub(super) const I_CONNECT: usize = 0;
pub(super) const I_DISCONNECT: usize = 1;
pub(super) const I_PROGRAM: usize = 2;
pub(super) const I_REFRESH: usize = 3;
pub(super) const I_ERASE: usize = 4;
pub(super) const I_SAVE: usize = 5;
pub(super) const I_READ: usize = 6;
pub(super) const I_FILE: usize = 7;
pub(super) const I_RESET: usize = 8;
pub(super) const I_SHIELD: usize = 9;
pub(super) const I_TERMINAL: usize = 10;
pub(super) const I_MEMORY: usize = 11;
pub(super) const I_FOLDER: usize = 12;
pub(super) const I_SEND: usize = 13;

pub(super) fn glyph(i: usize) -> &'static str {
    icons().prog[i]
}

pub(super) fn feature_icon(f: Feature) -> &'static str {
    match f {
        Feature::Device => icons().chip,
        Feature::Memory => glyph(I_MEMORY),
        Feature::Program => glyph(I_PROGRAM),
        Feature::Erase => glyph(I_ERASE),
        Feature::OptionBytes => glyph(I_SHIELD),
        Feature::Serial => glyph(I_TERMINAL),
    }
}

/// The feature a job belongs to (its row shows it running).
pub(super) fn feature_of(op: Op) -> Feature {
    match op {
        Op::Probes | Op::Connect | Op::Reset => Feature::Device,
        Op::Boards | Op::Compile | Op::Upload | Op::IdfBuild | Op::IdfFlash | Op::IdfErase | Op::IdfTarget => Feature::Device, // not STM32's
        Op::ReadMem => Feature::Memory,
        Op::Program => Feature::Program,
        Op::EraseChip | Op::EraseSectors => Feature::Erase,
        Op::ReadOb => Feature::OptionBytes,
    }
}

/// A button (the kit's: its icon, its label) on `under`; not `on`: greyed, no click.
/// Drawn whole or not at all. Returns the x after it.
#[allow(clippy::too_many_arguments)]
pub(super) fn button(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), icon: &str, label: &str, look: Look, on: bool, hit: Hit, under: ratatui::style::Color) -> u16 {
    let text = if icon.is_empty() { label.to_string() } else { format!("{icon} {label}") };
    let r = kit::button_at(f, b, (x, y), &text, if on { look } else { Look::Off }, under);
    if on && !r.is_empty() {
        app.prog.hits.push((r, hit));
    }
    x + kit::button_w(&text) + 1
}

/// A key's name, dim, after a button.
pub(super) fn key_hint(f: &mut Frame, b: Rect, (x, y): (u16, u16), key: &str) {
    let r = Rect::new(x, y, key.width() as u16, 1);
    if r.right() <= b.right() && y < b.bottom() {
        f.render_widget(Span::styled(key.to_string(), Style::new().fg(P.dim)), r);
    }
}

/// A segmented control (the kit's), its choices clickable.
#[allow(clippy::too_many_arguments)]
pub(super) fn segmented(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), opts: &[String], sel: usize, hit: fn(usize) -> Hit, under: ratatui::style::Color) -> u16 {
    let (rects, after) = kit::segmented(f, b, (x, y), opts, sel, under);
    app.prog.hits.extend(rects.into_iter().enumerate().map(|(i, r)| (r, hit(i))));
    after
}

#[allow(clippy::too_many_arguments)]
pub(super) fn field(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), w: u16, text: &str, placeholder: &str, hit: Hit) -> u16 {
    let r = Rect::new(x, y, w, 1).intersection(b);
    if !r.is_empty() {
        let typing = match (&hit, &app.prog.editing) {
            (Hit::Ask(f), Some((g, t))) if f == g => Some(t.clone()),
            _ => None,
        };
        if let Some(t) = &typing {
            // Typing shows: the field's mark in the focus colour, the caret after the text.
            kit::input(f, r, t, placeholder, true);
            if app.focus == Focus::Editor {
                crate::ui::caret((r.x + 1 + (t.width() as u16).min(r.width.saturating_sub(2)), r.y));
            }
        } else {
            kit::input(f, r, text, placeholder, false);
        }
        let err = match (&hit, &app.prog.field_err) {
            (Hit::Ask(f), Some((g, e))) if f == g => Some(e.clone()),
            _ => None,
        };
        if let Some(e) = err.filter(|_| y + 1 < b.bottom()) {
            let er = Rect::new(r.x, y + 1, b.right().saturating_sub(r.x), 1);
            f.render_widget(Span::styled(fit(&e, er.width as usize), Style::new().fg(P.error_fg)), er);
        }
        app.prog.hits.push((r, hit));
    }
    x + w + 1
}

pub(super) fn check(app: &mut App, f: &mut Frame, b: Rect, (x, y): (u16, u16), on: bool, label: &str, hit: Hit) -> u16 {
    let r = kit::check(f, b, (x, y), on, label);
    if !r.is_empty() {
        app.prog.hits.push((r, hit));
    }
    x + 4 + label.width() as u16 + 2
}

/// A dim label at `x`, `w` wide.
pub(super) fn label(f: &mut Frame, b: Rect, (x, y): (u16, u16), text: &str) {
    let r = Rect::new(x, y, (text.width() as u16).min(b.right().saturating_sub(x)), 1);
    if y < b.bottom() {
        f.render_widget(Span::styled(text.to_string(), Style::new().fg(P.dim)), r);
    }
}

/// Text at (x, y), cut to `b`.
pub(super) fn text_at(f: &mut Frame, b: Rect, (x, y): (u16, u16), spans: Vec<Span<'static>>) {
    if y >= b.y && y < b.bottom() && x < b.right() {
        f.render_widget(Line::from(spans), Rect::new(x, y, b.right() - x, 1));
    }
}

pub(super) fn card(f: &mut Frame, r: Rect, title: &str, border: ratatui::style::Color) -> Rect {
    kit::card(f, r, title, border)
}

/// The page's scroll (its scrollbar's).
pub fn main_off(app: &App) -> usize {
    app.prog.main_off
}

pub fn set_main_off(app: &mut App, off: usize) {
    app.prog.main_off = off; // the page clamps it
}

/// "3 min ago" for a file's change.
pub(super) fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

pub(super) fn probe_text(s: &Programmer) -> String {
    match s.probes.as_deref() {
        None if s.job.as_ref().is_some_and(|j| j.op == Op::Probes) => "Looking for ST-LINK probes…".into(),
        None => "Not looked for yet".into(),
        Some([]) => "No ST-LINK probe found".into(),
        Some(ps) => probe_line(&ps[s.probe.min(ps.len() - 1)]),
    }
}

pub(super) fn probe_line(p: &Probe) -> String {
    let board = if p.board.is_empty() || p.board == "--" { String::new() } else { format!("{} · ", p.board) };
    format!("{board}ST-LINK {} · SN {}", p.fw, p.sn)
}

pub(super) fn info<'a>(s: &'a Programmer, k: &str) -> Option<&'a str> {
    s.info.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str())
}

/// The view: the target, the features, the probe and the tool; or what to do without it.
pub fn draw_side(app: &mut App, f: &mut Frame, r: Rect) {
    ensure_checked(app);
    let focused = app.focus == crate::panel::Focus::Side && !app.activity_lit;
    let card_r = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused);
    let pad = (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1);
    let area = crate::ui::inset(card_r, pad);
    app.ui.side_list = Rect::default();
    if area.width < 8 || area.height < 4 {
        return;
    }
    // The switch: what the view programs.
    let fams: Vec<String> = FAMILIES.iter().map(|(_, n)| n.to_string()).collect();
    let sel = FAMILIES.iter().position(|(f, _)| *f == app.prog.family).unwrap_or(0);
    segmented(app, f, area, (area.x, area.y), &fams, sel, Hit::Family, P.sidebar_bg);
    let area = Rect::new(area.x, area.y + 2, area.width, area.height - 2);
    match app.prog.family {
        Family::Arduino => return arduino::draw_side(app, f, area, card_r, focused),
        Family::Idf => return idf::draw_side(app, f, area, card_r, focused),
        Family::Stm32 => {}
    }
    let dim = Style::new().fg(P.dim);
    if app.prog.cli.is_none() {
        let note = if app.prog.installing.is_some() {
            Line::styled("Finish installing in the terminal below (or in your browser): this view looks for the tool every few seconds.", Style::new().fg(P.git_modified))
        } else {
            Line::styled("Install runs ST's installer in the terminal below. ST asks you to sign in to download it: without one in ~/Downloads, Install opens its download page.", dim)
        };
        let lines = vec![
            Line::styled("STM32CubeProgrammer is not installed.", Style::new().bold()),
            Line::raw(""),
            Line::raw(format!("termcode programs and reads STM32 chips through an ST-LINK with its command line tool, {CLI}.")),
            Line::raw(""),
            note,
        ];
        let p = Paragraph::new(lines).wrap(Wrap { trim: true }).style(Style::new().fg(P.sidebar_fg));
        let h = p.line_count(area.width) as u16;
        f.render_widget(p, area);
        let y = area.y + h + 1;
        let x = button(app, f, area, (area.x, y), glyph(I_SAVE), "Install", Look::Primary, true, Hit::Install, P.sidebar_bg);
        button(app, f, area, (x, y), glyph(I_REFRESH), "Check Again", Look::Secondary, true, Hit::Recheck, P.sidebar_bg);
        return;
    }
    // The target, as a header: the chip, its name, the connection.
    let s = &app.prog;
    let mut y = area.y;
    let name = info(s, "Device name").map_or("No target".to_string(), str::to_string);
    text_at(f, area, (area.x, y), vec![Span::styled(fit(&name, area.width as usize), Style::new().fg(P.sidebar_fg).bold())]);
    y += 1;
    let (st, c) = if s.connected { ("● Connected", P.git_added) } else { ("○ Not connected", P.dim) };
    text_at(f, area, (area.x, y), vec![Span::styled(st, Style::new().fg(c))]);
    y += 2;
    let running = s.job.as_ref().filter(|j| j.op.family() == Family::Stm32).map(|j| feature_of(j.op));
    let serial_on = s.serial.is_some();
    let rows: Vec<(&str, &str, Option<ratatui::style::Color>)> = FEATURES
        .iter()
        .map(|(feat, name, _)| {
            // What runs: the job's row, the console's.
            let dot = if running == Some(*feat) { Some(P.git_modified) } else { (*feat == Feature::Serial && serial_on).then_some(P.git_added) };
            (feature_icon(*feat), *name, dot)
        })
        .collect();
    y = feature_rows(app, f, area, card_r, focused, y, &rows, app.prog.feature, Hit::Feature) + 1;
    let probe = probe_text(&app.prog);
    let home = std::env::var("HOME").unwrap_or_default();
    let tool = app.prog.cli.as_ref().map_or(String::new(), |c| c.display().to_string());
    let tool = match tool.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => tool,
    };
    let mut lines = vec![Line::styled("PROBE", dim.bold()), Line::styled(probe, Style::new().fg(P.sidebar_fg))];
    if let Some(p) = &app.prog.serial {
        lines.extend([Line::raw(""), Line::styled("SERIAL", dim.bold()), Line::styled(format!("● {} at {} baud", tty_name(&p.path), p.baud), Style::new().fg(P.git_added))]);
    }
    lines.extend([Line::raw(""), Line::styled("TOOL", dim.bold()), Line::styled(tool, dim)]);
    if y < area.bottom() {
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), Rect::new(area.x, y, area.width, area.bottom() - y));
    }
}

/// The features, a row each (its icon, its name, a dot while it runs), the chosen one lit.
/// Returns the row after them.
#[allow(clippy::too_many_arguments)]
pub(super) fn feature_rows(app: &mut App, f: &mut Frame, area: Rect, card_r: Rect, focused: bool, mut y: u16, rows: &[(&str, &str, Option<ratatui::style::Color>)], chosen: usize, hit: fn(usize) -> Hit) -> u16 {
    for (i, (icon, name, dot)) in rows.iter().enumerate() {
        if y >= area.bottom() {
            break;
        }
        let rr = Rect::new(card_r.x, y, card_r.width, 1);
        let sel = i == chosen;
        let hot = app.hover.is_some_and(|p| rr.contains(p));
        let bg = match (sel, focused, hot) {
            (true, true, _) => P.list_active_bg,
            (true, false, _) => P.list_inactive_bg,
            (false, _, true) => P.list_hover_bg,
            _ => P.sidebar_bg,
        };
        fill(f, rr, bg);
        if sel {
            f.render_widget(Span::styled("▎", Style::new().fg(P.focus_border).bg(bg)), Rect::new(card_r.x, y, 1, 1));
        }
        let icon_c = if sel { P.focus_border } else { P.icon_fg };
        row(f, Rect::new(area.x, y, area.width, 1), bg, vec![Span::styled(format!("{icon} "), Style::new().fg(icon_c)), Span::styled(name.to_string(), Style::new().fg(P.sidebar_fg))]);
        if let Some(c) = dot.filter(|_| area.width > 4) {
            f.render_widget(Span::styled("●", Style::new().fg(c).bg(bg)), Rect::new(area.right() - 1, y, 1, 1));
        }
        app.prog.hits.push((rr, hit(i)));
        y += 1;
    }
    y
}

/// The page's top: the target (its name, the connection, Connect or Disconnect) and what
/// it is.
pub(super) fn target_bar(app: &mut App, f: &mut Frame, r: Rect) {
    let inner = card(f, r, &format!("{} STM32", icons().chip), P.widget_border);
    if inner.height == 0 {
        return;
    }
    let s = &app.prog;
    let on = s.job.is_none();
    let connected = s.connected;
    let name = info(s, "Device name").map(str::to_string);
    let details: Vec<String> = ["Board", "Device CPU", "NVM size", "Flash size", "Voltage", "SWD freq", "JTAG freq"].iter().filter_map(|k| info(s, k).map(|v| v.to_string())).collect();
    let details = if details.is_empty() { probe_text(s) } else { details.join(" · ") };
    // The right end: the state, then the button.
    let (icon, label, hit) = if connected { (glyph(I_DISCONNECT), "Disconnect", Hit::Disconnect) } else { (glyph(I_CONNECT), "Connect", Hit::Run(Op::Connect)) };
    let bw = kit::button_w(&format!("{icon} {label}"));
    let bx = inner.right().saturating_sub(bw);
    let (st, c) = if connected { ("● Connected", P.git_added) } else { ("○ Not connected", P.dim) };
    let sx = bx.saturating_sub(st.width() as u16 + 3);
    text_at(f, inner, (sx, inner.y), vec![Span::styled(st, Style::new().fg(c))]);
    button(app, f, inner, (bx, inner.y), icon, label, if connected { Look::Secondary } else { Look::Primary }, on, hit, P.editor_bg);
    let room = sx.saturating_sub(inner.x + 1) as usize;
    let title = match name {
        Some(n) => Span::styled(fit(&n, room), Style::new().fg(P.editor_fg).bold()),
        None => Span::styled(fit("No target connected", room), Style::new().fg(P.dim)),
    };
    text_at(f, inner, (inner.x, inner.y), vec![title]);
    if inner.height > 1 {
        text_at(f, inner, (inner.x, inner.y + 1), vec![Span::styled(fit(&details, inner.width as usize), Style::new().fg(P.dim))]);
    }
}

/// A Program job's steps: each one's name, and whether it is done, running or to come.
pub(super) fn steps(s: &Programmer, j: &Job) -> Vec<(&'static str, u8)> {
    let mut v: Vec<(&str, &[&str])> = vec![("Erase", &["Erasing", "erase"]), ("Download", &["Download in Progress", "Memory Programming"])];
    if s.verify {
        v.push(("Verify", &["Verifying"]));
    }
    if s.run_after {
        v.push(("Run", &["reset is performed", "MCU Reset"]));
    }
    let at = j.out.iter().filter_map(|l| v.iter().rposition(|(_, keys)| keys.iter().any(|k| l.contains(k)))).max().unwrap_or(0);
    v.iter().enumerate().map(|(i, (n, _))| (*n, if i < at { 2 } else if i == at { 1 } else { 0 })).collect()
}

/// The running job, on the page: its bar and (programming) its steps; else how the last
/// one ended. Returns the rows it took.
pub(super) fn job_card(app: &mut App, f: &mut Frame, r: Rect) -> u16 {
    let s = &app.prog;
    let ascii = crate::theme::ascii();
    let Some(j) = &s.job else {
        let here = |op: Op| match s.family {
            Family::Arduino => (op == Op::Boards && s.ard.feature == 0) || (matches!(op, Op::Compile | Op::Upload) && s.ard.feature == arduino::SKETCH),
            Family::Idf => op.family() == Family::Idf && s.idf.feature != idf::SERIAL,
            Family::Stm32 => op.family() == Family::Stm32 && feature_of(op) == FEATURES[s.feature].0,
        };
        let Some((ok, text)) = s.last.as_ref().filter(|_| s.last_op.is_some_and(here)) else { return 0 };
        let (mark, c) = if *ok { (if ascii { "ok" } else { "✓" }, P.git_added) } else { (if ascii { "x" } else { "✗" }, P.error_fg) };
        text_at(f, r, (r.x + 1, r.y), vec![Span::styled(format!("{mark} {text}"), Style::new().fg(c))]);
        return 2;
    };
    let t = j.since.elapsed().as_secs();
    let program = j.op == Op::Program;
    let h = if program { 4 } else { 3 };
    let inner = card(f, Rect::new(r.x, r.y, r.width, h.min(r.height)), &format!("{} {} · {}:{:02}", if ascii { ">" } else { "▶" }, j.what, t / 60, t % 60), P.focus_border);
    if inner.height == 0 {
        return h;
    }
    let bar_w = inner.width.saturating_sub(6) as usize;
    let (on, off) = if ascii { ("#", "-") } else { ("━", "─") };
    let bar = match j.pct {
        Some(p) => {
            let n = bar_w * p.min(100) as usize / 100;
            vec![Span::styled(on.repeat(n), Style::new().fg(P.focus_border)), Span::styled(off.repeat(bar_w - n), Style::new().fg(P.widget_border)), Span::styled(format!(" {p:>3}%"), Style::new().fg(P.editor_fg).bold())]
        }
        None => vec![Span::styled("Working…", Style::new().fg(P.dim))],
    };
    text_at(f, inner, (inner.x, inner.y), bar);
    if program && inner.height > 1 {
        let mut spans = Vec::new();
        for (i, (name, state)) in steps(s, j).into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(if ascii { "  >  " } else { "  ›  " }, Style::new().fg(P.dim)));
            }
            let (mark, c) = match state {
                2 => (if ascii { "v" } else { "✓" }, P.git_added),
                1 => (if ascii { "*" } else { "●" }, P.focus_border),
                _ => (if ascii { "o" } else { "○" }, P.dim),
            };
            spans.push(Span::styled(format!("{mark} "), Style::new().fg(c)));
            spans.push(Span::styled(name, Style::new().fg(if state == 0 { P.dim } else { P.editor_fg })));
        }
        text_at(f, inner, (inner.x, inner.y + 1), spans);
    }
    h + 1
}

/// The page: the target, the feature's title, the running job, the feature's cards.
pub fn draw_page(app: &mut App, f: &mut Frame, body: Rect) {
    fill(f, body, P.editor_bg);
    ensure_checked(app);
    app.prog.main_area = Rect::default();
    let b = Rect::new(body.x + 1, body.y, body.width.saturating_sub(2), body.height);
    if b.width < 30 || b.height < 6 {
        return;
    }
    match app.prog.family {
        Family::Arduino => return arduino::draw_page(app, f, b),
        Family::Idf => return idf::draw_page(app, f, b),
        Family::Stm32 => {}
    }
    if app.prog.cli.is_none() {
        text_at(f, b, (b.x + 1, b.y + 1), vec![Span::styled(format!("{CLI} is not installed: the Programmer view in the sidebar installs it."), Style::new().fg(P.dim))]);
        return;
    }
    let mut y = b.y;
    let bar_h = 4.min(b.height);
    target_bar(app, f, Rect::new(b.x, y, b.width, bar_h));
    y += bar_h + 1;
    let (feature, title, about) = FEATURES[app.prog.feature];
    text_at(f, b, (b.x + 1, y), vec![Span::styled(format!("{} ", feature_icon(feature)), Style::new().fg(P.focus_border)), Span::styled(title.to_string(), Style::new().fg(P.editor_fg).bold()), Span::styled(format!("   {about}"), Style::new().fg(P.dim))]);
    y += 2;
    if y < b.bottom() {
        y += job_card(app, f, Rect::new(b.x, y, b.width, b.bottom() - y));
    }
    if y + 3 > b.bottom() {
        return;
    }
    let rest = Rect::new(b.x, y, b.width, b.bottom() - y);
    match feature {
        Feature::Device => page_device(app, f, rest),
        Feature::Memory => page_memory(app, f, rest),
        Feature::Program => page_program(app, f, rest),
        Feature::Erase => page_erase(app, f, rest),
        Feature::OptionBytes => page_ob(app, f, rest),
        Feature::Serial => page_serial(app, f, rest),
    }
}

/// `r`'s top `h` rows, and what is left under them (a row between).
pub(super) fn take(r: Rect, h: u16) -> (Rect, Rect) {
    let h = h.min(r.height);
    (Rect::new(r.x, r.y, r.width, h), Rect::new(r.x, r.y + h + u16::from(r.height > h), r.width, r.height.saturating_sub(h + 1)))
}

pub(super) fn page_device(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (conn, r) = take(r, 5);
    let c = card(f, conn, "Connection", P.widget_border);
    let lx = c.x + 9;
    for (i, (name, y)) in [("Port", c.y), ("Mode", c.y + 1), ("Speed", c.y + 2)].into_iter().enumerate() {
        label(f, c, (c.x, y), name);
        let (opts, sel, hit): (Vec<String>, usize, fn(usize) -> Hit) = match i {
            0 => (PORTS.iter().map(|p| p.to_string()).collect(), app.prog.port, Hit::Port),
            1 => (MODES.iter().map(|(m, _)| m.to_string()).collect(), app.prog.mode, Hit::Mode),
            _ => (FREQS.iter().map(|(n, _)| n.to_string()).collect(), app.prog.freq, Hit::Freq),
        };
        segmented(app, f, c, (lx, y), &opts, sel, hit, P.editor_bg);
    }
    // The probes: one a row, the one used marked; Refresh and Reset under them.
    let probes = app.prog.probes.clone().unwrap_or_default();
    let (pc, r) = take(r, probes.len().max(1) as u16 + 2);
    let c = card(f, pc, "Probes", P.widget_border);
    if probes.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled(probe_text(&app.prog), Style::new().fg(P.dim))]);
    }
    for (i, p) in probes.iter().enumerate() {
        let y = c.y + i as u16;
        if y >= c.bottom() {
            break;
        }
        let used = i == app.prog.probe.min(probes.len() - 1);
        let rr = Rect::new(c.x, y, c.width, 1);
        let hot = app.hover.is_some_and(|q| rr.contains(q));
        let mark = if used { "◉" } else { "○" };
        text_at(f, c, (c.x, y), vec![Span::styled(format!("{mark} "), Style::new().fg(if used { P.focus_border } else { P.dim })), Span::styled(probe_line(p), Style::new().fg(P.editor_fg).add_modifier(if hot { Modifier::UNDERLINED } else { Modifier::empty() }))]);
        app.prog.hits.push((rr, Hit::ProbeRow(i)));
    }
    if r.height == 0 {
        return;
    }
    let (acts, r) = take(r, 1);
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_RESET), "Reset Target", Look::Secondary, on, Hit::Run(Op::Reset), P.editor_bg);
    button(app, f, acts, (x, acts.y), glyph(I_REFRESH), "Refresh Probes", Look::Secondary, on, Hit::Run(Op::Probes), P.editor_bg);
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Target", P.widget_border);
    let s = &app.prog;
    if s.info.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Connect (F5) reads the target: its name, ID, flash size and core.", Style::new().fg(P.dim))]);
        return;
    }
    // Two columns where they fit.
    let kw = s.info.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let vw = s.info.iter().map(|(_, v)| v.width()).max().unwrap_or(0);
    let col = (kw + vw + 6) as u16;
    let cols = if c.width >= col * 2 { 2 } else { 1 };
    let per = s.info.len().div_ceil(cols);
    for (i, (k, v)) in s.info.iter().enumerate() {
        let (cx, cy) = (c.x + (i / per) as u16 * col, c.y + (i % per) as u16);
        text_at(f, c, (cx, cy), vec![Span::styled(format!("{k:<kw$}  "), Style::new().fg(P.dim)), Span::styled(v.clone(), Style::new().fg(P.editor_fg))]);
    }
}

pub(super) fn page_memory(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 5);
    let c = card(f, top, "Range", P.widget_border);
    if c.height == 0 {
        return;
    }
    let s = &app.prog;
    let (a, sz, n) = (hex(s.addr), hex(s.size), size_text(s.size as usize));
    label(f, c, (c.x, c.y), "Address");
    let x = field(app, f, c, (c.x + 9, c.y), 14, &a, "", Hit::Ask(Field::Addr));
    label(f, c, (x + 1, c.y), "Size");
    let x = field(app, f, c, (x + 6, c.y), 14, &sz, "", Hit::Ask(Field::Size));
    label(f, c, (x + 1, c.y), &n);
    // (A refused value says why on the row under the fields.)
    label(f, c, (c.x, c.y + 2), "Width");
    let widths: Vec<String> = WIDTHS.iter().map(|w| format!("{}-bit", w * 8)).collect();
    segmented(app, f, c, (c.x + 9, c.y + 2), &widths, app.prog.width, Hit::Width, P.editor_bg);
    // The actions at the right end: Read (F5) on the first row, Save As… on the last.
    let x = c.right().saturating_sub(kit::button_w(&format!("{} Save As…", glyph(I_SAVE))).max(kit::button_w(&format!("{} Read", glyph(I_READ))) + 3));
    let after = button(app, f, c, (x, c.y), glyph(I_READ), "Read", Look::Primary, on, Hit::Run(Op::ReadMem), P.editor_bg);
    key_hint(f, c, (after, c.y), "F5");
    if app.prog.mem.is_some() {
        button(app, f, c, (x, c.y + 2), glyph(I_SAVE), "Save As…", Look::Secondary, true, Hit::Ask(Field::SaveAs), P.editor_bg);
    }
    if r.height < 4 {
        return;
    }
    let title = match &app.prog.mem {
        Some((base, b)) => format!("Memory · {} – {} · {}", hex(*base), hex(base.wrapping_add(b.len().saturating_sub(1) as u32)), size_text(b.len())),
        None => "Memory".into(),
    };
    let c = card(f, r, &title, P.widget_border);
    app.prog.main_area = c;
    if app.prog.mem.is_none() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Read (F5) shows the memory here, as STM32CubeProgrammer does.", Style::new().fg(P.dim))]);
        return;
    }
    draw_grid(app, f, c);
}

pub(super) fn page_program(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 6);
    let c = card(f, top, "Firmware", P.widget_border);
    if c.height == 0 {
        return;
    }
    let file = app.prog.file.clone();
    let browse = format!("{} Browse…", glyph(I_FOLDER));
    let bx = c.right().saturating_sub(kit::button_w(&browse) + 1);
    button(app, f, c, (bx, c.y), glyph(I_FOLDER), "Browse…", Look::Secondary, true, Hit::Browse, P.editor_bg);
    let room = bx.saturating_sub(c.x + 3);
    // The file's name (a click browses too) and what it is.
    let nr = Rect::new(c.x, c.y, room, 1);
    match &file {
        Some(p) => {
            let name = p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            text_at(f, c, (c.x, c.y), vec![Span::styled(format!("{} ", glyph(I_FILE)), Style::new().fg(P.focus_border)), Span::styled(fit(&name, room.saturating_sub(2) as usize), Style::new().fg(P.editor_fg).bold())]);
            let m = fs::metadata(p).ok();
            let kind = p.extension().map_or(String::new(), |e| e.to_string_lossy().to_uppercase());
            let what = match &m {
                Some(m) => format!("{} · {} · {kind} · changed {}", tilde(p.parent().unwrap_or(p)), size_text(m.len() as usize), m.modified().map_or(String::new(), ago)),
                None => format!("{}: missing", tilde(p)),
            };
            text_at(f, c, (c.x + 2, c.y + 1), vec![Span::styled(fit(&what, c.width.saturating_sub(2) as usize), Style::new().fg(if m.is_some() { P.dim } else { P.error_fg }))]);
        }
        None => text_at(f, c, (c.x, c.y), vec![Span::styled(fit("No file chosen: Browse… picks one, folder by folder.", room as usize), Style::new().fg(P.dim))]),
    }
    app.prog.hits.push((nr, Hit::Browse));
    label(f, c, (c.x, c.y + 2), "Address");
    let a = hex(app.prog.bin_addr);
    let x = field(app, f, c, (c.x + 9, c.y + 2), 14, &a, "", Hit::Ask(Field::BinAddr));
    let bin = file.as_deref().is_none_or(is_bin);
    label(f, c, (x + 1, c.y + 2), if bin { "where a .bin goes" } else { "not used: this file has its own addresses" });
    if r.height < 3 {
        return;
    }
    let (opts, r) = take(r, 3);
    let c = card(f, opts, "Options", P.widget_border);
    let s = &app.prog;
    let (v, e, rr) = (s.verify, s.full_erase, s.run_after);
    let x = check(app, f, c, (c.x, c.y), v, "Verify", Hit::Verify);
    let x = check(app, f, c, (x, c.y), e, "Erase the full chip first", Hit::FullErase);
    check(app, f, c, (x, c.y), rr, "Reset and run after", Hit::RunAfter);
    if r.height == 0 {
        return;
    }
    let (acts, r) = take(r, 1);
    let ready = on && file.is_some();
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_PROGRAM), "Start Programming", Look::Primary, ready, Hit::Run(Op::Program), P.editor_bg);
    key_hint(f, acts, (x, acts.y), "F5");
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Command", P.widget_border);
    if let Ok((args, _, _)) = build(&app.prog, Op::Program) {
        let p = Paragraph::new(Line::from(vec![Span::styled("$ ", Style::new().fg(P.dim)), Span::styled(shown_cmd(CLI, &args), Style::new().fg(P.editor_fg))])).wrap(Wrap { trim: false });
        f.render_widget(p, c);
    }
}

pub(super) fn page_erase(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 4);
    let c = card(f, top, "Sectors", P.widget_border);
    if c.height == 0 {
        return;
    }
    label(f, c, (c.x, c.y), "Sectors");
    let s = app.prog.sectors.clone();
    let x = field(app, f, c, (c.x + 9, c.y), 18, &s, "0-3, or 0 1 2", Hit::Ask(Field::Sectors));
    button(app, f, c, (x + 1, c.y), glyph(I_ERASE), "Erase Sectors", Look::Secondary, on, Hit::Confirm(Op::EraseSectors), P.editor_bg);
    if app.prog.field_err.as_ref().is_none_or(|(f, _)| *f != Field::Sectors) {
        label(f, c, (c.x + 9, c.y + 1), "0-3 erases sectors 0 to 3; 0 1 2 those three.");
    }
    if r.height < 4 {
        return;
    }
    let (dz, _) = take(r, 4);
    let c = card(f, dz, "Danger zone", P.error_fg);
    text_at(f, c, (c.x, c.y), vec![Span::styled("Erase the whole flash: every byte on the chip goes, and it can't be undone.", Style::new().fg(P.editor_fg))]);
    button(app, f, c, (c.x, c.y + 1), glyph(I_ERASE), "Erase Full Chip", Look::Danger, on, Hit::Confirm(Op::EraseChip), P.editor_bg);
}

pub(super) fn page_ob(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (acts, r) = take(r, 1);
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_SHIELD), "Read Option Bytes", Look::Primary, on, Hit::Run(Op::ReadOb), P.editor_bg);
    key_hint(f, acts, (x, acts.y), "F5");
    label(f, acts, (x + 4, acts.y), "Read only: change them in STM32CubeProgrammer.");
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Option bytes", P.widget_border);
    app.prog.main_area = c;
    let s = &app.prog;
    if s.ob.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Read (F5) shows them here.", Style::new().fg(P.dim))]);
        return;
    }
    let (dim, fg) = (Style::new().fg(P.dim), Style::new().fg(P.editor_fg));
    let w = s.ob.iter().map(|o| if let Ob::Field(k, _, _) = o { k.width() } else { 0 }).max().unwrap_or(0);
    let vw = s.ob.iter().map(|o| if let Ob::Field(_, v, _) = o { v.width() } else { 0 }).max().unwrap_or(0);
    let mut lines: Vec<Line> = Vec::new();
    for o in &s.ob {
        match o {
            Ob::Group(g) => {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::from(vec![Span::styled("▎", Style::new().fg(P.focus_border)), Span::styled(g.clone(), fg.bold())]));
            }
            Ob::Field(k, v, m) => {
                // Read protection says how locked the chip is, in colour.
                let mc = match () {
                    _ if k == "RDP" && m.contains("Level 0") => P.git_added,
                    _ if k == "RDP" && m.contains("Level 1") => P.gh_yellow,
                    _ if k == "RDP" => P.error_fg,
                    _ if m.contains("protection active") => P.gh_yellow,
                    _ => P.dim,
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {k:<w$}  "), dim),
                    Span::styled(format!(" {v:<vw$} "), Style::new().fg(P.input_fg).bg(P.input_bg)),
                    Span::raw("  "),
                    Span::styled(m.clone(), Style::new().fg(mc)),
                ]));
            }
        }
    }
    let view = c.height as usize;
    let most = lines.len().saturating_sub(view);
    app.prog.main_off = app.prog.main_off.min(most);
    let off = app.prog.main_off;
    if lines.len() > view {
        crate::ui::scrollbar(app, f, Rect::new(c.right(), c.y, 1, c.height), crate::panel::ScrollTarget::Prog, lines.len(), view, off, most);
    }
    f.render_widget(Paragraph::new(lines.into_iter().skip(off).collect::<Vec<_>>()), c);
}

pub(super) fn page_serial(app: &mut App, f: &mut Frame, r: Rect) {
    let (top, r) = take(r, 5);
    let c = card(f, top, "Port", P.widget_border);
    if c.height == 0 {
        return;
    }
    let open = app.prog.serial.is_some();
    // The buttons at the right end, the port and rate left of them.
    let bw = |i: &str, l: &str| kit::button_w(&format!("{i} {l}"));
    let rw = (bw(glyph(I_DISCONNECT), "Disconnect") + 3).max(bw(glyph(I_SEND), "Send…") + 1 + bw(glyph(I_ERASE), "Clear")) + 1;
    let bx = c.right().saturating_sub(rw);
    let (icon, name) = if open { (glyph(I_DISCONNECT), "Disconnect") } else { (glyph(I_CONNECT), "Connect") };
    let x = button(app, f, c, (bx, c.y), icon, name, if open { Look::Secondary } else { Look::Primary }, true, Hit::SerialToggle, P.editor_bg);
    key_hint(f, c, (x, c.y), "F5");
    let x = button(app, f, c, (bx, c.y + 1), glyph(I_SEND), "Send…", Look::Secondary, open, Hit::Ask(Field::Send), P.editor_bg);
    button(app, f, c, (x, c.y + 1), glyph(I_ERASE), "Clear", Look::Secondary, !app.prog.serial_out.is_empty(), Hit::SerialClear, P.editor_bg);
    let tty = app.prog.tty.clone().or_else(|| default_tty(&app.prog));
    let what = tty.as_ref().map_or(String::new(), |p| {
        let name = crate::serial::ports().into_iter().find(|(q, _)| q == p).map_or_else(|| tty_name(p), |(_, w)| w);
        format!("{name}  {}", icons().chev_down)
    });
    label(f, c, (c.x, c.y), "Port");
    let w = bx.saturating_sub(c.x + 11).max(10);
    field(app, f, c, (c.x + 9, c.y), w, &what, "No serial port found: plug the board in", Hit::Tty);
    label(f, c, (c.x, c.y + 1), "Baud");
    let baud = app.prog.baud.to_string();
    let x = field(app, f, c, (c.x + 9, c.y + 1), 12, &baud, "", Hit::Ask(Field::Baud));
    if app.prog.field_err.as_ref().is_none_or(|(f, _)| *f != Field::Baud) {
        let (st, col) = match &app.prog.serial {
            Some(p) => (format!("● {} at {} baud", tty_name(&p.path), p.baud), P.git_added),
            None => ("○ Not connected".to_string(), P.dim),
        };
        text_at(f, c, (x + 1, c.y + 1), vec![Span::styled(fit(&st, bx.saturating_sub(x + 2) as usize), Style::new().fg(col))]);
    }
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Monitor", P.widget_border);
    app.prog.main_area = c;
    let s = &app.prog;
    if s.serial_out.iter().all(String::is_empty) {
        text_at(f, c, (c.x, c.y), vec![Span::styled("What the target prints on its serial port shows here (and in the panel's Serial tab).", Style::new().fg(P.dim))]);
        return;
    }
    serial_lines(s, f, c);
}

/// The console's lines, the newest at the bottom (scrolled up by `serial_off`).
pub(super) fn serial_lines(s: &Programmer, f: &mut Frame, r: Rect) {
    let h = r.height as usize;
    // The line still coming shows once it has text.
    let len = s.serial_out.len() - usize::from(s.serial_out.last().is_some_and(String::is_empty));
    let end = len.saturating_sub(s.serial_off);
    for (k, l) in s.serial_out[end.saturating_sub(h)..end].iter().enumerate() {
        let note = l.starts_with("── ") || l.starts_with("-- ");
        let st = Style::new().fg(if note { P.dim } else { P.editor_fg });
        f.render_widget(Span::styled(fit(&plain(l), r.width as usize), st), Rect::new(r.x, r.y + k as u16, r.width, 1));
    }
}

/// A row of Memory's grid: the address, the data in `width`-byte values (little-endian,
/// as the target reads them), the bytes as text.
pub(super) fn grid_row(addr: u64, chunk: &[u8], width: usize) -> (String, String, String) {
    let cell = |i: usize| match chunk.get(i * width..((i + 1) * width).min(chunk.len())) {
        Some(b) if !b.is_empty() => {
            let v = b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64);
            format!("{:>w$}", format!("{v:0n$X}", n = b.len() * 2), w = width * 2)
        }
        _ => " ".repeat(width * 2),
    };
    let sep = if width == 4 { "  " } else { " " };
    let data = (0..16 / width).map(cell).collect::<Vec<_>>().join(sep);
    let text = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
    (format!("0x{addr:08X}"), data, text)
}

pub(super) fn grid_header(width: usize) -> String {
    let sep = if width == 4 { "  " } else { " " };
    (0..16 / width).map(|i| format!("{:<w$}", format!("{:X}", i * width), w = width * 2)).collect::<Vec<_>>().join(sep)
}

/// Memory as CubeProgrammer shows it: Address, a column per value, ASCII; the header
/// stays, rows alternate, zero values are dim; a scrollbar at the right.
pub(super) fn draw_grid(app: &mut App, f: &mut Frame, a: Rect) {
    let rows = app.prog.mem.as_ref().map_or(0, |(_, b)| b.len().div_ceil(16));
    let h = a.height.saturating_sub(1) as usize;
    let off = app.prog.main_off.min(rows.saturating_sub(h));
    app.prog.main_off = off;
    let s = &app.prog;
    let Some((base, bytes)) = &s.mem else { return };
    let w = WIDTHS[s.width];
    let (dim, fg) = (Style::new().fg(P.dim), Style::new().fg(P.editor_fg));
    let bar = if crate::theme::ascii() { "|" } else { "│" };
    let head = Rect::new(a.x, a.y, a.width.saturating_sub(1), 1);
    row(f, head, P.widget_bg, vec![Span::styled(format!("{:<10}   {}  {bar} ASCII", "Address", grid_header(w)), Style::new().fg(P.dim).bold())]);
    for (k, chunk) in bytes.chunks(16).skip(off).take(h).enumerate() {
        let (addr, data, text) = grid_row(*base as u64 + ((off + k) * 16) as u64, chunk, w);
        let bg = if (off + k) % 2 == 1 { P.cur_line } else { P.editor_bg };
        let mut spans = vec![Span::styled(addr, dim), Span::raw("   ")];
        // Each value: dim when it is zero.
        for (i, cell) in data.split(if w == 4 { "  " } else { " " }).enumerate() {
            if i > 0 {
                spans.push(Span::raw(if w == 4 { "  " } else { " " }));
            }
            let zero = cell.trim().chars().all(|c| c == '0');
            spans.push(Span::styled(cell.to_string(), if zero { dim } else { fg }));
        }
        spans.push(Span::styled(format!("  {bar} "), Style::new().fg(P.widget_border)));
        spans.extend(text.chars().map(|c| Span::styled(c.to_string(), if c == '.' { dim } else { fg })));
        row(f, Rect::new(a.x, a.y + 1 + k as u16, a.width.saturating_sub(1), 1), bg, spans);
    }
    if rows > h {
        crate::ui::scrollbar(app, f, Rect::new(a.right().saturating_sub(1), a.y + 1, 1, h as u16), crate::panel::ScrollTarget::Prog, rows, h, off, rows - h);
    }
}

/// The panel's tabs on its top border: Terminal, Programmer, and Serial once the console
/// was used. Returns where the terminals' own tabs go.
pub fn panel_tabs(app: &mut App, f: &mut Frame, r: Rect) -> u16 {
    let mut x = r.x + 2;
    let s = &app.prog;
    let shown_now = if !s.log_shown { 0 } else if s.serial_tab { 2 } else { 1 };
    let mut tabs = vec![(" Terminal ", Hit::PanelTerm), (" Programmer ", Hit::PanelLog)];
    if s.has_serial() {
        tabs.push((" Serial ", Hit::PanelSerial));
    }
    for (i, (name, hit)) in tabs.into_iter().enumerate() {
        let w = (name.width() as u16).min(r.right().saturating_sub(x + 1));
        if w == 0 {
            break;
        }
        let shown = i == shown_now;
        let st = if shown { Style::new().fg(P.editor_fg).bold() } else { Style::new().fg(P.dim) };
        let tr = Rect::new(x, r.y, w, 1);
        let word = fit(name.trim(), w.saturating_sub(2) as usize);
        let line = Line::from(vec![Span::styled(" ", st), Span::styled(word, if shown { st.underlined() } else { st }), Span::styled(" ", st)]);
        f.render_widget(line, tr);
        app.prog.hits.push((tr, hit));
        x += w + 1;
    }
    x
}

/// The Serial tab: the port's state, then what the target printed.
pub(super) fn draw_serial(app: &mut App, f: &mut Frame, r: Rect) {
    let s = &app.prog;
    let status = match &s.serial {
        Some(p) => Span::styled(format!(" ● {} at {} baud", tty_name(&p.path), p.baud), Style::new().fg(P.git_added)),
        None => Span::styled(" ○ Not connected (Serial Console connects)", Style::new().fg(P.dim)),
    };
    row(f, Rect::new(r.x, r.y, r.width, 1), P.editor_bg, vec![status]);
    serial_lines(s, f, Rect::new(r.x + 1, r.y + 1, r.width.saturating_sub(1), r.height.saturating_sub(1)));
}

/// The Programmer tab: the job's progress (or how the last one ended), then the log; or
/// the Serial tab.
pub fn draw_log(app: &mut App, f: &mut Frame, r: Rect) {
    fill(f, r, P.editor_bg);
    if r.height == 0 || r.width < 10 {
        return;
    }
    if app.prog.serial_tab {
        return draw_serial(app, f, r);
    }
    let s = &app.prog;
    let ascii = crate::theme::ascii();
    let status: Vec<Span<'static>> = match (&s.job, &s.last) {
        (Some(j), _) => {
            let t = j.since.elapsed().as_secs();
            let mut v = vec![Span::styled(format!(" {} {}  ", if ascii { ">" } else { "▶" }, j.what), Style::new().fg(P.editor_fg).bold())];
            if let Some(p) = j.pct {
                let n = 30 * p.min(100) as usize / 100;
                let (on, off) = if ascii { ("#", "-") } else { ("━", "─") };
                v.push(Span::styled(on.repeat(n), Style::new().fg(P.focus_border)));
                v.push(Span::styled(off.repeat(30 - n), Style::new().fg(P.dim)));
                v.push(Span::styled(format!(" {p:>3}%"), Style::new().fg(P.editor_fg)));
            }
            v.push(Span::styled(format!("  {}:{:02}", t / 60, t % 60), Style::new().fg(P.dim)));
            v
        }
        (None, Some((ok, text))) => {
            let (mark, c) = if *ok { (if ascii { "ok" } else { "✓" }, P.git_added) } else { (if ascii { "x" } else { "✗" }, P.error_fg) };
            vec![Span::styled(format!(" {mark} {text}"), Style::new().fg(c))]
        }
        (None, None) => vec![Span::styled(" No job has run yet.", Style::new().fg(P.dim))],
    };
    row(f, Rect::new(r.x, r.y, r.width, 1), P.editor_bg, status);
    let h = r.height.saturating_sub(1) as usize;
    let end = s.log.len().saturating_sub(s.log_off);
    for (k, l) in s.log[end.saturating_sub(h)..end].iter().enumerate() {
        let c = if l.starts_with("$ ") {
            P.focus_border
        } else if l.trim_start().to_lowercase().starts_with("error") {
            P.error_fg
        } else if l.contains("successfully") || l.contains("complete") {
            P.git_added
        } else {
            P.editor_fg
        };
        let y = r.y + 1 + k as u16;
        f.render_widget(Span::styled(format!(" {}", fit(l, r.width.saturating_sub(1) as usize)), Style::new().fg(c)), Rect::new(r.x, y, r.width, 1));
    }
}

