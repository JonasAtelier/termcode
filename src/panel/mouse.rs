//! `App`: the mouse.

use super::*;

impl App {
    // ---- mouse ---------------------------------------------------------------------------

    pub fn on_mouse(&mut self, m: MouseEvent) -> bool {
        if self.settings_dlg.is_some() {
            return crate::settingsui::mouse(self, m);
        }
        if self.prompt.is_none() && self.quick.is_none() {
            if let Some(redraw) = self.term_mouse(m) {
                return redraw;
            }
        }
        let p = Position::new(m.column, m.row);
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.last_click.is_some_and(|(t, q)| t.elapsed() < DOUBLE_CLICK && q == p);
                self.last_click = Some((Instant::now(), p));
                self.clicks = if double { self.clicks.saturating_add(1) } else { 1 };
                self.click(p, double, shift);
            }
            // With a prompt open, a drag that started before it must not drop anything.
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) if self.prompt.is_some() => {
                self.drag = Drag::None;
                self.drop = None;
            }
            MouseEventKind::Up(MouseButton::Left) if matches!(self.drag, Drag::Close) => {
                self.hover = Some(p);
                self.drag = if self.ui.close.contains(p) { Drag::ClosePending } else { Drag::None };
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(p),
            MouseEventKind::Up(MouseButton::Left) => self.release(),
            // Horizontal input stays horizontal even at either end of the range.
            MouseEventKind::ScrollUp if shift && self.hwheel(p, -6) => {}
            MouseEventKind::ScrollDown if shift && self.hwheel(p, 6) => {}
            MouseEventKind::ScrollLeft => return self.hwheel(p, -6),
            MouseEventKind::ScrollRight => return self.hwheel(p, 6),
            MouseEventKind::ScrollUp => return self.wheel(p, -3),
            MouseEventKind::ScrollDown => return self.wheel(p, 3),
            // Redraw only when what the frame shows for the pointer changes.
            MouseEventKind::Moved => {
                let released = matches!(self.drag, Drag::Close);
                if released { self.drag = Drag::None; }
                let old = self.hover.replace(p);
                let redraw = if hover_keyed() {
                    old.map(|q| self.hover_key(q)) != Some(self.hover_key(p))
                } else {
                    let watched = |q: Position| self.hover_key(q) != HoverKey::default();
                    old != Some(p) && (watched(p) || old.is_some_and(watched))
                };
                let tip = self.track_tip(p);
                return crate::scm::moved(self, p) || redraw || released || tip;
            }
            _ => return false,
        }
        true
    }

    /// Start the tooltip timer on a cut Explorer name, keep it while the pointer stays on
    /// that row; true when a shown tooltip goes.
    pub(super) fn track_tip(&mut self, p: Position) -> bool {
        if self.tip.as_ref().is_some_and(|t| t.shown) && self.ui.tip_ext.contains(p) {
            return false; // along the grown name: still that row
        }
        let hit = self.ui.tips.iter().rev().find(|(r, _, _)| r.contains(p)).cloned();
        if let (Some(t), Some((r, _, _))) = (&self.tip, &hit) {
            if t.rect == *r {
                return false;
            }
        }
        let was = self.tip.as_ref().is_some_and(|t| t.shown);
        self.tip = hit.map(|(rect, text, x)| Tip { rect, text, x, since: Instant::now(), shown: false });
        was
    }

    /// Everything a frame draws differently under the pointer: the sidebar row (and a
    /// sticky one), a GRAPH or TIMELINE row, a tab, a scrollbar, a minimap, a peek
    /// button, a commit-dialog splitter.
    pub(super) fn hover_key(&self, p: Position) -> HoverKey {
        let s = self.side as usize;
        let side = self.ui.side_list.contains(p);
        let g = &self.gitui.hits;
        HoverKey {
            close: self.ui.close.contains(p),
            row: side.then(|| self.ui.side_item(p.y, self.off[s])).flatten(),
            sticky: side.then(|| self.ui.sticky_at(p.y)).flatten(),
            git_row: (g.graph.contains(p) || g.timeline.contains(p) || g.refs.contains(p)).then_some(p.y),
            tab: self.ui.groups.iter().enumerate().find_map(|(gi, gh)| gh.tabs.iter().position(|(t, x)| t.contains(p) || x.contains(p)).map(|i| (gi, i))),
            fold: self.ui.groups.iter().enumerate().find_map(|(g, gh)| {
                (gh.folds_x.is_some_and(|x| (x..gh.text.x).contains(&p.x)) && (gh.text.y..gh.text.bottom()).contains(&p.y)).then_some((g, p.y))
            }),
            scrollbar: self.ui.scrollbars.iter().position(|sb| sb.track.contains(p)).or_else(|| {
                self.gitui.dlg.as_ref()?.hits.hscroll.filter(|sb| sb.track.contains(p)).map(|_| usize::MAX)
            }),
            minimap: self.ui.minimaps.iter().position(|m| m.area.contains(p)),
            peek: self.ui.peek_btns.iter().position(|(r, _)| r.contains(p)).or_else(|| self.ui.peek_zone.contains(p).then_some(usize::MAX)),
            sash: self.gitui.dlg.as_ref().and_then(|d| d.hits.sashes.iter().position(|r| r.contains(p))).or_else(|| {
                g.areas.iter().any(|(r, h)| matches!(h, crate::scm::Hit::GraphSash | crate::scm::Hit::TimelineSash) && r.contains(p)).then_some(usize::MAX)
            }).or_else(|| self.ui.term_sash.contains(p).then_some(usize::MAX - 1))
            .or_else(|| self.ui.outline_sash.contains(p).then_some(usize::MAX - 2)),
            outline: self.ui.outline.contains(p).then_some(p.y),
            gh: self.gh.hits.area.contains(p).then_some((p.x, p.y)),
            activity: self.ui.activity.iter().position(|(r, _)| r.contains(p)).or_else(|| self.ui.settings_btn.contains(p).then_some(SIDES.len())),
            button: match self.gitui.dlg.as_ref() {
                Some(d) => d.hits.btns.iter().position(|(r, _)| r.contains(p)),
                None => self.ui.commit_btn.contains(p).then_some(usize::MAX).or_else(|| {
                    g.areas.iter().position(|(r, h)| matches!(h, crate::scm::Hit::CardCopy | crate::scm::Hit::CardOpen) && r.contains(p))
                }),
            },
            status: self.ui.status_bar.contains(p).then(|| {
                let on_bar = |r: &Rect| r.y == self.ui.status_bar.y && r.contains(p);
                g.cmds.iter().map(|(r, _)| r).chain(&self.ui.status_items).find(|r| on_bar(r)).map(|r| r.x)
            }).flatten(),
        }
    }

    pub(super) fn click(&mut self, p: Position, double: bool, shift: bool) {
        self.activity_lit = self.ui.activity.iter().any(|(r, _)| r.contains(p));
        if self.ui.notice_close.contains(p) {
            self.settings_err_closed = self.settings_err.clone();
            return;
        }
        if self.ui.status_line.contains(p) {
            return self.run_command(Cmd::GotoLine);
        }
        // A click in the name being edited places the caret (a second selects the word,
        // a third the whole name, a drag selects); anywhere else accepts the name first.
        let clicks = self.clicks;
        if let Some(r) = self.rename.as_mut() {
            if self.ui.rename.contains(p) || self.ui.rename_field.0.contains(p) {
                let field = self.ui.rename_field;
                if field.0.contains(p) {
                    let at = r.caret_at(field, p.x);
                    match (clicks, editor::word_at(&r.text, at)) {
                        (n, _) if n >= 3 => (r.anchor, r.cur) = (Some(0), r.text.len()),
                        (2, Some((a, b))) => (r.anchor, r.cur) = (Some(a), b),
                        _ => {
                            r.go(at, shift);
                            self.drag = Drag::Rename;
                        }
                    }
                }
                return;
            }
            self.commit_rename();
        }
        // Every press starts afresh: a release the terminal never reported (outside
        // the pane, a lost event) must not leave a sash, tab or file drag behind.
        self.drag = Drag::None;
        self.drop = None;
        crate::scm::drop_drags(self);
        if let Some(pr) = self.prompt.as_ref() {
            if let Some(&(_, i)) = self.ui.prompt_btns.iter().find(|(r, _)| r.contains(p)) {
                let act = pr.buttons[i].1.clone();
                self.run_prompt(act);
            }
            return;
        }
        if let Some(q) = self.quick.as_ref() {
            if let Some(sb) = self.ui.scrollbars.iter().find(|s| s.target == ScrollTarget::Quick && s.track.contains(p)).copied() {
                return self.scrollbar_click(sb, p);
            }
            if self.ui.quick_list.contains(p) {
                let i = q.off + (p.y - self.ui.quick_list.y) as usize;
                self.quick_pick(i);
            } else if !self.ui.quick.contains(p) {
                self.quick = None;
            }
            return;
        }
        if crate::scm::click(self, p, double) || crate::github::click(self, p, double) {
            return;
        }
        if self.ui.close.contains(p) {
            self.hover = Some(p);
            self.drag = Drag::Close;
            return;
        }
        if self.ui.settings_btn.contains(p) {
            return crate::settingsui::open(self);
        }
        if let Some(g) = self.ui.groups.iter().position(|gh| gh.crumbs.contains(p)) {
            self.ag = g;
            self.focus = Focus::Editor;
            return self.open_goto("@");
        }
        if let Some(&(_, g, side)) = self.ui.welcome.iter().find(|(r, _, _)| r.contains(p)) {
            self.ag = g;
            if let Some(side) = side {
                self.show_side(side, side == Side::Search);
            } else {
                self.open_quick();
            }
            return;
        }
        if let Some(&(_, s)) = self.ui.activity.iter().find(|(r, _)| r.contains(p)) {
            if s == self.side && self.side_visible {
                self.side_visible = false; // clicking the active view hides the sidebar
                self.focus = Focus::Editor;
                self.save_layout();
            } else {
                self.show_side(s, s == Side::Search);
            }
            return;
        }
        if self.ui.divider.contains(p) {
            self.drag = Drag::Divider;
            return;
        }
        if self.ui.outline_btns.iter().any(|r| r.contains(p)) {
            return self.run_command(Cmd::ToggleOutline);
        }
        if self.ui.outline_sash.contains(p) {
            self.drag = Drag::OutlineSash;
            return;
        }
        if self.ui.outline.contains(p) {
            return self.outline_click(p, double);
        }
        if self.ui.term_btns.iter().any(|r| r.contains(p)) {
            return if self.term_open { self.hide_terminal() } else { self.toggle_terminal() };
        }
        if self.ui.term_sash.contains(p) {
            self.drag = Drag::TermSash;
            return;
        }
        if let Some(sb) = self.ui.scrollbars.iter().find(|s| s.track.contains(p)).copied() {
            return self.scrollbar_click(sb, p);
        }
        if let Some(&(_, b)) = self.ui.peek_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.peek_button(b);
        }
        if self.ui.peek_zone.contains(p) {
            self.focus = Focus::Editor;
            return;
        }
        if let Some(m) = self.ui.minimaps.iter().find(|m| m.area.contains(p)).copied() {
            return self.minimap_click(m, p);
        }
        if let Some(&(_, b)) = self.ui.find_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.find_button(b);
        }
        if let Some(&(_, field)) = self.ui.find_inputs.iter().find(|(r, _)| r.contains(p)) {
            if let Some(f) = self.find.as_mut() {
                // A double-click selects the field's whole text.
                (f.field, f.all) = (field, double);
            }
            self.focus = Focus::Find;
            return;
        }
        if self.ui.side_input.contains(p) {
            // A double-click selects the whole query.
            (self.focus, self.query_all) = (Focus::SearchInput, double && !self.query.is_empty());
            return;
        }
        if self.ui.commit_btn.contains(p) {
            return self.commit();
        }
        if let Some(&(_, a)) = self.ui.scm_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.scm_action(a);
        }
        if let Some(&(_, g)) = self.ui.split_btns.iter().find(|(r, _)| r.contains(p)) {
            return self.split_right(g);
        }
        if let Some(&(_, i)) = self.ui.search_folds.iter().rev().find(|(r, _)| r.contains(p)) {
            return self.toggle_search_group(i);
        }
        if let Some(&(_, i)) = self.ui.sticky_segs.iter().find(|(r, _)| r.contains(p)) {
            return self.reveal_pinned(i);
        }
        if let Some(i) = self.ui.sticky_at(p.y).filter(|_| self.ui.side_list.contains(p)) {
            return self.reveal_pinned(i);
        }
        if self.ui.side_list.contains(p) {
            let s = self.side as usize;
            // A blank row between items selects nothing.
            let Some(i) = self.ui.side_item(p.y, self.off[s]) else { return };
            if i < self.side_count() {
                self.sel[s] = i;
                self.focus = Focus::Side;
                if self.side == Side::Explorer {
                    if let Some(n) = self.tree.get(i).filter(|n| !n.dir) {
                        self.drag = Drag::File { rel: n.path.clone(), from: p, active: false };
                    }
                }
                // Double-clicking a name edits it in place (the first click opened the file
                // or folded the folder).
                if double && self.side == Side::Explorer && i < self.tree.len() {
                    self.drag = Drag::None;
                    self.last_click = None;
                    return self.start_rename(i);
                }
                self.activate_side(i, double);
            }
            return;
        }
        for g in 0..self.ui.groups.len() {
            let gh = self.ui.groups[g].clone();
            if let Some(i) = gh.tabs.iter().position(|(_, x)| x.contains(p)) {
                return self.close_tab(g, i, false);
            }
            if let Some(i) = gh.tabs.iter().position(|(r, _)| r.contains(p)) {
                self.groups[g].active = i;
                self.ag = g;
                self.focus = Focus::Editor;
                if double {
                    self.toggle_keep_open(g, i);
                    // A third click starts over: another double-click toggles it back.
                    self.last_click = None;
                }
                self.drag = Drag::Tab { g, i, from: p, active: false };
                return self.find_refresh();
            }
            if gh.area.contains(p) {
                self.ag = g;
                self.focus = Focus::Editor;
                if gh.folds_x.is_some_and(|x| (x..gh.text.x).contains(&p.x)) && (gh.text.y..gh.text.bottom()).contains(&p.y) {
                    if let Some(line) = self.line_at(g, p.y) {
                        if !self.ed().is_some_and(|(_, t)| t.view.fold_ends.get(line).is_some_and(Option::is_some)) { return; }
                        let cmd = if self.ed().is_some_and(|(_, t)| t.view.collapsed.contains(&line)) { Cmd::Unfold } else { Cmd::Fold };
                        return self.fold_command(cmd, Some(line));
                    }
                }
                // A quick-diff marker opens the peek on its change (VS Code).
                if gh.marks_x == Some(p.x) {
                    if let Some(line) = self.line_at(g, p.y) {
                        let marked = self.groups[g].tabs.get(self.groups[g].active).and_then(|t| self.qdiff.get(&t.key)).and_then(|q| q.marks.get(line).copied().flatten());
                        if marked.is_some() {
                            return self.peek_at_line(g, line);
                        }
                    }
                }
                if !gh.tabbar.contains(p) {
                    if let Some(texts) = gh.diff_text {
                        return self.diff_click(g, p, texts, shift);
                    }
                    let pos = self.pos_at(g, p);
                    if let (Some(pos), Some((b, tab))) = (pos, self.ed()) {
                        if double {
                            b.select_word(&mut tab.view, pos);
                        } else {
                            b.move_to(&mut tab.view, pos, shift);
                            tab.view.want_x = None;
                        }
                        self.drag = Drag::Select(g);
                    }
                }
                return self.find_refresh();
            }
        }
    }

    /// A click on the minimap: on the slider it starts a drag, elsewhere it
    /// scrolls so the clicked line is centred (VS Code).
    pub(super) fn minimap_click(&mut self, m: MiniHit, p: Position) {
        let row = p.y - m.area.y;
        let (top, len) = m.mini.slider(m.scroll);
        self.ag = m.group;
        self.focus = Focus::Editor;
        if (top..top + len).contains(&row) {
            self.drag = Drag::Minimap(m, row - top);
        } else {
            self.set_scroll(ScrollTarget::Editor(m.group), m.mini.click_scroll(m.scroll, row));
            // Keep dragging from here, as VS Code does after a jump.
            let view = &self.groups[m.group].tabs[self.groups[m.group].active].view;
            let (top, _) = m.mini.slider(view.row_of(view.scroll));
            self.drag = Drag::Minimap(m, row.saturating_sub(top));
        }
    }

    pub(super) fn scrollbar_click(&mut self, sb: ScrollHit, p: Position) {
        let cur = self.scroll_of(sb.target);
        let at = if matches!(sb.target, ScrollTarget::EditorHorizontal(_)) { p.x - sb.track.x } else { p.y - sb.track.y };
        match decor::track_hit(at, sb.slider) {
            TrackHit::PageUp => self.set_scroll(sb.target, cur.saturating_sub(sb.page)),
            TrackHit::PageDown => self.set_scroll(sb.target, (cur + sb.page).min(sb.max_off)),
            TrackHit::Slider(grab) => self.drag = Drag::Scroll(sb, grab),
        }
    }

    pub(super) fn scroll_of(&self, t: ScrollTarget) -> usize {
        match t {
            ScrollTarget::Editor(g) => self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map_or(0, |t| t.view.row_of(t.view.scroll)),
            ScrollTarget::EditorHorizontal(g) => self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map_or(0, |t| t.view.hscroll),
            ScrollTarget::Side => self.off[self.side as usize],
            ScrollTarget::Quick => self.quick.as_ref().map_or(0, |q| q.off),
            ScrollTarget::Git(w) => crate::scm::scroll_of(self, w),
        }
    }

    /// Scroll a list or editor to `off` (clamped where it is drawn).
    pub(super) fn set_scroll(&mut self, t: ScrollTarget, off: usize) {
        match t {
            ScrollTarget::EditorHorizontal(g) => {
                if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
                    t.view.hscroll = off;
                }
            }
            ScrollTarget::Editor(g) => {
                let max = {
                    let grp = self.groups.get(g);
                    match grp.and_then(|grp| grp.tabs.get(grp.active)).and_then(|t| self.docs.get(&t.key)) {
                        Some(Doc::Text(b)) => b.lines.len(),
                        Some(Doc::Diff(d)) => d.rows.len(),
                        None => self.ui.groups.get(g).map_or(0, |gh| gh.preview_rows),
                    }
                };
                if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
                    t.view.scroll = t.view.line_of(off).min(max.saturating_sub(1));
                }
            }
            ScrollTarget::Side => {
                let n = self.side_count();
                let h = self.ui.side_rows;
                self.off[self.side as usize] = off.min(n.saturating_sub(h));
            }
            ScrollTarget::Git(w) => crate::scm::set_scroll(self, w, off),
            ScrollTarget::Quick => {
                let h = self.ui.quick_list.height.max(1) as usize;
                if let Some(q) = self.quick.as_mut() {
                    q.off = off.min(q.items.len().saturating_sub(h));
                    q.sel = q.sel.clamp(q.off, q.off + h - 1).min(q.items.len().saturating_sub(1));
                }
            }
        }
    }

    /// Buffer position under screen cell `p` in group `g`'s text area.
    /// The active tab's side-by-side diff and its view.
    pub(super) fn diff_tab(&mut self) -> Option<(&DiffDoc, &mut View)> {
        let grp = self.groups.get_mut(self.ag)?;
        let tab = grp.tabs.get_mut(grp.active)?;
        match self.docs.get(&tab.key)? {
            Doc::Diff(d) => Some((d, &mut tab.view)),
            Doc::Text(_) => None,
        }
    }

    /// The selected text of the active editor or diff.
    pub(super) fn active_selection(&mut self) -> Option<String> {
        if let Some((b, t)) = self.ed() {
            return b.selected_text(&t.view);
        }
        let (d, v) = self.diff_tab()?;
        diff_selected_text(d, v)
    }

    /// The diff row and byte column on `side` under `p` (`texts`: each side's text
    /// area): rows clamp to the diff, columns to the side's text, so a drag past its
    /// edge (or onto the other side) stays on its own side.
    pub(super) fn diff_pos(&mut self, p: Position, texts: [Rect; 2], side: u8) -> Option<Pos> {
        let r = texts[side as usize];
        let (d, v) = self.diff_tab()?;
        let last = d.rows.len().checked_sub(1)?;
        let row = (v.scroll + p.y.saturating_sub(r.y) as usize).min(last);
        let x = v.hscroll + p.x.clamp(r.x, r.right().max(r.x + 1) - 1).saturating_sub(r.x) as usize;
        let past = p.x >= r.right();
        let col = diff_cell(d, row, side).map_or(0, |t| if past { t.len() } else { editor::byte_at_display(t, x) });
        Some(Pos::new(row, col))
    }

    /// A click in a diff, as in VS Code's diff editor: the side under it takes the
    /// cursor (F7 goes on from there); a second and a third click select the word and
    /// the line; Shift extends on the same side. A drag selects on from there.
    pub(super) fn diff_click(&mut self, g: usize, p: Position, texts: [Rect; 2], shift: bool) {
        let side = u8::from(p.x >= texts[0].right());
        let clicks = self.clicks;
        let Some(pos) = self.diff_pos(p, texts, side) else { return };
        let Some((d, v)) = self.diff_tab() else { return };
        if shift && v.side == side {
            v.anchor.get_or_insert(v.cursor);
            v.cursor = pos;
        } else {
            let text = diff_cell(d, pos.line, side).unwrap_or("");
            let (a, c) = match clicks {
                2 => editor::word_at(text, pos.col).unwrap_or((pos.col, pos.col)),
                n if n >= 3 => (0, text.len()),
                _ => (pos.col, pos.col),
            };
            (v.side, v.anchor, v.cursor) = (side, (a != c).then_some(Pos::new(pos.line, a)), Pos::new(pos.line, c));
        }
        v.want_x = Some(0);
        self.drag = Drag::Select(g);
    }

    /// Ctrl+A in a diff: all of its focused side.
    pub(super) fn diff_select_all(&mut self) {
        let Some((d, v)) = self.diff_tab() else { return };
        let Some(last) = d.rows.len().checked_sub(1) else { return };
        let end = diff_cell(d, last, v.side).map_or(0, str::len);
        (v.anchor, v.cursor) = (Some(Pos::new(0, 0)), Pos::new(last, end));
    }

    pub(super) fn pos_at(&self, g: usize, p: Position) -> Option<Pos> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let tab = grp.tabs.get(grp.active)?;
        let Some(Doc::Text(b)) = self.docs.get(&tab.key) else { return None };
        if let Some(&last) = gh.rows.last() {
            // Below the last row: the end of the last line.
            let (line, start) = match gh.rows.get(p.y.checked_sub(gh.text.y)? as usize) {
                Some(&r) => r?,
                None => (last?.0, usize::MAX / 2),
            };
            let line = line.min(b.lines.len() - 1);
            let text = &b.lines[line];
            let starts = editor::wrap_starts(text, gh.text.width as usize);
            let k = editor::wrap_row(&starts, start.min(editor::display_col(text, text.len())));
            // A row ends before the next one starts: a click past it stays on it.
            let end = starts.get(k + 1).map_or(usize::MAX, |&n| n.saturating_sub(1));
            let x = (start + p.x.saturating_sub(gh.text.x) as usize).min(end);
            return Some(Pos::new(line, editor::byte_at_display(text, x)));
        }
        let line = self.line_at(g, p.y)?;
        let x = tab.view.hscroll + p.x.saturating_sub(gh.text.x) as usize;
        Some(Pos::new(line, editor::byte_at_display(&b.lines[line], x)))
    }

    /// The buffer line drawn on screen row `y` of group `g`: rows below the peek
    /// zone are shifted down by its height; a row inside it is no line.
    pub(crate) fn line_at(&self, g: usize, y: u16) -> Option<usize> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let tab = grp.tabs.get(grp.active)?;
        let Some(Doc::Text(b)) = self.docs.get(&tab.key) else { return None };
        if let Some(&last) = gh.rows.last() {
            let r = gh.rows.get(y.saturating_sub(gh.text.y) as usize).copied().unwrap_or(last)?;
            return Some(r.0.min(b.lines.len() - 1));
        }
        let mut row = y.saturating_sub(gh.text.y);
        if let Some((zy, zh)) = gh.zone {
            if y >= zy + zh {
                row -= zh;
            } else if y >= zy {
                return None;
            }
        }
        Some(tab.view.line_of(tab.view.row_of(tab.view.scroll) + row as usize).min(b.lines.len() - 1))
    }

    /// The screen row buffer line `line` of group `g` is drawn on, if any: `line_at` backwards.
    pub(crate) fn line_y(&self, g: usize, line: usize) -> Option<u16> {
        let gh = self.ui.groups.get(g)?;
        let grp = self.groups.get(g)?;
        let v = &grp.tabs.get(grp.active)?.view;
        if !gh.rows.is_empty() {
            let i = gh.rows.iter().position(|r| r.is_some_and(|(l, _)| l == line))?;
            return Some(gh.text.y + i as u16);
        }
        if v.hidden(line) || line < v.scroll { return None; }
        let row = u16::try_from(v.row_of(line).checked_sub(v.row_of(v.scroll))?).ok()?;
        let mut y = gh.text.y.checked_add(row)?;
        if let Some((_, zh)) = gh.zone.filter(|&(zy, _)| y >= zy) {
            y = y.checked_add(zh)?;
        }
        (y < gh.text.bottom()).then_some(y)
    }

    /// Where a dragged file would land: the group under the cursor and which part.
    /// `bar`: a tab bar takes the drop at a slot between its tabs (a dragged tab).
    pub(super) fn drop_target(&self, p: Position, bar: bool) -> Option<(usize, Zone)> {
        let can_split = self.groups.len() < MAX_GROUPS;
        self.ui.groups.iter().enumerate().find(|(_, g)| g.area.contains(p)).map(|(i, g)| {
            let rx = (p.x - g.area.x) as u32 * 10 / g.area.width.max(1) as u32;
            let tabs = g.tabbar;
            let zone = if bar && tabs.contains(p) {
                // Before the first drawn tab whose middle is right of the pointer.
                Zone::Tab(g.tabs.iter().take_while(|(r, x)| p.x >= (r.x + x.right()) / 2).count())
            } else if tabs.contains(p) || !can_split {
                Zone::Center
            } else if rx < 3 {
                Zone::Left
            } else if rx >= 7 {
                Zone::Right
            } else {
                Zone::Center
            };
            (i, zone)
        })
    }

    pub(super) fn drag_to(&mut self, p: Position) {
        if crate::scm::drag(self, p) {
            return;
        }
        match &mut self.drag {
            // The terminal's own (term_mouse) never get here.
            Drag::None | Drag::TermSelect(_) | Drag::TermMouse => {}
            Drag::Close | Drag::ClosePending => self.hover = Some(p),
            Drag::Divider => {
                let start = self.ui.sidebar.x;
                let max = self.ui.editor_area.right().saturating_sub(start + 20).max(12);
                self.side_w = Some(p.x.saturating_sub(start).clamp(12, max));
                self.side_w_legacy = false;
            }
            // The panel's bottom stays; its top border follows the pointer (ui.rs clamps it).
            Drag::TermSash => self.term_h = Some((self.ui.term.bottom() + 1).saturating_sub(p.y).max(3)),
            // The panel's right edge stays; its left border follows the pointer.
            Drag::OutlineSash => self.outline_w = Some((self.ui.outline.right() + 1).saturating_sub(p.x).max(16)),
            Drag::GhSash => crate::github::drag_to(self, p.y),
            Drag::File { from, active, .. } => {
                if !*active && (from.x.abs_diff(p.x) + from.y.abs_diff(p.y)) >= 2 {
                    *active = true;
                }
                if *active {
                    self.drop = self.drop_target(p, false);
                }
            }
            Drag::Tab { g, i, from, active } => {
                if !*active && (from.x.abs_diff(p.x) + from.y.abs_diff(p.y)) >= 2 {
                    *active = true;
                }
                if *active {
                    let (g, i) = (*g, *i);
                    self.drop = self.drop_target(p, true).filter(|&(t, z)| !self.tab_drop_is_noop(g, i, t, z));
                }
            }
            Drag::Minimap(m, grab) => {
                let (m, grab) = (*m, *grab);
                let off = m.mini.drag_scroll(p.y as i32 - m.area.y as i32 - grab as i32);
                self.set_scroll(ScrollTarget::Editor(m.group), off);
            }
            Drag::Scroll(sb, grab) => {
                let (sb, grab) = (*sb, *grab);
                let (at, len) = if matches!(sb.target, ScrollTarget::EditorHorizontal(_)) {
                    (p.x as i32 - sb.track.x as i32, sb.track.width)
                } else { (p.y as i32 - sb.track.y as i32, sb.track.height) };
                let off = decor::drag_offset(at, grab, len, sb.slider.1, sb.max_off);
                self.set_scroll(sb.target, off);
            }
            Drag::Rename => {
                let field = self.ui.rename_field;
                if let Some(r) = self.rename.as_mut() {
                    let at = r.caret_at(field, p.x);
                    r.go(at, true);
                }
            }
            Drag::Select(g) => {
                let g = *g;
                let Some(gh) = self.ui.groups.get(g).cloned() else { return };
                // Auto-scroll past the top/bottom edge.
                let scroll: isize = if p.y < gh.text.y {
                    -1
                } else if p.y >= gh.text.bottom() {
                    1
                } else {
                    0
                };
                if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
                    t.view.scroll = (t.view.scroll as isize + scroll).max(0) as usize;
                }
                // A diff selection stays on the side its drag started on.
                if let (Some(texts), true) = (gh.diff_text, g == self.ag) {
                    let q = Position::new(p.x, p.y.clamp(gh.text.y, gh.text.bottom().saturating_sub(1)));
                    let side = self.diff_tab().map_or(0, |(_, v)| v.side);
                    if let Some(pos) = self.diff_pos(q, texts, side) {
                        if let Some((_, v)) = self.diff_tab() {
                            v.anchor.get_or_insert(v.cursor);
                            v.cursor = pos;
                        }
                    }
                    return;
                }
                let q = Position::new(p.x.max(gh.text.x), p.y.clamp(gh.text.y, gh.text.bottom().saturating_sub(1)));
                let pos = self.pos_at(g, q);
                if let (Some(pos), true) = (pos, g == self.ag) {
                    if let Some((b, tab)) = self.ed() {
                        b.move_to(&mut tab.view, pos, true);
                    }
                }
            }
        }
    }

    pub(super) fn release(&mut self) {
        if crate::scm::release(self) {
            return;
        }
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Divider | Drag::TermSash | Drag::OutlineSash | Drag::GhSash => self.save_layout(),
            Drag::File { rel, active: true, .. } => {
                if let Some((g, zone)) = self.drop.take() {
                    let target = match zone {
                        Zone::Center | Zone::Tab(_) => g,
                        Zone::Left | Zone::Right => {
                            let at = if zone == Zone::Left { g } else { g + 1 };
                            self.groups.insert(at, Group::default());
                            at
                        }
                    };
                    self.open_file(target, &rel, false);
                    self.focus = Focus::Editor;
                }
            }
            Drag::Tab { g, i, active: true, .. } => {
                if let Some((t, zone)) = self.drop.take() {
                    self.move_tab(g, i, t, zone);
                }
            }
            _ => {}
        }
        self.drop = None;
    }

    /// Dropping tab `i` of group `g` on (`t`, `zone`) changes nothing: its own
    /// group's content, or a split off a group it is the only tab of.
    pub(super) fn tab_drop_is_noop(&self, g: usize, i: usize, t: usize, zone: Zone) -> bool {
        let alone = self.groups.get(g).is_some_and(|grp| grp.tabs.len() == 1);
        t == g && match zone {
            Zone::Center => true,
            Zone::Tab(k) => k == i || k == i + 1,
            Zone::Left | Zone::Right => alone,
        }
    }

    /// VS Code's tab drop: tab `i` of group `g` moves (pinned, with its view) into
    /// group `t` at the tab-bar slot, after its active tab, or into a new group on
    /// the `zone` side. The document is not touched: a dirty buffer keeps its
    /// version, undo and backup. A group left empty closes.
    pub(super) fn move_tab(&mut self, g: usize, i: usize, t: usize, zone: Zone) {
        if g >= self.groups.len() || t >= self.groups.len() || i >= self.groups[g].tabs.len() || self.tab_drop_is_noop(g, i, t, zone) {
            return;
        }
        let mut tab = self.groups[g].tabs.remove(i);
        tab.preview = false;
        let src = &mut self.groups[g];
        if i < src.active || src.active >= src.tabs.len() {
            src.active = src.active.saturating_sub(1);
        }
        // (target group, slot in it, the source group after an insert before it)
        let (mut dst, at, src) = match zone {
            Zone::Left | Zone::Right => {
                let at = if zone == Zone::Left { t } else { t + 1 };
                self.groups.insert(at, Group::default());
                (at, 0, if at <= g { g + 1 } else { g })
            }
            // A slot right of the tab's old place moved left by one when it left.
            Zone::Tab(k) => (t, if t == g && k > i { k - 1 } else { k }, g),
            Zone::Center => (t, self.groups[t].active + 1, g),
        };
        let grp = &mut self.groups[dst];
        // Already open there (a split shows a file twice): that tab takes the focus.
        grp.active = match grp.tabs.iter().position(|x| x.key == tab.key) {
            Some(e) => e,
            None => {
                let at = at.min(grp.tabs.len());
                grp.tabs.insert(at, tab);
                at
            }
        };
        if self.groups[src].tabs.is_empty() && self.groups.len() > 1 {
            self.groups.remove(src);
            if dst > src {
                dst -= 1;
            }
        }
        self.peek = None; // its group and lines are another editor's now
        self.ag = dst;
        self.focus = Focus::Editor;
        self.find_refresh();
    }

    /// Sideways scroll of the editor under `p`; a diff shares the offset on both sides.
    /// True when handled, including at an edge, so Shift+wheel never turns vertical.
    pub(super) fn hwheel(&mut self, p: Position, d: isize) -> bool {
        if self.prompt.is_some() || self.quick.is_some() || self.gitui.cmds.picker.is_some() { return false; }
        if self.gitui.dlg.is_some() { return crate::commitdlg::hwheel(self, p, d); }
        let bar_group = self.ui.scrollbars.iter().find_map(|s| match s.target {
            ScrollTarget::EditorHorizontal(g) if s.track.contains(p) => Some(g),
            _ => None,
        });
        let Some(g) = bar_group.or_else(|| self.ui.groups.iter().position(|g| g.area.contains(p))) else { return false };
        let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) else { return false };
        if !self.docs.contains_key(&t.key) { return false; }
        t.view.hscroll = t.view.hscroll.saturating_add_signed(d);
        true
    }

    /// Scroll what is under `p`; false when it could not move (no redraw needed).
    pub(super) fn wheel(&mut self, p: Position, d: isize) -> bool {
        if let Some(q) = self.quick.as_mut() {
            let old = q.sel;
            q.sel = (q.sel as isize + d).clamp(0, (q.items.len() as isize - 1).max(0)) as usize;
            return q.sel != old;
        }
        if crate::scm::wheel(self, p, d) || crate::github::wheel(self, p, d) {
            return true;
        }
        if self.ui.outline.contains(p) {
            let old = self.outline_off;
            self.outline_off = self.outline_off.saturating_add_signed(d); // the frame clamps it
            return self.outline_off != old;
        }
        if let Some(t) = self.term.as_mut().filter(|_| self.ui.term.contains(p)) {
            t.wheel(d, (p.x - self.ui.term.x, p.y - self.ui.term.y));
            return true;
        }
        if self.ui.peek_zone.contains(p) {
            let max = self.ui.peek_max;
            let Some(pk) = self.peek.as_mut() else { return false };
            let old = pk.scroll;
            pk.scroll = (pk.scroll.min(max) as isize + d).clamp(0, max as isize) as usize;
            return pk.scroll != old;
        }
        // Over an editor's scrollbar the wheel scrolls that editor.
        if let Some(sb) = self.ui.scrollbars.iter().find(|s| s.track.contains(p)).copied() {
            if matches!(sb.target, ScrollTarget::Editor(_) | ScrollTarget::EditorHorizontal(_)) {
                let cur = self.scroll_of(sb.target);
                self.set_scroll(sb.target, cur.saturating_add_signed(d));
                return self.scroll_of(sb.target) != cur;
            }
        }
        if self.ui.side_list.contains(p) {
            let s = self.side as usize;
            let n = self.side_count();
            let h = self.ui.side_rows;
            let old = self.off[s];
            self.off[s] = (self.off[s] as isize + d).clamp(0, n.saturating_sub(h) as isize) as usize;
            return self.off[s] != old;
        }
        let Some(g) = self.ui.groups.iter().position(|g| g.area.contains(p)) else { return false };
        let max = {
            let grp = &self.groups[g];
            match grp.tabs.get(grp.active).and_then(|t| self.docs.get(&t.key)) {
                Some(Doc::Text(b)) => b.lines.len(),
                Some(Doc::Diff(dd)) => dd.rows.len(),
                None => self.ui.groups[g].preview_rows,
            }
        };
        let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) else { return false };
        let old = t.view.scroll;
        t.view.scroll = t.view.shift_line(t.view.scroll, d).min(max.saturating_sub(1));
        t.view.scroll != old
    }

    /// A drawn editor's (or the definition peek's) first highlighting is still catching
    /// up. Only what was drawn last frame counts: a hidden tab cannot keep the loop awake.
    pub(crate) fn hl_pending(&self) -> bool {
        let shown = |g: usize| self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).and_then(|t| self.docs.get(&t.key));
        // A terminal too small to draw (under 30x5) records no groups: then nothing counts,
        // a peek kept from a bigger frame included.
        (0..self.ui.groups.len()).any(|g| self.ui.groups[g].text.height > 0 && matches!(shown(g), Some(Doc::Text(b)) if b.hl_pending()))
            || (!self.ui.groups.is_empty() && crate::navui::hl_pending(self))
            || self.ui.groups.iter().enumerate().any(|(g, gh)| {
                let key = self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map(|t| &t.key);
                gh.text.height > 0 && key.and_then(|k| self.diff_syntax.get(k)).and_then(Option::as_ref)
                    .is_some_and(|(_, sides)| sides.iter().any(Buffer::hl_pending))
            })
            || self.gitui.dlg.as_ref().is_some_and(|d| !d.hits.diff.is_empty() && d.diff.as_ref().is_some_and(|doc| {
                d.diff_syntax.as_ref().is_some_and(|(id, sides)| *id == doc.id && sides.iter().any(Buffer::hl_pending))
            }))
            // A drawn diff minimap highlights its own buffer (ui::diff_mini).
            || self.ui.minimaps.iter().any(|m| {
                let key = self.groups.get(m.group).and_then(|grp| grp.tabs.get(grp.active)).map(|t| &t.key);
                key.and_then(|k| self.diff_minis.get(k)).is_some_and(|e| e.1.hl_pending())
            })
    }
}
