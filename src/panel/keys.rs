//! `App`: the keyboard.

use super::*;

impl App {
    // ---- keyboard --------------------------------------------------------------------------

    pub fn on_key(&mut self, k: KeyEvent) {
        self.activity_lit = false;
        if self.settings_dlg.is_some() {
            return crate::settingsui::key(self, k);
        }
        // Esc cancels a close press or an active file/tab drag before other key handling.
        if k.code == KeyCode::Esc && matches!(self.drag, Drag::Close | Drag::ClosePending | Drag::File { active: true, .. } | Drag::Tab { active: true, .. }) {
            self.drag = Drag::None;
            self.drop = None;
            self.tree_drop = None;
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let is = |c: char| matches!(k.code, KeyCode::Char(x) if x.eq_ignore_ascii_case(&c));
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if self.rename.is_some() {
            return self.rename_key(k);
        }
        if self.quick.is_some() {
            return self.quick_key(k, ctrl || alt);
        }
        if self.gitui.dlg.is_some() {
            return crate::commitdlg::key(self, k);
        }
        if self.gitui.cmds.picker.is_some() {
            return crate::gitcmds::picker_key(self, k);
        }
        // Ctrl+C with text selected in a commit card copies it.
        if k.modifiers == KeyModifiers::CONTROL && k.code == KeyCode::Char('c') {
            if let Some(text) = self.gitui.card.as_ref().and_then(crate::scm::Card::selected_text) {
                self.msg = format!("copied via {}", clipboard_set(&text));
                self.clip = text;
                return;
            }
        }
        if k.code == KeyCode::Esc && (self.gitui.card.take().is_some() || self.gitui.cmds.toast.take().is_some()) {
            return;
        }
        // A palette command's keys (`replaying`) are termcode's, never the shell's.
        if self.focus == Focus::Term && !self.replaying {
            return self.term_key(k);
        }
        // The user's keybindings come before the default keys ("" turns a key off).
        if !self.replaying {
            let key = crate::parse::chord(&k);
            if let Some((_, name)) = self.settings.keybindings.iter().find(|(c, _)| *c == key) {
                let name = name.clone();
                if let Some((_, cmd)) = all_commands().find(|(l, _)| *l == name) {
                    self.pending_k = false;
                    self.run_command(cmd);
                }
                return;
            }
        }
        if std::mem::take(&mut self.pending_k) {
            if k.code == KeyCode::Enter { return self.pin_active(); }
            if ctrl && is('0') { return self.fold_command(Cmd::FoldAll, None); }
            if ctrl && is('j') { return self.fold_command(Cmd::UnfoldAll, None); }
            if !ctrl && is('v') { return self.md_preview(true); }
        }
        if ctrl && is('q') {
            return self.request_quit();
        }
        if ctrl && is(',') {
            return crate::settingsui::open(self);
        }
        if crate::term::toggle_key(&k) {
            return self.toggle_terminal();
        }
        if ctrl && shift && is('p') {
            return self.open_palette();
        }
        if ((ctrl && !shift) || alt) && is('p') {
            return self.open_quick();
        }
        // Ctrl+Shift+F only arrives distinctly with kitty keyboard support; legacy
        // terminals send it as Ctrl+F, which is in-file find (VS Code's Ctrl+F).
        if ctrl && shift && is('f') {
            return self.show_side(Side::Search, true);
        }
        // Like Ctrl+Shift+F, these two arrive distinctly only with kitty keyboard support.
        if ctrl && shift && is('o') {
            return self.open_goto("@");
        }
        if ctrl && shift && is('v') {
            return self.md_preview(false);
        }
        if alt && !ctrl && is('z') {
            return self.run_command(Cmd::ToggleWrap);
        }
        if ctrl && is('e') {
            return self.show_side(Side::Explorer, false);
        }
        if ctrl && is('g') {
            return self.show_side(Side::Scm, false);
        }
        if ctrl && is('f') {
            return self.open_find(false);
        }
        if ctrl && is('h') {
            return self.open_find(true);
        }
        if ctrl && is('s') {
            return self.save_active();
        }
        if ctrl && is('w') {
            let g = self.ag;
            let i = self.groups.get(g).map_or(0, |grp| grp.active);
            return self.close_tab(g, i, false);
        }
        if ctrl && is('k') {
            self.pending_k = true;
            return;
        }
        match k.code {
            KeyCode::F(1) => return self.open_palette(),
            KeyCode::F(2) => return self.show_side(Side::Scm, false),
            KeyCode::F(4) => {
                // VS Code seeds Find in Files with a one-line selection (editor or diff).
                let seed = self.active_selection().filter(|s| !s.is_empty() && !s.contains('\n'));
                self.show_side(Side::Search, true);
                if let Some(s) = seed {
                    self.query = s;
                }
                return;
            }
            KeyCode::F(3) => return self.find_step(if shift { -1 } else { 1 }),
            // VS Code's Go to Next/Previous Change (Alt+F5); F7 where Alt+F5 does not arrive.
            // In a diff tab F7 walks the diff's changes instead.
            KeyCode::F(5) if alt => return self.peek_step(if shift { -1 } else { 1 }),
            KeyCode::F(7) if self.ed().is_none() => return crate::scm::jump_change(self, if shift { -1 } else { 1 }),
            KeyCode::F(7) => return self.peek_step(if shift { -1 } else { 1 }),
            KeyCode::Char(c @ ('1' | '2' | '4')) if alt => {
                let s = match c {
                    '1' => Side::Explorer,
                    '2' => Side::Scm,
                    _ => Side::Search,
                };
                return self.show_side(s, s == Side::Search);
            }
            KeyCode::Char(c @ '1'..='3') if ctrl => {
                let g = c as usize - '1' as usize;
                if g < self.groups.len() {
                    self.ag = g;
                    self.focus = Focus::Editor;
                    self.find_refresh();
                }
                return;
            }
            KeyCode::PageDown | KeyCode::Tab if ctrl => return self.cycle_tab(1),
            KeyCode::PageUp | KeyCode::BackTab if ctrl => return self.cycle_tab(-1),
            _ => {}
        }
        match self.focus {
            Focus::Find => self.find_key(k, ctrl || alt, shift),
            Focus::SearchInput => self.input_key(k, ctrl, alt),
            Focus::Side => self.side_key(k),
            Focus::Editor => {
                // The STM32 Programmer's page scrolls by keys too (it has no text).
                if !crate::programmer::page_key(self, k) {
                    self.editor_key(k, ctrl, alt, shift);
                }
            }
            Focus::Term => {}
            Focus::Outline => self.outline_key(k),
        }
    }

    pub(super) fn prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = self.prompt.as_mut() else { return };
        let n = p.buttons.len();
        match k.code {
            KeyCode::Left | KeyCode::BackTab => p.sel = (p.sel + n - 1) % n,
            KeyCode::Right | KeyCode::Tab => p.sel = (p.sel + 1) % n,
            KeyCode::Enter => {
                let act = p.buttons[p.sel].1.clone();
                self.run_prompt(act);
            }
            // A restore offer must be answered: editing on would overwrite the old backup.
            KeyCode::Esc if !p.buttons.iter().any(|b| matches!(b.1, PAct::RestoreBackup(..))) => self.prompt = None,
            _ => {}
        }
    }

    pub(super) fn find_key(&mut self, k: KeyEvent, modded: bool, shift: bool) {
        // VS Code's find widget keys, the widget has no buttons for them.
        if k.modifiers.contains(KeyModifiers::ALT) {
            match k.code {
                KeyCode::Char('r') => return self.find_button(FindBtn::Regex),
                KeyCode::Char('c') => return self.find_button(FindBtn::Case),
                KeyCode::Char('w') => return self.find_button(FindBtn::Word),
                KeyCode::Enter if self.find.as_ref().is_some_and(|f| f.field == 1) => return self.replace_all(),
                _ => {}
            }
        }
        let Some(f) = self.find.as_mut() else { return };
        let all = std::mem::take(&mut f.all);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let mut field = if f.field == 1 { f.replace.as_mut() } else { Some(&mut f.query) };
        let text = field.as_deref().cloned().unwrap_or_default();
        // A selected field goes as the first change starts (typing, Backspace, a cut).
        let all = all && !text.is_empty();
        if all && matches!(k.code, KeyCode::Backspace | KeyCode::Delete | KeyCode::Char(_)) && (!modded || ctrl && matches!(k.code, KeyCode::Char('x' | 'v'))) {
            if let Some(s) = field.as_deref_mut() {
                s.clear();
            }
        }
        match k.code {
            KeyCode::Char('a') if ctrl => f.all = true,
            // Nothing selected: Ctrl+C still takes the whole field.
            KeyCode::Char('c') if ctrl && !text.is_empty() => {
                f.all = all;
                self.msg = format!("copied via {}", clipboard_set(&text));
                self.clip = text;
            }
            KeyCode::Char('x') if ctrl && all => {
                self.msg = format!("cut via {}", clipboard_set(&text));
                self.clip = text;
                self.find_incremental();
            }
            KeyCode::Char('v') if ctrl => {
                let t = clipboard_get().unwrap_or_else(|| self.clip.clone());
                self.paste_text(&t);
            }
            KeyCode::Esc => {
                self.find = None;
                self.focus = Focus::Editor;
            }
            KeyCode::Enter if f.field == 1 => self.replace_one(),
            KeyCode::Enter | KeyCode::Down => self.find_step(if shift { -1 } else { 1 }),
            KeyCode::Up => self.find_step(-1),
            KeyCode::Tab | KeyCode::BackTab => {
                if f.replace.is_some() {
                    f.field = 1 - f.field;
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = field {
                    s.pop();
                }
                self.find_incremental();
            }
            KeyCode::Char(c) if !modded => {
                if let Some(s) = field {
                    s.push(c);
                }
                self.find_incremental();
            }
            _ => {}
        }
    }

    /// Find as you type: the first match at or after where the selection starts (so a
    /// longer query keeps the match it extends) is selected and revealed, as VS Code does;
    /// nothing found leaves the cursor where it is.
    pub(super) fn find_incremental(&mut self) {
        self.find_refresh();
        let Some(f) = self.find.as_ref().filter(|f| f.field == 0 && !f.matches.is_empty()) else { return };
        let (matches, side) = (f.matches.clone(), f.diff_side);
        let Some(tab) = self.active_tab() else { return };
        let v = &mut tab.view;
        let from = v.anchor.map_or(v.cursor, |a| a.min(v.cursor));
        let (a, e) = matches[matches.iter().position(|(a, _)| *a >= from).unwrap_or(0)];
        if let Some(side) = side {
            v.side = side;
        }
        (v.anchor, v.cursor) = (Some(a), e);
        self.reveal();
        self.find_refresh();
    }

    pub(super) fn input_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool) {
        let all = std::mem::take(&mut self.query_all) && !self.query.is_empty();
        // A selected query goes as the first change starts (typing, Backspace, Ctrl+X).
        let take = |app: &mut App| {
            if all {
                app.query.clear();
                app.search_on_type();
            }
            all
        };
        match k.code {
            KeyCode::Char('a') if ctrl => self.query_all = true,
            // Nothing selected: Ctrl+C still takes the whole query (as a rename's name).
            KeyCode::Char('c') if ctrl && !self.query.is_empty() => {
                self.query_all = all;
                self.msg = format!("copied via {}", clipboard_set(&self.query));
                self.clip = self.query.clone();
            }
            KeyCode::Char('x') if ctrl && all => {
                self.msg = format!("cut via {}", clipboard_set(&self.query));
                self.clip = std::mem::take(&mut self.query);
                self.search_on_type();
            }
            KeyCode::Char('v') if ctrl => {
                self.query_all = all;
                let text = clipboard_get().unwrap_or_else(|| self.clip.clone());
                self.paste_text(&text);
            }
            KeyCode::Backspace | KeyCode::Delete if take(self) => {}
            KeyCode::Enter => {
                self.search_due = None;
                self.start_search();
                self.focus = Focus::Side;
            }
            KeyCode::Esc if self.search.is_some() || self.queued_search.is_some() || self.search_due.is_some() => {
                self.search_due = None;
                self.queued_search = None;
                self.stop_search();
                self.msg = "search cancelled".into();
            }
            KeyCode::Esc | KeyCode::Down => self.focus = Focus::Side,
            KeyCode::Backspace => {
                if self.query.pop().is_some() {
                    self.search_on_type();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                take(self);
                self.query.push(c);
                self.search_on_type();
            }
            _ => {}
        }
    }

    /// The query changed: search once typing pauses (an emptied query clears the results).
    pub(super) fn search_on_type(&mut self) {
        self.search_due = Some(Instant::now() + SEARCH_DEBOUNCE);
    }

    pub(super) fn side_key(&mut self, k: KeyEvent) {
        if self.side == Side::Scm && crate::scm::key(self, k) {
            return;
        }
        if self.side == Side::Programmer {
            if !crate::programmer::key(self, k) && k.code == KeyCode::Esc {
                self.focus = Focus::Editor;
            }
            return;
        }
        if self.side == Side::GitHub {
            if !crate::github::key(self, k) && k.code == KeyCode::Esc {
                self.focus = Focus::Editor;
            }
            return;
        }
        // Delete: to the Trash; Shift+Delete: for good (each asked first).
        if self.side == Side::Explorer && k.code == KeyCode::Delete {
            return self.ask_delete(k.modifiers.contains(KeyModifiers::SHIFT));
        }
        // Ctrl+Z / Ctrl+Y (Ctrl+Shift+Z): the Explorer's file operations taken back or redone.
        if self.side == Side::Explorer && k.modifiers.contains(KeyModifiers::CONTROL) {
            let shift = k.modifiers.contains(KeyModifiers::SHIFT);
            match k.code {
                KeyCode::Char('z') if !shift => return self.undo_file_op(),
                KeyCode::Char('y') | KeyCode::Char('Z') => return self.redo_file_op(),
                KeyCode::Char('z') => return self.redo_file_op(),
                _ => {}
            }
        }
        // The Explorer's file clipboard: Ctrl+C, Ctrl+X, Ctrl+V.
        if self.side == Side::Explorer && k.modifiers == KeyModifiers::CONTROL {
            match k.code {
                KeyCode::Char('c') => return self.tree_copy(false),
                KeyCode::Char('x') => return self.tree_copy(true),
                KeyCode::Char('v') => return self.tree_paste(),
                _ => {}
            }
        }
        if self.side == Side::Explorer {
            let e = Side::Explorer as usize;
            match k.code {
                // Shift+↑↓: the selection grows from the anchor.
                KeyCode::Up | KeyCode::Down if k.modifiers.contains(KeyModifiers::SHIFT) => {
                    if self.tree_marks.is_empty() {
                        self.tree_anchor = self.tree.get(self.sel[e]).map(|n| n.path.clone());
                    }
                    self.move_side(if k.code == KeyCode::Up { -1 } else { 1 });
                    return self.tree_mark(self.sel[e], true);
                }
                KeyCode::Char('a') if k.modifiers == KeyModifiers::CONTROL => {
                    self.tree_marks = self.tree.iter().map(|n| n.path.clone()).collect();
                    return;
                }
                KeyCode::Esc if self.file_job.is_some() => {
                    self.stop_paste();
                    return;
                }
                KeyCode::Esc if !self.tree_marks.is_empty() => return self.tree_marks.clear(),
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => self.tree_marks.clear(),
                _ => {}
            }
        }
        let page = self.ui.side_rows.max(1) as isize;
        let s = self.side as usize;
        match k.code {
            KeyCode::Up => self.move_side(-1),
            KeyCode::Down => self.move_side(1),
            KeyCode::PageUp => self.move_side(-page),
            KeyCode::PageDown => self.move_side(page),
            KeyCode::Home => self.move_side(-1_000_000),
            KeyCode::End => self.move_side(1_000_000),
            KeyCode::Enter => self.activate_side(self.sel[s], true),
            KeyCode::Char(' ') if self.side == Side::Search && matches!(self.search_rows().get(self.sel[s]), Some(HRow::File(..))) => {
                self.toggle_search_group(self.sel[s]);
            }
            KeyCode::Char(' ') => self.activate_side(self.sel[s], false),
            KeyCode::Left | KeyCode::Right if self.side == Side::Search => {
                let i = self.sel[s];
                if matches!(self.search_rows().get(i), Some(HRow::File(p, _)) if self.search_closed.contains(p) == (k.code == KeyCode::Right)) {
                    self.toggle_search_group(i);
                }
            }
            KeyCode::Left | KeyCode::Right if self.side == Side::Explorer => {
                let i = self.sel[s];
                if self.tree.get(i).is_some_and(|n| n.dir && n.open == (k.code == KeyCode::Left)) {
                    self.toggle(i);
                }
            }
            KeyCode::Char('/') if self.side == Side::Search => (self.focus, self.query_all) = (Focus::SearchInput, false),
            // h: the file's history, in the TIMELINE under the tree.
            KeyCode::Char('h') if self.side == Side::Explorer => {
                if let Some(n) = self.tree.get(self.sel[s]).filter(|n| !n.dir) {
                    let p = n.path.clone();
                    self.open_file(self.ag, &p, true);
                    self.gitui.timeline.open = true;
                }
            }
            KeyCode::Char('r') => self.reload_all(),
            // Source Control list: + stage, - unstage, Delete discard (row or section header).
            KeyCode::Left | KeyCode::Right if self.side == Side::Scm => {
                let i = self.sel[s];
                self.fold_scm(i, Some(k.code == KeyCode::Left));
            }
            KeyCode::Char('+' | '-') | KeyCode::Delete if self.side == Side::Scm => {
                let op = match k.code {
                    KeyCode::Char(c) => c,
                    _ => 'd',
                };
                let changes = |i: usize| self.scm_files[i].section == Section::Changes;
                let act = match self.scm_rows().get(self.sel[s]) {
                    Some(SRow::File(i)) if op == '+' && changes(*i) => ScmAct::Stage(*i),
                    Some(SRow::File(i)) if op == '-' && !changes(*i) => ScmAct::Unstage(*i),
                    Some(SRow::File(i)) if op == 'd' && changes(*i) => ScmAct::Discard(*i),
                    Some(SRow::Head(Section::Changes, _)) if op == '+' => ScmAct::StageAll,
                    Some(SRow::Head(Section::Staged, _)) if op == '-' => ScmAct::UnstageAll,
                    Some(SRow::Head(Section::Changes, _)) if op == 'd' => ScmAct::DiscardAll,
                    _ => return,
                };
                self.scm_action(act);
            }
            KeyCode::Esc | KeyCode::Tab => self.focus = Focus::Editor,
            _ => {}
        }
    }

    pub(super) fn editor_key(&mut self, k: KeyEvent, ctrl: bool, alt: bool, shift: bool) {
        if !keeps_cursors(k.code, ctrl, alt, shift) {
            if let Some((_, tab)) = self.ed() {
                tab.view.extra.clear();
            }
        }
        if ctrl && k.code == KeyCode::Char('/') {
            return self.toggle_comment();
        }
        if ctrl && shift && matches!(k.code, KeyCode::Char('[' | ']' | '{' | '}')) {
            return self.fold_command(if matches!(k.code, KeyCode::Char('[' | '{')) { Cmd::Fold } else { Cmd::Unfold }, None);
        }
        if ctrl && k.code == KeyCode::Char('c') {
            return self.copy_cut(false);
        }
        if ctrl && k.code == KeyCode::Char('x') {
            return self.copy_cut(true);
        }
        if ctrl && k.code == KeyCode::Char('v') {
            let text = clipboard_get().unwrap_or_else(|| self.clip.clone());
            return self.paste_text(&text);
        }
        if k.code == KeyCode::Esc && self.peek.is_some() {
            self.peek = None;
            return;
        }
        if k.code == KeyCode::Esc && self.find.is_some() {
            self.find = None;
            return;
        }
        let page = self.ui.groups.get(self.ag).map_or(20, |g| g.text.height.max(1) as isize);
        if self.ed().is_none() && ctrl && k.code == KeyCode::Char('a') {
            return self.diff_select_all();
        }
        if self.ed().is_none() && k.code == KeyCode::Esc {
            if let Some((_, v)) = self.diff_tab() {
                v.anchor = None;
            }
            return;
        }
        if self.ed().is_none() {
            // Diff views scroll, both sides at once (F7 is routed in on_key).
            let rows = match self.active_tab().map(|t| t.key.clone()).and_then(|k| self.docs.get(&k)) {
                Some(Doc::Diff(d)) => d.rows.len(),
                _ => self.ui.groups.get(self.ag).map_or(0, |g| g.preview_rows),
            };
            if let Some(t) = self.active_tab() {
                let v = &mut t.view;
                let last = rows.saturating_sub(1) as isize;
                let to = |d: isize| (v.scroll as isize + d).clamp(0, last) as usize;
                match k.code {
                    KeyCode::Up => v.scroll = to(-1),
                    KeyCode::Down => v.scroll = to(1),
                    KeyCode::PageUp => v.scroll = to(-page),
                    KeyCode::PageDown => v.scroll = to(page),
                    KeyCode::Home if ctrl => v.scroll = 0,
                    // The last row at the bottom.
                    KeyCode::End if ctrl => v.scroll = rows.saturating_sub(page as usize),
                    KeyCode::Home => v.hscroll = 0,
                    // Clamped where drawn: as far as the longest visible line needs.
                    KeyCode::End => v.hscroll = 1 << 30,
                    _ => {}
                }
            }
            return;
        }
        let mut readonly_hit = None;
        let (brackets, quotes) = (self.settings.auto_brackets, self.settings.auto_quotes);
        // Wrapped, Up/Down and the page keys go by screen rows (one cursor only).
        let wrap_w = self.active_tab_ref().filter(|t| self.wraps(&t.key)).and_then(|_| self.ui.groups.get(self.ag)).map(|g| g.text.width.max(1) as usize);
        {
            let Some((b, tab)) = self.ed() else { return };
            let v = &mut tab.view;
            let before = b.version();
            let rows = match k.code {
                KeyCode::Up | KeyCode::Down if !ctrl && !alt => 1,
                KeyCode::PageUp | KeyCode::PageDown if !ctrl && !alt => page,
                _ => 0,
            };
            if let Some(w) = wrap_w.filter(|_| rows > 0 && v.extra.is_empty()) {
                let up = matches!(k.code, KeyCode::Up | KeyCode::PageUp);
                b.vertical_wrapped(v, if up { -rows } else { rows }, shift, w);
                if matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) {
                    v.scroll = v.shift_line(v.scroll, if up { -page } else { page }).min(b.lines.len() - 1);
                }
                self.reveal();
                return;
            }
            match k.code {
                KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                    'a' => b.select_all(v),
                    'z' if shift => {
                        b.redo(v);
                    }
                    'z' => {
                        b.undo(v);
                    }
                    'y' => {
                        b.redo(v);
                    }
                    _ => return,
                },
                KeyCode::Char(_) if alt => return,
                KeyCode::Char(c) => b.at_cursors(v, editor::Kind::Type, |b, v| b.type_char(v, c, brackets, quotes)),
                KeyCode::Enter => b.at_cursors(v, editor::Kind::Other, |b, v| b.newline(v)),
                KeyCode::Tab => b.indent(v),
                KeyCode::BackTab => b.outdent(v),
                KeyCode::Backspace => b.at_cursors(v, editor::Kind::Erase, |b, v| b.backspace_pair(v, brackets, quotes)),
                KeyCode::Delete => b.at_cursors(v, editor::Kind::Other, |b, v| b.delete(v)),
                KeyCode::Left => b.move_cursors(v, |b, v| b.left(v, shift, ctrl)),
                KeyCode::Right => b.move_cursors(v, |b, v| b.right(v, shift, ctrl)),
                KeyCode::Up if ctrl && shift => b.add_cursor(v, false),
                KeyCode::Down if ctrl && shift => b.add_cursor(v, true),
                KeyCode::Up if alt => b.move_lines(v, false),
                KeyCode::Down if alt && shift => b.duplicate_lines(v),
                KeyCode::Down if alt => b.move_lines(v, true),
                KeyCode::Up if ctrl => v.scroll = v.shift_line(v.scroll, -1),
                KeyCode::Down if ctrl => v.scroll = v.shift_line(v.scroll, 1).min(b.lines.len() - 1),
                KeyCode::Up => b.move_cursors(v, |b, v| b.vertical(v, -1, shift)),
                KeyCode::Down => b.move_cursors(v, |b, v| b.vertical(v, 1, shift)),
                KeyCode::PageUp => {
                    b.vertical(v, -page, shift);
                    v.scroll = v.shift_line(v.scroll, -page);
                }
                KeyCode::PageDown => {
                    b.vertical(v, page, shift);
                    v.scroll = v.shift_line(v.scroll, page).min(b.lines.len() - 1);
                }
                KeyCode::Home if ctrl => b.move_to(v, Pos::new(0, 0), shift),
                KeyCode::End if ctrl => {
                    let last = b.lines.len() - 1;
                    let p = Pos::new(last, b.lines[last].len());
                    b.move_to(v, p, shift);
                }
                KeyCode::Home => b.move_cursors(v, |b, v| b.home(v, shift)),
                KeyCode::End => b.move_cursors(v, |b, v| b.end_of_line(v, shift)),
                KeyCode::Esc => v.anchor = None,
                _ => return,
            }
            let typing = !ctrl && matches!(k.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete | KeyCode::Tab);
            if b.version() != before {
                tab.preview = false; // editing keeps the tab open
            } else if typing {
                readonly_hit = b.readonly.clone();
            }
        }
        if let Some(ro) = readonly_hit {
            self.msg = format!("read-only: {ro}");
        }
        self.reveal();
        self.find_refresh();
    }

    pub(super) fn copy_cut(&mut self, cut: bool) {
        if self.ed().is_none() {
            // A diff is read-only: Ctrl+C and Ctrl+X copy its selection, one side's text.
            let Some(text) = self.diff_tab().and_then(|(d, v)| diff_selected_text(d, v)) else { return };
            self.msg = format!("copied via {}", clipboard_set(&text));
            self.clip = text;
            return;
        }
        let Some((b, tab)) = self.ed() else { return };
        let v = &mut tab.view;
        let text = match b.selected_text(v) {
            Some(t) => t,
            None => {
                // No selection: VS Code copies/cuts the whole line.
                let c = b.clamp(v.cursor);
                if cut {
                    let (a, e) = if c.line + 1 < b.lines.len() {
                        (Pos::new(c.line, 0), Pos::new(c.line + 1, 0))
                    } else if c.line > 0 {
                        (Pos::new(c.line - 1, b.lines[c.line - 1].len()), Pos::new(c.line, b.lines[c.line].len()))
                    } else {
                        (Pos::new(0, 0), Pos::new(0, b.lines[0].len()))
                    };
                    v.anchor = Some(a);
                    v.cursor = e;
                }
                format!("{}\n", b.lines[c.line])
            }
        };
        if cut && b.readonly.is_none() {
            b.backspace(v);
            tab.preview = false;
        }
        let how = clipboard_set(&text);
        self.clip = text;
        self.msg = format!("{} via {how}", if cut { "cut" } else { "copied" });
        self.reveal();
        self.find_refresh();
    }

    /// A bracketed paste: a launcher request when it starts with
    /// `termcode:<our token>:` (never inserted anywhere), else ordinary text.
    pub fn on_paste(&mut self, s: &str) {
        let req = self.token.as_deref().and_then(|t| s.strip_prefix("termcode:")?.strip_prefix(t)?.strip_prefix(':'));
        match req {
            Some(body) => self.request(body),
            None => self.paste_text(s),
        }
    }

    /// A launcher request, `open:<abs path>:<line>:<col>`: open that file pinned,
    /// cursor at line:col (from 1, col in characters). Anything else is refused.
    pub fn request(&mut self, body: &str) {
        match self.parse_request(body) {
            Ok((rel, line, col)) => {
                self.quick = None;
                self.open_file(self.ag, &rel, false);
                if let Some((b, tab)) = self.ed() {
                    let l = line.saturating_sub(1).min(b.lines.len() - 1);
                    let c = b.lines[l].char_indices().nth(col.saturating_sub(1)).map_or(b.lines[l].len(), |(i, _)| i);
                    tab.view.cursor = Pos::new(l, c);
                    tab.view.anchor = None;
                    tab.view.scroll = l.saturating_sub(5);
                }
                self.reveal();
                self.focus = Focus::Editor;
                self.msg = format!("opened {rel}:{line}");
            }
            Err(e) => self.msg = format!("request refused: {e}"),
        }
    }

    /// (path relative to the root, line, col) of a valid `open:` request.
    pub(super) fn parse_request(&self, body: &str) -> Result<(String, usize, usize), String> {
        let bad = || format!("{body:?}");
        let rest = body.strip_prefix("open:").ok_or_else(bad)?;
        let (rest, col) = rest.rsplit_once(':').ok_or_else(bad)?;
        let (path, line) = rest.rsplit_once(':').ok_or_else(bad)?;
        let (line, col) = line.parse::<usize>().ok().zip(col.parse::<usize>().ok()).ok_or_else(bad)?;
        if !Path::new(path).is_absolute() {
            return Err(bad());
        }
        let p = fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
        let rel = p.strip_prefix(&self.root).map_err(|_| format!("{path} is outside {}", self.root.display()))?;
        if !fs::metadata(&p).is_ok_and(|m| m.is_file()) {
            return Err(format!("{path} is not a regular file"));
        }
        Ok((rel.to_string_lossy().into_owned(), line, col))
    }

    /// Bracketed paste or Ctrl+V: into the focused input or the editor.
    pub fn paste_text(&mut self, text: &str) {
        if self.settings_dlg.is_some() {
            return crate::settingsui::paste(self, text);
        }
        if self.prompt.is_some() || self.quick.is_some() {
            return;
        }
        if let Some(r) = self.rename.as_mut() {
            return r.paste(text);
        }
        if self.gitui.dlg.is_some() {
            return crate::commitdlg::paste(self, text);
        }
        if self.gitui.cmds.picker.is_some() {
            return crate::gitcmds::picker_paste(self, text);
        }
        if self.focus == Focus::Editor && crate::programmer::paste(self, text) {
            return;
        }
        match self.focus {
            Focus::SearchInput => {
                if std::mem::take(&mut self.query_all) {
                    self.query.clear();
                }
                self.query.push_str(&text.replace(['\r', '\n'], " "));
                self.search_on_type();
            }
            Focus::Find => {
                if let Some(f) = self.find.as_mut() {
                    let all = std::mem::take(&mut f.all);
                    let s = if f.field == 1 { f.replace.get_or_insert_with(String::new) } else { &mut f.query };
                    if all {
                        s.clear();
                    }
                    s.push_str(text.lines().next().unwrap_or(""));
                }
                self.find_refresh();
            }
            // A paste while a list has focus is dropped, never redirected into a file (a
            // GitHub title being edited takes it).
            Focus::Side => {
                crate::github::paste(self, text);
            }
            Focus::Term if self.term_find.is_some() => self.term_find_paste(text),
            Focus::Term => {
                if let Some(t) = self.terms.get_mut(self.term_at) {
                    t.paste(text);
                }
            }
            Focus::Outline => {}
            Focus::Editor => {
                if let Some((b, tab)) = self.ed() {
                    b.at_cursors(&mut tab.view, editor::Kind::Other, |b, v| b.insert(v, text));
                    if b.readonly.is_none() {
                        tab.preview = false;
                    }
                }
                self.reveal();
                self.find_refresh();
            }
        }
    }
}
