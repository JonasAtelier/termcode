//! Git for the Source Control view: VS Code's "Staged Changes" / "Changes"
//! split and the write actions. Every call is argv with `--` before paths.

use crate::parse::StatusEntry;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

const CAP: u64 = 1 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Section {
    Staged,
    Changes,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScmFile {
    pub section: Section,
    pub path: String,
    pub orig: Option<String>,
    /// M A D R C T, U = untracked, ! = conflict.
    pub letter: char,
}

fn conflict(e: &StatusEntry) -> bool {
    matches!((e.x, e.y), ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U'))
}

/// Split porcelain entries the way VS Code shows them: untracked and
/// conflicted files live in "Changes"; a file can be in both sections.
pub fn split(entries: &[StatusEntry]) -> (Vec<ScmFile>, Vec<ScmFile>) {
    let (mut staged, mut changes) = (Vec::new(), Vec::new());
    for e in entries {
        let file = |section, letter| ScmFile { section, path: e.path.clone(), orig: e.orig.clone(), letter };
        if conflict(e) {
            changes.push(file(Section::Changes, '!'));
        } else if e.x == '?' {
            changes.push(file(Section::Changes, 'U'));
        } else if e.x != '!' {
            if e.x != ' ' {
                staged.push(file(Section::Staged, e.x));
            }
            if e.y != ' ' {
                changes.push(file(Section::Changes, e.y));
            }
        }
    }
    (staged, changes)
}

/// Explorer decoration letter for a status entry.
pub fn decoration(e: &StatusEntry) -> Option<char> {
    if conflict(e) {
        Some('!')
    } else if e.x == '?' {
        Some('U')
    } else if e.x == '!' {
        None
    } else if e.y != ' ' {
        Some(e.y)
    } else {
        Some(e.x)
    }
}

/// VS Code's decoration propagation: a folder takes the strongest decoration
/// below it (conflict over changes over untracked).
pub fn folder_marks<'a>(files: impl IntoIterator<Item = (&'a str, char)>) -> HashMap<String, char> {
    fn rank(c: char) -> u8 {
        match c {
            '!' => 3,
            'U' | '?' => 1,
            _ => 2,
        }
    }
    let mut dirs: HashMap<String, char> = HashMap::new();
    for (path, c) in files {
        let mut p = path;
        while let Some((dir, _)) = p.rsplit_once('/') {
            let d = dirs.entry(dir.to_string()).or_insert(c);
            if rank(c) > rank(*d) {
                *d = c;
            }
            p = dir;
        }
    }
    dirs
}

#[cfg(test)]
thread_local! {
    /// git commands built on this thread, for "no git without a repo" tests.
    pub static CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `git -C root`, with paths after `--` taken literally: a file named `*.txt`
/// must never act as a glob for `clean`/`restore`/`add`.
pub fn cmd(root: &Path) -> Command {
    #[cfg(test)]
    CALLS.with(|n| n.set(n.get() + 1));
    let mut c = Command::new("git");
    c.arg("--literal-pathspecs").arg("-C").arg(root).stdin(Stdio::null());
    c
}

/// Run git; Ok(stdout) or Err(first lines of stderr).
pub fn run(root: &Path, args: &[&str]) -> Result<String, String> {
    let o = cmd(root).args(args).output().map_err(|e| format!("git: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        Err(err.lines().take(3).collect::<Vec<_>>().join(" ").trim().to_string())
    }
}

/// `head -- paths`; with `orig`, a rename's source path is included too.
fn with_paths<'a>(head: &[&'a str], files: &'a [ScmFile], orig: bool) -> Vec<&'a str> {
    let mut v = head.to_vec();
    v.push("--");
    for f in files {
        if let Some(o) = f.orig.as_deref().filter(|_| orig) {
            v.push(o);
        }
        v.push(&f.path);
    }
    v
}

pub fn has_head(root: &Path) -> bool {
    run(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok()
}

/// `git add -A` so deletions are staged too.
pub fn stage(root: &Path, files: &[ScmFile]) -> Result<String, String> {
    run(root, &with_paths(&["add", "-A"], files, false))
}

/// A staged rename is unstaged with both its paths, or the source stays deleted in the index.
pub fn unstage(root: &Path, files: &[ScmFile]) -> Result<String, String> {
    if has_head(root) {
        run(root, &with_paths(&["restore", "--staged"], files, true))
    } else {
        // Before the first commit there is no HEAD to restore from.
        run(root, &with_paths(&["rm", "--cached", "-r", "-q"], files, false))
    }
}

/// Discard working-tree changes: tracked files go back to the index
/// (`git restore`), untracked files are deleted (`git clean -f`).
pub fn discard(root: &Path, files: &[ScmFile]) -> Result<String, String> {
    let (untracked, tracked): (Vec<ScmFile>, Vec<ScmFile>) = files.iter().cloned().partition(|f| f.letter == 'U');
    if !tracked.is_empty() {
        run(root, &with_paths(&["restore"], &tracked, false))?;
    }
    if !untracked.is_empty() {
        run(root, &with_paths(&["clean", "-f", "-q"], &untracked, false))?;
    }
    Ok(String::new())
}

/// Commit what is staged; never stages anything itself.
pub fn commit(root: &Path, message: &str) -> Result<String, String> {
    run(root, &["commit", "-q", "-m", message])
}

/// Up to 1 MB of `git <args>` stdout, ignoring the exit code. Past the cap the
/// pipe is closed, so git stops at its next write (EPIPE); no signal is sent.
pub fn capped(root: &Path, args: &[&str]) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(mut c) = cmd(root).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        if let Some(o) = c.stdout.take() {
            let _ = o.take(CAP).read_to_end(&mut buf);
        }
        let _ = c.wait();
    }
    buf
}

/// A blob (`:0:path` = the index), or None when git has no such object or it
/// is over 1 MB.
pub fn blob(root: &Path, spec: &str) -> Option<Vec<u8>> {
    let mut c = cmd(root).args(["cat-file", "blob", spec]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut buf = Vec::new();
    // The pipe closes at the end of this statement, so an oversized blob ends git.
    let read = c.stdout.take().map(|o| o.take(CAP + 1).read_to_end(&mut buf));
    let ok = c.wait().is_ok_and(|s| s.success());
    (ok && read.is_some_and(|r| r.is_ok()) && buf.len() as u64 <= CAP).then_some(buf)
}

fn show(root: &Path, spec: &str) -> Vec<u8> {
    capped(root, &["show", "--no-color", spec])
}

fn worktree(root: &Path, path: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    let p = root.join(path);
    // Regular files only: a FIFO would block the loader thread forever.
    if std::fs::metadata(&p).is_ok_and(|m| m.is_file()) {
        if let Ok(f) = std::fs::File::open(&p) {
            let _ = f.take(CAP).read_to_end(&mut buf);
        }
    }
    buf
}

/// The two sides VS Code compares for a row: HEAD vs index for staged
/// files, index vs working tree for changes.
pub fn diff_sides(root: &Path, f: &ScmFile) -> (Vec<u8>, Vec<u8>) {
    let old = f.orig.as_deref().unwrap_or(&f.path);
    let head = || show(root, &format!("HEAD:{old}"));
    let index = || show(root, &format!(":0:{}", f.path));
    match (f.section, f.letter) {
        (Section::Staged, 'A') => (Vec::new(), index()),
        (Section::Staged, 'D') => (head(), Vec::new()),
        (Section::Staged, _) => (head(), index()),
        (Section::Changes, 'U') => (Vec::new(), worktree(root, &f.path)),
        (Section::Changes, 'D') => (index(), Vec::new()),
        (Section::Changes, '!') => (head(), worktree(root, &f.path)),
        (Section::Changes, _) => (index(), worktree(root, &f.path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_porcelain_z;

    #[test]
    fn scm_rows_follow_vscode_sections() {
        let out = "MM both.rs\0A  new.rs\0 D gone.rs\0?? 新檔.txt\0UU clash.rs\0R  b.rs\0a.rs\0!! ignored\0";
        let (staged, changes) = split(&parse_porcelain_z(out.as_bytes()));
        let s: Vec<(&str, char)> = staged.iter().map(|f| (f.path.as_str(), f.letter)).collect();
        let c: Vec<(&str, char)> = changes.iter().map(|f| (f.path.as_str(), f.letter)).collect();
        assert_eq!(s, vec![("both.rs", 'M'), ("new.rs", 'A'), ("b.rs", 'R')]);
        assert_eq!(c, vec![("both.rs", 'M'), ("gone.rs", 'D'), ("新檔.txt", 'U'), ("clash.rs", '!')]);
        assert_eq!(staged[2].orig.as_deref(), Some("a.rs"));
        let e = parse_porcelain_z(b"MM x\0");
        assert_eq!(decoration(&e[0]), Some('M'));
        assert_eq!(decoration(&parse_porcelain_z(b"A  y\0")[0]), Some('A'));
    }

    #[test]
    fn folders_take_the_strongest_decoration_below() {
        let m = folder_marks([("a/b/x.c", 'U'), ("a/b/y.c", 'M'), ("a/z.c", '!'), ("top.c", 'M'), ("c/d/n.txt", 'U')]);
        assert_eq!(m.get("a/b"), Some(&'M'), "modified beats untracked");
        assert_eq!(m.get("a"), Some(&'!'), "a conflict wins");
        assert_eq!(m.get("c"), Some(&'U'));
        assert_eq!(m.get("c/d"), Some(&'U'));
        assert_eq!(m.len(), 4, "files at the top level decorate no folder");
    }

    #[test]
    fn argv_puts_paths_after_double_dash() {
        let f = [ScmFile { section: Section::Changes, path: "-rf".into(), orig: None, letter: 'U' }];
        assert_eq!(with_paths(&["clean", "-f"], &f, false), vec!["clean", "-f", "--", "-rf"]);
        let r = [ScmFile { section: Section::Staged, path: "new".into(), orig: Some("old".into()), letter: 'R' }];
        assert_eq!(with_paths(&["restore", "--staged"], &r, true), vec!["restore", "--staged", "--", "old", "new"]);
    }

    #[test]
    fn glob_named_files_are_literal() {
        let d = std::env::temp_dir().join(format!("herdr-code-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(&d).status().unwrap().success());
        for n in ["*.txt", "a.txt"] {
            std::fs::write(d.join(n), "x").unwrap();
        }
        let star = |section| [ScmFile { section, path: "*.txt".into(), orig: None, letter: 'U' }];
        stage(&d, &star(Section::Changes)).unwrap();
        let st = run(&d, &["status", "--porcelain"]).unwrap();
        assert!(st.contains("A  *.txt") && st.contains("?? a.txt"), "{st}");
        unstage(&d, &star(Section::Staged)).unwrap();
        discard(&d, &star(Section::Changes)).unwrap();
        assert!(!d.join("*.txt").exists());
        assert!(d.join("a.txt").exists(), "glob must not delete a.txt");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
