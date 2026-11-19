//! Installing extensions: `termcode ext install|list|remove|new`, and the same steps for
//! the Extensions view (manager.rs): fetch (a folder, or a git clone), show what it is
//! and may do, place it once the user agreed. Nothing here prints but the CLI.

use super::process::{installed, root, Manifest, MANIFEST};
use crate::api::Tier;
use serde_json::json;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: termcode ext install <folder|git-url> [--link] [--yes]
       termcode ext list
       termcode ext remove <id> [--yes]
       termcode ext new <id>
       termcode ext --skill      the guide for agents writing one
Extensions add a view to termcode: doc/extensions.md, and termcode ext new for a start.";

pub fn cli(args: &[String]) -> Result<(), String> {
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let link = args.iter().any(|a| a == "--link");
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    let root = root();
    match words.as_slice() {
        ["install", src] => {
            let f = fetch(&root, src, &|_| {})?;
            for l in describe(&f.m) {
                println!("{l}");
            }
            let old = Manifest::read(&root.join(&f.m.id)).ok();
            let q = old.map_or_else(|| "Install?".to_string(), |o| format!("Replace {} {} with {}?", o.id, o.version, f.m.version));
            if !confirm(&q, yes) {
                f.discard();
                return Err("not installed".into());
            }
            let id = f.m.id.clone();
            let dest = place(&root, f, link)?;
            if let Err(e) = build(&dest, false, &|_| {}) {
                let _ = remove(&root, &id, &|_| {});
                return Err(e);
            }
            println!("Installed in {}. Reload termcode's window (F1: Developer: Reload Window), or install from its Extensions view (+) to see it at once.", dest.display());
            Ok(())
        }
        ["list"] => {
            let (ok, bad) = installed(&root);
            if ok.is_empty() && bad.is_empty() {
                println!("No extensions in {}", root.display());
            }
            for m in ok {
                let linked = std::fs::read_link(&m.dir).map(|t| format!("  -> {}", t.display())).unwrap_or_default();
                println!("{}  {} {}  {}{}", m.id, m.name, m.version, perms(&m.permissions), linked);
            }
            for b in bad {
                println!("broken: {b}");
            }
            Ok(())
        }
        ["remove", id] => {
            let m = Manifest::read(&root.join(id)).map_err(|_| format!("no extension {id:?} (termcode ext list)"))?;
            if !confirm(&format!("Remove {} {}: {}?", m.name, m.version, what(&root, id)), yes) {
                return Err("not removed".into());
            }
            remove(&root, id, &|_| {})?;
            println!("Removed {id}.");
            Ok(())
        }
        ["new", id] => new(Path::new(id), id),
        [] if args.iter().any(|a| a == "--skill") => {
            print!("{}", include_str!("../../skills/termcode-extension/SKILL.md"));
            Ok(())
        }
        _ if args.iter().any(|a| a == "-h" || a == "--help") || args.is_empty() => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

pub fn perms(p: &[Tier]) -> String {
    let mut s = vec!["read"];
    s.extend(p.iter().map(|t| if *t == Tier::Edit { "edit" } else { "run" }));
    s.join(", ")
}

/// What the user agrees to: what it is, runs, may do.
pub fn describe(m: &Manifest) -> Vec<String> {
    let mut l = vec![format!("{} {} ({})", m.name, m.version, m.id)];
    if !m.description.is_empty() {
        l.push(m.description.clone());
    }
    if !m.build.is_empty() {
        l.push(format!("Builds: {}", m.build.join(" ")));
    }
    l.push(format!("Runs: {}", m.run.join(" ")));
    l.push(format!("May: {}", perms(&m.permissions)));
    l.push("It runs as you, with your files and network. Install only what you trust.".into());
    l
}

/// Asks on the terminal; false without one (and no --yes).
fn confirm(q: &str, yes: bool) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        let _ = writeln!(std::io::stderr(), "{q} (no terminal to ask: pass --yes)");
        return false;
    }
    print!("{q} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut a = String::new();
    let _ = std::io::stdin().lock().read_line(&mut a);
    matches!(a.trim(), "y" | "Y" | "yes")
}

/// A git URL termcode clones (https, ssh); else a folder.
fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("ssh://") || (s.starts_with("git@") && s.contains(':'))
}

/// An extension fetched, not yet installed: its manifest, its folder (a clone's, to
/// discard if the user says no).
#[derive(Debug)]
pub struct Fetched {
    pub m: Manifest,
    from: PathBuf,
    /// The clone `from` is in (to remove), when fetched by git.
    clone: Option<PathBuf>,
}

impl Fetched {
    pub fn discard(self) {
        if let Some(c) = self.clone {
            let _ = std::fs::remove_dir_all(c);
        }
    }
}

/// How far a step has got, 0 to 100 (told as it goes).
pub type Progress<'a> = &'a dyn Fn(u8);

/// What a tool says of its progress in a line of its output: "37%", or cargo's
/// "Building [====>   ] 12/45".
fn percent_in(seg: &str) -> Option<u8> {
    if let Some(i) = seg.find('%') {
        let digits: String = seg[..i].chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
        return digits.parse::<u32>().ok().filter(|p| *p <= 100).map(|p| p as u8);
    }
    let rest = seg.trim_start().strip_prefix("Building")?;
    let (n, m) = rest.split([' ', ']', ':']).find_map(|w| w.split_once('/').and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))))?;
    (m > 0).then(|| (n.min(m) * 100 / m) as u8)
}

/// A pipe's output, a line (or a progress update: up to a \r) at a time.
fn follow(mut r: impl std::io::Read, mut on: impl FnMut(&str)) {
    let mut buf = [0u8; 4096];
    let mut seg = Vec::new();
    while let Ok(n) = r.read(&mut buf) {
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' {
                on(&String::from_utf8_lossy(&seg));
                seg.clear();
            } else {
                seg.push(b);
            }
        }
    }
    if !seg.is_empty() {
        on(&String::from_utf8_lossy(&seg));
    }
}

/// `src` read: a folder as it is, a git URL cloned (no prompts, no ext:: or file::
/// transports, its progress told); `URL#folder` takes the extension in that folder.
pub fn fetch(root: &Path, src: &str, on: Progress) -> Result<Fetched, String> {
    let src = src.trim();
    if !is_url(src) {
        let from = std::fs::canonicalize(src).map_err(|e| format!("{src}: {e}"))?;
        return Ok(Fetched { m: Manifest::read(&from)?, from, clone: None });
    }
    let (src, sub) = src.split_once('#').map_or((src, ""), |(u, s)| (u, s.trim_matches('/')));
    if !Path::new(sub).components().all(|c| matches!(c, std::path::Component::Normal(_))) {
        return Err(format!("#{sub}: a folder inside the repository"));
    }
    let _ = std::fs::create_dir_all(root);
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp = root.join(format!(".install-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut child = std::process::Command::new("git")
        .args(["-c", "protocol.ext.allow=never", "-c", "protocol.file.allow=never", "clone", "--progress", "--depth", "1", "--", src])
        .arg(&tmp)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git: {e}"))?;
    // Receiving the objects is most of it; resolving the deltas the last tenth.
    let mut why = String::new();
    if let Some(err) = child.stderr.take() {
        follow(err, |seg| match percent_in(seg) {
            Some(p) if seg.contains("Receiving objects") => on((u32::from(p) * 9 / 10) as u8),
            Some(p) if seg.contains("Resolving deltas") => on(90 + p / 10),
            Some(_) => {}
            None if !seg.trim().is_empty() => why = seg.trim().to_string(),
            None => {}
        });
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    if !ok {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!("git clone {src} failed{}", if why.is_empty() { String::new() } else { format!(": {why}") }));
    }
    on(100);
    let from = tmp.join(sub);
    match Manifest::read(&from) {
        Ok(m) => Ok(Fetched { m, from, clone: Some(tmp) }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

/// `f` installed under `root` (replacing one with its id): a clone moved in, a folder
/// copied, or (`link`) linked. Its folder.
pub fn place(root: &Path, f: Fetched, link: bool) -> Result<PathBuf, String> {
    let _ = std::fs::create_dir_all(root);
    let dest = root.join(&f.m.id);
    let done = (|| {
        if std::fs::symlink_metadata(&dest).is_ok() {
            remove_path(&dest)?;
        }
        match &f.clone {
            None if link => std::os::unix::fs::symlink(&f.from, &dest).map_err(|e| e.to_string()),
            None => copy_dir(&f.from, &dest),
            // From a clone: its folder moved in, the rest of the clone (its history) gone.
            Some(_) => {
                let _ = std::fs::remove_dir_all(f.from.join(".git"));
                std::fs::rename(&f.from, &dest).map_err(|e| e.to_string())
            }
        }
    })();
    f.discard();
    done.map(|()| dest)
}

fn remove_path(p: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if meta.is_symlink() || meta.is_file() { std::fs::remove_file(p) } else { std::fs::remove_dir_all(p) }.map_err(|e| format!("{}: {e}", p.display()))
}

/// `from` into `to`, files and folders (no .git; links as links, never followed).
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let t = e.file_type().map_err(|e| e.to_string())?;
        // Not its history, nor a Rust build's output (the build step makes its own).
        if e.file_name() == ".git" || (t.is_dir() && e.file_name() == "target") {
            continue;
        } else if t.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src).map_err(|e| e.to_string())?, &dst).map_err(|e| e.to_string())?;
        } else if t.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
    }
    Ok(())
}

/// Its `build` command run in its folder (`dest`), if it has one: on the terminal, or
/// (`quiet`) its progress told from what it prints ("NN%", cargo's "Building … n/m",
/// which cargo prints here too), its output's end in the error.
pub fn build(dest: &Path, quiet: bool, on: Progress) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let m = Manifest::read(dest)?;
    let Some((prog, args)) = m.build.split_first() else { return Ok(()) };
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(dest).stdin(Stdio::null()).env("TERMCODE_EXTENSION_DIR", dest);
    if !quiet {
        let ok = cmd.status().map_err(|e| format!("{prog}: {e}"))?.success();
        return if ok { Ok(()) } else { Err(format!("its build failed ({})", m.build.join(" "))) };
    }
    cmd.env("CARGO_TERM_PROGRESS_WHEN", "always").env("CARGO_TERM_PROGRESS_WIDTH", "100").stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    // Both streams read (one in a thread), what each says of its progress told.
    let mut tail = String::new();
    let out = child.stdout.take();
    let side = std::thread::spawn(move || {
        let mut t = String::new();
        if let Some(o) = out {
            follow(o, |seg| {
                if !seg.trim().is_empty() {
                    t = seg.trim().to_string();
                }
            });
        }
        t
    });
    if let Some(err) = child.stderr.take() {
        follow(err, |seg| match percent_in(seg) {
            Some(p) => on(p),
            None if !seg.trim().is_empty() => tail = seg.trim().to_string(),
            None => {}
        });
    }
    let ok = child.wait().is_ok_and(|s| s.success());
    let out_tail = side.join().unwrap_or_default();
    if ok {
        on(100);
        return Ok(());
    }
    let tail = if tail.is_empty() { out_tail } else { tail };
    Err(format!("its build failed ({}){}", m.build.join(" "), if tail.is_empty() { String::new() } else { format!(": {tail}") }))
}

/// What removing `id` takes: its folder, or only the link to one.
pub fn what(root: &Path, id: &str) -> String {
    let dest = root.join(id);
    match std::fs::read_link(&dest) {
        Ok(t) => format!("the link to {}", t.display()),
        Err(_) => dest.display().to_string(),
    }
}

/// Extension `id` removed, file by file (its progress told): only the link, if it is one.
pub fn remove(root: &Path, id: &str, on: Progress) -> Result<(), String> {
    let dest = root.join(id);
    Manifest::read(&dest).map_err(|_| format!("no extension {id:?}"))?;
    if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.is_symlink()) {
        remove_path(&dest)?;
        on(100);
        return Ok(());
    }
    fn count(p: &Path) -> usize {
        std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| if e.file_type().is_ok_and(|t| t.is_dir()) { 1 + count(&e.path()) } else { 1 }).sum()
    }
    fn rm(p: &Path, done: &mut usize, total: usize, on: Progress) -> std::io::Result<()> {
        for e in std::fs::read_dir(p)?.flatten() {
            if e.file_type()?.is_dir() {
                rm(&e.path(), done, total, on)?;
                std::fs::remove_dir(e.path())?;
            } else {
                std::fs::remove_file(e.path())?;
            }
            *done += 1;
            on((*done * 100 / total.max(1)).min(100) as u8);
        }
        Ok(())
    }
    let total = count(&dest);
    rm(&dest, &mut 0, total, on).and_then(|()| std::fs::remove_dir(&dest)).map_err(|e| format!("{}: {e}", dest.display()))?;
    on(100);
    Ok(())
}

/// A start: a manifest and a Python extension filling every slot (doc/extensions.md).
fn new(dir: &Path, id: &str) -> Result<(), String> {
    if std::fs::symlink_metadata(dir).is_ok() {
        return Err(format!("{} exists", dir.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let manifest = json!({"id": id, "name": id, "version": "0.1.0", "description": "", "icon": "◆", "run": ["python3", "main.py"], "permissions": []});
    let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).map_err(|e| e.to_string());
    write(MANIFEST, &(serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n"))?;
    write("main.py", TEMPLATE)?;
    Manifest::read(dir)?;
    println!("Made {}. Try it: termcode ext install {} --link, then reload termcode.", dir.display(), dir.display());
    Ok(())
}

const TEMPLATE: &str = include_str!("../../examples/extension/main.py");

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("termcode-ext-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_copies_or_links_a_folder_and_remove_takes_it_away() {
        let (root, src) = (tmp("root"), tmp("src"));
        std::fs::write(src.join(MANIFEST), json!({"id": "demo", "version": "1", "run": ["./run.sh"]}).to_string()).unwrap();
        std::fs::write(src.join("run.sh"), "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        place(&root, fetch(&root, src.to_str().unwrap(), &|_| {}).unwrap(), false).unwrap();
        assert!(root.join("demo/run.sh").is_file() && !root.join("demo/.git").exists());
        assert_eq!(installed(&root).0.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["demo"]);
        place(&root, fetch(&root, src.to_str().unwrap(), &|_| {}).unwrap(), true).unwrap();
        assert!(std::fs::symlink_metadata(root.join("demo")).unwrap().is_symlink(), "--link replaces the copy with a link");
        // Removed file by file, its progress told up to 100.
        let seen = std::sync::Mutex::new(Vec::new());
        remove(&root, "demo", &|p| seen.lock().unwrap().push(p)).unwrap();
        assert_eq!(seen.lock().unwrap().last(), Some(&100));
        assert!(src.join("run.sh").is_file(), "removing a link leaves its folder");
        assert!(installed(&root).0.is_empty());
        assert!(remove(&root, "demo", &|_| {}).is_err());
        assert!(fetch(&root, "/nonexistent-folder", &|_| {}).is_err());
        // Progress as tools print it.
        assert_eq!(percent_in("Receiving objects:  37% (10/27)"), Some(37));
        assert_eq!(percent_in("    Building [=====>      ] 12/48: serde"), Some(25));
        assert_eq!(percent_in("   Compiling libc v0.2.1"), None);
        let n = tmp("new");
        new(&n.join("mine"), "mine").unwrap();
        assert_eq!(Manifest::read(&n.join("mine")).unwrap().run, ["python3", "main.py"]);
        for d in [root, src, n] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
