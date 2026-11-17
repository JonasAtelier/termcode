//! Side-by-side diffs: the view, its minimap and overview ruler, word-level marks. Split from `ui`, no behaviour change.

use super::*;

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
/// Each side keeps its own parser state in original file-line order.
pub(crate) type DiffSyntax = Option<(DiffId, [Buffer; 2])>;

/// What a diff view laid out: its scrollbar, its minimap, each side's text area.
pub(crate) type DiffHits = (Option<ScrollHit>, Option<(Rect, Mini)>, Option<[Rect; 2]>, Option<ScrollHit>);

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
    syntax: &mut DiffSyntax,
    find: Option<&Find>,
    bottom_border: bool,
) -> DiffHits {
    if let Some(note) = &d.note {
        *syntax = None;
        f.render_widget(Paragraph::new(note.as_str()).style(Style::new().fg(P.dim)), area);
        return (None, None, None, None);
    }
    let n = d.rows.len();
    v.scroll = v.scroll.min(n.saturating_sub(1));
    if syntax.as_ref().is_none_or(|(id, _)| *id != d.id) {
        let sides = std::array::from_fn(|side| {
            let lines = d.rows.iter().filter_map(|r| if side == 0 { r.left.as_ref() } else { r.right.as_ref() }).map(|(_, text)| text.clone()).collect();
            let path = if side == 0 { d.file.orig.as_deref().unwrap_or(&d.file.path) } else { &d.file.path };
            Buffer::from_lines(path, lines, "diff")
        });
        *syntax = Some((d.id, sides));
    }
    let buffers = &mut syntax.as_mut().unwrap().1;
    let end = v.scroll.saturating_add(area.height as usize).min(n);
    for (side, b) in buffers.iter_mut().enumerate() {
        let mut lines = d.rows[v.scroll..end].iter().filter_map(|r| if side == 0 { r.left.as_ref() } else { r.right.as_ref() }).map(|(n, _)| *n);
        if let Some(first) = lines.next() {
            b.highlight_view(first, lines.next_back().unwrap_or(first) + 1);
        }
    }
    let mut track = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
    let mut area = Rect::new(area.x, area.y, area.width.saturating_sub(1 + blank), area.height);
    // As the editor's gutter: [pad][number][sign]code, pad = `gap` (editorPadding).
    // A patch numbers its lines as its files do.
    let top = d.patch.as_ref().map_or(n, |p| p.nums.iter().flatten().flatten().max().copied().unwrap_or(0));
    let digits = top.to_string().len().max(2) as u16;
    let gw = gap + digits + 1;
    // The minimap takes its columns from the modified side, as in the editor.
    let right_w = area.width.saturating_sub(area.width.saturating_sub(1) / 2 + 1);
    let mw = mini.as_ref().map_or(0, |m| minimap::width(right_w.saturating_sub(gw), area.width, m.cfg.max_column, m.cfg.min_text));
    let text_area = Rect::new(area.x, area.y, area.width - mw, area.height);
    let half = text_area.width.saturating_sub(1) / 2;
    let mut sides = [Rect::new(area.x, area.y, half, area.height), Rect::new(area.x + half + 1, area.y, text_area.width.saturating_sub(half + 1), area.height)];
    // One range across the whole diff keeps both sides aligned while scrolling vertically.
    let tw = sides[0].width.min(sides[1].width).saturating_sub(gw) as usize;
    let longest = buffers.iter_mut().map(Buffer::max_display_width).max().unwrap_or(0).saturating_add(1);
    v.hscroll = v.hscroll.min(longest.saturating_sub(tw));
    let hsb = if longest > tw && tw > 1 && area.height > 1 {
        if !bottom_border {
            area.height -= 1;
            track.height -= 1;
            for side in &mut sides { side.height -= 1; }
        }
        Some(horizontal_scrollbar(Rect::new(area.x + gw, area.bottom(), text_area.width.saturating_sub(gw), 1), longest, tw, v.hscroll))
    } else { None };
    let sep: Vec<Line> = (0..area.height).map(|_| Line::raw("│")).collect();
    f.render_widget(Paragraph::new(sep).style(Style::new().fg(P.group_border)), Rect::new(area.x + half, area.y, 1, area.height));
    // Each side's text, right of its gutter: where clicks and drags select (`panel::diff_pos`).
    let texts = sides.map(|r| Rect::new(r.x + gw.min(r.width), r.y, r.width.saturating_sub(gw), r.height));
    // The selection, on one side (`v.side`), in the editor's selection colour.
    let (sel, sel_bg) = (v.selection(), if focus { P.selection } else { P.sel_inactive });
    for (k, r) in d.rows.iter().skip(v.scroll).take(area.height as usize).enumerate() {
        let y = area.y + k as u16;
        // A patch's heads across both sides: a file's name, a hunk's @@ line, its message.
        if let Some(head) = d.patch.as_ref().and_then(|p| p.heads.get(&(v.scroll + k))) {
            use crate::patch::Head;
            let bar = Rect::new(area.x, y, text_area.width, 1);
            let (text, st) = match head {
                Head::File(name) => (format!(" {name}"), Style::new().fg(P.editor_fg).bg(P.widget_bg).bold()),
                Head::Hunk(h) => (format!(" {h}"), Style::new().fg(P.dim).bg(P.widget_bg)),
                Head::Text(t) => (format!(" {t}"), Style::new().fg(P.editor_fg).bg(P.editor_bg)),
            };
            // Over the line between the sides.
            f.render_widget(ratatui::widgets::Clear, bar);
            row(f, bar, st.bg.unwrap_or(P.editor_bg), vec![Span::styled(fit(&text, bar.width as usize), st)]);
            continue;
        }
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
                    let num = d.patch.as_ref().map_or(Some(n + 1), |p| p.nums.get(v.scroll + k).and_then(|x| x[s]));
                    let num = num.map_or(String::new(), |x| x.to_string());
                    let g = Span::styled(format!("{}{:>w$}{sign}", sp(gap), num, w = digits as usize), Style::new().fg(P.line_nr).bg(bg));
                    f.render_widget(g, Rect::new(sr.x, y, gw.min(sr.width), 1));
                    let tr = Rect::new(sr.x + gw.min(sr.width), y, sr.width.saturating_sub(gw), 1);
                    let word_bg = if s == 0 { P.diff_del_text } else { P.diff_ins_text };
                    let ranges = if s == 0 { &words.0 } else { &words.1 };
                    let mut overlays: Vec<_> = ranges.iter().map(|w| (w.start, w.end, word_bg.over(bg))).collect();
                    if let Some(fd) = find.filter(|fd| fd.diff_side == Some(s as u8)) {
                        for (n, (a, e)) in fd.matches.iter().enumerate().filter(|(_, (a, _))| a.line == v.scroll + k) {
                            overlays.push((a.col, e.col, if fd.cur == Some(n) { P.find_match } else { P.find_other }));
                        }
                    }
                    // Last, so selection wins over diff and find backgrounds.
                    let mut eol = None;
                    let i = v.scroll + k;
                    if let Some((a, b)) = sel.filter(|(a, b)| s as u8 == v.side && (a.line..=b.line).contains(&i)) {
                        let start = if i == a.line { a.col.min(text.len()) } else { 0 };
                        let end = if i == b.line { b.col.min(text.len()) } else { text.len() };
                        overlays.push((start, end, sel_bg));
                        eol = (i < b.line).then_some(sel_bg);
                    }
                    f.render_widget(render_line(text, buffers[s].segs(*n), &overlays, eol, v.hscroll, tr.width as usize, Style::new().fg(P.editor_fg).bg(bg)), tr);
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
            bottom_border: false,
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
            whitespace: editor::Whitespace::None,
            occurrences: false,
            match_brackets: false,
            bracket_colors: false,
            wrap: false,
            peek: None,
            rulers: &[],
            problems: &[],
        };
        // App::hl_pending counts this buffer (by its MiniHit) while it catches up.
        out_mini = Some((mr, draw_minimap(f, m.buf, v, mr, &m.cfg, &deco, ms)));
    }
    let h = track.height as usize;
    if h == 0 {
        return (None, out_mini, Some(texts), hsb);
    }
    let s = decor::slider(track.height, n + h - 1, h, v.scroll, n.saturating_sub(1));
    paint_slider(f, track, s, slider);
    diff_overview(f, &d.rows, v, track);
    (Some(ScrollHit { track, target: ScrollTarget::Editor(0), slider: s, max_off: n.saturating_sub(1), page: h }), out_mini, Some(texts), hsb)
}

/// A stable word span without diff work on the render path. Multiple edits on
/// one line share a span; long lines retain their line highlight only.
pub(super) fn diff_word_spans(old: &str, new: &str) -> (Option<std::ops::Range<usize>>, Option<std::ops::Range<usize>>) {
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
pub(super) fn diff_overview(f: &mut Frame, rows: &[crate::diff::Row], v: &View, track: Rect) {
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

/// How many of Find's toggles (Match Case, Whole Word, Regex) are on: each shows in the
/// query box, two cells wide.
pub(super) fn on_count(fd: &crate::panel::Find) -> u16 {
    [fd.case, fd.word, fd.regex].into_iter().filter(|&on| on).count() as u16
}
