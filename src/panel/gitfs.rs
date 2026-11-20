//! `App`: git status, file loads and the disk.

use super::*;

impl App {
    // ---- git / filesystem ------------------------------------------------------

    /// A repo path (git status, SCM rows) as a document path: relative under the root, else absolute.
    pub fn doc_path(&self, repo_rel: &str) -> String {
        match (repo_rel.strip_prefix(&self.prefix), &self.git_root) {
            (Some(r), _) => r.to_string(),
            (None, Some(g)) => g.join(repo_rel).to_string_lossy().into_owned(),
            (None, None) => repo_rel.to_string(),
        }
    }

    /// A document path (relative to the root, or absolute) as a repo path; None outside the repo.
    pub fn repo_path(&self, rel: &str) -> Option<String> {
        let g = self.git_root.as_ref()?;
        Some(self.root.join(rel).strip_prefix(g).ok()?.to_string_lossy().into_owned())
    }

    /// Re-read git status (in the background: the old status shows until the new one
    /// lands), decorations, and the disk state of open files.
    pub fn refresh(&mut self) {
        self.refresh_files();
        self.status_polled = false;
        self.refresh_status();
    }

    /// `git status` as a job. One at a time: a refresh meanwhile reruns it after.
    pub(super) fn refresh_status(&mut self) {
        let Some(g) = self.git_root.clone() else { return };
        if !async_status() {
            return self.apply_status(self.gitui.logged(|| read_status(&g)));
        }
        if self.loading.contains_key(STATUS_KEY) {
            self.status_again = true;
            return;
        }
        self.status_since = Some(Instant::now());
        // A poll stays out of Git Output: it would bury what the user ran under a few
        // lines every two seconds.
        if self.status_polled {
            return self.load(STATUS_KEY.into(), move |_| Load::Status(read_status(&g)));
        }
        let log = self.gitui.log.clone();
        self.load(STATUS_KEY.into(), move |_| Load::Status(crate::gitops::with_log(&log, || read_status(&g))));
    }

    /// The status `git status` reported: SCM rows, Explorer marks, the sync state, then
    /// what follows them. Until it lands the last status shows.
    pub(super) fn apply_status(&mut self, (branch, raw, sync): Status) {
        self.status_since = None;
        self.branch = branch;
        self.gitui.cmds.sync = sync;
        self.status = parse::parse_porcelain_z(&raw);
        let (staged, changes) = git::split(&self.status);
        self.scm_files = staged.into_iter().chain(changes).collect();
        // The Explorer shows the root: its files' marks, relative to it.
        self.marks = self
            .status
            .iter()
            .filter_map(|e| Some((e.path.strip_prefix(&self.prefix)?.to_string(), git::decoration(e)?)))
            .collect();
        self.dirty_dirs = git::folder_marks(self.marks.iter().map(|(p, c)| (p.as_str(), *c)));
        crate::scm::on_refresh(self);
    }

    /// The disk state of open files, the gutter bases, settings, visible diffs.
    pub(super) fn refresh_files(&mut self) {
        let mut notes = Vec::new();
        for (key, doc) in self.docs.iter_mut() {
            if let Doc::Text(b) = doc {
                match b.check_disk() {
                    Some(true) => notes.push(format!("reloaded {}", base(key))),
                    Some(false) => notes.push(format!("{} changed on disk (unsaved edits kept)", base(key))),
                    None => {}
                }
            }
        }
        if !notes.is_empty() {
            self.msg = notes.join("; ");
        }
        // Staging changes the index: the gutter compares against the new one.
        self.load_bases();
        self.check_settings();
        // Visible diff editors follow the new state.
        let visible: Vec<(String, ScmFile)> = self
            .groups
            .iter()
            .filter_map(|g| g.tabs.get(g.active))
            .filter_map(|t| match self.docs.get(&t.key) {
                // A commit's diff never changes.
                Some(Doc::Diff(d)) if !t.key.starts_with("cdiff:") && !t.key.starts_with("patch:") => Some((t.key.clone(), d.file.clone())),
                _ => None,
            })
            .collect();
        for (key, f) in visible {
            self.load_diff(key, f, None);
        }
    }

    pub(super) fn read_dir(&self, rel: &str, depth: usize) -> Vec<Node> {
        let mut v: Vec<Node> = fs::read_dir(self.root.join(rel))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == ".git" {
                    return None;
                }
                let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                // A link to a folder is a folder (it opens); every link knows where it points.
                let ft = e.file_type().ok()?;
                let target = if ft.is_symlink() { fs::metadata(e.path()).ok() } else { None };
                let link = ft.is_symlink().then(|| (fs::read_link(e.path()).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default(), target.is_some()));
                let dir = ft.is_dir() || target.is_some_and(|m| m.is_dir());
                Some(Node { path, name, depth, dir, open: false, link })
            })
            .collect();
        v.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }

    pub(crate) fn toggle(&mut self, i: usize) {
        let (dir, open, depth, path) = {
            let n = &self.tree[i];
            (n.dir, n.open, n.depth, n.path.clone())
        };
        if !dir {
            return;
        }
        self.tree_parents.get_mut().clear();
        if open {
            let end = (i + 1..self.tree.len()).find(|&j| self.tree[j].depth <= depth).unwrap_or(self.tree.len());
            self.tree.drain(i + 1..end);
        } else {
            let kids = self.read_dir(&path, depth + 1);
            self.tree.splice(i + 1..i + 1, kids);
        }
        self.tree[i].open = !open;
    }

    pub(crate) fn reload_all(&mut self) {
        self.refresh();
        let open: Vec<String> = self.tree.iter().filter(|n| n.open).map(|n| n.path.clone()).collect();
        self.tree = self.read_dir("", 0);
        self.tree_parents.get_mut().clear();
        let mut i = 0;
        while i < self.tree.len() {
            if open.contains(&self.tree[i].path) {
                self.toggle(i);
            }
            i += 1;
        }
        self.files.clear();
        if self.side == Side::SCM {
            crate::scm::load_graph(self, true);
        }
    }
}
