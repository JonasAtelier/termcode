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
                self.record(FileOp::Moved { from: r.path.clone(), to: new.clone() });
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

    /// The folder a dragged Explorer item `rel` would move into under `p`: a folder row's, a
    /// file row's folder, the root below the rows. None off the Explorer, where the item
    /// already is, and (a folder) inside itself.
    pub(super) fn tree_target(&self, p: Position, rel: &str) -> Option<String> {
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
        let inside = into == rel || into.starts_with(&format!("{rel}/"));
        (into != parent_of(rel) && !inside).then_some(into)
    }

    /// A drop on a folder: asks first (a drop can land by mistake), or says why it cannot.
    pub(super) fn ask_move(&mut self, rel: String, into: String) {
        let name = base(&rel).to_string();
        let to = join_rel(&into, &name);
        let shown = if into.is_empty() { self.repo.clone() } else { base(&into).to_string() };
        if fs::symlink_metadata(self.root.join(&to)).is_ok() {
            self.msg = format!("{name} already exists in {shown}");
            return;
        }
        self.prompt = Some(Prompt {
            text: vec![format!("Are you sure you want to move '{name}' into '{shown}'?"), "Its open editors follow it.".into()],
            buttons: vec![("Move".into(), PAct::Move(rel, into)), ("Cancel".into(), PAct::Cancel)],
            sel: 0,
        });
    }

    /// Moves `rel` into folder `into` (open editors along, see `rename_path`) and shows it.
    pub(super) fn move_into(&mut self, rel: &str, into: &str) {
        let to = join_rel(into, base(rel));
        match self.rename_path(rel, &to) {
            Ok(()) => {
                self.reveal_path(&to);
                self.record(FileOp::Moved { from: rel.to_string(), to: to.clone() });
                self.msg = format!("moved {} into {}", base(rel), if into.is_empty() { self.repo.clone() } else { base(into).to_string() });
            }
            Err(e) => self.msg = e,
        }
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

    /// Ctrl+C (Ctrl+X: `cut`) in the Explorer: the selected item, for Ctrl+V.
    pub(super) fn tree_copy(&mut self, cut: bool) {
        let Some(n) = self.tree.get(self.sel[Side::Explorer as usize]) else { return };
        self.msg = format!("{} {}: Ctrl+V puts it in a folder", if cut { "cut" } else { "copied" }, n.name);
        self.tree_clip = Some((n.path.clone(), cut));
    }

    /// Ctrl+V in the Explorer: the copied item into the selected folder (a file's: its
    /// folder), as "name (1)", "name (2)" … where the name is taken; a cut one moves there.
    pub(super) fn tree_paste(&mut self) {
        let Some((from, cut)) = self.tree_clip.clone() else { return };
        let into = match self.tree.get(self.sel[Side::Explorer as usize]) {
            Some(n) if n.dir => n.path.clone(),
            Some(n) => parent_of(&n.path).to_string(),
            None => String::new(),
        };
        if fs::symlink_metadata(self.root.join(&from)).is_err() {
            self.tree_clip = None;
            self.msg = format!("{} is gone", base(&from));
            return;
        }
        if into == from || into.starts_with(&format!("{from}/")) {
            self.msg = format!("{} cannot go inside itself", base(&from));
            return;
        }
        if cut {
            if parent_of(&from) == into {
                self.msg = format!("{} is already there", base(&from));
                return;
            }
            if fs::symlink_metadata(self.root.join(join_rel(&into, base(&from)))).is_ok() {
                self.msg = format!("{} already exists there", base(&from));
                return;
            }
            self.tree_clip = None;
            return self.move_into(&from, &into);
        }
        let to = join_rel(&into, &self.free_name(&into, base(&from)));
        match copy_path(&self.root.join(&from), &self.root.join(&to)) {
            Ok(()) => {
                self.reload_all();
                self.reveal_path(&to);
                self.record(FileOp::Copied { from, to: to.clone() });
                self.msg = format!("pasted {}", base(&to));
            }
            Err(e) => self.msg = format!("paste failed: {e}"),
        }
    }

    /// `name` in folder `dir` if free, else the first free of "stem (1).ext", "stem (2).ext" …
    pub(super) fn free_name(&self, dir: &str, name: &str) -> String {
        let taken = |n: &str| fs::symlink_metadata(self.root.join(join_rel(dir, n))).is_ok();
        if !taken(name) {
            return name.to_string();
        }
        // The extension stays last (a dotfile's leading dot is not one).
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 && !self.root.join(join_rel(dir, name)).is_dir() => name.split_at(i),
            _ => (name, ""),
        };
        (1..).map(|k| format!("{stem} ({k}){ext}")).find(|n| !taken(n)).expect("a free name")
    }
}

impl App {
    /// Delete (Shift+Delete: `forever`) in the Explorer: asks first, as VS Code does.
    pub(super) fn ask_delete(&mut self, forever: bool) {
        let Some(n) = self.tree.get(self.sel[Side::Explorer as usize]) else { return };
        let what = if n.dir { format!("'{}' and its contents", n.name) } else { format!("'{}'", n.name) };
        let (text, button) = if forever {
            (vec![format!("Are you sure you want to permanently delete {what}?"), "This can't be undone.".into()], "Delete")
        } else {
            (vec![format!("Are you sure you want to delete {what}?"), "You can restore it from the Trash.".into()], "Move to Trash")
        };
        self.prompt = Some(Prompt { text, buttons: vec![(button.into(), PAct::Delete(n.path.clone(), forever)), ("Cancel".into(), PAct::Cancel)], sel: 0 });
    }

    /// Deletes `rel`: to the Trash, or (`forever`) for good. Where the Trash cannot take it
    /// (another disk), asks whether to delete it for good. Open editors of what went close,
    /// but those with unsaved edits stay (saving brings the file back).
    pub(super) fn delete_path(&mut self, rel: &str, forever: bool) {
        let path = self.root.join(rel);
        let res = if forever {
            match fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&path),
                Ok(_) => fs::remove_file(&path),
                Err(e) => Err(e),
            }
        } else {
            trash(&path, &self.trash).map(|name| self.record(FileOp::Trashed { rel: rel.to_string(), name }))
        };
        match res {
            Ok(()) => {
                let gone = |k: &str| {
                    let p = k.split_once(':').filter(|(kind, _)| matches!(*kind, "file" | "preview")).map(|(_, p)| p);
                    p.is_some_and(|p| p == rel || p.starts_with(&format!("{rel}/")))
                };
                while let Some((g, i)) = self.groups.iter().enumerate().find_map(|(g, grp)| {
                    grp.tabs.iter().position(|t| gone(&t.key) && !matches!(self.docs.get(&t.key), Some(Doc::Text(b)) if b.dirty())).map(|i| (g, i))
                }) {
                    self.close_tab(g, i, true);
                }
                let at = self.sel[Side::Explorer as usize];
                self.reload_all();
                self.sel[Side::Explorer as usize] = at.min(self.tree.len().saturating_sub(1));
                self.msg = if forever { format!("deleted {}", base(rel)) } else { format!("moved {} to the Trash", base(rel)) };
            }
            Err(e) if !forever && e.raw_os_error() == Some(libc::EXDEV) => {
                self.prompt = Some(Prompt {
                    text: vec![format!("'{}' is on another disk than the Trash.", base(rel)), "Delete it permanently? This can't be undone.".into()],
                    buttons: vec![("Delete".into(), PAct::Delete(rel.to_string(), true)), ("Cancel".into(), PAct::Cancel)],
                    sel: 1,
                });
            }
            Err(e) => self.msg = format!("couldn't delete {}: {e}", base(rel)),
        }
    }
}

/// Moves `path` into the freedesktop Trash `trash` (what file managers restore from): the
/// item to files/NAME, its origin and time to info/NAME.trashinfo, NAME made free first
/// ("NAME.2", "NAME.3" …) by creating its info file. Same disk only: a rename. Returns NAME.
pub(super) fn trash(path: &Path, trash: &Path) -> io::Result<String> {
    let (files, info) = (trash.join("files"), trash.join("info"));
    fs::create_dir_all(&files)?;
    fs::create_dir_all(&info)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
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
fn local_iso(t: i64) -> String {
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
    /// A move to the Trash, as NAME there.
    Trashed { rel: String, name: String },
}

/// File operations kept for Ctrl+Z.
const FILE_UNDO: usize = 50;

impl App {
    /// A new file operation: kept for Ctrl+Z (what Ctrl+Z took back cannot be redone after).
    fn record(&mut self, op: FileOp) {
        self.file_undo.push(op);
        if self.file_undo.len() > FILE_UNDO {
            self.file_undo.remove(0);
        }
        self.file_redo.clear();
    }

    /// Ctrl+Z in the Explorer: the last file operation taken back, where things still are as
    /// it left them: a move moved back, a pasted copy to the Trash, a deleted item restored.
    pub(super) fn undo_file_op(&mut self) {
        let Some(op) = self.file_undo.pop() else {
            self.msg = "nothing to undo".into();
            return;
        };
        let exists = |rel: &str| fs::symlink_metadata(self.root.join(rel)).is_ok();
        let res: Result<(String, FileOp), String> = match &op {
            Moved { from, to } if exists(to) && !exists(from) => self.rename_path(to, from).map(|()| {
                self.reveal_path(from);
                (format!("moved {} back", base(from)), op.clone())
            }),
            Copied { to, .. } if exists(to) => trash(&self.root.join(to), &self.trash).map_err(|e| e.to_string()).map(|_| {
                self.reload_all();
                (format!("took back the copy {} (it is in the Trash)", base(to)), op.clone())
            }),
            Trashed { rel, name } if !exists(rel) && self.trash.join("files").join(name).exists() => {
                let to = self.root.join(rel);
                let restored = to.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::rename(self.trash.join("files").join(name), &to));
                restored.map_err(|e| e.to_string()).map(|()| {
                    let _ = fs::remove_file(self.trash.join("info").join(format!("{name}.trashinfo")));
                    self.reload_all();
                    self.reveal_path(rel);
                    (format!("restored {}", base(rel)), op.clone())
                })
            }
            _ => Err("it is no longer where it was".into()),
        };
        match res {
            Ok((said, op)) => {
                self.file_redo.push(op);
                self.msg = said;
            }
            Err(e) => self.msg = format!("couldn't undo: {e}"),
        }
    }

    /// Ctrl+Y (Ctrl+Shift+Z) in the Explorer: the last undone file operation done again.
    pub(super) fn redo_file_op(&mut self) {
        let Some(op) = self.file_redo.pop() else {
            self.msg = "nothing to redo".into();
            return;
        };
        let exists = |rel: &str| fs::symlink_metadata(self.root.join(rel)).is_ok();
        let res: Result<(String, FileOp), String> = match &op {
            Moved { from, to } if exists(from) && !exists(to) => self.rename_path(from, to).map(|()| {
                self.reveal_path(to);
                (format!("moved {} again", base(to)), op.clone())
            }),
            Copied { from, to } if exists(from) && !exists(to) => copy_path(&self.root.join(from), &self.root.join(to)).map_err(|e| e.to_string()).map(|()| {
                self.reload_all();
                self.reveal_path(to);
                (format!("pasted {} again", base(to)), op.clone())
            }),
            Trashed { rel, .. } if exists(rel) => trash(&self.root.join(rel), &self.trash).map_err(|e| e.to_string()).map(|name| {
                self.reload_all();
                (format!("moved {} to the Trash again", base(rel)), Trashed { rel: rel.clone(), name })
            }),
            _ => Err("it is no longer where it was".into()),
        };
        match res {
            Ok((said, op)) => {
                self.file_undo.push(op);
                self.msg = said;
            }
            Err(e) => self.msg = format!("couldn't redo: {e}"),
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
fn copy_path(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? {
            let e = e?;
            copy_path(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}
