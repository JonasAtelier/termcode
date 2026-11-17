//! `App`: search in files.

use super::*;

impl App {
    // ---- search ------------------------------------------------------------------

    /// Search for the query and toggles as they are now (Enter or a toggle).
    pub fn start_search(&mut self) {
        let req = (self.query.clone(), self.s_opts);
        self.run_search(req);
    }

    pub(super) fn run_search(&mut self, (query, (case, word, regex)): (String, (bool, bool, bool))) {
        self.stop_search();
        self.hits.clear();
        self.search_closed.clear();
        self.sel[1] = 0;
        self.off[1] = 0;
        self.queued_search = None;
        if query.is_empty() {
            return;
        }
        // Without kill, an abandoned rg runs until it ends: cap them, and run only
        // the latest request, as submitted, once one has ended. The slot is ours from here.
        if !take_slot(&self.live_rg) {
            self.queued_search = Some((query, (case, word, regex)));
            self.msg = "waiting for the previous search…".into();
            return;
        }
        let mut flags = vec![if case { "--case-sensitive" } else { "--ignore-case" }];
        if word {
            flags.push("--word-regexp");
        }
        if !regex {
            flags.push("--fixed-strings");
        }
        let child = Command::new("rg")
            .args(["--null", "--line-number", "--column", "--no-heading", "--color", "never", "--max-columns", "300"])
            .args(flags)
            .arg("--")
            .arg(&query)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.live_rg.fetch_sub(1, Ordering::SeqCst);
                self.msg = format!("rg: {e}");
                return;
            }
        };
        let Some(out) = child.stdout.take() else {
            reap(child, Some(self.live_rg.clone()));
            return;
        };
        let (tx, gen) = (self.tx.clone(), self.gen);
        let stop = Arc::new(AtomicBool::new(false));
        self.search_stop = stop.clone();
        std::thread::spawn(move || {
            let mut sent = 0;
            for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
                // Cancelled or past the cap: stop reading. Closing the pipe ends rg
                // at its next write (EPIPE); no signal is sent.
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let Some(h) = parse::parse_rg_line(&String::from_utf8_lossy(&line)) else { continue };
                sent += 1;
                if tx.send((gen, Some(h))).is_err() || sent > MAX_HITS {
                    return;
                }
            }
            let _ = tx.send((gen, None));
        });
        self.search = Some(child);
        self.msg = format!("searching {query:?}…");
    }

    /// Abandon the running search: its reader stops, rg exits on the closed
    /// pipe (or when done) and is reaped in the background.
    pub fn stop_search(&mut self) {
        self.gen += 1;
        self.search_stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.search.take() {
            reap(c, Some(self.live_rg.clone()));
        }
    }

    /// Pull streamed search hits and finished background loads.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok((gen, hit)) = self.rx.try_recv() {
            if gen != self.gen {
                continue;
            }
            changed = true;
            match hit {
                Some(_) if self.hits.len() >= MAX_HITS => {
                    self.stop_search();
                    self.msg = format!("{MAX_HITS}+ hits, search stopped");
                }
                Some(h) => self.hits.push(h),
                None => {
                    // rg closed stdout, so it is exiting: wait (not kill) to keep its exit code.
                    self.gen += 1;
                    let code = self.search.take().and_then(|mut c| {
                        let st = c.wait().ok();
                        self.live_rg.fetch_sub(1, Ordering::SeqCst);
                        st
                    });
                    let code = code.and_then(|s| s.code());
                    let n = self.hits.len();
                    self.msg = match code {
                        Some(2) if n == 0 => "rg error (invalid pattern?)".into(),
                        Some(2) => format!("{n} hits for {:?} (some files unreadable)", self.query),
                        _ => format!("{n} hits for {:?}", self.query),
                    };
                }
            }
        }
        while let Ok((key, gen, load)) = self.load_rx.try_recv() {
            if self.loading.get(&key) != Some(&gen) {
                continue;
            }
            self.loading.remove(&key);
            if let Load::Status(st) = load {
                let since = self.status_since;
                if let Some(t) = since {
                    self.status_took = t.elapsed();
                }
                // A poll that found what the last status said changes nothing: no frame.
                let seen = (st.0.clone(), st.1.clone(), format!("{:?}", st.2));
                if std::mem::take(&mut self.status_polled) && !self.status_again && self.status_seen.as_ref() == Some(&seen) {
                    self.status_since = None;
                    continue;
                }
                self.status_seen = Some(seen);
                self.apply_status(st);
                if std::mem::take(&mut self.status_again) {
                    // What landed is already older than the refresh asked for meanwhile:
                    // the spinner keeps its start, so it stays on until the rerun lands.
                    self.refresh_status();
                    self.status_since = since.or(self.status_since);
                }
                changed = true;
                continue;
            }
            if let Load::Base(blob) = load {
                let doc = key.trim_start_matches("base:").to_string();
                // Decoded exactly as the buffer is, so Revert writes the index's own lines.
                let utf8 = blob.as_ref().is_none_or(|b| std::str::from_utf8(b).is_ok());
                let base: Option<Vec<String>> = blob.map(|b| editor::split_lines(&String::from_utf8_lossy(&b)).0);
                // Focus and view switches refetch the index: an unchanged one changes nothing.
                if self.docs.contains_key(&doc) && self.qdiff.get(&doc).is_none_or(|q| q.base != base) {
                    self.qdiff.insert(doc, QDiff { base, base_utf8: utf8, ..Default::default() });
                    self.qdiff_due.get_or_insert(Instant::now() + QDIFF_DEBOUNCE);
                }
                continue;
            }
            // A tab closed meanwhile does not resurrect its document.
            if !self.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key)) {
                continue;
            }
            let doc = match load {
                Load::Diff(d) => Doc::Diff(d),
                Load::Base(_) | Load::Status(..) | Load::Cancelled => continue,
            };
            // A refresh that changed the rows drops the selection: it would mark other text.
            let stale = matches!((self.docs.get(&key), &doc), (Some(Doc::Diff(a)), Doc::Diff(b)) if a.rows != b.rows);
            if stale {
                for t in self.groups.iter_mut().flat_map(|g| g.tabs.iter_mut()).filter(|t| t.key == key) {
                    t.view.anchor = None;
                }
            }
            self.docs.insert(key, doc);
            self.find_refresh();
            changed = true;
        }
        if self.queued_search.is_some() && self.live_rg.load(Ordering::SeqCst) < MAX_LIVE_RG {
            if let Some(req) = self.queued_search.take() {
                self.run_search(req);
            }
            changed = true;
        }
        changed | crate::navui::poll(self) | crate::scm::drain(self)
    }

    pub(super) fn load(&mut self, key: String, job: impl FnOnce(PathBuf) -> Load + Send + 'static) {
        self.load_gen += 1;
        let gen = self.load_gen;
        self.loading.insert(key.clone(), gen);
        // Every job is git's: it runs in the repo.
        let (tx, root) = (self.load_tx.clone(), self.git_root.clone().unwrap_or_else(|| self.root.clone()));
        std::thread::spawn(move || {
            let r = job(root);
            let _ = tx.send((key, gen, r));
        });
    }

    pub(super) fn load_diff(&mut self, key: String, f: ScmFile, cancel: Option<Arc<AtomicBool>>) {
        self.load(key, move |root| {
            let cancelled = || cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst));
            if cancelled() {
                return Load::Cancelled;
            }
            let (old, new) = git::diff_sides(&root, &f);
            if cancelled() {
                return Load::Cancelled;
            }
            let binary = |b: &[u8]| b[..b.len().min(8192)].contains(&0);
            let side = match f.section {
                Section::Staged => "Index",
                Section::Changes if f.letter == 'U' => "Untracked",
                Section::Changes => "Working Tree",
            };
            let title = format!("{} ({side})", base(&f.path));
            let (rows, note) = if binary(&old) || binary(&new) {
                (Vec::new(), Some("binary file: no text diff".to_string()))
            } else {
                (diff::align(&String::from_utf8_lossy(&old), &String::from_utf8_lossy(&new)), None)
            };
            Load::Diff(DiffDoc { title, rows, note, file: f, id: Default::default(), patch: None })
        });
    }
}
