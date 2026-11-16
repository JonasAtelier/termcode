//! The terminal panel, the Outline and Markdown previews. Split from `ui`, no behaviour change.

use super::*;

/// The Outline in a frame: the active editor's symbols (Go to Symbol's), nested as the
/// breadcrumbs nest them, the one holding the cursor highlighted and followed into view.
/// Its left edge is the width's handle.
pub(super) fn outline_panel(app: &mut App, f: &mut Frame, r: Rect) {
    let focused = app.focus == Focus::Outline;
    let hot = matches!(app.drag, Drag::OutlineSash) || app.hover.is_some_and(|p| p.x == r.x && (r.y..r.bottom()).contains(&p.y));
    let block = Block::bordered()
        .border_set(crate::blocks::border_set())
        .border_style(Style::new().fg(crate::blocks::border(focused)).bg(P.editor_bg))
        .title(Span::styled(" Outline ", Style::new().fg(P.sidebar_title).bold()))
        .style(Style::new().bg(P.sidebar_bg).fg(P.sidebar_fg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if hot {
        for y in r.y..r.bottom() {
            f.buffer_mut()[(r.x, y)].set_fg(P.sash_hover);
        }
    }
    app.ui.outline_sash = Rect::new(r.x, r.y, 1, r.height);
    app.ui.outline = inner;
    let note = |f: &mut Frame, text: &str| {
        f.render_widget(Paragraph::new(text.to_string()).wrap(ratatui::widgets::Wrap { trim: true }).style(Style::new().fg(P.dim)), inner);
    };
    let grp = &app.groups[app.ag.min(app.groups.len().saturating_sub(1))];
    let Some(tab) = grp.tabs.get(grp.active) else { return note(f, " Open a file to see its outline.") };
    let (cursor, folds, key) = (tab.view.cursor, tab.view.fold_ends.clone(), tab.key.clone());
    let Some(Doc::Text(b)) = app.docs.get_mut(&key) else { return note(f, " No outline for this editor.") };
    let syms = b.symbols().to_vec();
    if syms.is_empty() {
        return note(f, " No symbols found in this file.");
    }
    let depths = crate::symbols::depths(&syms, &b.lines, &folds);
    let tree = crate::symbols::tree(&syms, &depths);
    // The rows shown: a folded row hides the symbols nested in it.
    let mut items: Vec<(usize, crate::panel::OutlineItem)> = Vec::new();
    let mut hide: Option<usize> = None;
    for (i, (s, (path, kids))) in syms.iter().zip(&tree).enumerate() {
        let d = depths[i];
        if hide.is_some_and(|h| d > h) {
            continue;
        }
        let open = !(*kids && app.outline_folded.contains(&format!("{key}\n{path}")));
        hide = (!open).then_some(d);
        let col = b.lines[s.line].find(s.name.as_str()).unwrap_or(0);
        items.push((i, crate::panel::OutlineItem { to: editor::Pos::new(s.line, col), path: path.clone(), depth: d, kids: *kids, open }));
    }
    // The cursor's symbol, or the row it is folded into.
    let line = b.clamp(cursor).line;
    let cur = crate::symbols::path_at(&syms, &b.lines, &folds, line).last().and_then(|s| syms.iter().position(|x| x.line == s.line));
    let cur = cur.and_then(|c| {
        let p = &tree[c].0;
        items.iter().rposition(|(i, it)| *i <= c && (*p == it.path || p.starts_with(&format!("{}/", it.path))))
    });
    // Unfocused, the selection follows the cursor; focused, the keys move it.
    if !focused {
        app.outline_sel = cur.map(|k| items[k].1.path.clone());
    }
    let mut sel = app.outline_sel.as_ref().and_then(|p| items.iter().position(|(_, it)| it.path == *p));
    if sel.is_none() && !items.is_empty() {
        sel = cur.or(Some(0));
        app.outline_sel = sel.map(|k| items[k].1.path.clone());
    }
    let h = inner.height as usize;
    // Follow the cursor into another symbol, and the selection while it moves; the wheel
    // scrolls freely meanwhile.
    if cur != app.outline_cur {
        app.outline_cur = cur;
        if let Some(i) = cur.filter(|&i| !focused && (i < app.outline_off || i >= app.outline_off + h)) {
            app.outline_off = i.saturating_sub(h / 2);
        }
    }
    if let Some(i) = sel.filter(|_| focused) {
        if i < app.outline_off {
            app.outline_off = i;
        } else if i >= app.outline_off + h {
            app.outline_off = i + 1 - h;
        }
    }
    app.outline_off = app.outline_off.min(items.len().saturating_sub(h));
    let off = app.outline_off;
    let ic = icons();
    for (k, (i, it)) in items.iter().enumerate().skip(off).take(h) {
        let s = &syms[*i];
        let rr = Rect::new(inner.x, inner.y + (k - off) as u16, inner.width, 1);
        let bg = if focused && Some(k) == sel {
            P.list_active_bg
        } else if Some(k) == cur {
            P.list_inactive_bg
        } else if app.hover.is_some_and(|p| rr.contains(p)) {
            P.list_hover_bg
        } else {
            P.sidebar_bg
        };
        // Indent, the chevron of a row with symbols in it, the kind's icon, the name.
        let kind = crate::symbols::kind_index(s.kind);
        let lead = format!(" {}", "  ".repeat(it.depth));
        let chev = match (it.kids, it.open) {
            (false, _) => " ",
            (true, true) => ic.chev_down,
            (true, false) => ic.chev_right,
        };
        let used = lead.width() + 3;
        let name = fit(&format!(" {}", s.name), (inner.width as usize).saturating_sub(used));
        let spans = vec![
            Span::raw(lead.clone()),
            Span::styled(format!("{chev} "), Style::new().fg(P.icon_fg)),
            Span::styled(ic.symbols[kind], Style::new().fg(P.symbol[kind])),
            Span::styled(name, Style::new().fg(P.sidebar_fg)),
        ];
        row(f, rr, bg, spans);
        if it.kids {
            app.ui.outline_chevs.push((Rect::new(rr.x + lead.width() as u16, rr.y, 2, 1), k));
        }
        app.ui.outline_rows.push((rr, k));
    }
    app.ui.outline_items = items.into_iter().map(|(_, it)| it).collect();
}

/// The integrated terminal in a frame titled Terminal: its top edge is the height's
/// handle (lit while hovered or dragged), its border blue while it has the keys.
pub(super) fn terminal_panel(app: &mut App, f: &mut Frame, r: Rect) {
    let focused = app.focus == Focus::Term;
    let hot = matches!(app.drag, Drag::TermSash) || app.hover.is_some_and(|p| p.y == r.y && (r.x..r.right()).contains(&p.x));
    let block = Block::bordered()
        .border_set(crate::blocks::border_set())
        .border_style(Style::new().fg(crate::blocks::border(focused)).bg(P.editor_bg))
        .title(Span::styled(" Terminal ", Style::new().fg(P.editor_fg).bold()))
        .style(Style::new().bg(P.editor_bg).fg(P.editor_fg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if hot {
        let buf = f.buffer_mut();
        for x in r.x..r.right() {
            if buf[(x, r.y)].fg == crate::blocks::border(focused) {
                buf[(x, r.y)].set_fg(P.sash_hover);
            }
        }
    }
    app.ui.term_sash = Rect::new(r.x, r.y, r.width, 1);
    app.ui.term = inner;
    if let Some(t) = app.term.as_mut() {
        t.resize(inner.height, inner.width);
        t.draw(f.buffer_mut(), inner, (P.editor_fg, P.editor_bg, P.selection), focused);
    }
}

/// A `preview:` tab: the Markdown file rendered, scrolled by its view.
pub(super) fn markdown_preview(app: &mut App, f: &mut Frame, g: usize, rel: &str, body: Rect, gh: &mut GroupHit) {
    let src: Vec<String> = match app.docs.get(&format!("file:{rel}")) {
        Some(Doc::Text(b)) => b.lines.clone(),
        _ => match std::fs::read_to_string(app.root.join(rel)) {
            Ok(t) => t.lines().map(str::to_string).collect(),
            Err(e) => vec![format!("Cannot read {rel}: {e}")],
        },
    };
    let empty = serde_json::Map::new();
    let colors = app.settings.theme.json["colors"].as_object().unwrap_or(&empty);
    let link = crate::theme::lookup(colors, "textLink.foreground").map_or(P.focus_border, |c| c.over(P.editor_bg));
    let text = Rect::new(body.x + 2, body.y, body.width.saturating_sub(5), body.height);
    let lines = crate::mdpreview::render(&src, text.width as usize, link);
    let h = text.height as usize;
    let max = lines.len().saturating_sub(h);
    let grp = &mut app.groups[g];
    let view = &mut grp.tabs[grp.active].view;
    view.scroll = view.scroll.min(max);
    let off = view.scroll;
    let shown: Vec<Line> = lines.iter().skip(off).take(h).cloned().collect();
    f.render_widget(Paragraph::new(shown).style(Style::new().fg(P.editor_fg).bg(P.editor_bg)), text);
    gh.text = text;
    gh.preview_rows = lines.len();
    if lines.len() > h {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        scrollbar(app, f, track, ScrollTarget::Editor(g), lines.len(), h, off, max);
    }
}
