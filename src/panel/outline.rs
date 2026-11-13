//! `App`: the Outline's keys and clicks (ui.rs draws it, from the active editor's
//! symbols, and leaves its rows in `ui.outline_items`).

use super::*;

impl App {
    /// The selected row's index among the rows shown.
    fn outline_at(&self) -> Option<usize> {
        let p = self.outline_sel.as_ref()?;
        self.ui.outline_items.iter().position(|it| &it.path == p)
    }

    fn outline_select(&mut self, k: usize) {
        if let Some(it) = self.ui.outline_items.get(k) {
            self.outline_sel = Some(it.path.clone());
        }
    }

    /// Folds (`open` false) or unfolds row `k`; None flips it.
    fn outline_fold(&mut self, k: usize, open: Option<bool>) {
        let Some(it) = self.ui.outline_items.get(k).filter(|it| it.kids) else { return };
        let Some(key) = self.groups.get(self.ag).and_then(|g| g.tabs.get(g.active)).map(|t| t.key.clone()) else { return };
        let id = format!("{key}\n{}", it.path);
        if open.unwrap_or(!it.open) {
            self.outline_folded.remove(&id);
        } else {
            self.outline_folded.insert(id);
        }
    }

    /// Row `k`'s symbol in the editor; the Outline keeps the keys unless `focus_editor`.
    fn outline_go(&mut self, k: usize, focus_editor: bool) {
        let Some(to) = self.ui.outline_items.get(k).map(|it| it.to) else { return };
        self.outline_select(k);
        self.jump_to(to);
        if !focus_editor {
            self.focus = Focus::Outline;
        }
    }

    /// VS Code's tree keys: ↑ ↓ PgUp PgDn Home End move, → unfolds (or steps in), ← folds
    /// (or steps out), Enter goes there, Space shows it and stays, Esc goes back.
    pub(super) fn outline_key(&mut self, k: KeyEvent) {
        let items = &self.ui.outline_items;
        let n = items.len();
        if n == 0 {
            if k.code == KeyCode::Esc {
                self.focus = Focus::Editor;
            }
            return;
        }
        let at = self.outline_at();
        let page = self.ui.outline.height.max(1) as isize;
        let by = |d: isize| at.map_or(0, |a| (a as isize + d).clamp(0, n as isize - 1) as usize);
        let a = at.unwrap_or(0);
        let it = items[a].clone();
        match k.code {
            KeyCode::Up => self.outline_select(by(-1)),
            KeyCode::Down => self.outline_select(by(1)),
            KeyCode::PageUp => self.outline_select(by(-page)),
            KeyCode::PageDown => self.outline_select(by(page)),
            KeyCode::Home => self.outline_select(0),
            KeyCode::End => self.outline_select(n - 1),
            KeyCode::Right if it.kids && !it.open => self.outline_fold(a, Some(true)),
            KeyCode::Right if it.kids => self.outline_select(a + 1),
            KeyCode::Left if it.kids && it.open => self.outline_fold(a, Some(false)),
            KeyCode::Left => {
                if let Some(p) = (0..a).rev().find(|&j| items[j].depth < it.depth) {
                    self.outline_select(p);
                }
            }
            KeyCode::Enter => self.outline_go(a, true),
            KeyCode::Char(' ') => self.outline_go(a, false),
            KeyCode::Esc => self.focus = Focus::Editor,
            _ => {}
        }
    }

    /// A click in the Outline: its chevron folds or unfolds the row; the row goes to its
    /// symbol, the Outline keeping the keys (a double-click gives them to the editor).
    pub(super) fn outline_click(&mut self, p: Position, double: bool) {
        self.focus = Focus::Outline;
        if let Some(&(_, k)) = self.ui.outline_chevs.iter().find(|(r, _)| r.contains(p)) {
            self.outline_select(k);
            return self.outline_fold(k, None);
        }
        if let Some(&(_, k)) = self.ui.outline_rows.iter().find(|(r, _)| r.contains(p)) {
            self.outline_go(k, double);
        }
    }
}
