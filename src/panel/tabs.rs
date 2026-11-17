//! `App`: documents, tabs and editor groups.

use super::*;

impl App {
    // ---- documents, tabs, groups --------------------------------------------------

    pub(super) fn file_doc(&mut self, rel: &str) -> String {
        let key = format!("file:{rel}");
        if !self.docs.contains_key(&key) {
            self.docs.insert(key.clone(), Doc::Text(Buffer::open(&self.root.join(rel))));
        }
        key
    }

    /// Show `key` in group `g`: reuse its tab, else replace the group's preview
    /// tab (preview), else add a tab after the active one.
    pub(crate) fn show_tab(&mut self, g: usize, key: String, preview: bool) {
        if let Some(i) = self.groups[g].tabs.iter().position(|t| t.key == key) {
            let grp = &mut self.groups[g];
            grp.active = i;
            if !preview {
                grp.tabs[i].preview = false;
            }
            self.ag = g;
            return;
        }
        // A file opened again comes back where it was (workbench.editor.restoreViewState).
        let view = crate::session::recall(self, &key);
        let grp = &mut self.groups[g];
        if let Some(i) = grp.tabs.iter().position(|t| t.preview).filter(|_| preview) {
            let old = std::mem::replace(&mut grp.tabs[i], Tab { key, preview: true, view });
            grp.active = i;
            crate::session::remember(self, &old.key, &old.view);
            self.gc(&old.key);
        } else {
            let at = if grp.tabs.is_empty() { 0 } else { grp.active + 1 };
            grp.tabs.insert(at, Tab { key, preview, view });
            grp.active = at;
        }
        self.ag = g;
    }

    /// A restored tab's document, first drawn (`session::materialize`): a file (with
    /// its hot-exit backup offered), a working-tree or index diff, or a commit diff.
    pub(crate) fn open_restored(&mut self, key: &str) {
        if let Some(rel) = key.strip_prefix("file:") {
            self.file_doc(rel);
            // One prompt at a time: with another one open, this offer follows it.
            crate::session::offer(self, key);
            self.load_bases();
        } else if let Some((sec, path)) = key.strip_prefix("diff:").and_then(|r| r.split_once(':')) {
            let f = self.scm_files.iter().find(|f| f.path == path && (f.section == Section::Staged) == (sec == "staged")).cloned();
            if let Some(f) = f {
                self.load_diff(key.to_string(), f, None);
            }
        } else if let Some((hash, path)) = key.strip_prefix("cdiff:").and_then(|r| r.split_once(':')) {
            crate::scm::load_commit_diff(self, key, hash, path);
        }
    }

    /// Drop a document nobody shows any more (a dirty one is kept for the quit prompt).
    pub(super) fn gc(&mut self, key: &str) {
        let used = self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key));
        let dirty = matches!(self.docs.get(key), Some(Doc::Text(b)) if b.dirty());
        if !used {
            self.md_views.remove(key);
        }
        if !used && !dirty {
            self.drop_doc(key);
        }
        // A Markdown preview (a patch's diff) shown in place of its source: the source goes
        // with it.
        if let Some((rel, _)) = other_look(key).filter(|_| !used && !key.starts_with("file:")) {
            self.gc(&format!("file:{rel}"));
        }
    }

    /// Forget document `key` and everything kept per document, in one place: the
    /// buffer or diff, its loads (and its index base's), the quick diff and its peek,
    /// a diff's minimap buffer, its conflict markers. Backups are bookkept by
    /// `sync_backups`, which also deletes the file of one no longer needed.
    pub(super) fn drop_doc(&mut self, key: &str) {
        self.docs.remove(key);
        self.patch_stamps.remove(key);
        self.logs.forget(key);
        self.loading.remove(key);
        self.loading.remove(&format!("base:{key}"));
        self.qdiff.remove(key);
        self.diff_minis.remove(key);
        self.diff_syntax.remove(key);
        self.gitui.cmds.conflicts.remove(key);
        crate::session::forget(self, key);
        if self.peek.as_ref().is_some_and(|p| p.key == key) {
            self.peek = None;
        }
        if self.diff_cancel.as_ref().is_some_and(|(k, _)| k == key) {
            if let Some((_, stop)) = self.diff_cancel.take() {
                stop.store(true, Ordering::SeqCst);
            }
        }
    }

    pub fn open_file(&mut self, g: usize, rel: &str, preview: bool) {
        // Shown here as its other look (the source turned into it): that tab.
        let fk = format!("file:{rel}");
        if let Some(grp) = self.groups.get(g).filter(|grp| !grp.tabs.iter().any(|t| t.key == fk)) {
            let other = other_look(&fk).map(|(_, look)| format!("{look}{rel}"));
            if let Some(ok) = other.filter(|ok| grp.tabs.iter().any(|t| t.key == *ok)) {
                return self.show_tab(g, ok, preview);
            }
        }
        let fresh = !self.docs.contains_key(&fk);
        let key = self.file_doc(rel);
        self.show_tab(g, key.clone(), preview);
        if fresh {
            self.offer_restore(&key);
            self.load_bases();
        }
        self.find_refresh();
    }

    pub(super) fn open_file_at(&mut self, rel: &str, line: usize, col: usize) {
        self.open_file(self.ag, rel, true);
        // A place in the text: the source, not the preview.
        self.md_show(self.ag, false);
        let q = self.query.clone();
        if let Some((b, tab)) = self.ed() {
            let p = b.clamp(Pos::new(line.saturating_sub(1), col.saturating_sub(1)));
            let end = Pos::new(p.line, p.col + q.len());
            let hit = b.lines[p.line].get(p.col..end.col).is_some_and(|s| s.eq_ignore_ascii_case(&q));
            tab.view.anchor = hit.then_some(p);
            tab.view.cursor = if hit { end } else { p };
            tab.view.scroll = p.line.saturating_sub(5);
        }
        self.reveal();
    }

    pub fn open_diff(&mut self, i: usize, preview: bool) {
        let Some(f) = self.scm_files.get(i).cloned() else { return };
        let sec = if f.section == Section::Staged { "staged" } else { "changes" };
        let key = format!("diff:{sec}:{}", f.path);
        self.show_tab(self.ag, key.clone(), preview);
        // Clicking through rows replaces the preview: stop the diff nobody shows now.
        if let Some((old, flag)) = self.diff_cancel.take() {
            if old != key && !self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == old)) {
                flag.store(true, Ordering::SeqCst);
                self.loading.remove(&old);
            }
        }
        if !self.docs.contains_key(&key) && !self.loading.contains_key(&key) {
            let flag = Arc::new(AtomicBool::new(false));
            self.diff_cancel = Some((key.clone(), flag.clone()));
            self.load_diff(key, f, Some(flag));
        }
    }

    pub fn tab_title(&self, key: &str) -> String {
        match self.docs.get(key) {
            Some(Doc::Diff(d)) => d.title.clone(),
            _ => match key.split_once(':') {
                Some(("preview", rel)) => format!("Preview {}", base(rel)),
                Some(("usage", rel)) => format!("Usage {}", base(rel)),
                Some(("log", rel)) => format!("Log {}", base(rel)),
                // A repository's page; a file of one (its path after the repository).
                Some(("repo", repo)) => repo.to_string(),
                Some(("programmer", _)) => "Programmer".into(),
                Some(("ghfile", rest)) => base(rest.split_once(':').map_or(rest, |(_, p)| p)).to_string(),
                Some(("diff" | "cdiff", rest)) => base(rest.split_once(':').map_or(rest, |(_, p)| p)).to_string(),
                Some((_, rel)) => base(rel).to_string(),
                None => key.to_string(),
            },
        }
    }

    /// Active text editor of the active group.
    pub fn ed(&mut self) -> Option<(&mut Buffer, &mut Tab)> {
        let grp = self.groups.get_mut(self.ag)?;
        let tab = grp.tabs.get_mut(grp.active)?;
        match self.docs.get_mut(&tab.key)? {
            Doc::Text(b) => Some((b, tab)),
            Doc::Diff(_) => None,
        }
    }

    pub(crate) fn active_tab(&mut self) -> Option<&mut Tab> {
        let grp = self.groups.get_mut(self.ag)?;
        grp.tabs.get_mut(grp.active)
    }

    /// Keep the active editor's cursor on screen.
    pub fn reveal(&mut self) {
        let (h, w) = self
            .ui
            .groups
            .get(self.ag)
            .map_or((20, 80), |g| {
                let side = self.groups.get(self.ag).and_then(|grp| grp.tabs.get(grp.active))
                    .filter(|t| matches!(self.docs.get(&t.key), Some(Doc::Diff(_)))).map(|t| t.view.side as usize);
                let width = side.and_then(|s| g.diff_text.map(|r| r[s.min(1)].width)).unwrap_or(g.text.width);
                (g.text.height.max(1) as usize, width.max(1) as usize)
            });
        if let Some((d, v)) = self.diff_tab() {
            if v.cursor.line < v.scroll {
                v.scroll = v.cursor.line;
            } else if v.cursor.line >= v.scroll + h {
                v.scroll = v.cursor.line + 1 - h;
            }
            let text = diff_cell(d, v.cursor.line, v.side).unwrap_or("");
            let x = editor::display_col(text, v.cursor.col);
            if x < v.hscroll {
                v.hscroll = x;
            } else if x >= v.hscroll + w {
                v.hscroll = x + 1 - w;
            }
            return;
        }
        // Rows the peek zone takes above the cursor are not text rows.
        let zone = self.ui.groups.get(self.ag).and_then(|g| g.zone).map_or(0, |(_, zh)| zh as usize);
        let before = self.peek.as_ref().filter(|p| p.group == self.ag).and_then(|p| self.qdiff.get(&p.key)?.hunks.get(p.hunk)).map(Hunk::peek_before);
        let wrap = self.active_tab_ref().is_some_and(|t| self.wraps(&t.key));
        let Some((b, tab)) = self.ed() else { return };
        let v = &mut tab.view;
        let c = b.clamp(v.cursor);
        v.unfold_at(c.line);
        let h = if before.is_some_and(|l| c.line >= l) { h.saturating_sub(zone).max(1) } else { h };
        if wrap {
            // Rows, not lines: walk up from the cursor's row while the rows still fit.
            v.hscroll = 0;
            if c.line < v.scroll {
                v.scroll = c.line;
                return;
            }
            let rows = |l: usize| editor::wrap_starts(&b.lines[l], w).len();
            let line = &b.lines[c.line];
            let mut used = editor::wrap_row(&editor::wrap_starts(line, w), editor::display_col(line, c.col)) + 1;
            let mut top = c.line;
            while top > v.scroll {
                let prev = v.shift_line(top, -1);
                if prev >= top || used + rows(prev) > h {
                    break;
                }
                used += rows(prev);
                top = prev;
            }
            if top > v.scroll {
                v.scroll = top;
            }
            return;
        }
        if c.line < v.scroll {
            v.scroll = c.line;
        } else if v.row_of(c.line) >= v.row_of(v.scroll) + h {
            v.scroll = v.line_of(v.row_of(c.line) + 1 - h);
        }
        let x = editor::display_col(&b.lines[c.line], c.col);
        if x < v.hscroll {
            v.hscroll = x;
        } else if x >= v.hscroll + w {
            v.hscroll = x + 1 - w;
        }
    }

    /// Double-click on a tab: keep it open (upright), or make it the group's preview again
    /// (italic, replaced by the next file opened as a preview). One preview a group: the one
    /// it had is kept open instead. A tab with unsaved changes stays open.
    pub(super) fn toggle_keep_open(&mut self, g: usize, i: usize) {
        let Some(t) = self.groups.get(g).and_then(|grp| grp.tabs.get(i)) else { return };
        if t.preview {
            self.groups[g].tabs[i].preview = false;
        } else if matches!(self.docs.get(&t.key), Some(Doc::Text(b)) if b.dirty()) {
            self.msg = "unsaved changes keep the tab open".into();
        } else {
            for (k, t) in self.groups[g].tabs.iter_mut().enumerate() {
                t.preview = k == i;
            }
        }
    }

    pub(super) fn pin_active(&mut self) {
        if let Some(t) = self.active_tab() {
            t.preview = false;
        }
    }

    pub(super) fn cycle_tab(&mut self, d: isize) {
        if let Some(grp) = self.groups.get_mut(self.ag) {
            if !grp.tabs.is_empty() {
                grp.active = (grp.active as isize + d).rem_euclid(grp.tabs.len() as isize) as usize;
            }
        }
        self.find_refresh();
    }

    /// Close tab `i` of group `g`; asks first when it is the last view of a dirty file.
    pub fn close_tab(&mut self, g: usize, i: usize, force: bool) {
        let Some(key) = self.groups.get(g).and_then(|grp| grp.tabs.get(i)).map(|t| t.key.clone()) else { return };
        // A Markdown preview (a patch's diff) in place of its unsaved source: closed as that
        // source (asks).
        if let Some((rel, _)) = other_look(&key).filter(|_| !key.starts_with("file:")) {
            let fk = format!("file:{rel}");
            let hidden = !self.groups.iter().any(|grp| grp.tabs.iter().any(|t| t.key == fk));
            if hidden && matches!(self.docs.get(&fk), Some(Doc::Text(b)) if b.dirty()) {
                self.groups[g].active = i;
                self.md_show(g, false);
                return self.close_tab(g, i, force);
            }
        }
        let refs = self.groups.iter().map(|grp| grp.tabs.iter().filter(|t| t.key == key).count()).sum::<usize>();
        let dirty = matches!(self.docs.get(&key), Some(Doc::Text(b)) if b.dirty());
        if !force && refs == 1 && dirty {
            self.prompt = Some(Prompt {
                text: vec![
                    format!("Do you want to save the changes you made to {}?", base(&key)),
                    "Your changes will be lost if you don't save them.".into(),
                ],
                buttons: vec![
                    ("Save".into(), PAct::SaveClose(key.clone())),
                    ("Don't Save".into(), PAct::DropClose(key.clone())),
                    ("Cancel".into(), PAct::Cancel),
                ],
                sel: 0,
            });
            return;
        }
        let grp = &mut self.groups[g];
        let tab = grp.tabs.remove(i);
        if i < grp.active || grp.active >= grp.tabs.len() {
            grp.active = grp.active.saturating_sub(1);
        }
        if grp.tabs.is_empty() && self.groups.len() > 1 {
            self.groups.remove(g);
            if self.ag >= g && self.ag > 0 {
                self.ag -= 1;
            }
        }
        crate::session::remember(self, &tab.key, &tab.view);
        if refs == 1 {
            // Closing the last view (after Save or an explicit Don't Save) also
            // retires its hot-exit backup.
            self.drop_doc(&key);
            self.sync_backups();
        }
        if refs == 1 && key == crate::programmer::KEY {
            crate::programmer::closed(self);
        }
        // A preview's (a patch diff's) source shown nowhere goes with it.
        if let Some((rel, _)) = other_look(&key).filter(|_| !key.starts_with("file:")) {
            self.gc(&format!("file:{rel}"));
        }
        self.find_refresh();
    }

    /// The (group, index) currently showing `key`, preferring the active group.
    pub(super) fn tab_of(&self, key: &str) -> Option<(usize, usize)> {
        let find = |g: usize| self.groups.get(g)?.tabs.iter().position(|t| t.key == key).map(|i| (g, i));
        find(self.ag).or_else(|| (0..self.groups.len()).find_map(find))
    }

    pub(super) fn save_active(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        tab.preview = false;
        let key = tab.key.clone();
        if !matches!(self.docs.get(&key), Some(Doc::Text(_))) {
            return;
        }
        self.msg = match self.save_doc(&key) {
            Ok(()) => format!("saved {}", base(&key)),
            Err(e) => e,
        };
        // Saving settings.json in this editor applies it at once.
        self.check_settings();
    }

    pub fn request_quit(&mut self) {
        if self.settings_dlg.is_some() {
            return crate::settingsui::key(self, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        }
        // Queued or running git writes (a commit, a stage...) would be lost: ask, Wait first.
        let w = self.gitui.writes();
        if w > 0 && !self.gitui.quit_anyway {
            self.prompt = Some(Prompt {
                text: vec![
                    format!("Git is still writing ({w} operation{}).", if w == 1 { "" } else { "s" }),
                    "Wait quits when it is done; Quit Anyway lets it finish after the window closes.".into(),
                ],
                buttons: vec![("Wait".into(), PAct::Git(crate::scm::GitAct::QuitWait)), ("Quit Anyway".into(), PAct::Git(crate::scm::GitAct::QuitAnyway))],
                sel: 0,
            });
            return;
        }
        // A program in the terminal ends with the window: ask first.
        if !self.term_quit_ok {
            if let Some(p) = self.terms.iter().find_map(crate::term::Term::running) {
                self.prompt = Some(Prompt {
                    text: vec![format!("{p} is still running in the terminal."), "Quitting ends it.".into()],
                    buttons: vec![("Quit".into(), PAct::QuitEndTerminal), ("Cancel".into(), PAct::Cancel)],
                    sel: 1,
                });
                return;
            }
        }
        let n = self.docs.values().filter(|d| matches!(d, Doc::Text(b) if b.dirty())).count();
        if n == 0 {
            self.quit = true;
            return;
        }
        self.prompt = Some(Prompt {
            text: vec![
                format!("Do you want to save the changes to {n} file(s)?"),
                "Your changes will be lost if you don't save them.".into(),
            ],
            buttons: vec![
                ("Save All".into(), PAct::SaveAllQuit),
                ("Don't Save".into(), PAct::QuitNoSave),
                ("Cancel".into(), PAct::Cancel),
            ],
            sel: 0,
        });
    }

    pub(crate) fn run_prompt(&mut self, act: PAct) {
        self.prompt = None;
        match act {
            PAct::Cancel => {}
            PAct::Discard(files) => {
                let Some(g) = self.git_root.clone() else { return };
                // The index writer runs it, maybe after a slow op queued ahead: it acts only
                // on the files as they were when the user confirmed.
                let seen = file_stamps(&g, &files);
                self.gitui.write("discard", move || {
                    if file_stamps(&g, &files) != seen {
                        return crate::scm::Msg::Wrote(Err("File changed since you confirmed; discard cancelled".into()), true);
                    }
                    let r = git::discard(&g, &files).map(|_| format!("discarded {} file(s)", files.len()));
                    crate::gitops::refresh_index(&g);
                    crate::scm::Msg::Wrote(r, true)
                });
            }
            PAct::SaveClose(key) => match self.save_doc(&key) {
                Ok(()) => {
                    if let Some((g, i)) = self.tab_of(&key) {
                        self.close_tab(g, i, true);
                    }
                }
                Err(e) => self.msg = e,
            },
            PAct::DropClose(key) => {
                if let Some((g, i)) = self.tab_of(&key) {
                    self.close_tab(g, i, true);
                }
            }
            PAct::Git(a) => crate::scm::run_act(self, a),
            PAct::SaveAllQuit => {
                let dirty: Vec<String> = self
                    .docs
                    .iter()
                    .filter(|(_, d)| matches!(d, Doc::Text(b) if b.dirty()))
                    .map(|(k, _)| k.clone())
                    .collect();
                let errs: Vec<String> = dirty
                    .iter()
                    .filter_map(|k| self.save_doc(k).err().map(|e| format!("{}: {e}", base(k))))
                    .collect();
                if errs.is_empty() {
                    self.quit = true;
                } else {
                    self.msg = errs.join("; ");
                }
            }
            PAct::GitHub(a) => crate::github::run(self, a),
            PAct::Prog(op) => crate::programmer::run(self, op),
            PAct::Move(rels, into) => self.move_into(&rels, &into),
            PAct::Delete(rels, forever) => self.delete_paths(&rels, forever),
            PAct::KillTerminal(i) => self.kill_terminal(i, false),
            PAct::QuitEndTerminal => {
                self.term_quit_ok = true;
                self.request_quit();
                self.term_quit_ok = self.quit; // asked again next time if Cancel follows
            }
            PAct::QuitNoSave => {
                // An explicit Don't Save: drop the edits and their backups.
                let dirty: Vec<String> = self.docs.iter().filter(|(_, d)| matches!(d, Doc::Text(b) if b.dirty())).map(|(k, _)| k.clone()).collect();
                for k in dirty {
                    self.drop_doc(&k);
                }
                self.sync_backups();
                self.quit = true;
            }
            PAct::RestoreBackup(key, file) => {
                let rec = key.strip_prefix("file:").and_then(|rel| editor::read_backup(&file, &self.root.join(rel)));
                let restored = match (rec, self.tab_of(&key)) {
                    (Some(rec), Some((g, i))) => {
                        let tab = &mut self.groups[g].tabs[i];
                        tab.preview = false;
                        match self.docs.get_mut(&key) {
                            Some(Doc::Text(b)) => b.restore_backup(&mut tab.view, &rec),
                            _ => false,
                        }
                    }
                    _ => false,
                };
                self.msg = if restored {
                    format!("restored unsaved changes to {}", base(&key))
                } else {
                    format!("could not restore {} (backup kept)", base(&key))
                };
                self.sync_backups();
                // Our own backup now holds the restored text: retire the chosen one,
                // unless it is that very file or ours could not be written.
                let ours = key.strip_prefix("file:").zip(self.backup_dir.as_ref()).map(|(rel, dir)| {
                    editor::backup_file(dir, &self.backup_scope, &self.backup_instance, &self.root.join(rel))
                });
                if restored && self.backed.contains_key(&key) && ours.as_ref() != Some(&file) {
                    let _ = fs::remove_file(&file);
                }
            }
            PAct::DiscardBackup(file) => {
                let _ = fs::remove_file(&file);
            }
        }
    }
}
