//! herdr-code: open a second terminal window on the caller's herdr session that
//! shows a VS Code-like editor and git panel for the caller's repo or worktree.
//!
//!   herdr-code                                  launcher (shell in a herdr pane, or plugin action)
//!   herdr-code attach --root R --session S      runs inside the new window
//!   herdr-code panel                            the TUI, a plugin `[[panes]]` entrypoint

mod diff;
mod editor;
mod git;
mod panel;
mod parse;
mod theme;
mod ui;

use serde_json::{json, Value};
use std::env;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const PLUGIN_ID: &str = "jonas.herdr-code";
const USAGE: &str = "usage: herdr-code | herdr-code attach --root DIR --session NAME | herdr-code panel";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => launch(),
        Some("attach") => attach(&args[1..]),
        Some("panel") => panel::run(),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("herdr-code: {e}");
            if args.first().map(String::as_str) == Some("attach") {
                // The window closes when attach exits; keep the error readable.
                eprintln!("press Enter to close");
                let _ = std::io::stdin().read_line(&mut String::new());
            }
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

/// Remove the pane identity herdr injects, so a client started from here is not
/// refused as nested (src/main.rs should_block_nested) and nothing targets the caller's pane.
fn clean_env(cmd: &mut Command) {
    const STRIP: &[&str] = &[
        "HERDR_ENV",
        "HERDR_PANE_ID",
        "HERDR_TAB_ID",
        "HERDR_WORKSPACE_ID",
        "HERDR_SOCKET_PATH",
        "HERDR_CLIENT_SOCKET_PATH",
        "HERDR_SESSION",
    ];
    for (k, _) in env::vars_os() {
        if let Some(k) = k.to_str() {
            if STRIP.contains(&k) || k.starts_with("HERDR_PLUGIN_") {
                cmd.env_remove(k);
            }
        }
    }
}

/// Run `herdr [--session S] args...` and return stdout. With a session the
/// inherited herdr env is stripped so `--session` alone picks the server.
fn herdr_raw(session: Option<&str>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(herdr_bin());
    if let Some(s) = session {
        cmd.args(["--session", s]);
        clean_env(&mut cmd);
    }
    let out = cmd
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

fn herdr(session: Option<&str>, args: &[&str]) -> Result<Value, String> {
    let out = herdr_raw(session, args)?;
    serde_json::from_str(&out).map_err(|e| format!("herdr {}: bad JSON: {e}", args.join(" ")))
}

fn lock_path(session: &str, root: &Path) -> PathBuf {
    let base = env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    let key = parse::lock_key(session, &root.to_string_lossy());
    base.join("herdr-code").join(format!("{key}.json"))
}

pub fn read_lock(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Open (creating) the lock file and take a non-blocking exclusive flock.
/// `Some(file)` while held; the kernel releases it when the file is dropped or
/// the process dies, so a crash never leaves a stale lock. std opens files
/// O_CLOEXEC, so spawned children do not inherit it.
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

/// Replace the lock file's data (it stays the flock target; never deleted).
fn write_lock(f: &fs::File, v: &Value) {
    let _ = f.set_len(0);
    let _ = f.write_all_at(v.to_string().as_bytes(), 0);
}

fn launch() -> Result<(), String> {
    let socket = env::var("HERDR_SOCKET_PATH")
        .map_err(|_| "run herdr-code from a shell inside a herdr pane (HERDR_SOCKET_PATH is not set)")?;
    // A plugin action runs in the plugin dir; follow the invoking pane instead.
    let cwd = if env::var_os("HERDR_PLUGIN_ACTION_ID").is_some() {
        let pane = env::var("HERDR_PANE_ID").map_err(|_| "plugin action has no HERDR_PANE_ID")?;
        let v = herdr(None, &["pane", "get", &pane])?;
        let p = &v["result"]["pane"];
        PathBuf::from(
            p["foreground_cwd"]
                .as_str()
                .or(p["cwd"].as_str())
                .ok_or("pane get: no cwd")?,
        )
    } else {
        env::current_dir().map_err(|e| e.to_string())?
    };
    let root = repo_root(&cwd)
        .ok_or_else(|| format!("{} is not inside a git repository", cwd.display()))?;
    let sessions = herdr_raw(None, &["session", "list", "--json"])?;
    let session = parse::session_for_socket(&sessions, &socket)
        .ok_or_else(|| format!("no herdr session uses socket {socket}"))?;

    // Probe only: the attach in the new window takes and holds the lock.
    let lock = lock_path(&session, &root);
    let open = try_lock(&lock)?.is_none();
    if open {
        let ws = read_lock(&lock).and_then(|l| l["workspace_id"].as_str().map(String::from));
        println!(
            "herdr-code: already open for {} (workspace {}). Raising the existing window is not supported; switch to it manually.",
            root.display(),
            ws.as_deref().unwrap_or("?")
        );
        return Ok(());
    }

    let mut argv: Vec<String> = match env::var("HERDR_CODE_TERMINAL") {
        Ok(t) if !t.trim().is_empty() => t.split_whitespace().map(String::from).collect(),
        _ => vec!["ptyxis".into(), "--new-window".into(), "--".into()],
    };
    // Absolute path: the terminal may spawn the command with its own PATH.
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    argv.extend([
        exe.to_string_lossy().into_owned(),
        "attach".into(),
        "--root".into(),
        root.to_string_lossy().into_owned(),
        "--session".into(),
        session.clone(),
    ]);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    clean_env(&mut cmd);
    // SAFETY: setsid is async-signal-safe; detaches the terminal from this pane's session.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| format!("cannot start terminal {:?}: {e}", argv[0]))?;
    println!("herdr-code: opening a Code window for {} (session {session})", root.display());
    Ok(())
}

extern "C" fn on_signal(_: libc::c_int) {}

fn attach(args: &[String]) -> Result<(), String> {
    // First thing: survive the window closing (HUP), Ctrl+C (INT) and TERM until
    // cleanup has run. A handler, unlike SIG_IGN, is reset to default in exec'd children.
    for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        // SAFETY: installs a no-op handler; no shared state.
        unsafe {
            libc::signal(sig, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    let (mut root, mut session) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => root = it.next().cloned(),
            "--session" => session = it.next().cloned(),
            _ => return Err(format!("unknown argument {a:?}\n{USAGE}")),
        }
    }
    let (root, session) = (root.ok_or("missing --root")?, session.ok_or("missing --session")?);
    let s = Some(session.as_str());
    let name = Path::new(&root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.clone());
    let label = format!("Code: {name}");
    let lock = lock_path(&session, Path::new(&root));
    // Held until this process exits: one window per (session, worktree). Retry
    // ~500 ms so a launcher's brief probe of the same lock cannot bounce us.
    let mut lockf = None;
    for _ in 0..25 {
        lockf = try_lock(&lock)?;
        if lockf.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let Some(lockf) = lockf else {
        println!("herdr-code: already open for {root}");
        return Ok(());
    };

    // Reuse a workspace a crashed run left behind, but only if it still carries
    // our label. Matching on label alone could hijack another repo with the same dir name.
    let prev = read_lock(&lock).and_then(|l| l["workspace_id"].as_str().map(String::from));
    let list = herdr(s, &["workspace", "list"])?;
    let existing = prev.filter(|id| {
        list["result"]["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|w| w["workspace_id"].as_str() == Some(id) && w["label"].as_str() == Some(&label))
    });
    let ws = match existing {
        Some(id) => id,
        None => herdr(s, &["workspace", "create", "--no-focus", "--cwd", &root, "--label", &label])?
            ["result"]["workspace"]["workspace_id"]
            .as_str()
            .ok_or("workspace create: no workspace_id")?
            .to_string(),
    };
    // Record W at once, so a crash from here on can still find it next time.
    let mut state = json!({
        "attach_pid": std::process::id(),
        "workspace_id": ws,
        "session": session,
        "root": root,
    });
    write_lock(&lockf, &state);

    let res = run_window(s, &session, &root, &ws, &lock, &lockf, &mut state);
    // Whatever happened after W existed, take it down again. No printing on this
    // path: the window may already be gone (EIO on the tty).
    let _ = herdr(s, &["workspace", "close", &ws]);
    write_lock(&lockf, &json!({ "session": session, "root": root }));
    res
}

/// Open the panel in `ws`, run this window's herdr client until it exits.
fn run_window(
    s: Option<&str>,
    session: &str,
    root: &str,
    ws: &str,
    lock: &Path,
    lockf: &fs::File,
    state: &mut Value,
) -> Result<(), String> {
    // plugin.pane.open --focus moves the server's active workspace to `ws` without
    // moving attached clients (it is not a "public focus" method), so the client we
    // start next lands on `ws`. Nothing else runs between the two.
    let lock_env = format!("HERDR_CODE_LOCK={}", lock.display());
    let opened = herdr(
        s,
        &[
            "plugin", "pane", "open", "--plugin", PLUGIN_ID, "--entrypoint", "panel",
            "--placement", "tab", "--workspace", ws, "--cwd", root, "--env", &lock_env, "--focus",
        ],
    )?;
    let panel_tab = opened["result"]["plugin_pane"]["pane"]["tab_id"]
        .as_str()
        .ok_or("plugin pane open: no tab_id")?
        .to_string();
    let mut cmd = Command::new(herdr_bin());
    cmd.args(["--session", session]);
    clean_env(&mut cmd);
    let mut client = cmd.spawn().map_err(|e| format!("cannot start herdr client: {e}"))?;
    state["client_pid"] = json!(client.id());
    write_lock(lockf, state);

    // Exactly one fresh panel: drop the shell from `workspace create` and stale panels.
    if let Ok(tabs) = herdr(s, &["tab", "list", "--workspace", ws]) {
        for id in tabs["result"]["tabs"].as_array().into_iter().flatten().filter_map(|t| t["tab_id"].as_str()) {
            if id != panel_tab {
                let _ = herdr(s, &["tab", "close", id]);
            }
        }
    }
    let _ = client.wait();
    Ok(())
}
