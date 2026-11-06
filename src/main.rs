//! herdr-code: a VS Code-like editor and git panel for the caller's repo or
//! worktree, as a tab in the caller's herdr workspace.
//!
//!   herdr-code         launcher: a shell in a herdr pane, or the plugin action
//!   herdr-code panel   the TUI, the plugin's `[[panes]]` entrypoint

mod decor;
mod diff;
mod editor;
mod git;
mod icons;
mod panel;
mod parse;
mod semantic;
mod settings;
mod theme;
mod ui;

use serde_json::Value;
use std::env;
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const PLUGIN_ID: &str = "jonas.herdr-code";
/// herdr labels a plugin pane with its `[[panes]]` title.
const PANE_LABEL: &str = "Code";
const USAGE: &str = "usage: herdr-code | herdr-code panel";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => launch(),
        Some("panel") => panel::run(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown argument {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("herdr-code: {e}");
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
    let base = env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    let key = parse::lock_key(scope, &root.to_string_lossy());
    base.join("herdr-code").join(format!("launch-{key}.lock"))
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
fn open_args<'a>(ws: &'a str, root: &'a str) -> Vec<&'a str> {
    vec![
        "plugin", "pane", "open", "--plugin", PLUGIN_ID, "--entrypoint", "panel", "--placement", "tab", "--workspace", ws,
        "--cwd", root, "--no-focus",
    ]
}

/// Find or open the Code tab for the caller's worktree in the caller's workspace,
/// without focusing anything: herdr 0.9.1 can only focus a tab for every client
/// attached to the session (`tab focus`), and other clients must not move. Even the
/// workspace's active tab stays as it was; the user clicks the new tab.
fn launch() -> Result<(), String> {
    let socket = env::var("HERDR_SOCKET_PATH")
        .map_err(|_| "run herdr-code from a shell inside a herdr pane (HERDR_SOCKET_PATH is not set)")?;
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
    let root = repo_root(&cwd).ok_or_else(|| format!("{} is not inside a git repository", cwd.display()))?;
    let root_s = root.to_string_lossy().into_owned();
    // Find-or-create under a lock: two launches at once still make one tab.
    let _held = lock_wait(&lock_path(&format!("{socket}\0{ws}"), &root), Duration::from_secs(5))?;
    let panes = herdr(&["pane", "list", "--workspace", &ws])?;
    let name = root.file_name().map_or(root_s.clone(), |n| n.to_string_lossy().into_owned());
    let label = format!("Code: {name}");
    if find_code_pane(&panes, &root).is_none() {
        let v = herdr(&open_args(&ws, &root_s))?;
        let tab = v["result"]["plugin_pane"]["pane"]["tab_id"].as_str().ok_or("plugin pane open: no tab_id")?;
        // Cosmetic: the tab still works under herdr's default label.
        let _ = herdr_raw(&["tab", "rename", tab, &label]);
    }
    println!("Code tab ready: click '{label}' in the tab bar");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_tab_without_focus() {
        let a = open_args("w1", "/r");
        assert!(a.contains(&"--no-focus") && !a.contains(&"--focus"), "{a:?}");
        assert_eq!(&a[a.len() - 3..], ["--cwd", "/r", "--no-focus"]);
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
