//! `App`: the quick diff in the gutter and its peek.

use super::*;

impl App {
    // ---- quick diff ------------------------------------------------------------

    /// Fetch the index version of every open file (after open, stage, focus...).
    pub(super) fn load_bases(&mut self) {
        let keys: Vec<String> = self
            .docs
            .iter()
            .filter(|(k, d)| k.starts_with("file:") && matches!(d, Doc::Text(b) if b.readonly.is_none()))
            .map(|(k, _)| k.clone())
            .collect();
        self.qdiff.retain(|k, _| keys.contains(k));
        for key in keys {
            // No repo, or outside it: no base, no gutter marks, no git call.
            let Some(rel) = self.repo_path(&key["file:".len()..]) else { continue };
            self.load(format!("base:{key}"), move |root| Load::Base(git::blob(&root, &format!(":0:{rel}"))));
        }
    }

    /// Recompute the gutter marks of buffers edited since their last diff.
    pub(super) fn update_qdiff(&mut self) {
        self.qdiff_due = None;
        for (key, qd) in self.qdiff.iter_mut() {
            let Some(Doc::Text(b)) = self.docs.get(key) else { continue };
            if qd.ver == Some(b.version()) && qd.marks.len() == b.lines.len() {
                continue;
            }
            qd.ver = Some(b.version());
            (qd.marks, qd.hunks) = match &qd.base {
                Some(base) => decor::quick_diff(base, &b.lines),
                None => (Vec::new(), Vec::new()),
            };
        }
        // The peek follows its hunks: the same index if it still exists, else gone.
        if let Some(pk) = &mut self.peek {
            match self.qdiff.get(&pk.key).map_or(0, |q| q.hunks.len()) {
                0 => self.peek = None,
                n => pk.hunk = pk.hunk.min(n - 1),
            }
        }
    }

    // ---- quick-diff peek ----------------------------------------------------------

    /// The active editor's document key and cursor line.
    pub(super) fn active_text(&self) -> Option<(usize, String, usize)> {
        let grp = self.groups.get(self.ag)?;
        let tab = grp.tabs.get(grp.active)?;
        matches!(self.docs.get(&tab.key), Some(Doc::Text(_))).then(|| (self.ag, tab.key.clone(), tab.view.cursor.line))
    }

    pub(super) fn hunks(&self, key: &str) -> &[Hunk] {
        self.qdiff.get(key).map_or(&[][..], |q| &q.hunks)
    }

    /// Open the peek on hunk `i` of group `g`'s active document and bring it into view.
    pub(super) fn show_peek(&mut self, g: usize, key: String, i: usize) {
        let Some(h) = self.hunks(&key).get(i).cloned() else { return };
        self.peek = Some(Peek { group: g, key, hunk: i, scroll: 0 });
        self.ag = g;
        self.focus = Focus::Editor;
        if let Some(t) = self.groups.get_mut(g).and_then(|grp| grp.tabs.get_mut(grp.active)) {
            t.view.collapsed.clear();
            t.view.rebuild_folds();
            let rows = self.ui.groups.get(g).map_or(20, |gh| gh.text.height as usize);
            // As VS Code: the cursor on the hunk's last line, and the zone below it on
            // screen even for a hunk taller than the view (context above when it fits).
            if h.mark_line() < t.view.scroll || h.peek_before() + rows / 3 > t.view.scroll + rows {
                t.view.scroll = h.mark_line().saturating_sub(3).max((h.peek_before() + rows / 3).saturating_sub(rows));
            }
            t.view.cursor = Pos::new(if h.cur.is_empty() { h.mark_line() } else { h.cur.end - 1 }, 0);
            t.view.anchor = None;
        }
    }

    /// A click on the gutter marker of `line`.
    pub(super) fn peek_at_line(&mut self, g: usize, line: usize) {
        let Some(key) = self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).map(|t| t.key.clone()) else { return };
        if let Some(i) = self.hunks(&key).iter().position(|h| h.has_line(line)) {
            self.show_peek(g, key, i);
        }
    }

    /// Alt+F5 / Shift+Alt+F5 (and the palette): the next / previous change from
    /// the peek or the cursor, wrapping around, shown in the peek.
    pub fn peek_step(&mut self, d: isize) {
        let Some((g, key, line)) = self.active_text() else { return };
        let n = self.hunks(&key).len();
        if n == 0 {
            self.msg = "No changes".into();
            return;
        }
        let i = match self.peek.as_ref().filter(|p| p.key == key && p.group == g) {
            Some(p) => (p.hunk as isize + d).rem_euclid(n as isize) as usize,
            None if d > 0 => self.hunks(&key).iter().position(|h| h.mark_line() > line).unwrap_or(0),
            None => self.hunks(&key).iter().rposition(|h| h.mark_line() < line).unwrap_or(n - 1),
        };
        self.show_peek(g, key, i);
    }

    /// Revert: the hunk's lines become the index version again, in the buffer
    /// (one undo step; not a git operation).
    /// The peek's hunk still describes the buffer (the diff runs 200 ms after an
    /// edit): else re-diff now and ask again, rather than act on moved lines.
    pub(super) fn peek_fresh(&mut self) -> bool {
        let Some(pk) = self.peek.as_ref() else { return false };
        let ver = match self.docs.get(&pk.key) {
            Some(Doc::Text(b)) => b.version(),
            _ => return false,
        };
        if self.qdiff.get(&pk.key).is_some_and(|q| q.ver == Some(ver)) {
            return true;
        }
        self.update_qdiff();
        self.msg = "Changes moved, try again".into();
        false
    }

    pub(super) fn peek_revert(&mut self) {
        if !self.peek_fresh() {
            return;
        }
        let Some(pk) = self.peek.clone() else { return };
        let Some(q) = self.qdiff.get(&pk.key) else { return };
        if !q.base_utf8 {
            self.msg = "the index version is not UTF-8: Revert is disabled".into();
            return;
        }
        let (Some(base), Some(h)) = (q.base.as_ref(), q.hunks.get(pk.hunk).cloned()) else { return };
        let with = base[h.base.clone()].to_vec();
        let Some(tab) = self.groups.get_mut(pk.group).and_then(|grp| grp.tabs.iter_mut().find(|t| t.key == pk.key)) else { return };
        let ok = match self.docs.get_mut(&pk.key) {
            Some(Doc::Text(b)) => b.replace_lines(&mut tab.view, h.cur.start, h.cur.end, &with),
            _ => false,
        };
        if ok {
            tab.preview = false;
            self.msg = "reverted the change".into();
            self.update_qdiff();
            self.schedule_backup();
        } else {
            self.msg = "read-only: cannot revert".into();
        }
    }

    /// The peek's Stage. git stages the file on disk, so an unsaved buffer is
    /// saved first, after asking (Cancel by default).
    pub(super) fn stage_hunk(&mut self) -> Result<(), String> {
        let pk = self.peek.clone().ok_or("no change selected")?;
        if matches!(self.docs.get(&pk.key), Some(Doc::Text(b)) if b.dirty()) {
            self.prompt = Some(Prompt {
                text: vec!["Save the file and stage this change?".into(), "Staging works on the file as saved.".into()],
                buttons: vec![("Save & Stage".into(), PAct::Git(crate::scm::GitAct::SaveStage(pk.key))), ("Cancel".into(), PAct::Cancel)],
                sel: 1,
            });
            return Ok(());
        }
        self.stage_peek_hunk()
    }

    /// Stage the peek's hunk: the git hunk (index vs the file as git stages it)
    /// over the same buffer lines, through `gitcmds::stage_hunk`, which refreshes
    /// and refetches the gutter base.
    pub(crate) fn stage_peek_hunk(&mut self) -> Result<(), String> {
        let pk = self.peek.clone().ok_or("no change selected")?;
        let g = self.git_root.clone().ok_or("no git repository")?;
        let rel = pk.key.strip_prefix("file:").and_then(|r| self.repo_path(r)).ok_or("not a file in the repository")?;
        let cur = self.qdiff.get(&pk.key).and_then(|q| q.hunks.get(pk.hunk)).map(|h| h.cur.clone()).ok_or("Changes moved, try again")?;
        // The change as the peek shows it: the git hunk must carry exactly these lines.
        let want: Vec<String> = match self.docs.get(&pk.key) {
            Some(Doc::Text(b)) => b.lines.get(cur.clone()).unwrap_or_default().to_vec(),
            _ => return Err("not a text file".into()),
        };
        // All of it on the index writer: the sides, the matching git hunk, the apply.
        self.gitui.write("stage the change", move || {
            let staged = crate::gitops::stage_sides(&g, &rel).and_then(|(index, work)| {
                let h = crate::gitops::hunks(&index, &work)
                    .into_iter()
                    .find(|h| {
                        let n = h.new_start..h.new_start + h.new_len;
                        let lines = String::from_utf8_lossy(&work).split_inclusive('\n').skip(n.start).take(n.len()).map(|l| l.trim_end_matches(['\n', '\r']).to_string()).collect::<Vec<_>>();
                        let same = lines.iter().map(|l| l.trim_end_matches('\r')).eq(want.iter().map(|l| l.trim_end_matches('\r')));
                        let at = if n.is_empty() || cur.is_empty() { n.is_empty() && cur.is_empty() && n.start == cur.start } else { n.start < cur.end && cur.start < n.end };
                        at && same
                    })
                    .ok_or("Changes moved, try again")?;
                crate::gitops::stage_hunk(&g, &rel, &h)
            });
            crate::gitops::refresh_index(&g);
            crate::scm::Msg::Wrote(staged.map(|_| "staged the change".into()), false)
        });
        self.peek = None;
        self.msg = "staging the change…".into();
        Ok(())
    }

    pub(super) fn peek_button(&mut self, b: PeekBtn) {
        match b {
            PeekBtn::Stage => {
                if !self.peek_fresh() {
                    return;
                }
                if let Err(e) = self.stage_hunk() {
                    self.msg = e;
                }
            }
            PeekBtn::Revert => self.peek_revert(),
            PeekBtn::Prev => self.peek_step(-1),
            PeekBtn::Next => self.peek_step(1),
            PeekBtn::Close => self.peek = None,
        }
    }

    pub(super) fn schedule_qdiff(&mut self) {
        let stale = self.qdiff.iter().any(|(k, q)| matches!(self.docs.get(k), Some(Doc::Text(b)) if q.ver != Some(b.version())));
        if stale && self.qdiff_due.is_none() {
            self.qdiff_due = Some(Instant::now() + QDIFF_DEBOUNCE);
        }
    }

    /// Files under the root with a backup no running panel owns.
    pub(super) fn pending_backups(&self) -> usize {
        let Some(dir) = &self.backup_dir else { return 0 };
        let files: std::collections::HashSet<String> = editor::orphan_backups(dir, PANEL_COMM)
            .into_iter()
            .filter_map(|(_, v)| v["path"].as_str().map(String::from))
            .filter(|p| Path::new(p).starts_with(&self.root))
            .collect();
        files.len()
    }

    /// Write backups for dirty buffers whose current version is not on disk yet;
    /// delete backups of buffers that became clean or were closed.
    pub fn sync_backups(&mut self) {
        self.backup_due = None;
        self.backup_first = None;
        let Some(dir) = self.backup_dir.clone() else { return };
        let mut seen = Vec::new();
        for (key, doc) in &self.docs {
            let Doc::Text(b) = doc else { continue };
            if b.path.is_none() || b.readonly.is_some() {
                continue;
            }
            if b.dirty() {
                seen.push(key.clone());
                if self.backed.get(key) != Some(&b.version()) && editor::write_backup(&dir, &self.backup_scope, &self.backup_instance, b).is_ok() {
                    self.backed.insert(key.clone(), b.version());
                }
            }
        }
        let stale: Vec<String> = self.backed.keys().filter(|k| !seen.contains(k)).cloned().collect();
        for key in stale {
            self.backed.remove(&key);
            if let Some(rel) = key.strip_prefix("file:") {
                editor::remove_backup(&dir, &self.backup_scope, &self.backup_instance, &self.root.join(rel));
            }
        }
    }

    /// After an edit: back up about 1 s after the last change, and no later than
    /// 5 s after the first unsaved one even while typing continues.
    pub(super) fn schedule_backup(&mut self) {
        let pending = self.docs.iter().any(|(k, d)| matches!(d, Doc::Text(b) if b.dirty() && self.backed.get(k) != Some(&b.version())));
        let clean_backed = self.backed.keys().any(|k| !matches!(self.docs.get(k), Some(Doc::Text(b)) if b.dirty()));
        if pending {
            let now = Instant::now();
            let first = *self.backup_first.get_or_insert(now);
            self.backup_due = Some((now + Duration::from_secs(1)).min(first + Duration::from_secs(5)));
        } else if clean_backed {
            self.sync_backups();
        }
    }

    /// Back up, then save, then drop the backup. A clean buffer is not written
    /// at all; a failed write leaves the buffer dirty with its backup kept, so
    /// closing still prompts.
    pub(crate) fn save_doc(&mut self, key: &str) -> Result<(), String> {
        let Some(Doc::Text(b)) = self.docs.get(key) else { return Ok(()) };
        if !b.dirty() {
            return Ok(());
        }
        let (trim, newline) = (self.settings.trim_trailing, self.settings.final_newline);
        let mut spare = View::default();
        let view = self.groups.iter_mut().flat_map(|g| g.tabs.iter_mut()).find(|t| t.key == key).map_or(&mut spare, |t| &mut t.view);
        let Some(Doc::Text(b)) = self.docs.get_mut(key) else { return Ok(()) };
        b.tidy(view, trim, newline);
        let b = &*b;
        if let Some(dir) = &self.backup_dir {
            editor::write_backup(dir, &self.backup_scope, &self.backup_instance, b).map_err(|e| format!("backup before save failed: {e}"))?;
            self.backed.insert(key.to_string(), b.version());
        }
        let r = match self.docs.get_mut(key) {
            Some(Doc::Text(b)) => b.save(),
            _ => Ok(()),
        };
        self.sync_backups();
        r
    }

    /// A file just opened has backups no running panel owns (any session or
    /// panel): offer the newest; the others wait for the next open.
    pub(crate) fn offer_restore(&mut self, key: &str) {
        let (Some(dir), Some(rel)) = (&self.backup_dir, key.strip_prefix("file:")) else { return };
        let path = self.root.join(rel);
        let mine = path.to_string_lossy();
        let found: Vec<(PathBuf, serde_json::Value)> =
            editor::orphan_backups(dir, PANEL_COMM).into_iter().filter(|(_, v)| v["path"].as_str() == Some(&mine)).collect();
        let Some((file, rec)) = found.first().cloned() else { return };
        let mut text = vec![
            format!("{} has unsaved changes from a closed session.", rel),
            "Restore them into the editor, or discard the backup?".into(),
        ];
        if found.len() > 1 {
            text.push(format!("{} backups found: this is the newest.", found.len()));
        }
        if Buffer::backup_is_stale(&rec, &path) {
            text.push("The file changed on disk after the backup was taken.".into());
        }
        self.prompt = Some(Prompt {
            text,
            buttons: vec![
                ("Restore".into(), PAct::RestoreBackup(key.to_string(), file.clone())),
                ("Discard Backup".into(), PAct::DiscardBackup(file)),
            ],
            sel: 0,
        });
    }

    pub(super) fn event_loop<B: Backend>(&mut self, t: &mut Terminal<B>) -> io::Result<()> {
        let mut dirty = true;
        while !self.quit {
            if SIGNALED.load(Ordering::SeqCst) {
                return Ok(()); // run() flushes the backups
            }
            self.step(t, &mut dirty, next_event)?;
        }
        Ok(())
    }

    /// One turn of the event loop: timers, background results, then the frame (so what
    /// `drain` brought is on screen before the wait), then at most one input event from
    /// `next`, given how long it may wait. `dirty` carries a redraw the event asked for.
    /// Returns that wait.
    pub(crate) fn step<B: Backend>(
        &mut self,
        t: &mut Terminal<B>,
        dirty: &mut bool,
        mut next: impl FnMut(Duration) -> io::Result<Option<Event>>,
    ) -> io::Result<Duration> {
        if self.backup_due.is_some_and(|t| Instant::now() >= t) {
            self.sync_backups();
        }
        if self.qdiff_due.is_some_and(|t| Instant::now() >= t) {
            self.update_qdiff();
            *dirty = true;
        }
        if self.search_due.is_some_and(|t| Instant::now() >= t) {
            self.search_due = None;
            self.start_search();
            *dirty = true;
        }
        if self.refresh_due.is_some_and(|t| Instant::now() >= t) {
            self.refresh_due = None;
            self.refresh();
            *dirty = true;
        }
        // Source Control timers: the resting-pointer card, the blame delay, the op spinner.
        if self.poll_status && self.status_poll_due.is_none_or(|t| Instant::now() >= t) {
            if !self.loading.contains_key(STATUS_KEY) {
                self.status_polled = true;
                self.refresh_status();
            }
            self.status_poll_due = Some(Instant::now() + (self.status_took * 10).clamp(STATUS_POLL, STATUS_POLL_MAX));
        }
        if self.upgrade.watch && self.upgrade.next.is_none_or(|t| Instant::now() >= t) {
            self.upgrade.next = Some(Instant::now() + UPGRADE_CHECK);
            let installed = fs::read_link("/proc/self/exe").ok().and_then(|l| replaced(&l));
            let told = self.upgrade.told;
            self.upgrade_tick(installed);
            *dirty |= self.upgrade.told != told;
        }
        if let Some(t) = self.tip.as_mut().filter(|t| !t.shown && t.since.elapsed() >= TIP_DELAY) {
            t.show(&self.root);
            *dirty = true;
        }
        *dirty |= self.tip.as_mut().is_some_and(Tip::tick);
        if JOB_PANICKED.swap(false, Ordering::SeqCst) {
            self.msg = "A background task failed. Please report it: F1 Help: Report Issue copies the details.".into();
            *dirty = true;
        }
        *dirty |= crate::scm::tick(self);
        *dirty |= crate::lint::tick(self);
        *dirty |= crate::session::tick(self);
        *dirty |= self.drain();
        *dirty |= self.term_pump();
        *dirty |= crate::github::tick(self);
        *dirty |= crate::programmer::tick(self);
        *dirty |= crate::xcomm::tick(self);
        *dirty |= self.auto_reveal();
        *dirty |= crate::media::tick(self);
        *dirty |= crate::logview::tick(self);
        *dirty |= self.minimap_auto();
        *dirty |= crate::api::tick(self);
        *dirty |= self.file_job_tick();
        if let Some((path, _)) = self.tree_spring.take_if(|(_, t)| Instant::now() >= *t) {
            if matches!(self.drag, Drag::File { active: true, .. }) {
                if let Some(i) = self.tree.iter().position(|n| n.path == path && n.dir && !n.open) {
                    self.toggle(i);
                    *dirty = true;
                }
            }
        }
        if std::mem::take(dirty) {
            // The terminal shows the frame whole (synchronized output), not half drawn.
            let sync = |on: bool| {
                if !cfg!(test) {
                    let mut out = io::stdout();
                    let _ = out.write_all(if on { b"\x1b[?2026h" } else { b"\x1b[?2026l" }).and_then(|()| out.flush());
                }
            };
            sync(true);
            let drawn = t.draw(|f| crate::ui::draw(self, f));
            sync(false);
            let frame = drawn.map_err(|e| io::Error::other(e.to_string()))?;
            crate::api::drawn(self, frame.buffer);
            self.last_draw = Some(Instant::now());
            if matches!(self.drag, Drag::ClosePending) {
                self.drag = Drag::None;
                if self.prompt.is_none() && self.hover.is_some_and(|p| self.ui.close.contains(p)) {
                    self.request_quit();
                    *dirty = true;
                    return Ok(Duration::ZERO);
                }
            }
        }
        // Wake often only while rg/git output streams in or a backup is pending,
        // so a HUP is noticed within 100 ms whenever there is something to save.
        let busy = self.search.is_some()
            || crate::api::waiting(self)
            || self.queued_search.is_some()
            || !self.loading.is_empty()
            || self.backup_due.is_some()
            || self.nav.busy()
            || self.gitui.busy()
            || self.gitui.refs.busy()
            || self.lints.busy()
            || self.gh.busy()
            || self.file_job.is_some()
            || self.prog.busy()
            || self.logs.busy()
            || self.tip.as_ref().is_some_and(|t| t.md5_rx.is_some());
        let mut wait = if busy { Duration::from_millis(100) } else { Duration::from_secs(5) };
        // A minimap slider held at its edge: the editor stepping on.
        if self.mini_auto.is_some() {
            wait = wait.min(super::mouse::MINI_STEP);
        }
        // A log in a tab: its file looked at for new lines twice a second.
        if self.logs.watching() {
            wait = wait.min(Duration::from_millis(500));
        }
        let tip_due = self.tip.as_ref().filter(|t| !t.shown).map(|t| t.since + TIP_DELAY);
        let upgrade_due = self.upgrade.next.filter(|_| self.upgrade.watch);
        let poll_due = self.status_poll_due.filter(|_| self.poll_status);
        for t in [self.tree_spring.as_ref().map(|s| s.1), self.qdiff_due, self.gitui.due(), self.refresh_due, self.session.due, self.nav.hover.due, tip_due, upgrade_due, self.search_due, poll_due, self.lints.due()].into_iter().flatten() {
            wait = wait.min(t.saturating_duration_since(Instant::now()));
        }
        // A far jump's first highlighting catches up one budgeted slice per frame
        // (editor::FrameBudget): draw again soon, input still comes first.
        if self.hl_pending() {
            *dirty = true;
            wait = wait.min(Duration::from_millis(2));
        }
        // A key or click held back by the last batch runs now, after that batch's frame.
        let ev = match self.pending_ev.take() {
            Some(ev) => Some(ev),
            None => match next(wait) {
                Ok(ev) => ev,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Ok(wait),
                Err(e) => return Err(e),
            },
        };
        let Some(ev) = ev else { return Ok(wait) };
        let batch = coalescable(&ev);
        *dirty |= self.handle_event(ev);
        if batch && coalesce() {
            // Wheel and plain motion come in bursts: take every one queued, and those that
            // arrive until a frame is due (FRAME after the last), then draw once. A key or
            // click ends the batch and waits for that frame: clicks hit-test the drawn
            // layout, and keys are never held for the cap.
            let start = Instant::now();
            let due = self.last_draw.map_or(start, |d| (d + FRAME).max(start));
            loop {
                let now = Instant::now();
                // A stream that never pauses still gets a frame every FRAME of handling.
                if now >= due.max(start) + FRAME {
                    break;
                }
                match next(due.saturating_duration_since(now)) {
                    Ok(Some(e)) if coalescable(&e) => *dirty |= self.handle_event(e),
                    Ok(Some(e)) => {
                        self.pending_ev = Some(e);
                        break;
                    }
                    Ok(None) => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => break,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(if self.pending_ev.is_some() { Duration::ZERO } else { wait })
    }

    pub(super) fn handle_event(&mut self, ev: Event) -> bool {
        let redraw = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                self.tip = None;
                crate::navui::on_key(self, k);
                self.schedule_backup();
                crate::session::touch(self);
                true
            }
            Event::Mouse(mut m) => {
                let mut tip = false;
                if m.kind != MouseEventKind::Moved {
                    crate::session::touch(self); // clicks, drags, wheel: tabs, cursors, scroll
                    let button = matches!(m.kind, MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left));
                    let p = Position::new(m.column, m.row);
                    // The MD5 line's Copy button copies; elsewhere a click goes to the row under
                    // the line (the tooltip closes), as if it were not there.
                    if button && self.tip.as_ref().is_some_and(|t| t.shown) && self.ui.tip_copy.contains(p) {
                        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                            self.copy_md5();
                        }
                        return true;
                    }
                    match self.tip.as_ref().filter(|t| t.shown && button && self.ui.tip_ext.contains(p)) {
                        // The grown name is its row: a click there selects, opens or renames it.
                        Some(t) => (m.column, m.row) = (t.x, t.rect.y),
                        None => tip = self.tip.take().is_some_and(|t| t.shown),
                    }
                }
                crate::navui::on_mouse(self, m) || tip
            }
            Event::Paste(s) => {
                crate::hover::dismiss(self);
                self.on_paste(&s);
                self.schedule_backup();
                crate::session::touch(self);
                true
            }
            // Debounced: focus flickers between windows run one refresh.
            Event::FocusGained => {
                if !async_status() {
                    self.refresh();
                    return true;
                }
                self.refresh_due.get_or_insert(Instant::now() + FOCUS_DEBOUNCE);
                false
            }
            Event::FocusLost => {
                let pressed = matches!(self.drag, Drag::Close | Drag::ClosePending);
                if pressed { self.drag = Drag::None; }
                crate::hover::dismiss(self) || pressed
            }
            Event::Resize(..) => {
                if matches!(self.drag, Drag::Close | Drag::ClosePending) { self.drag = Drag::None; }
                crate::hover::dismiss(self);
                true
            }
            _ => false,
        };
        // Any edit (keys, paste, a Replace All click) re-diffs the gutter shortly after.
        self.schedule_qdiff();
        redraw
    }
}
