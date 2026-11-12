//! `App`: renaming in the Explorer.

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
            Ok(()) => self.msg = format!("renamed {old_name} to {name}"),
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
}
