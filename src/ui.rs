//! Drawing, VS Code layout: title bar, activity bar, resizable sidebar, editor
//! groups (tabs, text or side-by-side diff, find widget), status bar, popups.
//! Every clickable area is recorded in `app.ui` for the mouse handler.

use crate::diff::Kind;
use crate::editor::{self, Buffer, View};
use crate::git::Section;
use crate::panel::{base, App, Doc, DiffDoc, FindBtn, Focus, GroupHit, HRow, Hits, ScmAct, Side, SRow, Zone, SIDES};
use crate::theme::{git_color, icons, P};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};
use syntect::highlighting::{FontStyle, Style as SStyle};
use unicode_width::UnicodeWidthStr;

/// Truncate to `w` display cells with an ellipsis.
fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
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
    out.push('…');
    out
}

fn fill(f: &mut Frame, r: Rect, bg: Color) {
    f.buffer_mut().set_style(r, Style::new().bg(bg));
}

/// Draw `spans` left-aligned in a one-row rect over `bg`.
fn row(f: &mut Frame, r: Rect, bg: Color, spans: Vec<Span<'static>>) {
    fill(f, r, bg);
    f.render_widget(Line::from(spans), r);
}

fn width(spans: &[Span]) -> u16 {
    spans.iter().map(|s| s.width() as u16).sum()
}

pub fn draw(app: &mut App, f: &mut Frame) {
    app.ui = Hits::default();
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(P.editor_bg).fg(P.editor_fg)), area);
    if area.height < 5 || area.width < 30 {
        return;
    }
    let title = Rect::new(area.x, area.y, area.width, 1);
    let status = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    title_bar(app, f, title);
    status_bar(app, f, status);
    let act = Rect::new(body.x, body.y, 3, body.height);
    activity_bar(app, f, act);
    let mut x = act.right();
    if app.side_visible {
        let max = body.width.saturating_sub(3 + 1 + 20).max(12);
        let w = app.side_w.clamp(12, max);
        sidebar(app, f, Rect::new(x, body.y, w, body.height));
        x += w;
        let d = Rect::new(x, body.y, 1, body.height);
        app.ui.divider = d;
        let lines: Vec<Line> = (0..d.height).map(|_| Line::raw("│")).collect();
        f.render_widget(Paragraph::new(lines).style(Style::new().fg(P.widget_border).bg(P.sidebar_bg)), d);
        x += 1;
    }
    let ed = Rect::new(x, body.y, body.right().saturating_sub(x), body.height);
    app.ui.editor_area = ed;
    editor_area(app, f, ed);
    if let Some((g, zone)) = app.drop {
        if let Some(gh) = app.ui.groups.get(g) {
            let a = gh.area;
            let r = match zone {
                Zone::Left => Rect::new(a.x, a.y, a.width / 2, a.height),
                Zone::Right => Rect::new(a.x + a.width / 2, a.y, a.width - a.width / 2, a.height),
                Zone::Center => a,
            };
            fill(f, r, P.drop_bg);
        }
    }
    if app.quick.is_some() {
        quick(app, f, area);
    }
    if app.prompt.is_some() {
        prompt(app, f, area);
    }
}

fn title_bar(app: &mut App, f: &mut Frame, r: Rect) {
    let active = app
        .groups
        .get(app.ag)
        .and_then(|g| g.tabs.get(g.active))
        .map(|t| app.tab_title(&t.key));
    let text = match active {
        Some(t) => format!("{t} — {} — herdr-code", app.repo),
        None => format!("{} — herdr-code", app.repo),
    };
    let st = Style::new().fg(P.sidebar_fg).bg(P.title_bg);
    fill(f, r, P.title_bg);
    let t = fit(&text, r.width.saturating_sub(12) as usize);
    let x = r.x + (r.width.saturating_sub(t.width() as u16)) / 2;
    f.render_widget(Span::styled(t, st), Rect::new(x, r.y, r.right().saturating_sub(x), 1));
    let close = format!(" {} Close ", icons().close);
    let w = close.width() as u16;
    app.ui.close = Rect::new(r.right().saturating_sub(w), r.y, w, 1);
    f.render_widget(Span::styled(close, Style::new().fg(Color::White).bg(P.git_deleted)), app.ui.close);
}

fn status_bar(app: &mut App, f: &mut Frame, r: Rect) {
    let st = Style::new().fg(P.status_fg).bg(P.status_bg);
    fill(f, r, P.status_bg);
    let left = vec![
        Span::styled(format!(" {} {} ", icons().branch, app.branch), st),
        Span::styled(format!(" {}", app.msg), st),
    ];
    let mut right = Vec::new();
    if let Some(Doc::Text(b)) = app.groups.get(app.ag).and_then(|g| g.tabs.get(g.active)).and_then(|t| app.docs.get(&t.key)) {
        let v = &app.groups[app.ag].tabs[app.groups[app.ag].active].view;
        let c = b.clamp(v.cursor);
        let col = editor::display_col(&b.lines[c.line], c.col) + 1;
        let mut s = format!("Ln {}, Col {col}", c.line + 1);
        if let Some(sel) = b.selected_text(v) {
            s.push_str(&format!(" ({} selected)", sel.chars().count()));
        }
        let indent = if b.indent == "\t" { "Tab Size: 4".to_string() } else { format!("Spaces: {}", b.indent.len()) };
        let enc = if b.readonly.is_some() && !b.binary { format!("{} Read-only", icons().lock) } else { "UTF-8".into() };
        right.push(Span::styled(
            format!("{s}   {indent}   {enc}   {}   {} ", if b.crlf { "CRLF" } else { "LF" }, b.syntax_name()),
            st,
        ));
    }
    f.render_widget(Line::from(left), r);
    let w = width(&right);
    f.render_widget(Line::from(right), Rect::new(r.right().saturating_sub(w), r.y, w.min(r.width), 1));
}

fn activity_bar(app: &mut App, f: &mut Frame, r: Rect) {
    fill(f, r, P.activity_bg);
    let ic = icons();
    for (i, s) in SIDES.iter().enumerate() {
        let y = r.y + 1 + i as u16 * 2;
        if y >= r.bottom() {
            break;
        }
        let active = *s == app.side && app.side_visible;
        let icon = match s {
            Side::Explorer => ic.files,
            Side::Search => ic.search,
            Side::Scm => ic.scm,
            Side::History => ic.history,
        };
        let fg = if active { P.activity_fg } else { P.activity_inactive };
        let bar = if active { "▎" } else { " " };
        let rr = Rect::new(r.x, y, r.width, 1);
        f.render_widget(
            Line::from(vec![Span::styled(bar, Style::new().fg(P.activity_fg)), Span::styled(icon, Style::new().fg(fg))]),
            rr,
        );
        if *s == Side::Scm && !app.scm_files.is_empty() && y + 1 < r.bottom() {
            let n = app.scm_files.len().min(99).to_string();
            f.render_widget(
                Span::styled(n, Style::new().fg(Color::White).bg(P.status_bg)),
                Rect::new(r.x + 1, y + 1, 2, 1),
            );
        }
        app.ui.activity.push((rr, *s));
    }
}

/// Sidebar title row with right-aligned action icons.
fn side_title(app: &mut App, f: &mut Frame, r: Rect, title: &str) {
    row(f, r, P.sidebar_bg, vec![Span::styled(format!("  {title}"), Style::new().fg(P.sidebar_title))]);
    let rr = Rect::new(r.right().saturating_sub(3), r.y, 2, 1);
    f.render_widget(Span::styled(icons().refresh, Style::new().fg(P.sidebar_fg)), rr);
    app.ui.find_btns.push((rr, FindBtn::Refresh));
}

fn sidebar(app: &mut App, f: &mut Frame, r: Rect) {
    fill(f, r, P.sidebar_bg);
    let title = match app.side {
        Side::Explorer => "EXPLORER",
        Side::Search => "SEARCH",
        Side::Scm => "SOURCE CONTROL",
        Side::History => "HISTORY",
    };
    side_title(app, f, Rect::new(r.x, r.y, r.width, 1), title);
    let mut y = r.y + 1;
    let ic = icons();
    match app.side {
        Side::Explorer => {
            let hdr = format!("{} {}", ic.chev_down, app.repo.to_uppercase());
            row(f, Rect::new(r.x, y, r.width, 1), P.sidebar_bg, vec![Span::styled(hdr, Style::new().fg(P.sidebar_fg).bold())]);
            y += 1;
        }
        Side::Search => {
            let inp = Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1);
            let focused = app.focus == crate::panel::Focus::SearchInput;
            input(f, inp, &app.query, "Search", focused);
            app.ui.side_input = inp;
            let (c, w, rx) = app.s_opts;
            let toggles = [(ic.case, c, FindBtn::SearchCase), (ic.word, w, FindBtn::SearchWord), (ic.regex, rx, FindBtn::SearchRegex)];
            let mut tx = inp.right().saturating_sub(6);
            for (icon, on, b) in toggles {
                let tr = Rect::new(tx, y, 1, 1);
                let st = if on { Style::new().fg(Color::White).bg(P.focus_border) } else { Style::new().fg(P.sidebar_fg).bg(P.input_bg) };
                f.render_widget(Span::styled(icon, st), tr);
                app.ui.find_btns.push((tr, b));
                tx += 2;
            }
            if focused {
                let cx = inp.x + 1 + (app.query.width() as u16).min(inp.width.saturating_sub(8));
                f.set_cursor_position((cx, y));
            }
            y += 1;
            let files = app.search_rows().len() - app.hits.len();
            let summary = if app.hits.is_empty() { String::new() } else { format!("{} results in {files} files", app.hits.len()) };
            row(f, Rect::new(r.x, y, r.width, 1), P.sidebar_bg, vec![Span::styled(format!("  {summary}"), Style::new().fg(P.dim))]);
            y += 1;
        }
        Side::Scm => {
            let lines = app.commit_msg.split('\n').count().clamp(1, 5) as u16;
            let inp = Rect::new(r.x + 1, y, r.width.saturating_sub(2), lines);
            let focused = app.focus == Focus::CommitInput;
            fill(f, inp, P.input_bg);
            if app.commit_msg.is_empty() {
                let ph = format!("Message (Alt+Enter to commit on '{}')", app.branch);
                f.render_widget(Span::styled(fit(&ph, inp.width as usize - 1), Style::new().fg(P.placeholder)), inp);
            } else {
                let shown: Vec<Line> = app
                    .commit_msg
                    .split('\n')
                    .rev()
                    .take(lines as usize)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|l| Line::styled(format!(" {l}"), Style::new().fg(P.input_fg)))
                    .collect();
                f.render_widget(Paragraph::new(shown), inp);
            }
            if focused {
                let last = app.commit_msg.rsplit('\n').next().unwrap_or("");
                let cx = inp.x + 1 + (last.width() as u16).min(inp.width.saturating_sub(2));
                f.set_cursor_position((cx, inp.y + lines - 1));
            }
            app.ui.side_input = inp;
            y += lines;
            let btn = Rect::new(r.x + 1, y, r.width.saturating_sub(2), 1);
            let label = format!("{} Commit", ic.check);
            fill(f, btn, P.button_bg);
            let bx = btn.x + btn.width.saturating_sub(label.width() as u16) / 2;
            f.render_widget(Span::styled(label, Style::new().fg(P.button_fg).bg(P.button_bg)), Rect::new(bx, y, btn.right() - bx, 1));
            app.ui.commit_btn = btn;
            y += 2;
        }
        Side::History => {
            let t = match &app.history_path {
                Some(p) => format!("  {} (Esc: all)", fit(p, r.width as usize - 12)),
                None => "  all commits".into(),
            };
            row(f, Rect::new(r.x, y, r.width, 1), P.sidebar_bg, vec![Span::styled(t, Style::new().fg(P.dim))]);
            y += 1;
        }
    }
    let list = Rect::new(r.x, y, r.width, r.bottom().saturating_sub(y));
    app.ui.side_list = list;
    let s = app.side as usize;
    let n = app.side_count();
    app.sel[s] = app.sel[s].min(n.saturating_sub(1));
    app.off[s] = app.off[s].min(n.saturating_sub(1));
    let hover_row = app.hover.filter(|p| list.contains(*p)).map(|p| app.off[s] + (p.y - list.y) as usize);
    let scm_rows = if app.side == Side::Scm { app.scm_rows() } else { Vec::new() };
    let search_rows = if app.side == Side::Search { app.search_rows() } else { Vec::new() };
    for k in 0..list.height as usize {
        let i = app.off[s] + k;
        if i >= n {
            if i == 0 && app.side == Side::Search && app.query.is_empty() {
                row(f, Rect::new(list.x, list.y, list.width, 1), P.sidebar_bg, vec![Span::styled("  Type and press Enter (F4/Alt+4)", Style::new().fg(P.dim))]);
            }
            break;
        }
        let rr = Rect::new(list.x, list.y + k as u16, list.width, 1);
        let bg = if i == app.sel[s] && app.focus == Focus::Side {
            P.list_active_bg
        } else if i == app.sel[s] {
            P.list_inactive_bg
        } else if hover_row == Some(i) {
            P.list_hover_bg
        } else {
            P.sidebar_bg
        };
        let hot = i == app.sel[s] || hover_row == Some(i);
        match app.side {
            Side::Explorer => explorer_row(app, f, rr, bg, i),
            Side::Search => search_row(app, f, rr, bg, &search_rows[i]),
            Side::Scm => scm_row(app, f, rr, bg, &scm_rows[i], hot),
            Side::History => {
                let c = &app.commits[i];
                row(
                    f,
                    rr,
                    bg,
                    vec![
                        Span::styled(format!(" {} ", c.short), Style::new().fg(P.dim)),
                        Span::styled(fit(&c.subject, (rr.width as usize).saturating_sub(10)), Style::new().fg(P.sidebar_fg)),
                        Span::styled(format!("  {} · {}", c.author, c.date), Style::new().fg(P.dim)),
                    ],
                );
            }
        }
    }
}

/// Right-aligned spans inside a row.
fn right(f: &mut Frame, r: Rect, spans: Vec<Span<'static>>) -> u16 {
    let w = width(&spans).min(r.width);
    f.render_widget(Line::from(spans), Rect::new(r.right() - w, r.y, w, 1));
    r.right() - w
}

fn explorer_row(app: &App, f: &mut Frame, r: Rect, bg: Color, i: usize) {
    let n = &app.tree[i];
    let ic = icons();
    let mark = if n.dir { app.dirty_dirs.get(&n.path).copied() } else { app.marks.get(&n.path).copied() };
    let fg = mark.map_or(P.sidebar_fg, git_color);
    let (chev, icon) = match (n.dir, n.open) {
        (true, true) => (ic.chev_down, ic.folder_open),
        (true, false) => (ic.chev_right, ic.folder),
        _ => (" ", ic.file),
    };
    let indent = "  ".repeat(n.depth);
    let right_spans = match mark {
        Some(c) if n.dir => vec![Span::styled(format!("{} ", ic.dirty), Style::new().fg(git_color(c)))],
        Some(c) => vec![Span::styled(format!("{c} "), Style::new().fg(git_color(c)))],
        None => vec![],
    };
    let rw = width(&right_spans) as usize;
    let lead = format!(" {indent}{chev} {icon} ");
    let name = fit(&n.name, (r.width as usize).saturating_sub(lead.width() + rw + 1));
    row(f, r, bg, vec![Span::styled(lead, Style::new().fg(P.sidebar_fg)), Span::styled(name, Style::new().fg(fg))]);
    if !right_spans.is_empty() {
        right(f, r, right_spans);
    }
}

fn search_row(app: &App, f: &mut Frame, r: Rect, bg: Color, hr: &HRow) {
    let ic = icons();
    match hr {
        HRow::File(p, n) => {
            let dir = p.rsplit_once('/').map_or("", |(d, _)| d);
            row(
                f,
                r,
                bg,
                vec![
                    Span::styled(format!(" {} {} ", ic.chev_down, ic.file), Style::new().fg(P.sidebar_fg)),
                    Span::styled(base(p).to_string(), Style::new().fg(P.sidebar_fg)),
                    Span::styled(format!(" {dir}"), Style::new().fg(P.dim)),
                ],
            );
            right(f, r, vec![Span::styled(format!(" {n} "), Style::new().fg(Color::White).bg(P.list_inactive_bg))]);
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
                    Span::raw("     "),
                    Span::styled(clean(&keep), Style::new().fg(P.sidebar_fg)),
                    Span::styled(clean(m), Style::new().fg(P.sidebar_fg).bg(P.find_other)),
                    Span::styled(clean(z), Style::new().fg(P.sidebar_fg)),
                ],
            );
        }
    }
}

fn scm_row(app: &mut App, f: &mut Frame, r: Rect, bg: Color, sr: &SRow, hot: bool) {
    let ic = icons();
    match sr {
        SRow::Head(sec, n) => {
            let label = if *sec == Section::Staged { "Staged Changes" } else { "Changes" };
            row(f, r, bg, vec![Span::styled(format!(" {} {label}", ic.chev_down), Style::new().fg(P.sidebar_fg).bold())]);
            let acts: Vec<(&str, ScmAct)> = if *sec == Section::Staged {
                vec![(ic.remove, ScmAct::UnstageAll)]
            } else {
                vec![(ic.discard, ScmAct::DiscardAll), (ic.add, ScmAct::StageAll)]
            };
            let mut spans = Vec::new();
            for (icon, _) in &acts {
                spans.push(Span::styled(format!("{icon} "), Style::new().fg(P.sidebar_fg)));
            }
            spans.push(Span::styled(format!(" {n} "), Style::new().fg(Color::White).bg(P.list_inactive_bg)));
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
                    spans.push(Span::styled(format!("{icon} "), Style::new().fg(P.sidebar_fg)));
                }
            }
            spans.push(Span::styled(format!(" {letter} "), Style::new().fg(git_color(letter))));
            let rw = width(&spans) as usize;
            let deleted = letter == 'D';
            let mut name_st = Style::new().fg(git_color(letter));
            if deleted {
                name_st = name_st.add_modifier(Modifier::CROSSED_OUT);
            }
            let avail = (r.width as usize).saturating_sub(rw + 5);
            let name = fit(base(&file.path), avail);
            let dir = fit(&dir, avail.saturating_sub(name.width() + 1));
            row(
                f,
                r,
                bg,
                vec![
                    Span::styled(format!("   {} ", ic.file), Style::new().fg(P.sidebar_fg)),
                    Span::styled(name, name_st),
                    Span::styled(format!(" {dir}"), Style::new().fg(P.dim)),
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

fn input(f: &mut Frame, r: Rect, text: &str, placeholder: &str, focused: bool) {
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

fn editor_area(app: &mut App, f: &mut Frame, r: Rect) {
    let n = app.groups.len().max(1) as u16;
    let gw = r.width.saturating_sub(n - 1) / n;
    let mut x = r.x;
    for g in 0..app.groups.len() {
        let w = if g + 1 == app.groups.len() { r.right().saturating_sub(x) } else { gw };
        let area = Rect::new(x, r.y, w, r.height);
        group(app, f, g, area);
        x += w;
        if g + 1 < app.groups.len() {
            let lines: Vec<Line> = (0..r.height).map(|_| Line::raw("│")).collect();
            f.render_widget(Paragraph::new(lines).style(Style::new().fg(P.widget_border).bg(P.editor_bg)), Rect::new(x, r.y, 1, r.height));
            x += 1;
        }
    }
}

fn group(app: &mut App, f: &mut Frame, g: usize, area: Rect) {
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
    for (i, t) in app.groups[g].tabs.iter().enumerate() {
        let title = app.tab_title(&t.key);
        let dirty = matches!(app.docs.get(&t.key), Some(Doc::Text(b)) if b.dirty());
        let active = i == app.groups[g].active;
        let label = format!(" {} {} ", ic.file, fit(&title, 30));
        let w = label.width() as u16 + 2;
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
        let xr = Rect::new(x + w - 2, tabbar.y, 1, 1);
        let hovered = hover.is_some_and(|p| tr.contains(p));
        let tail = if dirty && !hovered { ic.dirty } else if active || hovered { ic.close } else { " " };
        row(f, tr, bg, vec![Span::styled(label, st), Span::styled(format!("{tail} "), Style::new().fg(fg).bg(bg))]);
        gh.tabs.push((Rect::new(x, tabbar.y, w - 2, 1), xr));
        x += w + 1;
    }
    if app.groups.len() < crate::panel::MAX_GROUPS && !app.groups[g].tabs.is_empty() {
        f.render_widget(Span::styled(ic.split, Style::new().fg(P.sidebar_fg)), split);
        app.ui.split_btns.push((split, g));
    }
    let body = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
    let active = app.groups[g].active;
    let Some(key) = app.groups[g].tabs.get(active).map(|t| t.key.clone()) else {
        let lines = vec![
            Line::raw(""),
            Line::styled("Open a file from the Explorer", Style::new().fg(P.dim)),
            Line::styled("Quick Open          Ctrl+P", Style::new().fg(P.dim)),
            Line::styled("Search in files     F4 / Alt+4", Style::new().fg(P.dim)),
            Line::styled("Source Control      F2 / Alt+2", Style::new().fg(P.dim)),
            Line::styled("Drag a file to an edge to split", Style::new().fg(P.dim)),
        ];
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), body);
        gh.text = body;
        app.ui.groups.push(gh);
        return;
    };
    let editor_focus = focused_group && app.focus == Focus::Editor;
    match app.docs.get_mut(&key) {
        None => {
            f.render_widget(Paragraph::new("Loading…").style(Style::new().fg(P.dim)), body);
            gh.text = body;
        }
        Some(Doc::Diff(d)) => {
            let view = &mut app.groups[g].tabs[active].view;
            diff_view(f, d, view, body);
            gh.text = body;
        }
        Some(Doc::Text(b)) => {
            let view = &mut app.groups[g].tabs[active].view;
            let find = if focused_group { app.find.as_ref() } else { None };
            gh.text = text_editor(f, b, view, body, focused_group, editor_focus, find);
        }
    }
    app.ui.groups.push(gh);
    if focused_group && app.find.is_some() {
        let text = app.ui.groups[g].text;
        find_widget(app, f, text);
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
fn render_line(
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

/// Returns the text rect (right of the gutter).
fn text_editor(
    f: &mut Frame,
    b: &mut Buffer,
    v: &mut View,
    area: Rect,
    group_focus: bool,
    editor_focus: bool,
    find: Option<&crate::panel::Find>,
) -> Rect {
    b.clamp_view(v);
    let mut area = area;
    if let Some(ro) = &b.readonly {
        let banner = Rect::new(area.x, area.y, area.width, 1);
        row(f, banner, P.widget_bg, vec![Span::styled(format!(" {} {ro}", icons().lock), Style::new().fg(P.git_modified))]);
        area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
        if b.binary {
            return area;
        }
    }
    let digits = b.lines.len().to_string().len().max(3) as u16;
    let gutter = digits + 2;
    let text = Rect::new(area.x + gutter, area.y, area.width.saturating_sub(gutter), area.height);
    let h = area.height as usize;
    b.highlight_upto(v.scroll + h);
    let c = b.clamp(v.cursor);
    let sel = v.selection();
    let sel_bg = if group_focus { P.selection } else { P.sel_inactive };
    for k in 0..h {
        let i = v.scroll + k;
        if i >= b.lines.len() {
            break;
        }
        let y = area.y + k as u16;
        let cur_line = i == c.line && sel.is_none();
        let bg = if cur_line { P.cur_line } else { P.editor_bg };
        let nr_st = Style::new().fg(if i == c.line { P.line_nr_active } else { P.line_nr }).bg(P.editor_bg);
        f.render_widget(Span::styled(format!("{:>w$}  ", i + 1, w = digits as usize), nr_st), Rect::new(area.x, y, gutter, 1));
        let line = &b.lines[i];
        let mut overlays: Vec<(usize, usize, Color)> = Vec::new();
        if let Some(fd) = find {
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
    }
    if editor_focus {
        let x = editor::display_col(&b.lines[c.line], c.col);
        if c.line >= v.scroll && c.line < v.scroll + h && x >= v.hscroll && x < v.hscroll + text.width as usize {
            f.set_cursor_position((text.x + (x - v.hscroll) as u16, area.y + (c.line - v.scroll) as u16));
        }
    }
    text
}

fn diff_view(f: &mut Frame, d: &DiffDoc, v: &mut View, area: Rect) {
    if let Some(note) = &d.note {
        f.render_widget(Paragraph::new(note.as_str()).style(Style::new().fg(P.dim)), area);
        return;
    }
    v.scroll = v.scroll.min(d.rows.len().saturating_sub(1));
    let half = area.width.saturating_sub(1) / 2;
    let sides = [Rect::new(area.x, area.y, half, area.height), Rect::new(area.x + half + 1, area.y, area.width - half - 1, area.height)];
    let sep: Vec<Line> = (0..area.height).map(|_| Line::raw("│")).collect();
    f.render_widget(Paragraph::new(sep).style(Style::new().fg(P.widget_border)), Rect::new(area.x + half, area.y, 1, area.height));
    let digits = d.rows.len().to_string().len().max(3) as u16;
    for (k, r) in d.rows.iter().skip(v.scroll).take(area.height as usize).enumerate() {
        let y = area.y + k as u16;
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
                        (true, 0) => P.diff_del_bg,
                        _ => P.diff_ins_bg,
                    };
                    let sign = match (changed, s) {
                        (false, _) => " ",
                        (true, 0) => "-",
                        _ => "+",
                    };
                    fill(f, sr, bg);
                    let g = Span::styled(format!("{:>w$} {sign}", n + 1, w = digits as usize), Style::new().fg(P.line_nr).bg(bg));
                    let gw = digits + 2;
                    f.render_widget(g, Rect::new(sr.x, y, gw.min(sr.width), 1));
                    let tr = Rect::new(sr.x + gw.min(sr.width), y, sr.width.saturating_sub(gw), 1);
                    f.render_widget(render_line(text, None, &[], None, v.hscroll, tr.width as usize, Style::new().fg(P.editor_fg).bg(bg)), tr);
                }
            }
        }
    }
}

fn find_widget(app: &mut App, f: &mut Frame, text: Rect) {
    let Some(fd) = app.find.as_ref() else { return };
    let ic = icons();
    let rows = if fd.replace.is_some() { 2 } else { 1 };
    let w = 52.min(text.width);
    if w < 30 || text.height < 3 {
        return;
    }
    let r = Rect::new(text.right() - w, text.y, w, rows);
    fill(f, r, P.widget_bg);
    let iw = w - 26;
    // Row 1: [>] [query  Aa ab] "N of M" [↑][↓][×]
    let toggle = Rect::new(r.x, r.y, 1, rows);
    f.render_widget(
        Span::styled(if fd.replace.is_some() { ic.chev_down } else { ic.chev_right }, Style::new().fg(P.sidebar_fg)),
        Rect::new(toggle.x, r.y, 1, 1),
    );
    let q = Rect::new(r.x + 2, r.y, iw, 1);
    input(f, q, &fd.query, "Find", app.focus == Focus::Find && fd.field == 0);
    let mut btns = vec![(toggle, FindBtn::ToggleReplace)];
    for (k, (icon, on, b)) in [(ic.case, fd.case, FindBtn::Case), (ic.word, fd.word, FindBtn::Word)].into_iter().enumerate() {
        let tr = Rect::new(q.right() - 4 + 2 * k as u16, r.y, 1, 1);
        let st = if on { Style::new().fg(Color::White).bg(P.focus_border) } else { Style::new().fg(P.sidebar_fg).bg(P.input_bg) };
        f.render_widget(Span::styled(icon, st), tr);
        btns.push((tr, b));
    }
    let count = match (fd.matches.len(), fd.cur) {
        (0, _) if !fd.query.is_empty() => "No results".to_string(),
        (0, _) => String::new(),
        (n, Some(c)) => format!("{} of {n}", c + 1),
        (n, None) => format!("? of {n}"),
    };
    f.render_widget(Span::styled(format!(" {count}"), Style::new().fg(P.sidebar_fg)), Rect::new(q.right(), r.y, 13, 1));
    let bx = r.right() - 7;
    for (k, (icon, b)) in [(ic.up, FindBtn::Prev), (ic.down, FindBtn::Next), (ic.close, FindBtn::Close)].into_iter().enumerate() {
        let br = Rect::new(bx + 2 * k as u16, r.y, 1, 1);
        f.render_widget(Span::styled(icon, Style::new().fg(P.sidebar_fg)), br);
        btns.push((br, b));
    }
    app.ui.find_inputs.push((q, 0));
    if let Some(rep) = &fd.replace {
        let rq = Rect::new(r.x + 2, r.y + 1, iw, 1);
        input(f, rq, rep, "Replace", app.focus == Focus::Find && fd.field == 1);
        app.ui.find_inputs.push((rq, 1));
        for (k, (icon, b)) in [(ic.replace, FindBtn::ReplaceOne), (ic.replace_all, FindBtn::ReplaceAll)].into_iter().enumerate() {
            let br = Rect::new(rq.right() + 2 + 2 * k as u16, rq.y, 1, 1);
            f.render_widget(Span::styled(icon, Style::new().fg(P.sidebar_fg)), br);
            btns.push((br, b));
        }
    }
    if app.focus == Focus::Find {
        let (field, s) = if fd.field == 1 { (1, fd.replace.as_deref().unwrap_or("")) } else { (0, fd.query.as_str()) };
        let cx = r.x + 3 + (s.width() as u16).min(iw.saturating_sub(6));
        f.set_cursor_position((cx, r.y + field));
    }
    app.ui.find_btns.extend(btns);
}

fn centered(area: Rect, w: u16, h: u16, top: bool) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    let y = if top { area.y + 1 } else { area.y + (area.height - h) / 2 };
    Rect::new(area.x + (area.width - w) / 2, y, w, h)
}

fn quick(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(q) = app.quick.as_mut() else { return };
    let pop = centered(area, (area.width * 6 / 10).max(40), (area.height * 6 / 10).max(6), true);
    f.render_widget(Clear, pop);
    fill(f, pop, P.widget_bg);
    let inp = Rect::new(pop.x + 1, pop.y, pop.width - 2, 1);
    input(f, inp, &q.query, "Search files by name", true);
    f.set_cursor_position((inp.x + 1 + (q.query.width() as u16).min(inp.width - 2), inp.y));
    let list = Rect::new(pop.x, pop.y + 1, pop.width, pop.height - 1);
    let lh = list.height.max(1) as usize;
    if q.sel < q.off {
        q.off = q.sel;
    } else if q.sel >= q.off + lh {
        q.off = q.sel + 1 - lh;
    }
    for (k, item) in q.items.iter().skip(q.off).take(lh).enumerate() {
        let rr = Rect::new(list.x, list.y + k as u16, list.width, 1);
        let bg = if q.off + k == q.sel { P.list_active_bg } else { P.widget_bg };
        let dir = item.rsplit_once('/').map_or("", |(d, _)| d);
        row(
            f,
            rr,
            bg,
            vec![
                Span::styled(format!(" {} ", icons().file), Style::new().fg(P.sidebar_fg)),
                Span::styled(base(item).to_string(), Style::new().fg(P.sidebar_fg)),
                Span::styled(format!("  {dir}"), Style::new().fg(P.dim)),
            ],
        );
    }
    app.ui.quick = pop;
    app.ui.quick_list = list;
}

fn prompt(app: &mut App, f: &mut Frame, area: Rect) {
    let Some(p) = app.prompt.as_ref() else { return };
    let w = p.text.iter().map(|t| t.width()).max().unwrap_or(20).max(36) as u16 + 4;
    let h = p.text.len() as u16 + 4;
    let r = centered(area, w, h, false);
    f.render_widget(Clear, r);
    fill(f, r, P.widget_bg);
    let lines: Vec<Line> = p.text.iter().enumerate().map(|(k, t)| {
        let st = if k == 0 { Style::new().fg(Color::White).bold() } else { Style::new().fg(P.sidebar_fg) };
        Line::styled(format!(" {t}"), st)
    }).collect();
    f.render_widget(Paragraph::new(lines), Rect::new(r.x + 1, r.y + 1, r.width - 2, p.text.len() as u16));
    let mut x = r.right() - 1;
    let y = r.bottom() - 2;
    let mut btns = Vec::new();
    for (i, (label, _)) in p.buttons.iter().enumerate().rev() {
        let t = format!(" {label} ");
        let bw = t.width() as u16;
        x = x.saturating_sub(bw + 1);
        let bg = if i == p.sel { P.button_bg } else { P.input_bg };
        let br = Rect::new(x, y, bw, 1);
        f.render_widget(Span::styled(t, Style::new().fg(Color::White).bg(bg)), br);
        btns.push((br, i));
    }
    app.ui.prompt_btns = btns;
}
