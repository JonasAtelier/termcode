//! `App`: Source Control actions and the sidebar.

use super::*;

impl App {
    // ---- source control ----------------------------------------------------------------

    /// VS Code's groups: Merge Changes (conflicts), Staged Changes, Changes.
    pub fn scm_rows(&self) -> Vec<SRow> {
        let idx = |keep: &dyn Fn(&ScmFile) -> bool| (0..self.scm_files.len()).filter(|&i| keep(&self.scm_files[i])).collect::<Vec<_>>();
        let merge = idx(&|f| f.section == Section::Changes && f.letter == '!');
        let staged = idx(&|f| f.section == Section::Staged);
        let changes = idx(&|f| f.section == Section::Changes && f.letter != '!');
        let gap = self.settings.spacing.section_gap;
        let mut rows = Vec::new();
        let f = self.scm_folded;
        for (head, files, folded) in [(SRow::MergeHead(merge.len()), merge, f[0]), (SRow::Head(Section::Staged, staged.len()), staged, f[1])] {
            if !files.is_empty() {
                rows.push(head);
                if !folded {
                    rows.extend(files.into_iter().map(SRow::File));
                }
                rows.extend((0..gap).map(|_| SRow::Gap));
            }
        }
        rows.push(SRow::Head(Section::Changes, changes.len()));
        if !f[2] {
            rows.extend(changes.into_iter().map(SRow::File));
        }
        rows
    }

    /// Which `scm_folded` slot a group header row is.
    pub fn scm_group(row: &SRow) -> Option<usize> {
        match row {
            SRow::MergeHead(_) => Some(0),
            SRow::Head(Section::Staged, _) => Some(1),
            SRow::Head(_, _) => Some(2),
            _ => None,
        }
    }

    /// Fold or unfold the group whose header is row `i` (`to`: None toggles).
    pub(super) fn fold_scm(&mut self, i: usize, to: Option<bool>) {
        let Some(g) = self.scm_rows().get(i).and_then(Self::scm_group) else { return };
        self.scm_folded[g] = to.unwrap_or(!self.scm_folded[g]);
        self.move_side(0); // the selection stays in the list
    }

    /// A group's files; Changes leaves out the conflicts (Merge Changes has them).
    pub(super) fn section_files(&self, s: Section) -> Vec<ScmFile> {
        self.scm_files.iter().filter(|f| f.section == s && (s == Section::Staged || f.letter != '!')).cloned().collect()
    }

    pub(super) fn confirm_discard(&mut self, files: Vec<ScmFile>) {
        if files.is_empty() {
            return;
        }
        let untracked = files.iter().filter(|f| f.letter == 'U').count();
        let first = if files.len() == 1 {
            format!("Are you sure you want to discard changes in {}?", files[0].path)
        } else {
            format!("Are you sure you want to discard ALL changes in {} files?", files.len())
        };
        let second = if untracked > 0 {
            format!("{untracked} untracked file(s) will be DELETED. This is IRREVERSIBLE!")
        } else {
            "This is IRREVERSIBLE!".to_string()
        };
        self.prompt = Some(Prompt {
            text: vec![first, second],
            buttons: vec![("Discard".into(), PAct::Discard(files)), ("Cancel".into(), PAct::Cancel)],
            sel: 1,
        });
    }

    pub fn scm_action(&mut self, a: ScmAct) {
        let Some(g) = self.git_root.clone() else { return };
        let one = |i: usize| self.scm_files.get(i).cloned().into_iter().collect::<Vec<_>>();
        let res = match a {
            ScmAct::OpenFile(i) => {
                if let Some(f) = self.scm_files.get(i).cloned() {
                    self.open_file(self.ag, &self.doc_path(&f.path), false);
                    self.focus = Focus::Editor;
                }
                return;
            }
            ScmAct::Discard(i) => return self.confirm_discard(one(i)),
            ScmAct::DiscardAll => return self.confirm_discard(self.section_files(Section::Changes)),
            ScmAct::Stage(i) => (true, one(i)),
            ScmAct::Unstage(i) => (false, one(i)),
            ScmAct::StageAll => (true, self.section_files(Section::Changes)),
            ScmAct::UnstageAll => (false, self.section_files(Section::Staged)),
            ScmAct::StageMerge => (true, self.scm_files.iter().filter(|f| f.letter == '!').cloned().collect()),
        };
        // On the index writer, in order; status follows each through drain.
        let (stage, files) = res;
        self.gitui.write(if stage { "stage" } else { "unstage" }, move || {
            let r = if stage { git::stage(&g, &files) } else { git::unstage(&g, &files) };
            crate::gitops::refresh_index(&g);
            crate::scm::Msg::Wrote(r.map(|_| String::new()), false)
        });
    }

    /// "Add commit": the git-gui style commit dialog.
    pub fn commit(&mut self) {
        crate::commitdlg::open(self);
    }

    // ---- sidebar ------------------------------------------------------------------------

    pub fn search_rows(&self) -> Vec<HRow> {
        let mut rows = Vec::new();
        for (i, h) in self.hits.iter().enumerate() {
            if i == 0 || self.hits[i - 1].path != h.path {
                let n = self.hits[i..].iter().take_while(|x| x.path == h.path).count();
                rows.push(HRow::File(h.path.clone(), n));
            }
            if !self.search_closed.contains(&h.path) {
                rows.push(HRow::Hit(i));
            }
        }
        rows
    }

    pub(super) fn toggle_search_group(&mut self, i: usize) {
        let Some(HRow::File(path, _)) = self.search_rows().into_iter().nth(i) else { return };
        if !self.search_closed.remove(&path) {
            self.search_closed.insert(path);
        }
        self.sel[1] = i;
        self.off[1] = self.off[1].min(i).min(self.side_count().saturating_sub(self.ui.side_rows.max(1)));
        self.focus = Focus::Side;
    }

    pub fn side_count(&self) -> usize {
        match self.side {
            Side::Explorer => self.tree.len(),
            Side::Search => self.search_rows().len(),
            Side::Scm => self.scm_rows().len(),
            // Its own list (github.rs, programmer.rs).
            Side::GitHub | Side::Programmer | Side::Serial => 0,
        }
    }

    pub fn show_side(&mut self, s: Side, input: bool) {
        self.side = s;
        self.side_visible = true;
        self.focus = match s {
            Side::Search if input => Focus::SearchInput,
            _ => Focus::Side,
        };
        self.query_all = false;
        self.refresh();
        self.save_layout();
        if s == Side::Programmer {
            crate::programmer::shown(self);
        }
    }

    pub(crate) fn save_layout(&self) {
        let Some(p) = &self.layout_file else { return };
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        let mut v = serde_json::json!({ "sidebar_visible": self.side_visible, "scm": self.gitui.layout(), "recent_commands": self.recent_cmds });
        v["layout_version"] = 2.into();
        if let Some(w) = self.side_w {
            // Written as the sidebar's outer width, as it is drawn.
            let w = w + if self.side_w_legacy { crate::blocks::frame(self.settings.blocks) } else { 0 };
            v["sidebar_width"] = w.into();
            v["sidebar_dragged"] = true.into();
        }
        if let Some(h) = self.term_h {
            v["terminal_height"] = h.into();
        }
        if let Some(w) = self.outline_w {
            v["outline_width"] = w.into();
        }
        if let Some(r) = &self.gh.chosen {
            v["github_repo"] = r.as_str().into();
        }
        if let Some(h) = self.gh.split {
            v["github_split"] = h.into();
        }
        let _ = fs::write(p, v.to_string());
    }

    /// Single click opens a preview; `pin` (double click, Enter) keeps it open.
    /// The keys' way through a sidebar list: in Search, a match the selection lands on
    /// shows at once (as a preview tab; the keys stay in the list).
    pub(super) fn step_side(&mut self, by: isize) {
        self.move_side(by);
        let i = self.sel[Side::Search as usize];
        if self.side == Side::Search && matches!(self.search_rows().get(i), Some(HRow::Hit(_))) {
            self.activate_side(i, false);
            self.focus = Focus::Side;
        }
    }

    pub(super) fn activate_side(&mut self, i: usize, pin: bool) {
        match self.side {
            Side::Explorer => match self.tree.get(i) {
                Some(n) if n.dir => {
                    if !pin {
                        self.toggle(i)
                    }
                }
                Some(n) => {
                    let p = n.path.clone();
                    self.open_file(self.ag, &p, !pin);
                    if pin {
                        self.focus = Focus::Editor;
                    }
                }
                None => {}
            },
            Side::Search => match self.search_rows().get(i) {
                Some(HRow::Hit(h)) => {
                    let h = self.hits[*h].clone();
                    self.open_file_at(&h.path, h.line, h.col);
                    if pin {
                        self.pin_active();
                    }
                }
                Some(HRow::File(p, _)) => {
                    let p = p.clone();
                    self.open_file(self.ag, &p, !pin);
                }
                None => {}
            },
            Side::Scm => match self.scm_rows().get(i) {
                Some(SRow::File(f)) => {
                    let f = *f;
                    self.open_diff(f, !pin);
                }
                // A group header (click, Enter, Space) folds or unfolds its files.
                Some(r) if Self::scm_group(r).is_some() => self.fold_scm(i, None),
                _ => {}
            },
            Side::GitHub | Side::Programmer | Side::Serial => {}
        }
    }

    /// "Split Editor Right": the active file again in a new group to the right.
    pub(super) fn split_right(&mut self, g: usize) {
        self.split(g, false);
    }

    /// The active tab of group `g` again in a new group: to its right (a new column), or
    /// (`down`) under it in its column.
    pub(crate) fn split(&mut self, g: usize, down: bool) {
        if self.groups.len() >= MAX_GROUPS {
            self.msg = format!("{MAX_GROUPS} editor groups at most");
            return;
        }
        let Some(t) = self.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)) else { return };
        let tab = Tab { key: t.key.clone(), preview: false, view: t.view.clone() };
        // To the right: after the whole column `g` is in.
        let at = if down { g + 1 } else { (g + 1..self.groups.len()).find(|&i| !self.groups[i].below).unwrap_or(self.groups.len()) };
        self.groups.insert(at, Group { tabs: vec![tab], active: 0, below: down });
        self.ag = at;
        self.focus = Focus::Editor;
    }

    /// The depth-`k` folder holding Explorer row `i`, by parent links rebuilt once
    /// per tree change (O(rows)); then O(depth) a call, however far the folder is.
    pub(super) fn tree_ancestor(&self, i: usize, k: usize) -> Option<usize> {
        if self.tree.get(i)?.depth <= k {
            return None;
        }
        let mut up = self.tree_parents.borrow_mut();
        if up.len() != self.tree.len() {
            up.clear();
            let mut open: Vec<usize> = Vec::new();
            for (j, n) in self.tree.iter().enumerate() {
                while open.last().is_some_and(|&o| self.tree[o].depth >= n.depth) {
                    open.pop();
                }
                up.push(open.last().copied().unwrap_or(usize::MAX));
                open.push(j);
            }
        }
        let mut a = i;
        // Links point backwards, so this ends even on a stale list.
        while self.tree[a].depth > k {
            a = up.get(a).copied().filter(|&p| p < a)?;
        }
        (self.tree[a].depth == k).then_some(a)
    }

    /// The separator shares the last pinned row (or its configured listItemGap).
    pub fn sticky_sep(&self) -> usize {
        0
    }

    /// Sticky scroll's pinned rows for the sidebar scrolled to `off`: Explorer folders
    /// over their contents, Search file groups over their hits, the CHANGES sections
    /// (Merge, Staged, Changes headers) over their files. `search` / `scm`: that
    /// view's rows, as the frame built them for drawing. One Explorer row (the
    /// default) is the breadcrumb of the folder you are in.
    pub fn pinned(&self, off: usize, cap: usize, search: &[HRow], scm: &[SRow]) -> Vec<usize> {
        let sep = self.sticky_sep();
        match self.side {
            Side::Explorer if cap == 1 => self.breadcrumb(off, sep).into_iter().collect(),
            Side::Explorer => decor::sticky(|i, k| self.tree_ancestor(i, k), off, cap, sep),
            Side::Search => decor::sticky(|i, k| decor::walk_back(|j| search.get(j).map(|r| matches!(r, HRow::Hit(_)) as usize), i, k), off, cap, sep),
            Side::Scm => decor::sticky(|i, k| decor::walk_back(|j| scm.get(j).map(|r| matches!(r, SRow::File(_)) as usize), i, k), off, cap, sep),
            Side::GitHub | Side::Programmer | Side::Serial => Vec::new(),
        }
    }

    /// The one-row sticky Explorer: the folder holding the first row visible below
    /// the sticky row and its separator (so it follows every folder end), while its
    /// path's top-level folder is scrolled above the list.
    pub(super) fn breadcrumb(&self, off: usize, sep: usize) -> Option<usize> {
        let first = off + 1 + sep;
        let folder = self.tree_ancestor(first, self.tree.get(first)?.depth.checked_sub(1)?)?;
        (self.tree_ancestor(folder, 0).unwrap_or(folder) < off).then_some(folder)
    }

    /// Explorer row `i` and its folders, the top-level one first.
    pub fn tree_path(&self, i: usize) -> Vec<usize> {
        let depth = self.tree.get(i).map_or(0, |n| n.depth);
        (0..depth).filter_map(|k| self.tree_ancestor(i, k)).chain(std::iter::once(i)).collect()
    }

    /// `pinned` for key handling, which has no rows built.
    pub(super) fn pinned_now(&self, off: usize) -> Vec<usize> {
        let (search, scm) = match self.side {
            Side::Explorer | Side::GitHub | Side::Programmer | Side::Serial => (Vec::new(), Vec::new()),
            Side::Search => (self.search_rows(), Vec::new()),
            Side::Scm => (Vec::new(), self.scm_rows()),
        };
        self.pinned(off, self.ui.sticky_cap, &search, &scm)
    }

    /// The scroll offset that shows item `i` right under its pinned ancestors and
    /// their separator line (a top-level item: at the top).
    pub(super) fn under_pinned(&self, i: usize) -> usize {
        let depth = match self.side {
            Side::Explorer => self.tree.get(i).map_or(0, |n| n.depth),
            Side::Search => matches!(self.search_rows().get(i), Some(HRow::Hit(_))) as usize,
            Side::Scm => matches!(self.scm_rows().get(i), Some(SRow::File(_))) as usize,
            Side::GitHub | Side::Programmer | Side::Serial => 0,
        };
        let pinned = depth.min(self.ui.sticky_cap);
        if pinned == 0 {
            return i;
        }
        i.saturating_sub(pinned + self.sticky_sep())
    }

    /// A click on a sticky row: that folder (header) scrolls to the top and is selected.
    pub(super) fn reveal_pinned(&mut self, i: usize) {
        let s = self.side as usize;
        self.off[s] = self.under_pinned(i);
        self.sel[s] = i;
        self.focus = Focus::Side;
    }

    /// explorer.autoReveal: as the active editor changes (a click in another group, a tab,
    /// a file opened), its file is shown in the Explorer: its folders opened, it selected
    /// and in view. Only on a change: a folder closed by hand stays closed meanwhile.
    pub(crate) fn auto_reveal(&mut self) -> bool {
        let key = self.active_tab_ref().map(|t| t.key.clone());
        if key == self.revealed || self.tree.is_empty() {
            return false;
        }
        self.revealed = key.clone();
        let rel = key.as_deref().and_then(|k| ["file:", "preview:", "diff:staged:", "diff:changes:"].iter().find_map(|p| k.strip_prefix(p)));
        match rel {
            Some(rel) if self.settings.auto_reveal => self.reveal_in_explorer(rel),
            _ => false,
        }
    }

    /// `rel` in the Explorer: the folders down to it opened, it selected and in view (when
    /// another view shows, as it will be in the Explorer).
    pub(crate) fn reveal_in_explorer(&mut self, rel: &str) -> bool {
        let parts: Vec<&str> = rel.split('/').collect();
        let mut at = String::new();
        for part in &parts[..parts.len() - 1] {
            at = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
            let Some(i) = self.tree.iter().position(|n| n.dir && n.path == at) else { return false };
            if !self.tree[i].open {
                self.toggle(i);
            }
        }
        let Some(i) = self.tree.iter().position(|n| n.path == rel) else { return false };
        let s = Side::Explorer as usize;
        if self.sel[s] == i && self.side != Side::Explorer {
            return false;
        }
        self.sel[s] = i;
        if self.side == Side::Explorer {
            self.move_side(0); // in view, under its pinned folders
        } else {
            self.off[s] = i.saturating_sub(3);
        }
        true
    }

    pub(super) fn move_side(&mut self, d: isize) {
        let n = self.side_count();
        if n == 0 {
            return;
        }
        let s = self.side as usize;
        self.sel[s] = (self.sel[s] as isize + d).clamp(0, n as isize - 1) as usize;
        if self.side == Side::Scm {
            let rows = self.scm_rows();
            let step = if d < 0 { -1 } else { 1 };
            for _ in 0..rows.len() {
                if !matches!(rows.get(self.sel[s]), Some(SRow::Gap)) {
                    break;
                }
                self.sel[s] = (self.sel[s] as isize + step).clamp(0, n as isize - 1) as usize;
            }
        }
        let h = self.ui.side_rows.max(1);
        if self.sel[s] < self.off[s] {
            self.off[s] = self.sel[s];
        } else if self.sel[s] >= self.off[s] + h {
            self.off[s] = self.sel[s] + 1 - h;
        }
        // A row under the sticky rows is hidden: show it just below its pinned folders.
        let pinned = self.pinned_now(self.off[s]).len();
        if pinned > 0 && self.sel[s] < self.off[s] + pinned + self.sticky_sep() {
            self.off[s] = self.under_pinned(self.sel[s]);
        }
    }
}
