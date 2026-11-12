//! termcode: a VS Code-like editor and git panel for a folder, file or repo,
//! in the caller's terminal (like vim) or as a tab in the caller's herdr workspace.
//!
//!   termcode [PATH[:LINE[:COL]]]         the TUI in this terminal; quit returns to the shell
//!   termcode --tab [PATH[:LINE[:COL]]]   launcher: a Code tab (also the plugin action)
//!   termcode panel                       the TUI, the plugin's `[[panes]]` entrypoint

mod blocks;
mod colors;
mod decor;
mod diff;
mod editor;
mod git;
mod minimap;
mod icons;
// Git backend (history, refs, stash, commit, remotes, hunks, blame, conflicts) and graph lanes.
mod gitops;
mod graph;
mod nav;
mod navui;
mod hover;
mod lint;
mod trust;
mod panel;
mod parse;
mod semantic;
mod session;
mod settings;
mod settingsui;
mod settingsgraphics;
mod symbols;
mod mdpreview;
mod theme;
mod ui;
// Source Control UI: the CHANGES / GRAPH view, Timeline, hover card, commit dialog.
mod commitdlg;
mod gitcmds;
mod scm;

use serde_json::Value;
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Where users report problems and ask for features (public; the source is not).
pub const ISSUES: &str = "https://github.com/JonasAtelier/termcode-releases";
const PLUGIN_ID: &str = "jonas.termcode";
/// herdr labels a plugin pane with its `[[panes]]` title.
const PANE_LABEL: &str = "Code";
const USAGE: &str = "usage: termcode [--tab] [PATH[:LINE[:COL]]]

Open PATH like `code PATH`, in this terminal; quitting returns to the shell.
  FOLDER        the workspace root (Explorer, Search, Quick Open); git from its repo
  FILE[:L[:C]]  open it at line L, column C; the root is its git repo (else its folder)
  (no PATH)     this folder's git repo, or the folder
  --tab         in a 'Code: <name>' tab of this herdr workspace instead (reused if open);
                also used when there is no terminal
  --default-config  print the default settings.json (every option, commented out)
  -V, --version     print the version
Relative paths resolve against this folder. Settings: ~/.config/termcode/settings.json.
(`termcode panel` is the plugin's pane entrypoint.)";

#[derive(Debug, PartialEq)]
enum Cmd {
    Help,
    Version,
    DefaultConfig,
    Panel,
    Open { tab: bool, path: Option<String> },
}

fn parse_args(args: &[String]) -> Result<Cmd, String> {
    if let [a] = args {
        if a == "panel" {
            return Ok(Cmd::Panel);
        }
    }
    let (mut tab, mut path) = (false, None);
    for a in args {
        match a.as_str() {
            "-h" | "--help" => return Ok(Cmd::Help),
            "-V" | "--version" => return Ok(Cmd::Version),
            "--default-config" => return Ok(Cmd::DefaultConfig),
            "--tab" => tab = true,
            s if s.starts_with('-') => return Err(format!("unknown option {s:?} (a path starting with '-' needs ./)\n{USAGE}")),
            s if path.is_none() => path = Some(s.to_string()),
            s => return Err(format!("unexpected argument {s:?}\n{USAGE}")),
        }
    }
    Ok(Cmd::Open { tab, path })
}

/// (tab mode, note): in place needs a terminal on stdin and stdout; without one
/// (a plugin action, a pipe) a tab opens instead.
fn mode(tab: bool, tty: bool) -> (bool, Option<&'static str>) {
    if !tab && !tty {
        return (true, Some("no terminal here: opening a Code tab instead"));
    }
    (tab, None)
}

fn main() -> ExitCode {
    migrate(&|k| env::var(k).ok());
    let args: Vec<String> = env::args().skip(1).collect();
    let result = parse_args(&args).and_then(|cmd| match cmd {
        Cmd::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Cmd::Version => {
            println!("termcode {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Cmd::DefaultConfig => {
            print!("{}", settingsui::template());
            Ok(())
        }
        // The launcher opens the pane with the workspace root as its cwd, and hands over
        // the launch folder (the session's key) when it is not the root.
        Cmd::Panel => env::current_dir().and_then(fs::canonicalize).map_err(|e| e.to_string()).and_then(|root| {
            let key = env::var_os("TERMCODE_KEY").map(PathBuf::from).filter(|k| k.is_absolute()).unwrap_or_else(|| root.clone());
            panel::run(root, key, env::var("TERMCODE_OPEN").ok(), true)
        }),
        Cmd::Open { tab, path } => match mode(tab, std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            (true, note) => {
                if let Some(n) = note {
                    eprintln!("termcode: {n}");
                }
                launch(path.as_deref())
            }
            (false, _) => {
                let cwd = env::current_dir().map_err(|e| e.to_string())?;
                let (root, file) = target(path.as_deref(), &cwd)?;
                let key = session_key(path.as_deref(), &cwd, &root);
                panel::run(root, key, file.map(|(p, l, c)| format!("open:{}:{l}:{c}", p.display())), false)
            }
        },
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("termcode: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Worktree root of `cwd`, canonicalized. Each linked worktree is its own root.
pub fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8(out.stdout).ok()?;
    fs::canonicalize(root.trim_end_matches('\n')).ok()
}

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

/// Run `herdr args...` against the caller's server (HERDR_SOCKET_PATH); stdout.
fn herdr_raw(args: &[&str]) -> Result<String, String> {
    let out = Command::new(herdr_bin())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run herdr: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("herdr {}: {} {}", args.join(" "), err.trim(), stdout.trim()));
    }
    Ok(stdout)
}

fn herdr(args: &[&str]) -> Result<Value, String> {
    let out = herdr_raw(args)?;
    serde_json::from_str(&out).map_err(|e| format!("herdr {}: bad JSON: {e}", args.join(" ")))
}

/// Lock file for one find-or-create, keyed by session socket, workspace and root.
fn lock_path(scope: &str, root: &Path) -> PathBuf {
    let key = parse::lock_key(scope, &root.to_string_lossy());
    data_dir(true).join(format!("launch-{key}.lock"))
}

/// Our folder, config or state: `$XDG_CONFIG_HOME/termcode` / `$XDG_STATE_HOME/termcode`
/// (else ~/.config/termcode, ~/.local/state/termcode). The same in place, as a herdr tab
/// and outside herdr, so settings, layout and backups are one set in every mode.
pub fn data_dir(state: bool) -> PathBuf {
    xdg_base(state, &|k| env::var(k).ok()).join("termcode")
}

/// The XDG base for config or state. Only absolute bases: an empty or relative XDG_* is
/// unset (the XDG spec), so nothing lands under whatever folder the panel runs in.
fn xdg_base(state: bool, var: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let (xdg, home_rel) = if state { ("XDG_STATE_HOME", ".local/state") } else { ("XDG_CONFIG_HOME", ".config") };
    let abs = |k: &str| var(k).filter(|d| Path::new(d).is_absolute());
    match (abs(xdg), abs("HOME")) {
        (Some(x), _) => PathBuf::from(x),
        (None, Some(h)) => PathBuf::from(h).join(home_rel),
        (None, None) => env::temp_dir(),
    }
}

/// Once: copy the folders termcode used as the herdr plugin `jonas.herdr-code` (settings,
/// sessions, backups, drafts) to its own, settings keys renamed (`herdr-code.icons` ->
/// `termcode.icons`). Copied, not moved, so a herdr-code still running keeps its files; of
/// two panels starting together, the copy renamed into place first wins.
fn migrate(var: &dyn Fn(&str) -> Option<String>) {
    for state in [false, true] {
        let (base, old) = (xdg_base(state, var), old_dir(state, var));
        let new = base.join("termcode");
        if new.exists() || !old.is_dir() {
            continue;
        }
        let tmp = base.join(format!(".termcode-migrate-{}", std::process::id()));
        if copy_tree(&old, &tmp).and_then(|_| fs::rename(&tmp, &new)).is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
    }
}

/// Where termcode's files were as the herdr plugin `jonas.herdr-code`.
pub fn old_data_dir(state: bool) -> PathBuf {
    old_dir(state, &|k| env::var(k).ok())
}

fn old_dir(state: bool, var: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    xdg_base(state, var).join("herdr").join(if state { "plugins" } else { "plugins/config" }).join("jonas.herdr-code")
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (src, dst, ty) = (e.path(), to.join(e.file_name()), e.file_type()?);
        if ty.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ty.is_file() && e.file_name() == "settings.json" {
            fs::write(&dst, fs::read_to_string(&src)?.replace("\"herdr-code.", "\"termcode."))?;
        } else if ty.is_file() {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The request token of the tab panel in `pane`, keyed by session socket and pane id.
pub fn token_path(socket: &str, pane: &str) -> PathBuf {
    data_dir(true).join(format!("req-{}.token", parse::lock_key(socket, pane)))
}

/// `PATH[:LINE[:COL]]` against `cwd`, canonicalized; line and column from 1.
/// Like `code -g`, but a path that exists as typed wins, so a name with `:` opens.
fn parse_target(arg: &str, cwd: &Path) -> Result<FilePos, String> {
    let canon = |p: &str| fs::canonicalize(cwd.join(p)).ok();
    let num = |s: &str| s.bytes().all(|b| b.is_ascii_digit()).then(|| s.parse::<usize>().ok()).flatten().map(|n| n.max(1));
    if let Some(p) = canon(arg) {
        return Ok((p, 1, 1));
    }
    if let Some((rest, last)) = arg.rsplit_once(':') {
        if let (Some((p, l)), Some(c)) = (rest.rsplit_once(':'), num(last)) {
            if let (Some(l), Some(p)) = (num(l), canon(p)) {
                return Ok((p, l, c));
            }
        }
        if let (Some(l), Some(p)) = (num(last), canon(rest)) {
            return Ok((p, l, 1));
        }
    }
    Err(format!("{arg}: no such file or directory"))
}

/// A file to open: canonical path, line, column (from 1).
type FilePos = (PathBuf, usize, usize);

/// The workspace root and the file to open, VS Code style: a
/// folder is its own root; a file opens in its git worktree, else in its folder.
/// No argument: the caller's worktree, else its folder.
fn target(arg: Option<&str>, cwd: &Path) -> Result<(PathBuf, Option<FilePos>), String> {
    let Some(arg) = arg else {
        let root = repo_root(cwd).or_else(|| fs::canonicalize(cwd).ok()).ok_or_else(|| format!("{}: no such directory", cwd.display()))?;
        return Ok((root, None));
    };
    let (p, line, col) = parse_target(arg, cwd)?;
    if p.is_dir() {
        return Ok((p, None));
    }
    if !p.is_file() {
        return Err(format!("{}: not a regular file", p.display()));
    }
    let dir = p.parent().unwrap_or(&p).to_path_buf();
    Ok((repo_root(&dir).unwrap_or(dir), Some((p, line, col))))
}

/// The session's key: the folder termcode was started from, so a no-argument launch
/// from arch/sh has its own session though its root is the repo; with a PATH, the root.
fn session_key(arg: Option<&str>, cwd: &Path, root: &Path) -> PathBuf {
    match arg {
        None => fs::canonicalize(cwd).unwrap_or_else(|_| root.to_path_buf()),
        Some(_) => root.to_path_buf(),
    }
}

/// Open (creating) `path` and take a non-blocking exclusive flock. `Some(file)`
/// while held; the kernel releases it when the file is dropped or the process
/// dies, so a crash never leaves a stale lock. std opens files O_CLOEXEC.
pub fn try_lock(path: &Path) -> Result<Option<fs::File>, String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // SAFETY: flock(2) on a descriptor we own.
    let held = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(held.then_some(f))
}

/// `try_lock`, retried for up to `wait`.
pub fn lock_wait(path: &Path, wait: Duration) -> Result<fs::File, String> {
    let start = Instant::now();
    loop {
        if let Some(f) = try_lock(path)? {
            return Ok(f);
        }
        if start.elapsed() >= wait {
            return Err(format!("{} stayed locked", path.display()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Our pane for `root` in `herdr pane list` output: labelled "Code", with the
/// worktree root as its cwd. Returns (tab_id, pane_id).
fn find_code_pane(list: &Value, root: &Path) -> Option<(String, String)> {
    let same = |c: &Value| c.as_str().is_some_and(|c| Path::new(c) == root || fs::canonicalize(c).is_ok_and(|p| p == root));
    list["result"]["panes"]
        .as_array()?
        .iter()
        .find(|p| p["label"].as_str() == Some(PANE_LABEL) && (same(&p["cwd"]) || same(&p["foreground_cwd"])))
        .and_then(|p| Some((p["tab_id"].as_str()?.to_string(), p["pane_id"].as_str()?.to_string())))
}

/// `herdr plugin pane open` for a new Code tab. The CLI focuses by default
/// (focus = true unless `--no-focus`), which would make the new tab the
/// workspace's active tab; `--no-focus` leaves every client and the active tab alone.
/// `envs` (`TERMCODE_OPEN=<request>`, `TERMCODE_KEY=<launch folder>`) go to the new panel.
fn open_args<'a>(ws: &'a str, root: &'a str, envs: &[&'a str]) -> Vec<&'a str> {
    let mut a = vec!["plugin", "pane", "open", "--plugin", PLUGIN_ID, "--entrypoint", "panel", "--placement", "tab", "--workspace", ws];
    for e in envs {
        a.extend(["--env", e]);
    }
    a.extend(["--cwd", root, "--no-focus"]);
    a
}

/// Find or open the Code tab for the caller's worktree in the caller's workspace,
/// without focusing anything: herdr 0.9.1 can only focus a tab for every client
/// attached to the session (`tab focus`), and other clients must not move. Even the
/// workspace's active tab stays as it was; the user clicks the new tab.
fn launch(arg: Option<&str>) -> Result<(), String> {
    let socket = env::var("HERDR_SOCKET_PATH")
        .map_err(|_| "a Code tab needs herdr: run termcode --tab from a herdr pane (HERDR_SOCKET_PATH is not set)")?;
    let ws = env::var("HERDR_WORKSPACE_ID").map_err(|_| "HERDR_WORKSPACE_ID is not set")?;
    // A plugin action runs in the plugin dir; follow the invoking pane instead.
    let cwd = if env::var_os("HERDR_PLUGIN_ACTION_ID").is_some() {
        let pane = env::var("HERDR_PANE_ID").map_err(|_| "plugin action has no HERDR_PANE_ID")?;
        let v = herdr(&["pane", "get", &pane])?;
        let p = &v["result"]["pane"];
        PathBuf::from(p["foreground_cwd"].as_str().or(p["cwd"].as_str()).ok_or("pane get: no cwd")?)
    } else {
        env::current_dir().map_err(|e| e.to_string())?
    };
    let (root, file) = target(arg, &cwd)?;
    let key = session_key(arg, &cwd, &root);
    let root_s = root.to_string_lossy().into_owned();
    // The panel's request body; a control character could end the bracketed paste early.
    let req = file.as_ref().map(|(p, l, c)| format!("open:{}:{l}:{c}", p.display()));
    if req.as_deref().is_some_and(|r| r.chars().any(char::is_control)) {
        return Err("the path contains control characters".into());
    }
    let name = root.file_name().map_or(root_s.clone(), |n| n.to_string_lossy().into_owned());
    let label = format!("Code: {name}");
    // Find-or-create under a lock: two launches at once still make one tab.
    let existing = {
        let _held = lock_wait(&lock_path(&format!("{socket}\0{ws}"), &root), Duration::from_secs(5))?;
        let panes = herdr(&["pane", "list", "--workspace", &ws])?;
        let found = find_code_pane(&panes, &root);
        if found.is_none() {
            // A new panel opens the file itself, from its environment, and keys its
            // session by the launch folder.
            let open = req.as_ref().map(|r| format!("TERMCODE_OPEN={r}"));
            let keyed = (key != root).then(|| format!("TERMCODE_KEY={}", key.display()));
            let envs: Vec<&str> = [open.as_deref(), keyed.as_deref()].into_iter().flatten().collect();
            let v = herdr(&open_args(&ws, &root_s, &envs))?;
            let tab = v["result"]["plugin_pane"]["pane"]["tab_id"].as_str().ok_or("plugin pane open: no tab_id")?;
            // Cosmetic: the tab still works under herdr's default label.
            let _ = herdr_raw(&["tab", "rename", tab, &label]);
        }
        found
    };
    if let (Some((_, pane)), Some(req)) = (existing, &req) {
        send_request(&socket, &pane, req)?;
    }
    println!("Code tab ready: click '{label}' in the tab bar");
    if let Some((p, line, _)) = &file {
        println!("opened {}:{line}", p.display());
    }
    Ok(())
}

/// Hand a request to the running panel in `pane` as one bracketed paste
/// carrying its token: it wakes the panel's event loop at once, no signal.
/// A panel still starting gets up to 5 s to write its token.
fn send_request(socket: &str, pane: &str, req: &str) -> Result<(), String> {
    let path = token_path(socket, pane);
    let start = Instant::now();
    let token = loop {
        match fs::read_to_string(&path) {
            Ok(t) if !t.trim().is_empty() => break t,
            _ if start.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(50)),
            _ => return Err("the Code tab does not take requests (opened by an older termcode?); close it and run again".into()),
        }
    };
    herdr_raw(&["pane", "send-text", pane, &request_paste(token.trim(), req)]).map(drop)
}

fn request_paste(token: &str, req: &str) -> String {
    format!("\x1b[200~termcode:{token}:{req}\x1b[201~")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_tab_without_focus() {
        for envs in [vec![], vec!["TERMCODE_OPEN=open:/r/a.c:3:1", "TERMCODE_KEY=/r/sub"]] {
            let a = open_args("w1", "/r", &envs);
            assert!(a.contains(&"--no-focus") && !a.contains(&"--focus"), "{a:?}");
            assert_eq!(&a[a.len() - 3..], ["--cwd", "/r", "--no-focus"]);
            let given: Vec<&str> = a.windows(2).filter(|w| w[0] == "--env").map(|w| w[1]).collect();
            assert_eq!(given, envs);
        }
    }

    /// A scratch tree: repo/ (git, with sub/f.rs and a file named `odd:2`) and plain/ (no git).
    fn scratch(tag: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("termcode-main-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("repo/sub")).unwrap();
        fs::create_dir_all(d.join("plain")).unwrap();
        assert!(Command::new("git").args(["init", "-q"]).arg(d.join("repo")).status().unwrap().success());
        for f in ["repo/sub/f.rs", "repo/odd:2", "plain/p.txt"] {
            fs::write(d.join(f), "x\n").unwrap();
        }
        d.canonicalize().unwrap()
    }

    #[test]
    fn parses_path_line_col_like_code_g() {
        let d = scratch("args");
        let (repo, f) = (d.join("repo"), d.join("repo/sub/f.rs"));
        assert_eq!(parse_target("sub", &repo), Ok((repo.join("sub"), 1, 1)), "a relative dir");
        assert_eq!(parse_target(f.to_str().unwrap(), &d), Ok((f.clone(), 1, 1)), "an absolute file");
        assert_eq!(parse_target("sub/f.rs:12", &repo), Ok((f.clone(), 12, 1)));
        assert_eq!(parse_target("sub/f.rs:12:7", &repo), Ok((f.clone(), 12, 7)));
        assert_eq!(parse_target("sub/./f.rs:0:0", &repo), Ok((f.clone(), 1, 1)), "clamped to 1");
        assert_eq!(parse_target("odd:2", &repo), Ok((repo.join("odd:2"), 1, 1)), "an existing name with ':' wins");
        assert_eq!(parse_target("odd:2:5", &repo), Ok((repo.join("odd:2"), 5, 1)));
        assert!(parse_target("nope", &repo).unwrap_err().contains("no such file"));
        assert!(parse_target("sub/f.rs:x", &repo).is_err());
        assert!(parse_target("sub/f.rs:+3", &repo).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn picks_the_workspace_root_like_vscode() {
        let d = scratch("root");
        let (repo, plain) = (d.join("repo"), d.join("plain"));
        assert_eq!(target(Some("sub"), &repo), Ok((repo.join("sub"), None)), "a subdir is its own root");
        assert_eq!(target(Some("."), &plain), Ok((plain.clone(), None)), "a non-git dir is allowed");
        assert_eq!(target(Some("sub/f.rs:3"), &repo), Ok((repo.clone(), Some((repo.join("sub/f.rs"), 3, 1)))), "a file: its repo");
        assert_eq!(target(Some("p.txt:2:4"), &plain), Ok((plain.clone(), Some((plain.join("p.txt"), 2, 4)))), "else its folder");
        assert_eq!(target(None, &repo.join("sub")), Ok((repo.clone(), None)), "no argument: the caller's repo");
        assert_eq!(target(None, &plain), Ok((plain.clone(), None)));
        assert!(target(Some("/dev/null"), &d).unwrap_err().contains("not a regular file"));
        fs::remove_dir_all(&d).unwrap();
    }

    /// Sessions are per launch folder: two no-argument launches under one repo, from the
    /// repo and from a subfolder of it, share the root yet keep two session files.
    #[test]
    fn each_launch_folder_keeps_its_own_session() {
        let d = scratch("key");
        let (repo, sub) = (d.join("repo"), d.join("repo/sub"));
        let (root_a, root_b) = (target(None, &repo).unwrap().0, target(None, &sub).unwrap().0);
        assert_eq!((&root_a, &root_b), (&repo, &repo), "one root: the repo");
        let (ka, kb) = (session_key(None, &repo, &root_a), session_key(None, &sub, &root_b));
        let state = d.join("state");
        assert_ne!(session::path(&state, &ka), session::path(&state, &kb), "two launch folders, two session files");
        assert_eq!((ka, kb), (repo.clone(), sub.clone()));
        // With a PATH the key is the root: a folder (its own root) or a file's repo.
        assert_eq!(session_key(Some("sub"), &repo, &sub), sub);
        assert_eq!(session_key(Some("sub/f.rs:3"), &repo, &repo), repo);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn in_place_without_a_tty_opens_a_tab() {
        assert!(mode(false, false).0, "no terminal on stdin/stdout: a Code tab instead of raw mode");
        assert!(!mode(false, true).0);
        assert!(parse_args(&["--icon-demo".into()]).is_err(), "the demo is gone");
    }

    #[test]
    fn argv_runs_in_place_unless_tab() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let open = |tab, p: Option<&str>| Ok(Cmd::Open { tab, path: p.map(String::from) });
        assert_eq!(a(&[]), open(false, None));
        assert_eq!(a(&["src/main.rs:12"]), open(false, Some("src/main.rs:12")));
        assert_eq!(a(&["--tab"]), open(true, None));
        assert_eq!(a(&["--tab", "drivers"]), open(true, Some("drivers")));
        assert_eq!(a(&["drivers", "--tab"]), open(true, Some("drivers")));
        assert_eq!(a(&["panel"]), Ok(Cmd::Panel));
        assert_eq!(a(&["--tab", "panel"]), open(true, Some("panel")), "a folder named panel");
        assert_eq!(a(&["-h"]), Ok(Cmd::Help));
        assert_eq!((a(&["--version"]), a(&["-V"]), a(&["x", "--version"])), (Ok(Cmd::Version), Ok(Cmd::Version), Ok(Cmd::Version)));
        assert_eq!(a(&["x", "--help"]), Ok(Cmd::Help));
        assert!(a(&["--bogus"]).unwrap_err().contains("unknown option"));
        assert!(a(&["a", "b"]).unwrap_err().contains("unexpected argument \"b\""));
    }

    #[test]
    fn without_a_terminal_a_tab_opens_instead() {
        assert_eq!(mode(false, true), (false, None), "in place");
        assert_eq!(mode(true, true), (true, None));
        assert_eq!(mode(true, false), (true, None));
        let (tab, note) = mode(false, false);
        assert!(tab && note.is_some_and(|n| n.contains("no terminal")));
    }

    fn vars(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: std::collections::HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| m.get(k).cloned()
    }

    #[test]
    fn data_dirs_follow_xdg_in_every_mode() {
        let dirs = |v: &[(&str, &str)]| {
            let get = vars(v);
            (xdg_base(false, &get).join("termcode"), xdg_base(true, &get).join("termcode"))
        };
        let p = |s: &str| PathBuf::from(s);
        let home = [("HOME", "/h")];
        assert_eq!(dirs(&home), (p("/h/.config/termcode"), p("/h/.local/state/termcode")));
        let xdg = [("HOME", "/h"), ("XDG_CONFIG_HOME", "/xc"), ("XDG_STATE_HOME", "/xs")];
        assert_eq!(dirs(&xdg), (p("/xc/termcode"), p("/xs/termcode")));
        let pane = [("HOME", "/h"), ("HERDR_PLUGIN_CONFIG_DIR", "/pc"), ("HERDR_PLUGIN_STATE_DIR", "/ps")];
        assert_eq!(dirs(&pane), dirs(&home), "a herdr tab uses the same folders");
        let blank_xdg = [("HOME", "/h"), ("XDG_CONFIG_HOME", ""), ("XDG_STATE_HOME", "")];
        assert_eq!(dirs(&blank_xdg), dirs(&home), "an empty XDG_* is unset");
        let relative = [("HOME", "/h"), ("XDG_CONFIG_HOME", "cfg"), ("XDG_STATE_HOME", "./st")];
        assert_eq!(dirs(&relative), dirs(&home), "a relative base is ignored");
        assert_eq!(dirs(&[("HOME", "h")]), dirs(&[]), "a relative HOME too");
        assert_eq!(dirs(&[]).1, env::temp_dir().join("termcode"));
    }

    #[test]
    fn the_herdr_code_folders_are_copied_once_with_settings_keys_renamed() {
        let home = env::temp_dir().join(format!("termcode-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let old_cfg = home.join(".config/herdr/plugins/config/jonas.herdr-code");
        let old_state = home.join(".local/state/herdr/plugins/jonas.herdr-code");
        fs::create_dir_all(&old_cfg).unwrap();
        fs::create_dir_all(old_state.join("sessions")).unwrap();
        fs::write(old_cfg.join("settings.json"), "{ // mine\n  \"herdr-code.icons\": \"ascii\", \"editor.tabSize\": 8 }").unwrap();
        fs::write(old_state.join("sessions/a.json"), "{}").unwrap();
        let get = vars(&[("HOME", home.to_str().unwrap())]);
        migrate(&get);
        let new_cfg = fs::read_to_string(home.join(".config/termcode/settings.json")).unwrap();
        assert_eq!(new_cfg, "{ // mine\n  \"termcode.icons\": \"ascii\", \"editor.tabSize\": 8 }");
        assert!(home.join(".local/state/termcode/sessions/a.json").is_file());
        assert!(old_cfg.join("settings.json").is_file(), "the old folders stay");
        // Once: later changes to the old folder are not copied again.
        fs::write(old_cfg.join("settings.json"), "{}").unwrap();
        migrate(&get);
        assert_eq!(fs::read_to_string(home.join(".config/termcode/settings.json")).unwrap(), new_cfg);
        assert!(fs::read_dir(home.join(".config")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with(".termcode-migrate")));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_request_is_one_bracketed_paste() {
        assert_eq!(request_paste("t0k", "open:/r/a.c:3:1"), "\x1b[200~termcode:t0k:open:/r/a.c:3:1\x1b[201~");
        assert_ne!(token_path("/s1", "w1:p2"), token_path("/s2", "w1:p2"), "another session's panel never collides");
    }

    #[test]
    fn finds_only_our_pane_for_this_root() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let r = root.to_string_lossy();
        let list = serde_json::json!({ "result": { "panes": [
            { "pane_id": "p1", "tab_id": "t1", "label": "Code", "cwd": "/elsewhere" },
            { "pane_id": "p2", "tab_id": "t2", "cwd": r },
            { "pane_id": "p3", "tab_id": "t3", "label": "Code", "cwd": "/x", "foreground_cwd": r },
        ]}});
        assert_eq!(find_code_pane(&list, &root), Some(("t3".into(), "p3".into())));
        assert_eq!(find_code_pane(&serde_json::json!({}), &root), None);
    }

    /// The panel and launcher never signal another process: the identifiers
    /// kill, killpg, raise, sigqueue and pthread_kill appear nowhere in code
    /// (comments aside), whether as `libc::…`, a `use libc::{…}` import, a
    /// `Child` method or a command name. Incoming handlers (libc::signal) are fine.
    #[test]
    fn no_code_sends_signals() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = [["ki", "ll"].concat(), ["ki", "llpg"].concat(), ["rai", "se"].concat(), ["sig", "queue"].concat(), ["pthread_k", "ill"].concat()];
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        for e in fs::read_dir(&dir).unwrap().flatten() {
            let text = fs::read_to_string(e.path()).unwrap();
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("").as_bytes();
                for f in &forbidden {
                    let hit = (0..code.len().saturating_sub(f.len() - 1)).any(|i| {
                        code[i..].starts_with(f.as_bytes())
                            && (i == 0 || !word(code[i - 1]))
                            && code.get(i + f.len()).is_none_or(|b| !word(*b))
                    });
                    assert!(!hit, "{}:{}: {f:?} in {line:?}", e.path().display(), n + 1);
                }
            }
        }
    }
}
