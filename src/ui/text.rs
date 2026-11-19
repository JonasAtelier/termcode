//! A text editor group's body: the text, its rulers and blanks, the minimap and the peek view. Split from `ui`, no behaviour change.

use super::*;

/// Lays out [minimap?][gutter][text][minimap?][blank][scrollbar] and draws the
/// text, its decorations and the peek zone below its hunk.
pub(super) fn text_editor(
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
    let mut out = EdOut { text: area, sb: None, hsb: None, mini: None, mini_img: None, marks_x: None, folds_x: None, zone: None, peek_btns: Vec::new(), peek_body: Rect::default(), peek_max: 0, rows: Vec::new(), caret: None };
    if let Some(ro) = &b.readonly {
        let banner = Rect::new(area.x, area.y, area.width, 1);
        let banner_text = format!(" {}{}{ro}", icons().lock, sp(d.icon_gap));
        row(f, banner, P.widget_bg, vec![Span::styled(banner_text, Style::new().fg(P.git_modified))]);
        area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));
        out.text = area;
    }
    let ic = icons();
    // A hexdump's offsets number its lines already.
    let digits = if b.binary { 0 } else { b.lines.len().to_string().len().max(2) as u16 };
    // Gutter: [pad][number][Git marker][fold button?]text [blank][scrollbar].
    // The chevron follows the Git lane directly; its trailing blank is clickable.
    let (pad, blank) = (d.gap, u16::from(d.comfy));
    let nums = pad + digits;
    let fold_width = if v.fold_ends.iter().any(Option::is_some) { 2 } else { 0 };
    let gutter = nums + 1 + fold_width;
    let base_text = area.width.saturating_sub(gutter + 1 + blank);
    let mw = d.minimap.map_or(0, |m| match d.mini_px {
        // In pixels it is as wide as its scale needs (within bounds).
        Some((cw, _)) => minimap::pixel_width(base_text, area.width, m.max_column, m.min_text, m.scale, cw),
        None => minimap::width(base_text, area.width, m.max_column, m.min_text),
    });
    let left = d.minimap.is_some_and(|m| m.left);
    let gx = area.x + if left { mw } else { 0 };
    let mut text = Rect::new(gx + gutter, area.y, area.width.saturating_sub(gutter + 1 + blank + mw), area.height);
    // The caret occupies a cell after the final character.
    let longest = b.max_display_width().saturating_add(1);
    v.hscroll = if d.wrap { 0 } else { v.hscroll.min(longest.saturating_sub(text.width as usize)) };
    if !d.wrap && longest > text.width as usize && text.width > 1 && area.height > 1 {
        if !d.bottom_border {
            area.height -= 1;
            text.height -= 1;
        }
        out.hsb = Some(horizontal_scrollbar(Rect::new(text.x, text.bottom(), text.width, 1), longest, text.width as usize, v.hscroll));
    }
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
    let brackets = (d.match_brackets && group_focus).then(|| b.bracket_pair(c)).flatten();
    let word = (d.occurrences && group_focus && sel.is_none()).then(|| editor::cursor_word(&b.lines[c.line], c.col).map(str::to_string)).flatten();
    // editor.bracketPairColorization: depth at each line start (plain text has no brackets).
    let depths = (d.bracket_colors && b.syntax_name() != "Plain Text").then(|| b.bracket_depths());
    // Text termcode made (reports, a PDF's text) aligns with spaces, not indents.
    let guides = (d.guides && !b.report).then(|| Guides::new(&b.lines, tab));
    let active = guides.as_ref().and_then(|g| g.active(c.line));
    // The peek zone sits before line `before` when that place is on screen.
    let zone = d.peek.as_ref().filter(|p| p.hunk.peek_before() >= v.scroll && p.hunk.peek_before() <= v.scroll + h).map(|p| {
        let body = p.hunk.base.len() + p.hunk.cur.len();
        (p.hunk.peek_before(), (2 + body as u16).min((area.height / 3).max(4)).min(area.height))
    });
    let (mut y, mut i) = (area.y, v.scroll);
    // The cursor's screen row and the display column that row starts at.
    let mut cursor_at = None;
    let tw = text.width as usize;
    while y < area.bottom() {
        if let Some((before, zh)) = zone {
            if out.zone.is_none() && i == before {
                // The gutter and the text only: a left minimap stays beside it.
                let zr = Rect::new(gx, y, text.right().saturating_sub(gx), zh.min(area.bottom() - y)).intersection(area);
                out.zone = Some(zr);
                if d.wrap {
                    out.rows.extend(std::iter::repeat_n(None, zr.height as usize));
                }
                y += zr.height;
                continue;
            }
        }
        if i >= b.lines.len() {
            break;
        }
        let cur_line = i == c.line && sel.is_none();
        let bg = if cur_line { P.cur_line } else { P.editor_bg };
        let nr_st = Style::new().fg(if i == c.line { P.line_nr_active } else { P.line_nr }).bg(P.editor_bg);
        if digits > 0 {
            f.render_widget(Span::styled(format!("{}{:>w$}", sp(pad), i + 1, w = digits as usize), nr_st), Rect::new(gx, y, nums, 1));
        }
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
        for (a, e) in word.iter().flat_map(|w| editor::find_in([(i, line.as_str())], w, true, true)) {
            overlays.push((a.col, e.col, P.word_hl));
        }
        for p in brackets.iter().flat_map(|&(a, e)| [a, e]).filter(|p| p.line == i) {
            overlays.push((p.col, p.col + 1, P.bracket));
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
        // Display columns of what is drawn over the text: blanks shown, coloured brackets.
        let selected = sel.filter(|(a, e)| a.line <= i && i <= e.line).map(|(a, e)| (if a.line == i { a.col } else { 0 }, if e.line == i { e.col } else { line.len() }));
        // A hexdump's or readelf's columns are spaces, not whitespace to show.
        let ws_mode = if b.binary || b.elf || b.report { editor::Whitespace::None } else { d.whitespace };
        let mut blanks = editor::whitespace_marks(line, ws_mode, selected).into_iter().peekable();
        let (mut ws, mut paint) = (Vec::new(), Vec::new());
        let mut depth = depths.as_ref().map_or(0, |dp| dp[i]);
        let mut col = 0;
        for (at, ch) in line.char_indices() {
            if blanks.peek().is_some_and(|m| m.0 == at) {
                ws.push((col, blanks.next().is_some_and(|m| m.1)));
            }
            if depths.is_some() {
                match ch {
                    '(' | '[' | '{' => {
                        paint.push((col, P.bracket_fg[depth as usize % 3]));
                        depth += 1;
                    }
                    ')' | ']' | '}' if depth == 0 => paint.push((col, P.bracket_bad)),
                    ')' | ']' | '}' => {
                        depth -= 1;
                        paint.push((col, P.bracket_fg[depth as usize % 3]));
                    }
                    _ => {}
                }
            }
            col = editor::advance(col, ch);
        }
        let line_end = col;
        // One screen row, or with word wrap one per wrapped piece.
        let starts = if d.wrap { editor::wrap_starts(line, tw) } else { vec![v.hscroll] };
        let cursor_row = (i == c.line).then(|| editor::wrap_row(&starts, editor::display_col(line, c.col)));
        for (k, &hs) in starts.iter().enumerate() {
            if k > 0 {
                y += 1;
                if y >= area.bottom() {
                    break;
                }
                // A wrapped row keeps the line's git marker, not its number.
                f.render_widget(Span::styled(mark, Style::new().fg(mfg).bg(P.editor_bg)), Rect::new(gx + nums, y, 1, 1));
            }
            let rw = if d.wrap { starts.get(k + 1).map_or(tw, |&n| n - hs).min(tw) } else { tw };
            let last = k + 1 == starts.len();
            let tr = Rect::new(text.x, y, text.width, 1);
            fill(f, tr, bg);
            let l = render_line(line, b.segs(i), &overlays, if last { eol } else { None }, hs, rw, Style::new().fg(P.editor_fg).bg(bg));
            f.render_widget(l, tr);
            let visible = |col: usize| (col >= hs && col - hs < rw).then(|| text.x + (col - hs) as u16);
            if last && v.collapsed.contains(&i) {
                let end = line_end.saturating_sub(hs);
                if end + 2 < tw {
                    f.render_widget(Span::styled(" …", Style::new().fg(P.line_nr).bg(bg)), Rect::new(text.x + end as u16, y, 2, 1));
                }
            }
            if let (Some(g), 0) = (&guides, k) {
                for lvl in 0..g.level(i) {
                    let on = active.is_some_and(|(ak, a, e)| ak == lvl && (a..=e).contains(&i));
                    if let Some(x) = visible(lvl * tab) {
                        mark_blank(f, x, y, ic.guide, if on { P.guide_active } else { P.guide });
                    }
                }
            }
            for &(s, col) in &thin {
                if let Some(x) = visible(editor::display_col(line, s)) {
                    mark_blank(f, x, y, ic.guide, col);
                }
            }
            for &(at, is_tab) in &ws {
                if let Some(x) = visible(at) {
                    mark_blank(f, x, y, if is_tab { ic.ws_tab } else { ic.ws_space }, P.whitespace);
                }
            }
            for &(at, fg) in &paint {
                if let Some(x) = visible(at) {
                    f.buffer_mut()[(x, y)].set_fg(fg);
                }
            }
            // Problems: underlined, text and line in the severity's colour (herdr carries
            // no underline colour, so the text colour is what makes the line red there).
            for &(_, s, e, sev) in d.problems.iter().filter(|p| p.0 == i) {
                let a = editor::display_col(line, s);
                let z = editor::display_col(line, e).max(a + 1);
                let c = P.problem[sev as usize];
                let st = Style::new().fg(c).underline_color(c).add_modifier(Modifier::UNDERLINED);
                for x in (a..z).filter_map(visible) {
                    f.buffer_mut()[(x, y)].set_style(st);
                }
            }
            for p in v.extra.iter().filter(|p| group_focus && p.line == i) {
                if let Some(x) = visible(editor::display_col(line, p.col)) {
                    f.buffer_mut()[(x, y)].set_style(Style::new().add_modifier(Modifier::REVERSED));
                }
            }
            draw_rulers(f, tr, if d.wrap { 0 } else { v.hscroll }, d.rulers, bg);
            if d.wrap {
                out.rows.push(Some((i, hs)));
            }
            if cursor_row == Some(k) {
                cursor_at = Some((y, hs));
            }
        }
        let next = v.shift_line(i, 1);
        i = if next > i { next } else { b.lines.len() };
        y += 1;
    }
    // Rulers run the full height, below the last line too (the peek zone clears its rows).
    for ry in y..area.bottom() {
        draw_rulers(f, Rect::new(text.x, ry, text.width, 1), if d.wrap { 0 } else { v.hscroll }, d.rulers, P.editor_bg);
    }
    if let (Some(zr), Some(pk)) = (out.zone, d.peek.as_ref()) {
        draw_peek(f, zr, gx, gutter, text.x, pk, &b.lines, d, &mut out);
    }
    if editor_focus {
        let x = editor::display_col(&b.lines[c.line], c.col);
        if let Some((cy, hs)) = cursor_at.filter(|&(_, hs)| x >= hs && (d.wrap || x < hs + tw)) {
            // Wrapped, a caret after a full row's last cell stays on that cell.
            out.caret = Some(Position::new(text.x + (x - hs).min(tw.saturating_sub(1)) as u16, cy));
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
        let mini = match d.mini_px {
            Some(cell) => {
                let (mini, img, rows) = minimap_pixels(b, v, mr, &cfg, d, slider, cell);
                out.mini_img = Some((img, rows));
                mini
            }
            None => draw_minimap(f, b, v, mr, &cfg, d, slider),
        };
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
        // Where the slider is when that line shows (the slider counts a page past the end).
        let row_of = |line: usize| usize::from(decor::mark_row(track.height, total + h.saturating_sub(1), h, v.row_of(line), v.row_of(v.scroll), max_off)).min(th - 1);
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
        for &(l, _, _, sev) in d.problems {
            put(row_of(l), 2, ic.lane_right, P.problem[sev as usize]);
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
pub(super) fn draw_minimap(f: &mut Frame, b: &mut Buffer, v: &View, r: Rect, cfg: &minimap::Cfg, d: &Deco, slider: Option<Rgba>) -> Mini {
    // Editor rows, not buffer lines: a folded block is not drawn here either.
    let n = v.row_of(b.lines.len().saturating_sub(1)) + 1;
    let scroll = v.row_of(v.scroll);
    let view = r.height as usize; // the editor shows as many rows as the minimap is tall
    let braille = cfg.render_characters;
    let slots = if braille { 4 } else { 2 };
    let mini = Mini::new(r.height, n, view, slots, cfg.size).follow(v.mini_start, scroll);
    let end = v.line_of(mini.slot_rows(scroll, r.height.saturating_sub(1), slots - 1).end.saturating_sub(1)) + 1;
    // Request every line the minimap shows. The shared frame budget keeps this
    // incremental, and hl_pending schedules redraws until its colours are ready. A large
    // file's minimap takes the colours it has: highlighting it all would keep the CPU
    // busy for many seconds.
    if b.lines.len() <= editor::LARGE_LINES {
        b.highlight_upto(end);
    }
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
        } else if let Some(m) = d.mini_rows.then(|| (l0..=l1).find_map(|l| d.marks.get(l).copied().flatten())).flatten() {
            change_tint(m).over(P.mini_bg)
        } else {
            P.mini_bg
        }
    };
    let ink = |c: Option<Color>, under: Color| c.map_or(under, |c| minimap::ink(c, P.mini_fg_opacity, under));
    for row in 0..r.height {
        let y = r.y + row;
        let span = mini.slot_rows(scroll, row, 0).start..mini.slot_rows(scroll, row, slots - 1).end;
        // Git lane: lit on the slots (dot rows, halves) whose lines changed, beside the
        // code they draw, in the strongest change's colour among them.
        let prio = |m: &Mark| match m {
            Mark::Modified => 3,
            Mark::Added => 2,
            Mark::Deleted => 1,
        };
        let mut lit = [false; 4];
        let mut lane = None;
        for (k, on) in lit.iter_mut().enumerate().take(slots) {
            let m = mini.slot_rows(scroll, row, k).filter_map(|row| d.marks.get(v.line_of(row)).copied().flatten()).max_by_key(prio);
            *on = m.is_some();
            if m.is_some_and(|m| lane.is_none_or(|l: Mark| prio(&m) > prio(&l))) {
                lane = m;
            }
        }
        let lc = match lane {
            Some(Mark::Added) => P.mini_added,
            Some(Mark::Modified) => P.mini_modified,
            Some(Mark::Deleted) => P.mini_deleted,
            None => P.mini_bg,
        };
        let cell = &mut f.buffer_mut()[(r.x, y)];
        cell.set_char(lane_glyph(&lit[..slots], ascii)).set_fg(lc).set_bg(P.mini_bg);
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

/// The minimap as pixels (Kitty), at VS Code's scale (`editor.minimap.scale`): a line
/// 2 × scale px tall, a character scale px wide; the git lane a bar in its first cell; find and selection lines tinted. Its
/// background is clear and its code translucent over the cells' background, where the
/// slider is drawn (its rows and colour returned): scrolling changes cells, not pixels,
/// so the picture is sent again only as the code or the minimap's stretch changes.
/// `(cw, ch)`: a cell's pixels.
pub(super) fn minimap_pixels(b: &mut Buffer, v: &View, r: Rect, cfg: &minimap::Cfg, d: &Deco, slider: Option<Rgba>, (cw, ch): (u16, u16)) -> (Mini, image::RgbaImage, Option<(u16, u16, Color)>) {
    use crate::settingsgraphics::pixel;
    let n = v.row_of(b.lines.len().saturating_sub(1)) + 1;
    let scroll = v.row_of(v.scroll);
    let view = r.height as usize;
    // Lines a cell row holds at 2 × scale px each: a fraction (every line the same height).
    let sc = u32::from(cfg.scale.max(1));
    let per_row = f64::from(ch) / f64::from(2 * sc);
    let mini = Mini::new(r.height, n, view, per_row.floor().max(1.0) as usize, cfg.size).per_row(per_row).follow(v.mini_start, scroll);
    let (w, h) = (u32::from(r.width) * u32::from(cw), u32::from(r.height) * u32::from(ch));
    let mut img = image::RgbaImage::new(w, h);
    let alpha = (f64::from(P.mini_fg_opacity) * minimap::INK).round() as u8;
    let ppl = f64::from(ch) / mini.lines_per_row();
    let start = mini.start(scroll);
    let shown = ((f64::from(h) / ppl).ceil() as usize).min(n.saturating_sub(start));
    if b.lines.len() <= editor::LARGE_LINES {
        b.highlight_upto(v.line_of(start + shown) + 1);
    }
    let sel = v.selection();
    let finds: Vec<usize> = d.find.map_or(Vec::new(), |fd| fd.matches.iter().map(|(a, _)| a.line).collect());
    let tab = editor::tab();
    let text_x = u32::from(cw);
    let mut last_y = None;
    for k in 0..shown {
        let y0 = (k as f64 * ppl) as u32;
        // Squeezed (fill): one line a pixel row at most.
        if last_y == Some(y0) {
            continue;
        }
        last_y = Some(y0);
        let y1 = (((k + 1) as f64 * ppl) as u32).max(y0 + 1).min(h);
        let line = v.line_of(start + k);
        let found = finds.binary_search(&line).is_ok();
        // A found or selected line's tint is its own (opaque); else the cells' shows.
        let bg = if found {
            Some(P.mini_find)
        } else if sel.is_some_and(|(a, e)| a.line <= line && e.line >= line) {
            Some(P.mini_sel)
        } else {
            d.marks.get(line).copied().flatten().filter(|_| d.mini_rows).map(|m| change_tint(m).over(P.mini_bg))
        };
        if let Some(bg) = bg {
            let bgp = pixel(bg);
            for y in y0..y1 {
                for x in text_x..w {
                    img.put_pixel(x, y, bgp);
                }
            }
        }
        // The git lane: a bar in the first cell.
        if let Some(m) = d.marks.get(line).copied().flatten() {
            let c = pixel(match m {
                Mark::Added => P.mini_added,
                Mark::Modified => P.mini_modified,
                Mark::Deleted => P.mini_deleted,
            });
            for y in y0..y1 {
                for x in (text_x.saturating_sub(4))..text_x.saturating_sub(1) {
                    img.put_pixel(x, y, c);
                }
            }
        }
        // Each character its glyph shrunk to scale px by 2 × scale (a block without a
        // font), in its token's colour.
        // A line's pixel rows can be one more than its glyph (cells not a multiple of a
        // line): the glyph at their top, the spare row clear.
        let glyphs = minimap::glyphs(cfg.scale).filter(|g| y1 - y0 >= g.h as u32);
        // A glyph's own coverage is its ink; a block stands for one at INK's share.
        let alpha = if glyphs.is_some() { (f64::from(P.mini_fg_opacity) * minimap::GLYPH_INK).round() as u8 } else { alpha };
        let stride = glyphs.map_or(1, |g| g.w);
        let segs = b.segs(line);
        let mut seg = 0;
        let mut col = 0usize;
        for (at, c) in b.lines[line].char_indices() {
            if col >= cfg.max_column || text_x + col as u32 * sc >= w {
                break;
            }
            let width = if c == '\t' { tab - col % tab } else { 1 };
            if !c.is_whitespace() {
                let fg = segs.map_or(P.editor_fg, |s| {
                    while seg + 1 < s.len() && s[seg + 1].0 <= at {
                        seg += 1;
                    }
                    let f = s[seg].1.foreground;
                    if f.a == 0 { P.editor_fg } else { Color::Rgb(f.r, f.g, f.b) }
                });
                let (r, g, bl) = match fg {
                    Color::Rgb(r, g, b) => (r, g, b),
                    _ => (0xcc, 0xcc, 0xcc),
                };
                let mask = glyphs.and_then(|gl| gl.mask(c));
                let rows = glyphs.map_or(y1, |g| (y0 + g.h as u32).min(y1));
                for y in y0..rows {
                    for (k, x) in (text_x + col as u32 * sc..(text_x + (col as u32 + 1) * sc).min(w)).enumerate() {
                        // How much of the glyph covers this pixel (all of it: a block).
                        let cover = mask.map_or(255, |m| u32::from(m[(y - y0) as usize * stride + k]));
                        if cover == 0 {
                            continue;
                        }
                        let a = (u32::from(alpha) * cover / 255) as u8;
                        let p = match bg {
                            Some(bg) => pixel(minimap::ink(fg, (u32::from(P.mini_fg_opacity) * cover / 255) as u8, bg)),
                            None => image::Rgba([r, g, bl, a]),
                        };
                        img.put_pixel(x, y, p);
                    }
                }
            }
            col += width;
        }
    }
    // The slider: its cell rows, under the picture.
    let slider = slider.map(|sc| {
        let (top, len) = mini.slider(scroll);
        (top, len, sc.over(P.mini_bg))
    });
    (mini, img, slider)
}

/// The git lane's cell for its lit slots: braille dot rows (4 slots), the upper or lower
/// quadrant (2), `|` in ASCII; blank when none changed.
pub(super) fn lane_glyph(lit: &[bool], ascii: bool) -> char {
    match lit {
        l if !l.contains(&true) => ' ',
        _ if ascii => '|',
        [a, b] => match (a, b) {
            (true, true) => '▌',
            (true, false) => '▘',
            _ => '▖',
        },
        l => {
            // Both dot columns of each lit row: 1+4, 2+5, 3+6, 7+8.
            let dots: u32 = l.iter().zip([0x09, 0x12, 0x24, 0xc0]).filter(|(on, _)| **on).map(|(_, d)| d).sum();
            char::from_u32(0x2800 + dots).unwrap_or('|')
        }
    }
}

/// The peek zone: a title row (file, N of M changes, Stage / Revert / ↑ / ↓ / ×),
/// the hunk's index lines on removed-line red and buffer lines on inserted-line
/// green with word highlights, and a bottom border in peekView.border.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_peek(f: &mut Frame, zr: Rect, gx: u16, gutter: u16, tx: u16, pk: &PeekDraw, lines: &[String], d: &Deco, out: &mut EdOut) {
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
            crate::ui::kit::look_at(crate::ui::kit::Look::Primary, r)
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
    /// A cell's pixels when the minimap is a picture (Kitty), as the editor's.
    pub px: Option<(u16, u16)>,
}

/// A change's tint for its minimap row (a diff's): inserted and changed lines as the
/// modified side shows them, removed ones as the original does.
fn change_tint(m: Mark) -> Rgba {
    match m {
        Mark::Added | Mark::Modified => P.diff_ins_text,
        Mark::Deleted => P.diff_del_text,
    }
}
