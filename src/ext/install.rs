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
            let f = fetch(&root, src)?;
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
            if let Err(e) = build(&dest, false) {
                let _ = remove(&root, &id);
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
            remove(&root, id)?;
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
    clone: bool,
}

impl Fetched {
    pub fn discard(self) {
        if self.clone {
            let _ = std::fs::remove_dir_all(&self.from);
        }
    }
}

/// `src` read: a folder as it is, a git URL cloned (no prompts, no ext:: or file:: transports).
pub fn fetch(root: &Path, src: &str) -> Result<Fetched, String> {
    let src = src.trim();
    if !is_url(src) {
        let from = std::fs::canonicalize(src).map_err(|e| format!("{src}: {e}"))?;
        return Ok(Fetched { m: Manifest::read(&from)?, from, clone: false });
    }
    let _ = std::fs::create_dir_all(root);
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let tmp = root.join(format!(".install-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&tmp);
    let out = std::process::Command::new("git")
        .args(["-c", "protocol.ext.allow=never", "-c", "protocol.file.allow=never", "clone", "--quiet", "--depth", "1", "--", src])
        .arg(&tmp)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        let why = String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim().to_string();
        return Err(format!("git clone {src} failed{}", if why.is_empty() { String::new() } else { format!(": {why}") }));
    }
    match Manifest::read(&tmp) {
        Ok(m) => Ok(Fetched { m, from: tmp, clone: true }),
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
        if link && !f.clone {
            std::os::unix::fs::symlink(&f.from, &dest).map_err(|e| e.to_string())
        } else if f.clone {
            let _ = std::fs::remove_dir_all(f.from.join(".git"));
            std::fs::rename(&f.from, &dest).map_err(|e| e.to_string())
        } else {
            copy_dir(&f.from, &dest)
        }
    })();
    if done.is_err() {
        f.discard();
    }
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
/// (`quiet`) with its output's end in the error.
pub fn build(dest: &Path, quiet: bool) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let m = Manifest::read(dest)?;
    let Some((prog, args)) = m.build.split_first() else { return Ok(()) };
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(dest).stdin(Stdio::null()).env("TERMCODE_EXTENSION_DIR", dest);
    let out = if quiet { cmd.output() } else { cmd.status().map(|status| std::process::Output { status, stdout: Vec::new(), stderr: Vec::new() }) };
    let out = out.map_err(|e| format!("{prog}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let tail: String = String::from_utf8_lossy(&out.stderr).lines().rfind(|l| !l.trim().is_empty()).unwrap_or("").to_string();
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

pub fn remove(root: &Path, id: &str) -> Result<(), String> {
    let dest = root.join(id);
    Manifest::read(&dest).map_err(|_| format!("no extension {id:?}"))?;
    remove_path(&dest)
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
        place(&root, fetch(&root, src.to_str().unwrap()).unwrap(), false).unwrap();
        assert!(root.join("demo/run.sh").is_file() && !root.join("demo/.git").exists());
        assert_eq!(installed(&root).0.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["demo"]);
        place(&root, fetch(&root, src.to_str().unwrap()).unwrap(), true).unwrap();
        assert!(std::fs::symlink_metadata(root.join("demo")).unwrap().is_symlink(), "--link replaces the copy with a link");
        remove(&root, "demo").unwrap();
        assert!(src.join("run.sh").is_file(), "removing a link leaves its folder");
        assert!(installed(&root).0.is_empty());
        assert!(remove(&root, "demo").is_err());
        assert!(fetch(&root, "/nonexistent-folder").is_err());
        let n = tmp("new");
        new(&n.join("mine"), "mine").unwrap();
        assert_eq!(Manifest::read(&n.join("mine")).unwrap().run, ["python3", "main.py"]);
        for d in [root, src, n] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
