//! `App`: Quick Open and the command palette.

use super::*;

impl App {
    // ---- quick open ----------------------------------------------------------------------

    pub(super) fn open_quick(&mut self) {
        if self.files.is_empty() {
            // From the root, ls-files lists only what is under it, relative to it.
            let out = match self.git_root {
                Some(_) => git::cmd(&self.root).args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).stderr(Stdio::null()).output(),
                None => Command::new("rg").args(["--files", "--null"]).current_dir(&self.root).stdin(Stdio::null()).stderr(Stdio::null()).output(),
            };
            let out = out.ok().filter(|o| o.status.success()).map(|o| o.stdout).unwrap_or_default();
            self.files = out
                .split(|&b| b == 0)
                .filter(|f| !f.is_empty())
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            self.files.dedup();
        }
        let items = filter(&self.files, "");
        self.quick = Some(Quick { query: String::new(), items, sel: 0, off: 0, commands: false, goto: Vec::new() });
    }

    pub(super) fn open_palette(&mut self) {
        let items = filter_commands_recent("", &self.recent_cmds);
        self.quick = Some(Quick { query: String::new(), items, sel: 0, off: 0, commands: true, goto: Vec::new() });
    }

    /// Quick Open already typed `prefix`: `:` Go to Line, `@` Go to Symbol.
    pub(super) fn open_goto(&mut self, prefix: &str) {
        self.open_quick();
        let (items, goto) = self.quick_rows(prefix);
        if let Some(q) = self.quick.as_mut() {
            q.query = prefix.into();
            (q.items, q.goto) = (items, goto);
        }
    }

    /// Quick Open's rows for `query`: files; after `:` the line to go to, after `@` the
    /// active editor's symbols, each with where it goes.
    pub(super) fn quick_rows(&mut self, query: &str) -> (Vec<String>, Vec<(Pos, String)>) {
        let mode = query.chars().next().filter(|c| matches!(c, ':' | '@'));
        let Some(mode) = mode else { return (filter(&self.files, query), Vec::new()) };
        let rest = query[1..].trim();
        let Some((b, tab)) = self.ed() else {
            return (vec![format!("Open a text editor first to go to a {}.", if mode == ':' { "line" } else { "symbol" })], Vec::new());
        };
        if mode == ':' {
            let n = b.lines.len();
            let mut nums = rest.split([':', ',']).map(|s| s.trim().parse::<usize>().ok());
            let Some(line) = nums.next().flatten() else {
                let c = b.clamp(tab.view.cursor);
                let ch = editor::display_col(&b.lines[c.line], c.col) + 1;
                return (vec![format!("Current Line: {}, Character: {ch}. Type a line number between 1 and {n} to navigate to.", c.line + 1)], Vec::new());
            };
            let line = line.clamp(1, n);
            let ch = nums.next().flatten();
            let col = ch.map_or(0, |ch| editor::byte_at_display(&b.lines[line - 1], ch.saturating_sub(1)));
            let label = match ch {
                Some(ch) => format!("Go to line {line}, character {ch}."),
                None => format!("Go to line {line}."),
            };
            return (vec![label], vec![(Pos::new(line - 1, col), String::new())]);
        }
        let lines = b.lines.clone();
        let mut hits: Vec<(usize, usize, String, (Pos, String))> = b
            .symbols()
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let tier = if rest.is_empty() { 0 } else { parse::match_score(&s.name, rest)? / 100_000 };
                let col = lines[s.line].find(s.name.as_str()).unwrap_or(0);
                let label = format!("{}{}", "  ".repeat(s.level.saturating_sub(1)), s.name);
                Some((tier, i, label, (Pos::new(s.line, col), s.kind.to_string())))
            })
            .collect();
        if hits.is_empty() {
            let why = if rest.is_empty() { "No symbols found in this file." } else { "No matching symbols." };
            return (vec![why.into()], Vec::new());
        }
        hits.sort_by_key(|h| (h.0, h.1));
        hits.into_iter().map(|(_, _, label, to)| (label, to)).unzip()
    }

    /// Enter or click on a Quick Open / palette row.
    pub(super) fn quick_pick(&mut self, i: usize) {
        let Some(q) = self.quick.take() else { return };
        if !q.commands && matches!(q.query.chars().next(), Some(':' | '@')) {
            let Some(&(p, _)) = q.goto.get(i) else { return };
            let h = self.ui.groups.get(self.ag).map_or(20, |g| g.text.height.max(1) as usize);
            if let Some((b, tab)) = self.ed() {
                b.move_to(&mut tab.view, p, false);
                tab.view.want_x = None;
                // Centred, as VS Code reveals a line it jumps to.
                let v = &mut tab.view;
                v.scroll = v.line_of(v.row_of(p.line).saturating_sub(h / 2));
            }
            self.focus = Focus::Editor;
            self.reveal();
            return;
        }
        let Some(item) = q.items.get(i).cloned() else { return };
        if q.commands {
            self.recent_cmds.retain(|r| *r != item);
            self.recent_cmds.insert(0, item.clone());
            self.recent_cmds.truncate(MAX_RECENT);
            self.save_layout();
            if let Some(c) = all_commands().find(|c| c.0 == item) {
                self.run_command(c.1);
            }
        } else {
            self.open_file(self.ag, &item, false);
            self.focus = Focus::Editor;
        }
    }

    pub(super) fn quick_key(&mut self, k: KeyEvent, modded: bool) {
        let Some(q) = self.quick.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.quick = None,
            KeyCode::Enter => {
                let i = q.sel;
                self.quick_pick(i);
            }
            KeyCode::Up => q.sel = q.sel.saturating_sub(1),
            KeyCode::Down => q.sel = (q.sel + 1).min(q.items.len().saturating_sub(1)),
            KeyCode::Backspace | KeyCode::Char(_) => {
                match k.code {
                    KeyCode::Char(c) if !modded => q.query.push(c),
                    KeyCode::Backspace => {
                        q.query.pop();
                    }
                    _ => return,
                }
                let (query, commands) = (q.query.clone(), q.commands);
                let (items, goto) = if commands { (filter_commands_recent(&query, &self.recent_cmds), Vec::new()) } else { self.quick_rows(&query) };
                if let Some(q) = self.quick.as_mut() {
                    (q.items, q.goto, q.sel) = (items, goto, 0);
                }
            }
            _ => {}
        }
    }
}
