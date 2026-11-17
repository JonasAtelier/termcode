//! `App`: the find and replace widget.

use super::*;

impl App {
    // ---- find widget ----------------------------------------------------------------

    pub(super) fn open_find(&mut self, replace: bool) {
        if let Some(p) = self.hover.filter(|p| self.find.is_none() || !self.ui.find_area.contains(*p)) {
            let target = self.ui.groups.iter().enumerate().find_map(|(g, gh)| {
                let texts = gh.diff_text?;
                let key = &self.groups.get(g)?.tabs.get(self.groups[g].active)?.key;
                (gh.text.contains(p) && matches!(self.docs.get(key), Some(Doc::Diff(_))))
                    .then_some((g, u8::from(p.x > texts[0].right())))
            });
            if let Some((g, side)) = target {
                self.ag = g;
                if let Some(tab) = self.active_tab() {
                    if tab.view.side != side {
                        tab.view.anchor = None;
                    }
                    tab.view.side = side;
                }
            }
        }
        let diff_side = self.groups.get(self.ag).and_then(|grp| grp.tabs.get(grp.active)).and_then(|t| {
            matches!(self.docs.get(&t.key), Some(Doc::Diff(_))).then_some(t.view.side)
        });
        let replace = replace && diff_side.is_none();
        let seed = self.active_selection().filter(|s| !s.contains('\n')).or_else(|| crate::media::selected_line(self));
        let f = self.find.get_or_insert(Find {
            query: String::new(),
            replace: None,
            field: 0,
            all: false,
            matches: Vec::new(),
            cur: None,
            case: false,
            word: false,
            diff_side,
            regex: false,
            invalid: false,
            stamp: (String::new(), u64::MAX, String::new(), false, false, false, None),
        });
        f.diff_side = diff_side;
        if diff_side.is_some() {
            f.replace = None;
        }
        if let Some(s) = seed {
            f.query = s;
        }
        if replace && f.replace.is_none() {
            f.replace = Some(String::new());
        }
        f.field = usize::from(replace);
        self.focus = Focus::Find;
        self.find_refresh();
    }

    /// Recompute matches when the query, the document or its content changed.
    pub fn find_refresh(&mut self) {
        if self.find.is_none() {
            return;
        }
        // A PDF: its words (read-only, so no replacing); the match shown is its view's.
        if let Some((key, ver, lines)) = crate::media::find_source(self) {
            let Some(f) = self.find.as_mut() else { return };
            (f.diff_side, f.replace, f.field) = (None, None, 0);
            let stamp = (key.clone(), ver, f.query.clone(), f.case, f.word, f.regex, None);
            if stamp != f.stamp {
                let asked = |s: &(String, u64, String, bool, bool, bool, Option<u8>)| (s.0.clone(), s.2.clone(), s.3, s.4, s.5);
                let new_query = asked(&stamp) != asked(&f.stamp);
                f.search(lines.iter().enumerate().map(|(i, l)| (i, l.as_str())));
                f.stamp = stamp;
                if new_query {
                    crate::media::set_find_cur(self, &key, None);
                }
            }
            let cur = crate::media::find_cur(self, &key);
            if let Some(f) = self.find.as_mut() {
                f.cur = cur.filter(|&i| i < f.matches.len());
            }
            return;
        }
        let Some(f) = self.find.as_mut() else { return };
        let Some(tab) = self.groups.get(self.ag).and_then(|grp| grp.tabs.get(grp.active)) else {
            f.matches.clear();
            f.cur = None;
            return;
        };
        let Some(doc) = self.docs.get(&tab.key) else {
            f.matches.clear();
            f.cur = None;
            return;
        };
        let version = match doc {
            Doc::Text(b) => {
                f.diff_side = None;
                b.version()
            }
            Doc::Diff(d) => {
                if f.stamp.0 != tab.key || f.diff_side.is_none() {
                    f.diff_side = Some(tab.view.side);
                }
                f.replace = None;
                f.field = 0;
                d.id.0
            }
        };
        let stamp = (tab.key.clone(), version, f.query.clone(), f.case, f.word, f.regex, f.diff_side);
        if stamp != f.stamp {
            let lines: Vec<(usize, &str)> = match doc {
                Doc::Text(b) => b.lines.iter().enumerate().map(|(i, l)| (i, l.as_str())).collect(),
                Doc::Diff(d) => d.rows.iter().enumerate().filter_map(|(i, r)| {
                    let cell = if f.diff_side == Some(0) { &r.left } else { &r.right };
                    cell.as_ref().map(|(_, text)| (i, text.as_str()))
                }).collect(),
            };
            f.search(lines);
            f.stamp = stamp;
        }
        let c = tab.view.cursor;
        f.cur = if f.diff_side.is_some_and(|side| side != tab.view.side) { None } else {
            f.matches.iter().position(|(a, e)| tab.view.anchor == Some(*a) && c == *e)
        };
    }

    pub fn find_step(&mut self, dir: isize) {
        if crate::media::find_step(self, dir.signum()) {
            return;
        }
        self.find_refresh();
        let Some(f) = self.find.as_ref() else { return };
        if f.matches.is_empty() {
            self.msg = "No results".into();
            return;
        }
        let matches = f.matches.clone();
        let cur = f.cur;
        let side = f.diff_side;
        let Some(tab) = self.active_tab() else { return };
        if let Some(side) = side {
            tab.view.side = side;
        }
        let c = tab.view.cursor;
        let n = matches.len();
        let i = match (cur, dir > 0) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => matches.iter().position(|(a, _)| *a >= c).unwrap_or(0),
            (None, false) => matches.iter().rposition(|(_, e)| *e <= c).unwrap_or(n - 1),
        };
        let (a, e) = matches[i];
        tab.view.anchor = Some(a);
        tab.view.cursor = e;
        self.reveal();
        self.find_refresh();
    }

    pub(super) fn replace_one(&mut self) {
        self.find_refresh();
        let Some(f) = self.find.as_ref() else { return };
        let with = f.replace.clone().unwrap_or_default();
        let re = f.compiled();
        if let Some(at) = f.cur.map(|i| f.matches[i].0) {
            if let Some((b, tab)) = self.ed() {
                let text = re.as_ref().map_or_else(|| with.clone(), |re| editor::expand(re, &b.lines[at.line], at.col, &with));
                b.insert(&mut tab.view, &text);
                tab.preview = false;
            }
        }
        self.find_step(1);
    }

    pub(super) fn replace_all(&mut self) {
        self.find_refresh();
        let Some(f) = self.find.as_ref() else { return };
        let (matches, with, re) = (f.matches.clone(), f.replace.clone().unwrap_or_default(), f.compiled());
        if let Some((b, tab)) = self.ed() {
            let each: Vec<_> = matches
                .iter()
                .map(|&(a, e)| (a, e, re.as_ref().map_or_else(|| with.clone(), |re| editor::expand(re, &b.lines[a.line], a.col, &with))))
                .collect();
            let n = b.replace_each(&mut tab.view, &each);
            if n > 0 {
                tab.preview = false;
            }
            self.msg = format!("replaced {n} occurrence(s)");
        }
        self.find_refresh();
    }

    pub fn find_button(&mut self, b: FindBtn) {
        match b {
            FindBtn::ReplaceOne => self.replace_one(),
            FindBtn::ReplaceAll => self.replace_all(),
            FindBtn::Case | FindBtn::Word => {
                if let Some(f) = self.find.as_mut() {
                    if b == FindBtn::Case {
                        f.case = !f.case;
                    } else {
                        f.word = !f.word;
                    }
                }
                self.find_refresh();
            }
            FindBtn::Regex => {
                if let Some(f) = self.find.as_mut() {
                    f.regex = !f.regex;
                }
                self.find_refresh();
            }
            FindBtn::SearchCase | FindBtn::SearchWord | FindBtn::SearchRegex => {
                let t = match b {
                    FindBtn::SearchCase => &mut self.s_opts.0,
                    FindBtn::SearchWord => &mut self.s_opts.1,
                    _ => &mut self.s_opts.2,
                };
                *t = !*t;
                self.start_search();
            }
        }
    }
}
