//! `App`: renaming, moving (a drag onto a folder) and copying (Ctrl+C / Ctrl+X / Ctrl+V)
//! in the Explorer.

use super::*;

impl App {
    // ---- rename ------------------------------------------------------------------

    /// Edit Explorer row `i`'s name in place.
    pub(super) fn start_rename(&mut self, i: usize) {
        let Some(n) = self.tree.get(i) else { return };
        self.rename = Some(Rename::new(&n.path, &n.name, n.dir));
        self.focus = Focus::Side;
        self.tip = None;
    }

    pub(super) fn rename_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.rename = None,
            KeyCode::Enter => self.commit_rename(),
            KeyCode::Char('v') if ctrl => {
                let text = clipboard_get().unwrap_or_else(|| self.clip.clone());
                self.paste_text(&text);
            }
            KeyCode::Char(c @ ('c' | 'x')) if ctrl => {
                let Some(r) = self.rename.as_mut() else { return };
                // Nothing selected: Ctrl+C takes the whole name, Ctrl+X nothing.
                let (a, b) = match r.selection() {
                    Some(s) => s,
                    None if c == 'c' => (0, r.text.len()),
                    None => return,
                };
                let text = r.text[a..b].to_string();
                if c == 'x' {
                    r.take_selection();
                }
                self.msg = format!("copied via {}", clipboard_set(&text));
                self.clip = text;
            }
            _ => {
                if let Some(r) = self.rename.as_mut() {
                    r.key(k);
                }
            }
        }
    }

    /// The renamed item's path for the name in the field.
    pub(super) fn rename_target(r: &Rename) -> String {
        let name = r.text.trim();
        match r.path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{name}"),
            None => name.to_string(),
        }
    }

    /// The name in the rename field is taken by another item, or is not a name: drawn red.
    pub fn rename_conflict(&self) -> bool {
        let Some(r) = &self.rename else { return false };
        let name = r.text.trim();
        if name == r.path.rsplit('/').next().unwrap_or(&r.path) {
            return false;
        }
        name.is_empty() || name.contains('/') || name == "." || name == ".." || fs::symlink_metadata(self.root.join(Self::rename_target(r))).is_ok()
    }

    /// Enter, or a click elsewhere: the new name on disk; a bad or taken one is refused
    /// and stays in the field.
    pub(super) fn commit_rename(&mut self) {
        let Some(r) = self.rename.take() else { return };
        let name = r.text.trim();
        let old_name = r.path.rsplit('/').next().unwrap_or(&r.path);
        if name == old_name {
            return;
        }
        let why = if name.is_empty() {
            Some("A file or folder name must be provided.".to_string())
        } else if name.contains('/') || name == "." || name == ".." {
            Some(format!("The name {name} is not valid as a file or folder name."))
        } else {
            None
        };
        let new = Self::rename_target(&r);
        let res = match why {
            Some(why) => Err(why),
            None => self.rename_path(&r.path, &new),
        };
        match res {
            Ok(()) => {
                self.record(vec![FileOp::Moved { from: r.path.clone(), to: new.clone() }]);
                self.msg = format!("renamed {old_name} to {name}");
            }
            Err(e) => {
                self.msg = e;
                self.rename = Some(r); // still editable: Esc gives up
            }
        }
    }

    /// Move `old` to `new` (both relative to the root) and every open document under it
    /// along: buffers keep their unsaved edits and undo, tabs their place.
    pub(super) fn rename_path(&mut self, old: &str, new: &str) -> Result<(), String> {
        let to = self.root.join(new);
        if fs::symlink_metadata(&to).is_ok() {
            return Err(format!("A file or folder {} already exists at this location.", base(new)));
        }
        fs::rename(self.root.join(old), &to).map_err(|e| format!("rename failed: {e}"))?;
        let moved = |rel: &str| (rel == old || rel.strip_prefix(old).is_some_and(|r| r.starts_with('/'))).then(|| format!("{new}{}", &rel[old.len()..]));
        let keys: Vec<(String, String)> = self.docs.keys().filter_map(|k| Some((k.clone(), moved(k.strip_prefix("file:")?)?))).collect();
        for (key, rel) in keys {
            let doc = self.docs.remove(&key);
            self.drop_doc(&key);
            if let Some(Doc::Text(mut b)) = doc {
                b.path = Some(self.root.join(&rel));
                self.docs.insert(format!("file:{rel}"), Doc::Text(b));
            }
        }
        for t in self.groups.iter_mut().flat_map(|g| g.tabs.iter_mut()) {
            if let Some((kind, rel)) = t.key.split_once(':').filter(|(k, _)| matches!(*k, "file" | "preview")) {
                if let Some(rel) = moved(rel) {
                    t.key = format!("{kind}:{rel}");
                }
            }
        }
        // A renamed folder and the folders in it stay expanded.
        for n in self.tree.iter_mut() {
            if let Some(path) = moved(&n.path) {
                n.path = path;
            }
        }
        self.reload_all();
        self.load_bases();
        if let Some(i) = self.tree.iter().position(|n| n.path == new) {
            self.sel[Side::Explorer as usize] = i;
            if self.side == Side::Explorer {
                self.move_side(0); // into view
            }
        }
        self.sync_backups();
        Ok(())
    }

    // ---- moving and copying (drag and drop, Ctrl+C / Ctrl+X / Ctrl+V) ---------------------

    /// The Explorer's selection: the marked items (Ctrl+click, Shift+click, Shift+↑↓), else
    /// the one under the cursor. What is inside a selected folder goes with the folder.
    pub(super) fn tree_selected(&self) -> Vec<String> {
        let shown = |p: &String| self.tree.iter().any(|n| &n.path == p);
        let mut v: Vec<String> = self.tree_marks.iter().filter(|p| shown(p)).cloned().collect();
        if v.is_empty() {
            v.extend(self.tree.get(self.sel[Side::Explorer as usize]).map(|n| n.path.clone()));
        }
        v.sort();
        v.dedup();
        let all = v.clone();
        v.retain(|p| !all.iter().any(|q| p.starts_with(&format!("{q}/"))));
        v
    }

    /// Ctrl+click: row `i` in or out of the selection; Shift+click or Shift+↑↓ (`range`): the
    /// rows from the anchor to it. The cursor moves to it.
    pub(super) fn tree_mark(&mut self, i: usize, range: bool) {
        let Some(path) = self.tree.get(i).map(|n| n.path.clone()) else { return };
        let s = Side::Explorer as usize;
        if range {
            let anchor = self.tree_anchor.as_ref().and_then(|a| self.tree.iter().position(|n| &n.path == a)).unwrap_or(self.sel[s].min(i));
            self.tree_anchor = Some(self.tree[anchor].path.clone());
            let (a, b) = (anchor.min(i), anchor.max(i));
            self.tree_marks = self.tree[a..=b].iter().map(|n| n.path.clone()).collect();
        } else {
            if self.tree_marks.is_empty() {
                self.tree_marks.extend(self.tree.get(self.sel[s]).map(|n| n.path.clone()));
            }
            match self.tree_marks.iter().position(|p| *p == path) {
                Some(k) => {
                    self.tree_marks.remove(k);
                }
                None => self.tree_marks.push(path.clone()),
            }
            self.tree_anchor = Some(path);
        }
        self.sel[s] = i;
    }

    /// What a drag of Explorer item `rel` carries: the selection when `rel` is in it.
    pub(super) fn dragged(&self, rel: &str) -> Vec<String> {
        if self.tree_marks.iter().any(|m| m == rel) { self.tree_selected() } else { vec![rel.to_string()] }
    }

    /// A plain click or move: one item selected again, the anchor at `i`.
    pub(super) fn tree_unmark(&mut self, i: usize) {
        self.tree_marks.clear();
        self.tree_anchor = self.tree.get(i).map(|n| n.path.clone());
    }

    /// The folder dragged Explorer items `rels` would go into under `p`: a folder row's, a
    /// file row's folder, the root below the rows. None off the Explorer, inside one of them
    /// (a folder), and, for a move (not `copy`), where they all already are.
    pub(super) fn tree_target(&self, p: Position, rels: &[String], copy: bool) -> Option<String> {
        if self.side != Side::Explorer || !self.side_visible || !self.ui.side_list.contains(p) {
            return None;
        }
        let off = self.off[Side::Explorer as usize];
        // A gap between rows belongs to the row above it.
        let i = self.ui.side_item(p.y, off).or_else(|| self.ui.side_item(p.y.saturating_sub(1), off));
        let into = match i.and_then(|i| self.tree.get(i)) {
            Some(n) if n.dir => n.path.clone(),
            Some(n) => parent_of(&n.path).to_string(),
            None => String::new(),
        };
        let inside = rels.iter().any(|rel| into == *rel || into.starts_with(&format!("{rel}/")));
        let stays = !copy && rels.iter().all(|rel| parent_of(rel) == into);
        (!inside && !stays && !rels.is_empty()).then_some(into)
    }

    /// The Copy button of a file's tooltip: its MD5 onto the clipboard.
    pub(super) fn copy_md5(&mut self) {
        let Some((Some(Ok(sum)), file)) = self.tip.as_ref().map(|t| (t.md5.clone(), t.file.clone().unwrap_or_default())) else { return };
        self.msg = format!("copied the MD5 of {} via {}", base(&file), clipboard_set(&sum));
        self.clip = sum;
    }

    /// The name of folder `into` as messages say it (the root: the folder's name).
    fn folder_name(&self, into: &str) -> String {
        if into.is_empty() { self.repo.clone() } else { base(into).to_string() }
    }

    /// True (and said) while a paste runs: file operations wait for it.
    fn file_busy(&mut self) -> bool {
        if self.file_job.is_some() {
            self.msg = "a paste is still running".into();
        }
        self.file_job.is_some()
    }

    /// A drop on a folder: asks first (a drop can land by mistake), or says why it cannot.
    pub(super) fn ask_move(&mut self, rels: Vec<String>, into: String) {
        let shown = self.folder_name(&into);
        if let Some(rel) = rels.iter().find(|rel| fs::symlink_metadata(self.root.join(join_rel(&into, base(rel)))).is_ok()) {
            self.msg = format!("{} already exists in {shown}", base(rel));
            return;
        }
        let text = match rels.as_slice() {
            [one] => vec![format!("Are you sure you want to move '{}' into '{shown}'?", base(one)), "Its open editors follow it.".into()],
            _ => vec![format!("Are you sure you want to move the following {} items into '{shown}'?", rels.len()), names(&rels), "Their open editors follow them.".into()],
        };
        self.prompt = Some(Prompt { text, buttons: vec![("Move".into(), PAct::Move(rels, into)), ("Cancel".into(), PAct::Cancel)], sel: 0 });
    }

    /// Moves `rels` into folder `into` (open editors along, see `rename_path`) and shows them.
    pub(super) fn move_into(&mut self, rels: &[String], into: &str) {
        if self.file_busy() {
            return;
        }
        let mut done = Vec::new();
        let mut err = None;
        for rel in rels {
            let to = join_rel(into, base(rel));
            match self.rename_path(rel, &to) {
                Ok(()) => done.push(FileOp::Moved { from: rel.clone(), to }),
                Err(e) => err = Some(e),
            }
        }
        self.tree_marks.clear();
        if let Some(FileOp::Moved { to, .. }) = done.last() {
            let to = to.clone();
            self.reveal_path(&to);
        }
        let n = done.len();
        self.msg = match (err, rels) {
            (Some(e), _) if n == 0 => e,
            (Some(e), _) => format!("moved {n} of {} items; {e}", rels.len()),
            (None, [one]) => format!("moved {} into {}", base(one), self.folder_name(into)),
            (None, _) => format!("moved {n} items into {}", self.folder_name(into)),
        };
        self.record(done);
    }

    /// Opens the folders down to `rel` and selects it.
    pub(super) fn reveal_path(&mut self, rel: &str) {
        let mut at = String::new();
        for part in parent_of(rel).split('/').filter(|p| !p.is_empty()) {
            at = join_rel(&at, part);
            if let Some(i) = self.tree.iter().position(|n| n.path == at && n.dir && !n.open) {
                self.toggle(i);
            }
        }
        if let Some(i) = self.tree.iter().position(|n| n.path == rel) {
            self.sel[Side::Explorer as usize] = i;
            if self.side == Side::Explorer {
                self.move_side(0); // into view
            }
        }
    }

    /// Ctrl+C (Ctrl+X: `cut`) in the Explorer: the selected items, for Ctrl+V.
    pub(super) fn tree_copy(&mut self, cut: bool) {
        let rels = self.tree_selected();
        let what = match rels.as_slice() {
            [] => return,
            [one] => format!("{}: Ctrl+V puts it in a folder", base(one)),
            _ => format!("{} items: Ctrl+V puts them in a folder", rels.len()),
        };
        self.msg = format!("{} {what}", if cut { "cut" } else { "copied" });
        self.tree_clip = Some((rels, cut));
    }

    /// Ctrl+V in the Explorer: the copied items into the selected folder (a file's: its
    /// folder), each as "name (1)", "name (2)" … where its name is taken; cut ones move there.
    pub(super) fn tree_paste(&mut self) {
        let Some((rels, cut)) = self.tree_clip.clone() else { return };
        let into = match self.tree.get(self.sel[Side::Explorer as usize]) {
            Some(n) if n.dir => n.path.clone(),
            Some(n) => parent_of(&n.path).to_string(),
            None => String::new(),
        };
        let rels: Vec<String> = rels.into_iter().filter(|r| fs::symlink_metadata(self.root.join(r)).is_ok()).collect();
        if rels.is_empty() {
            self.tree_clip = None;
            self.msg = "what was copied is gone".into();
            return;
        }
        if let Some(rel) = rels.iter().find(|rel| into == **rel || into.starts_with(&format!("{rel}/"))) {
            self.msg = format!("{} cannot go inside itself", base(rel));
            return;
        }
        if !cut {
            return self.copy_into(&rels, &into);
        }
        if let Some(rel) = rels.iter().find(|rel| parent_of(rel) == into) {
            self.msg = format!("{} is already there", base(rel));
            return;
        }
        if let Some(rel) = rels.iter().find(|rel| fs::symlink_metadata(self.root.join(join_rel(&into, base(rel)))).is_ok()) {
            self.msg = format!("{} already exists there", base(rel));
            return;
        }
        self.tree_clip = None;
        self.move_into(&rels, &into);
    }

    /// Copies `rels` into folder `into` (Ctrl+V, a Ctrl+drag), each under a free name, on a
    /// thread: a big folder never holds the window. `file_job_tick` finishes it.
    pub(super) fn copy_into(&mut self, rels: &[String], into: &str) {
        if self.file_busy() {
            return;
        }
        let mut taken: Vec<String> = Vec::new();
        let pairs = rels
            .iter()
            .map(|rel| {
                let name = self.free_name(into, base(rel), &taken);
                taken.push(name.clone());
                (rel.clone(), join_rel(into, &name))
            })
            .collect();
        self.tree_marks.clear();
        self.start_copies(pairs, false);
    }

    /// Copies (from, to) pairs on a thread; `redo`: Ctrl+Y's, kept on the undo stack as is.
    fn start_copies(&mut self, pairs: Vec<(String, String)>, redo: bool) {
        let root = self.root.clone();
        let (tx, rx) = mpsc::channel();
        self.msg = match pairs.as_slice() {
            [(from, _)] => format!("pasting {}…", base(from)),
            _ => format!("pasting {} items…", pairs.len()),
        };
        let total = pairs.len();
        let (files, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let p = Progress { files: files.clone(), stop: stop.clone() };
        std::thread::spawn(move || {
            let mut done = Vec::new();
            for (from, to) in pairs {
                if p.stop.load(Ordering::Relaxed) {
                    break;
                }
                let (src, dst) = (root.join(&from), root.join(&to));
                let r = copy_path(&src, &dst, &p).map_err(|e| {
                    // Stopped (Esc) or failed half way: the part copied goes, it was ours (not
                    // when the name turned out taken: that is someone else's).
                    let _ = if e.kind() == io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else if fs::symlink_metadata(&dst).is_ok_and(|m| m.is_dir()) { fs::remove_dir_all(&dst) } else { fs::remove_file(&dst) };
                    if p.stop.load(Ordering::Relaxed) { "stopped".to_string() } else { e.to_string() }
                });
                done.push((FileOp::Copied { from, to }, r));
            }
            let _ = tx.send(done);
        });
        self.file_job = Some(FileJob { rx, redo, files, stop, shown: 0, total });
    }

    /// Esc while a paste runs: it stops after the file it is on (that item is taken out).
    pub(super) fn stop_paste(&mut self) -> bool {
        let Some(job) = &self.file_job else { return false };
        job.stop.store(true, Ordering::Relaxed);
        self.msg = "stopping the paste…".into();
        true
    }

    /// A finished paste: the copies shown and kept for Ctrl+Z, a failure said. True when one ended.
    pub(crate) fn file_job_tick(&mut self) -> bool {
        let Some(job) = &self.file_job else { return false };
        let done: Vec<(FileOp, Result<(), String>)> = match job.rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => {
                // Still copying: how many files so far.
                let n = job.files.load(Ordering::Relaxed);
                if n == job.shown || job.stop.load(Ordering::Relaxed) {
                    return false;
                }
                self.msg = format!("pasting… {n} file{} copied (Esc stops)", if n == 1 { "" } else { "s" });
                if let Some(j) = self.file_job.as_mut() {
                    j.shown = n;
                }
                return true;
            }
            Err(mpsc::TryRecvError::Disconnected) => vec![],
        };
        let job = self.file_job.take().expect("looked at above");
        let (redo, n, stopped) = (job.redo, job.total, job.stop.load(Ordering::Relaxed));
        let err = done.iter().find_map(|(_, r)| r.clone().err());
        let ok: Vec<FileOp> = done.into_iter().filter(|(_, r)| r.is_ok()).map(|(op, _)| op).collect();
        self.reload_all();
        if let Some(FileOp::Copied { to, .. }) = ok.last() {
            let to = to.clone();
            self.reveal_path(&to);
        }
        self.msg = match (&err, ok.as_slice()) {
            _ if stopped && ok.is_empty() => "paste stopped".into(),
            _ if stopped => format!("pasted {} of {n} items, then stopped", ok.len()),
            (Some(e), []) => format!("paste failed: {e}"),
            (Some(e), _) => format!("pasted {} of {n} items; {e}", ok.len()),
            (None, [FileOp::Copied { to, .. }]) => format!("pasted {}", base(to)),
            (None, []) => "paste failed".into(),
            (None, _) => format!("pasted {} items", ok.len()),
        };
        if redo {
            self.push_undo(ok);
        } else {
            self.record(ok);
        }
        true
    }

    /// `name` in folder `dir` if free (and not in `taken`), else the first free of
    /// "stem (1).ext", "stem (2).ext" …
    pub(super) fn free_name(&self, dir: &str, name: &str, taken: &[String]) -> String {
        let used = |n: &str| taken.iter().any(|t| t == n) || fs::symlink_metadata(self.root.join(join_rel(dir, n))).is_ok();
        if !used(name) {
            return name.to_string();
        }
        // The extension stays last (a dotfile's leading dot is not one).
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 && !self.root.join(join_rel(dir, name)).is_dir() => name.split_at(i),
            _ => (name, ""),
        };
        (1..).map(|k| format!("{stem} ({k}){ext}")).find(|n| !used(n)).expect("a free name")
    }
}

/// Up to four items' names for a prompt, "and N more" after.
fn names(rels: &[String]) -> String {
    let mut s = rels.iter().take(4).map(|r| base(r)).collect::<Vec<_>>().join(", ");
    if rels.len() > 4 {
        s += &format!(" and {} more", rels.len() - 4);
    }
    s
}

impl App {
    /// Delete (Shift+Delete: `forever`) in the Explorer: the selection, asked first as VS Code does.
    pub(super) fn ask_delete(&mut self, forever: bool) {
        let rels = self.tree_selected();
        let what = match rels.as_slice() {
            [] => return,
            [one] if self.root.join(one).is_dir() => format!("'{}' and its contents", base(one)),
            [one] => format!("'{}'", base(one)),
            _ => format!("the following {} items", rels.len()),
        };
        let mut text = if forever {
            vec![format!("Are you sure you want to permanently delete {what}?"), "This can't be undone.".into()]
        } else {
            vec![format!("Are you sure you want to delete {what}?"), format!("You can restore {} from the Trash.", if rels.len() == 1 { "it" } else { "them" })]
        };
        if rels.len() > 1 {
            text.insert(1, names(&rels));
        }
        let button = if forever { "Delete" } else { "Move to Trash" };
        self.prompt = Some(Prompt { text, buttons: vec![(button.into(), PAct::Delete(rels, forever)), ("Cancel".into(), PAct::Cancel)], sel: 0 });
    }

    /// Deletes `rels`: to the Trash, or (`forever`) for good. What no Trash can take (a disk
    /// whose own Trash cannot be made) is offered for deleting for good. Open editors of what
    /// went close, but those with unsaved edits stay (saving brings the file back).
    pub(super) fn delete_paths(&mut self, rels: &[String], forever: bool) {
        if self.file_busy() {
            return;
        }
        let (mut done, mut gone, mut no_trash, mut err) = (Vec::new(), Vec::new(), Vec::new(), None);
        for rel in rels {
            let path = self.root.join(rel);
            let res = if forever {
                match fs::symlink_metadata(&path) {
                    Ok(m) if m.is_dir() => fs::remove_dir_all(&path),
                    Ok(_) => fs::remove_file(&path),
                    Err(e) => Err(e),
                }
            } else {
                trash(&path, &self.trash).map(|(dir, name)| done.push(FileOp::Trashed { rel: rel.clone(), name, dir }))
            };
            match res {
                Ok(()) => gone.push(rel.clone()),
                Err(e) if !forever && e.raw_os_error() == Some(libc::EXDEV) => no_trash.push(rel.clone()),
                Err(e) => err = Some(format!("couldn't delete {}: {e}", base(rel))),
            }
        }
        self.record(done);
        if !gone.is_empty() {
            let went = |k: &str| {
                let p = k.split_once(':').filter(|(kind, _)| matches!(*kind, "file" | "preview")).map(|(_, p)| p);
                p.is_some_and(|p| gone.iter().any(|rel| p == rel || p.starts_with(&format!("{rel}/"))))
            };
            while let Some((g, i)) = self.groups.iter().enumerate().find_map(|(g, grp)| {
                grp.tabs.iter().position(|t| went(&t.key) && !matches!(self.docs.get(&t.key), Some(Doc::Text(b)) if b.dirty())).map(|i| (g, i))
            }) {
                self.close_tab(g, i, true);
            }
            let at = self.sel[Side::Explorer as usize];
            self.tree_marks.clear();
            self.reload_all();
            self.sel[Side::Explorer as usize] = at.min(self.tree.len().saturating_sub(1));
        }
        self.msg = match (err, gone.as_slice()) {
            (Some(e), _) => e,
            (None, [one]) if forever => format!("deleted {}", base(one)),
            (None, [one]) => format!("moved {} to the Trash", base(one)),
            (None, many) if forever => format!("deleted {} items", many.len()),
            (None, many) => format!("moved {} items to the Trash", many.len()),
        };
        if !no_trash.is_empty() {
            let what = match no_trash.as_slice() {
                [one] => format!("'{}' is", base(one)),
                _ => format!("{} items are", no_trash.len()),
            };
            self.prompt = Some(Prompt {
                text: vec![format!("{what} on a disk without a Trash."), "Delete permanently? This can't be undone.".into()],
                buttons: vec![("Delete".into(), PAct::Delete(no_trash, true)), ("Cancel".into(), PAct::Cancel)],
                sel: 1,
            });
        }
    }
}

/// Moves `path` to the Trash: `home` (the freedesktop home Trash), or for an item on
/// another disk that disk's own, `$topdir/.Trash-$uid`, as file managers do. Returns the
/// Trash used and the item's NAME there.
pub(super) fn trash(path: &Path, home: &Path) -> io::Result<(PathBuf, String)> {
    match trash_in(path, home) {
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            let Some(dir) = disk_trash(path) else { return Err(e) };
            trash_in(path, &dir).map(|n| (dir, n)).map_err(|_| e)
        }
        r => r.map(|n| (home.to_path_buf(), n)),
    }
}

/// `$topdir/.Trash-$uid` for `path`'s disk, made (0700) if missing; None when it is not
/// ours to use (a symlink, someone else's) or cannot be made.
fn disk_trash(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    // Tests keep to a Trash of their own.
    let name = if cfg!(test) { format!(".Trash-test-{}", std::process::id()) } else { format!(".Trash-{uid}") };
    let dir = mount_top(path)?.join(name);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(_) => return None,
    }
    let m = fs::symlink_metadata(&dir).ok()?;
    (m.is_dir() && m.uid() == uid).then_some(dir)
}

/// The top folder of the disk (mount) `path` is on.
fn mount_top(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dir = path.parent()?.canonicalize().ok()?;
    let dev = fs::metadata(&dir).ok()?.dev();
    dir.ancestors().take_while(|a| fs::metadata(a).is_ok_and(|m| m.dev() == dev)).last().map(Path::to_path_buf)
}

/// Moves `path` into the freedesktop Trash `trash` (what file managers restore from): the
/// item to files/NAME, its origin and time to info/NAME.trashinfo, NAME made free first
/// ("NAME.2", "NAME.3" …) by creating its info file. Same disk only: a rename. Returns NAME.
fn trash_in(path: &Path, trash: &Path) -> io::Result<String> {
    let (files, info) = (trash.join("files"), trash.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // The item's own place: its folder resolved, never the item (a symlink restores as
    // the link, not over what it points to).
    let abs = path.parent().and_then(|d| d.canonicalize().ok()).map_or_else(|| path.to_path_buf(), |d| d.join(&name));
    let when = local_iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64));
    for k in 1u32.. {
        let n = if k == 1 { name.clone() } else { format!("{name}.{k}") };
        let note = info.join(format!("{n}.trashinfo"));
        let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(&note) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        writeln!(f, "[Trash Info]\nPath={}\nDeletionDate={when}", percent(&abs.to_string_lossy()))?;
        return match fs::rename(path, files.join(&n)) {
            Ok(()) => Ok(n),
            Err(e) => {
                let _ = fs::remove_file(&note);
                Err(e)
            }
        };
    }
    unreachable!("a free name")
}

/// A path as the Trash writes it: bytes but unreserved ones and "/" as %XX.
fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Local "YYYY-MM-DDTHH:MM:SS" (a .trashinfo's DeletionDate).
pub(crate) fn local_iso(t: i64) -> String {
    #[allow(deprecated)] // libc marks musl's time_t for a future change; it is 64-bit here
    let tt = t as libc::time_t;
    // SAFETY: tm is plain data; localtime_r only writes into it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// An Explorer file operation, as Ctrl+Z undoes it and Ctrl+Y does it again.
#[derive(Clone, Debug, PartialEq)]
pub enum FileOp {
    /// A move or a rename.
    Moved { from: String, to: String },
    /// A paste of a copy.
    Copied { from: String, to: String },
    /// A move to the Trash `dir`, as NAME there.
    Trashed { rel: String, name: String, dir: PathBuf },
}

impl FileOp {
    /// As a reload carries it (`session::save_for_reload`).
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Moved { from, to } => serde_json::json!(["moved", from, to]),
            Copied { from, to } => serde_json::json!(["copied", from, to]),
            Trashed { rel, name, dir } => serde_json::json!(["trashed", rel, name, dir.to_string_lossy()]),
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Option<FileOp> {
        let s = |i: usize| v.get(i)?.as_str().map(str::to_string);
        Some(match v.get(0)?.as_str()? {
            "moved" => Moved { from: s(1)?, to: s(2)? },
            "copied" => Copied { from: s(1)?, to: s(2)? },
            "trashed" => Trashed { rel: s(1)?, name: s(2)?, dir: PathBuf::from(s(3)?) },
            _ => return None,
        })
    }
}

/// A paste running on its thread: each copy and how it went.
pub struct FileJob {
    rx: mpsc::Receiver<Vec<(FileOp, Result<(), String>)>>,
    /// Ctrl+Y's: kept on the undo stack, the redo stack left as is.
    redo: bool,
    /// Files copied so far (the last count said), and Esc's stop.
    files: Arc<AtomicUsize>,
    shown: usize,
    stop: Arc<AtomicBool>,
    /// Items asked for.
    total: usize,
}

/// File operations kept for Ctrl+Z (each a group: one drag, paste or delete).
const FILE_UNDO: usize = 50;

impl App {
    /// A new group of file operations: kept for Ctrl+Z (what Ctrl+Z took back cannot be
    /// redone after).
    fn record(&mut self, ops: Vec<FileOp>) {
        if !ops.is_empty() {
            self.push_undo(ops);
            self.file_redo.clear();
        }
    }

    fn push_undo(&mut self, ops: Vec<FileOp>) {
        if ops.is_empty() {
            return;
        }
        self.file_undo.push(ops);
        if self.file_undo.len() > FILE_UNDO {
            self.file_undo.remove(0);
        }
    }

    /// Ctrl+Z in the Explorer: the last group of file operations taken back, each where
    /// things still are as it left them: a move moved back, a pasted copy to the Trash, a
    /// deleted item restored.
    pub(super) fn undo_file_op(&mut self) {
        if self.file_busy() {
            return;
        }
        let Some(ops) = self.file_undo.pop() else {
            self.msg = "nothing to undo".into();
            return;
        };
        let (mut back, mut said, mut err) = (Vec::new(), String::new(), None);
        for op in ops.into_iter().rev() {
            match self.undo_one(&op) {
                Ok(s) => {
                    said = s;
                    back.insert(0, op);
                }
                Err(e) => err = Some(e),
            }
        }
        self.reload_all();
        if let Some(rel) = back.last().map(|op| match op {
            Moved { from, .. } => from.clone(),
            Copied { to, .. } => parent_of(to).to_string(),
            Trashed { rel, .. } => rel.clone(),
        }) {
            self.reveal_path(&rel);
        }
        self.msg = match (err, back.len()) {
            (Some(e), 0) => format!("couldn't undo: {e}"),
            (Some(e), n) => format!("took back {n} of them; couldn't undo the rest: {e}"),
            (None, 1) => said,
            (None, n) => format!("took back {n} file operations"),
        };
        if !back.is_empty() {
            self.file_redo.push(back);
        }
    }

    fn undo_one(&mut self, op: &FileOp) -> Result<String, String> {
        let exists = |rel: &str| fs::symlink_metadata(self.root.join(rel)).is_ok();
        match op {
            Moved { from, to } if exists(to) && !exists(from) => self.rename_path(to, from).map(|()| format!("moved {} back", base(from))),
            Copied { to, .. } if exists(to) => trash(&self.root.join(to), &self.trash)
                .map_err(|e| e.to_string())
                .map(|_| format!("took back the copy {} (it is in the Trash)", base(to))),
            Trashed { rel, name, dir } if !exists(rel) && dir.join("files").join(name).exists() => {
                let to = self.root.join(rel);
                let restored = to.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::rename(dir.join("files").join(name), &to));
                restored.map_err(|e| e.to_string()).map(|()| {
                    let _ = fs::remove_file(dir.join("info").join(format!("{name}.trashinfo")));
                    format!("restored {}", base(rel))
                })
            }
            _ => Err("it is no longer where it was".into()),
        }
    }

    /// Ctrl+Y (Ctrl+Shift+Z) in the Explorer: the last undone group done again (copies
    /// again on a thread, as a paste).
    pub(super) fn redo_file_op(&mut self) {
        if self.file_busy() {
            return;
        }
        let Some(ops) = self.file_redo.pop() else {
            self.msg = "nothing to redo".into();
            return;
        };
        let root = self.root.clone();
        let exists = |rel: &str| fs::symlink_metadata(root.join(rel)).is_ok();
        let (mut again, mut copies, mut said, mut err) = (Vec::new(), Vec::new(), String::new(), None);
        for op in ops {
            let res = match &op {
                Moved { from, to } if exists(from) && !exists(to) => self.rename_path(from, to).map(|()| (format!("moved {} again", base(to)), op.clone())),
                Copied { from, to } if exists(from) && !exists(to) => {
                    copies.push((from.clone(), to.clone()));
                    continue;
                }
                Trashed { rel, .. } if exists(rel) => trash(&self.root.join(rel), &self.trash)
                    .map_err(|e| e.to_string())
                    .map(|(dir, name)| (format!("moved {} to the Trash again", base(rel)), Trashed { rel: rel.clone(), name, dir })),
                _ => Err("it is no longer where it was".into()),
            };
            match res {
                Ok((s, op)) => {
                    said = s;
                    again.push(op);
                }
                Err(e) => err = Some(e),
            }
        }
        self.reload_all();
        if let Some(Moved { to: rel, .. } | Trashed { rel, .. }) = again.last() {
            let rel = rel.clone();
            self.reveal_path(&rel);
        }
        self.msg = match (err, again.len()) {
            (Some(e), 0) if copies.is_empty() => format!("couldn't redo: {e}"),
            (Some(e), _) => format!("couldn't redo all of it: {e}"),
            (None, 1) => said,
            (None, 0) => String::new(),
            (None, n) => format!("did {n} file operations again"),
        };
        self.push_undo(again);
        if !copies.is_empty() {
            self.start_copies(copies, true);
        }
    }
}

use FileOp::{Copied, Moved, Trashed};

/// The folder of `rel` ("" for the root's items).
pub(super) fn parent_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(d, _)| d)
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Copies `from` to `to`: a file, a symlink as a symlink, a folder with all in it.
/// A running paste's count of files copied, and its stop.
struct Progress {
    files: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

fn copy_path(from: &Path, to: &Path, p: &Progress) -> io::Result<()> {
    if p.stop.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
    }
    p.files.fetch_add(1, Ordering::Relaxed);
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_path(&e.path(), &to.join(e.file_name()), p)?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}
