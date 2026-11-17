//! termcode's own socket: a private Unix socket for each running termcode
//! ($XDG_RUNTIME_DIR/termcode/<pid>.sock, its folder 0700, the socket 0600), one JSON
//! request a line, one reply. The shells in its terminals know it (TERMCODE_SOCKET), so
//! `termcode PATH` typed in one opens PATH in this termcode instead of a second one
//! inside it: a file of this folder in place, another folder by switching to it (the
//! way a reload does: the shells go on). The ground for an API.

use crate::panel::App;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;
use std::time::Duration;

/// Where a terminal's shell finds its termcode.
pub const ENV: &str = "TERMCODE_SOCKET";
/// A request this long at most (a line).
const MAX: u64 = 64 << 10;

static PATH: OnceLock<PathBuf> = OnceLock::new();
/// The read end of the pipe the socket's thread wakes the event loop with (-1: none).
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// This termcode's socket, for its shells.
pub fn path() -> Option<&'static Path> {
    PATH.get().map(PathBuf::as_path)
}

/// The event loop waits on this too: a request is answered at once, not at its next tick.
pub fn wake_fd() -> Option<i32> {
    Some(WAKE.load(Ordering::Relaxed)).filter(|&fd| fd >= 0)
}

type Request = (Value, Sender<Value>);

pub struct Server {
    rx: Receiver<Request>,
    wake: std::io::PipeReader,
    path: PathBuf,
}

/// The running termcode's server: its socket for its shells, its pipe for the event loop.
/// (Only `panel::run` arms one: tests' servers stay to themselves.)
pub fn arm(s: &Server) {
    let _ = PATH.set(s.path.clone());
    WAKE.store(s.wake.as_raw_fd(), Ordering::Relaxed);
}

/// The socket for this process in `dir` (made 0700), its requests to the event loop.
pub fn start(dir: &Path) -> Option<Server> {
    let _ = std::os::unix::fs::DirBuilderExt::mode(fs::DirBuilder::new().recursive(true), 0o700).create(dir);
    if fs::metadata(dir).ok()?.permissions().mode() & 0o077 != 0 {
        return None; // never a socket others can reach
    }
    let p = dir.join(format!("{}.sock", std::process::id()));
    let _ = fs::remove_file(&p); // this pid's before a reload, or a crash's
    // Those of termcodes gone without cleaning up (killed hard, the power cut).
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.strip_suffix(".sock").and_then(|pid| pid.parse::<u32>().ok()).is_some_and(|pid| !Path::new(&format!("/proc/{pid}")).exists()) {
            let _ = fs::remove_file(e.path());
        }
    }
    let listener = UnixListener::bind(&p).ok()?;
    // (bind made it with the umask's mode: no one else gets in, the folder being 0700.)
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).ok()?;
    let (tx, rx) = channel();
    let (wake, mut poke) = std::io::pipe().ok()?;
    // SAFETY: the pipe's read end is open; reads from it must not block the loop.
    unsafe {
        let fd = wake.as_raw_fd();
        libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
    }
    std::thread::Builder::new()
        .name("api".into())
        .spawn(move || {
            for s in listener.incoming().flatten() {
                serve(s, &tx, &mut poke);
            }
        })
        .ok()?;
    Some(Server { rx, wake, path: p })
}

/// One connection: a request line in, the event loop's reply out.
fn serve(s: UnixStream, tx: &Sender<Request>, poke: &mut std::io::PipeWriter) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let reply = match BufReader::new((&s).take(MAX)).read_line(&mut line) {
        Ok(_) => match serde_json::from_str::<Value>(&line) {
            Ok(req) => {
                let (rtx, rrx) = channel();
                let _ = tx.send((req, rtx));
                let _ = poke.write_all(b"!");
                rrx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| json!({"ok": false, "error": "termcode did not answer"}))
            }
            Err(e) => json!({"ok": false, "error": format!("not JSON: {e}")}),
        },
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    let _ = (&s).write_all(format!("{reply}\n").as_bytes());
}

/// The requests that came (from the event loop): each answered. True when one came.
pub fn tick(app: &mut App) -> bool {
    let Some(srv) = app.api.as_mut() else { return false };
    let mut buf = [0u8; 64];
    while matches!((&srv.wake).read(&mut buf), Ok(n) if n > 0) {}
    let reqs: Vec<Request> = srv.rx.try_iter().collect();
    for (req, reply) in &reqs {
        let _ = reply.send(handle(app, req));
    }
    !reqs.is_empty()
}

pub fn handle(app: &mut App, req: &Value) -> Value {
    match req["method"].as_str() {
        Some("ping") => json!({"ok": true, "root": app.root}),
        Some("open") => open(app, req),
        m => json!({"ok": false, "error": format!("unknown method {m:?}")}),
    }
}

/// `open`: {root, file?, line?, col?} as the CLI resolved them. This folder: the file
/// opened. Another: the switch to it, unless something would be lost or cut short.
fn open(app: &mut App, req: &Value) -> Value {
    let Some(root) = req["root"].as_str().map(PathBuf::from) else { return json!({"ok": false, "error": "no root"}) };
    let file = req["file"].as_str().map(PathBuf::from);
    let (line, col) = (req["line"].as_u64().unwrap_or(1), req["col"].as_u64().unwrap_or(1));
    // A file of this workspace opens here, whatever root it would have on its own (a
    // subfolder, out of git).
    if root == app.root || file.as_ref().is_some_and(|f| f.starts_with(&app.root)) {
        let Some(f) = file else { return json!({"ok": true, "note": "already open here"}) };
        app.request(&format!("open:{}:{line}:{col}", f.display()));
        return json!({"ok": true, "note": format!("opened {}", f.display())});
    }
    if app.tab {
        return json!({"ok": false, "tab": true}); // a herdr Code tab: that folder's own tab
    }
    // A git write (an index refresh just after opening a repository) takes a moment:
    // waited for, a few seconds at most, rather than refused.
    let since = std::time::Instant::now();
    while app.gitui.writes() > 0 && since.elapsed() < Duration::from_secs(3) {
        crate::scm::drain(app);
        std::thread::sleep(Duration::from_millis(20));
    }
    if let Some(why) = app.busy_for_switch() {
        return json!({"ok": false, "error": format!("{why}: termcode stays in {}", app.root.display())});
    }
    let arg = match &file {
        Some(f) => format!("{}:{line}:{col}", f.display()),
        None => root.display().to_string(),
    };
    app.reroot = Some(arg);
    (app.reload, app.quit) = (true, true);
    json!({"ok": true, "note": format!("switched to {}", root.display())})
}

/// What the CLI did with a forwarded `termcode PATH`.
#[derive(Debug, PartialEq)]
pub enum Forward {
    Done(String),
    /// The termcode there is a herdr tab: open PATH as a tab instead.
    Tab,
    Refused(String),
    /// No termcode answers there (a stale variable): start one here.
    Unreachable,
}

/// `termcode PATH` from a terminal of a termcode: PATH sent to it. Mid-switch (the
/// socket refusing, or closing unanswered, for the few milliseconds termcode takes to
/// start again in another folder) it is sent again, two seconds at most.
pub fn forward(sock: &Path, root: &Path, file: Option<&(PathBuf, usize, usize)>) -> Forward {
    let since = std::time::Instant::now();
    loop {
        match send_open(sock, root, file) {
            Some(f) => return f,
            None if sock.exists() && since.elapsed() < Duration::from_secs(2) => std::thread::sleep(Duration::from_millis(20)),
            None => return Forward::Unreachable,
        }
    }
}

/// One try: None when no termcode took it (none listening, or one gone mid-request).
fn send_open(sock: &Path, root: &Path, file: Option<&(PathBuf, usize, usize)>) -> Option<Forward> {
    let s = UnixStream::connect(sock).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
    let mut req = json!({"method": "open", "root": root});
    if let Some((f, l, c)) = file {
        (req["file"], req["line"], req["col"]) = (json!(f), json!(l), json!(c));
    }
    (&s).write_all(format!("{req}\n").as_bytes()).ok()?;
    let mut line = String::new();
    match BufReader::new(&s).read_line(&mut line) {
        Ok(0) => return None, // closed unanswered: the termcode before a switch
        Ok(_) => {}
        Err(_) => return Some(Forward::Refused("termcode did not answer".into())),
    }
    let v: Value = serde_json::from_str(&line).unwrap_or_default();
    Some(match (v["ok"].as_bool(), v["tab"].as_bool()) {
        (Some(true), _) => Forward::Done(v["note"].as_str().unwrap_or("done").to_string()),
        (_, Some(true)) => Forward::Tab,
        _ => Forward::Refused(v["error"].as_str().unwrap_or("refused").to_string()),
    })
}

/// The folder the sockets live in: the user's runtime directory, else termcode's state.
pub fn dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_absolute()).map_or_else(|| crate::data_dir(true).join("run"), |d| d.join("termcode"))
}

/// The socket goes with the process (a reload binds it again).
pub fn remove() {
    if let Some(p) = PATH.get() {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_forwarded_open_reaches_the_event_loop_and_its_answer_comes_back() {
        let dir = std::env::temp_dir().join(format!("termcode-api-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join("run/999999999.sock"), "").unwrap(); // a termcode long gone
        fs::write(dir.join("run/1.sock"), "").unwrap(); // pid 1 runs: not ours to judge
        let srv = start(&dir.join("run")).expect("a socket");
        assert!(!dir.join("run/999999999.sock").exists() && dir.join("run/1.sock").exists(), "only the dead's");
        let sock = dir.join(format!("run/{}.sock", std::process::id()));
        assert_eq!(fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(dir.join("run")).unwrap().permissions().mode() & 0o777, 0o700);
        // The event loop's side: answer what comes.
        let loop_side = std::thread::spawn(move || {
            let (req, reply) = srv.rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!((req["method"].as_str(), req["file"].as_str(), req["line"].as_u64()), (Some("open"), Some("/w/a.c"), Some(7)));
            reply.send(json!({"ok": true, "note": "opened /w/a.c"})).unwrap();
        });
        let got = forward(&sock, Path::new("/w"), Some(&(PathBuf::from("/w/a.c"), 7, 1)));
        loop_side.join().unwrap();
        assert_eq!(got, Forward::Done("opened /w/a.c".into()));
        assert_eq!(forward(&dir.join("none.sock"), Path::new("/w"), None), Forward::Unreachable, "a stale variable: start one here");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_opens_here_switches_folders_and_refuses_what_would_be_lost() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = std::env::temp_dir().join(format!("termcode-api-open-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (here, there) = (dir.join("here"), dir.join("there"));
        fs::create_dir_all(&here).unwrap();
        fs::create_dir_all(&there).unwrap();
        fs::write(here.join("a.c"), "int a;\n").unwrap();
        let mut app = App::new(fs::canonicalize(&here).unwrap(), None);
        let root = app.root.clone();
        let req = |root: &Path, file: Option<&Path>| {
            let mut v = json!({"method": "open", "root": root, "line": 1, "col": 1});
            if let Some(f) = file {
                v["file"] = json!(f);
            }
            v
        };
        // This folder: the file opens in place.
        let r = handle(&mut app, &req(&root, Some(&root.join("a.c"))));
        assert_eq!(r["ok"], true);
        assert!(app.groups[app.ag].tabs.iter().any(|t| t.key == "file:a.c"));
        assert_eq!(handle(&mut app, &req(&root, None))["note"], "already open here");
        // A file of this workspace whose own root would be a subfolder: here too.
        fs::create_dir_all(root.join("main")).unwrap();
        fs::write(root.join("main/b.c"), "int b;\n").unwrap();
        assert_eq!(handle(&mut app, &req(&root.join("main"), Some(&root.join("main/b.c"))))["ok"], true);
        assert!(app.reroot.is_none() && app.groups[app.ag].tabs.iter().any(|t| t.key == "file:main/b.c"));
        // Another folder with an unsaved file: refused, termcode stays.
        app.focus = crate::panel::Focus::Editor;
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let r = handle(&mut app, &req(&there, None));
        assert!(r["error"].as_str().unwrap().contains("1 unsaved file: save it first") && app.reroot.is_none() && !app.quit, "{r}");
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        // Saved: the switch, as a launch there.
        let r = handle(&mut app, &req(&there, None));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!((app.reroot.as_deref(), app.reload, app.quit), (Some(there.to_str().unwrap()), true, true));
        // A herdr Code tab: that folder's own tab instead.
        let mut tab = App::new(fs::canonicalize(&here).unwrap(), None);
        tab.tab = true;
        assert_eq!(handle(&mut tab, &req(&there, None))["tab"], true);
        assert_eq!(handle(&mut tab, &json!({"method": "bogus"}))["ok"], false);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_socket_folder_others_can_enter_is_refused() {
        let dir = std::env::temp_dir().join(format!("termcode-api-shared-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(start(&dir).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }
}
