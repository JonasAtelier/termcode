//! The Serial view, on xcomm (crates.io): the ports this machine has and who holds each
//! (the side bar's PORTS, xcomm's `list`), the channels open here with their state and
//! line controls (CHANNELS), and each channel's stream in an editor tab (`serial:<tty>`):
//! the device's output on a terminal screen, the keys typed there sent to it. A port is
//! taken as xcomm takes it (a UUCP lock, then an exclusive open) and, after an unplug,
//! opened again by the USB port it is plugged into when it comes back.

use crate::panel::{App, Focus};
use ratatui::prelude::*;
use std::cell::RefCell;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use xcomm::cli::LineSettings;
use xcomm::list::PortInfo;
use xcomm::port::Port;

/// A channel's editor tab: `serial:<tty>`.
pub const PREFIX: &str = "serial:";
/// Lines of a channel's output kept to scroll back through.
const SCROLLBACK: usize = 10_000;
/// How often the ports are listed while the view shows, and a lost port looked for.
const LIST_EVERY: Duration = Duration::from_secs(2);
const RETRY_EVERY: Duration = Duration::from_secs(1);

thread_local! {
    /// The open ports' descriptors: the event loop wakes when one has output.
    static FDS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// The ports' descriptors, for the event loop's poll.
pub fn fds() -> Vec<i32> {
    FDS.with(|f| f.borrow().clone())
}

/// A channel's link to its device.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Open,
    /// Gone (unplugged, power-cycled; a shared session ended): looked for every second.
    Waiting(String),
}

/// Where a channel's bytes come from and go.
enum Source {
    /// The port, opened here, and its UUCP lock (held only to be released with it).
    Own(Port, #[allow(dead_code)] xcomm::lock::UucpLock),
    /// Another xcomm session's port, through its control socket: `out` streams what the
    /// device sends (subscribe); `ctl`, when reading and writing, takes what is typed and
    /// the line's changes as that session's requests.
    Shared { out: std::os::unix::net::UnixStream, ctl: Option<std::os::unix::net::UnixStream> },
}

impl Source {
    fn fd(&self) -> i32 {
        match self {
            Source::Own(p, _) => p.fd().as_raw_fd(),
            Source::Shared { out, .. } => out.as_raw_fd(),
        }
    }
}

/// An open channel: its device (or the session sharing it), its screen.
pub struct Channel {
    /// The name in /dev it has now (`ttyUSB0`, `pts/3`).
    pub tty: String,
    /// What it is opened again by after an unplug (its /dev/serial/by-path link).
    reopen: PathBuf,
    source: Option<Source>,
    /// Shared: the xcomm session's socket, and who that is (for the status).
    shared: Option<(PathBuf, String)>,
    /// What is typed in its tab goes to the device (read and write); else only watched.
    pub write: bool,
    pub settings: LineSettings,
    pub screen: vt100::Parser,
    pub rx: u64,
    pub tx: u64,
    pub link: Link,
    retry: Instant,
    /// Output copied to a file as it comes (read-only channels too).
    pub capture: Option<(PathBuf, std::fs::File)>,
    /// The last thing a shared session refused, shown in the status.
    pub note: Option<String>,
}

/// What a click in the side bar hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Port(usize),
    Channel(usize),
    Act(usize, Act),
}

/// A channel's controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    BaudDown,
    BaudUp,
    Dtr,
    Rts,
    Break,
    Capture,
    /// Typing on or off (read and write, or read only).
    Write,
    Close,
}

#[derive(Default)]
pub struct Serial {
    pub channels: Vec<Channel>,
    pub ports: Vec<PortInfo>,
    listed: Option<Instant>,
    /// Why the ports could not be listed.
    pub problem: Option<String>,
    /// The selected row: the ports, then the channels.
    pub sel: usize,
    pub hits: Vec<(Rect, Hit)>,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// `path` taken: its UUCP lock, then the port opened at `settings`.
fn take(path: &Path, settings: &LineSettings, env: &dyn Fn(&str) -> Option<String>) -> Result<Source, String> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = xcomm::lock::lock_name(&real).ok_or_else(|| format!("{}: not a device", real.display()))?;
    let dir = xcomm::lock::default_lock_dir(env).map_err(|e| e.to_string())?;
    let (lock, _) = xcomm::lock::acquire(&dir, &name).map_err(|e| e.to_string())?;
    let (port, _) = Port::open(&real, settings, true).map_err(|e| e.to_string())?;
    FDS.with(|f| f.borrow_mut().push(port.fd().as_raw_fd()));
    Ok(Source::Own(port, lock))
}

/// The control socket of an xcomm session on `tty` (`ttyUSB0`), if one runs.
pub fn session_socket(tty: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let name = xcomm::lock::lock_name(&Path::new("/dev").join(tty))?;
    xcomm::ctl::path::socket_dirs(env).into_iter().map(|d| xcomm::ctl::path::socket_path(&d, &name)).find(|p| p.exists())
}

/// One request line to an xcomm session.
fn request(to: &mut std::os::unix::net::UnixStream, r: xcomm::ctl::proto::Request) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(&xcomm::ctl::proto::Envelope { request: r, deadline_ms: None }).map_err(std::io::Error::other)?;
    line.push(b'\n');
    to.write_all(&line)
}

/// The reply line to a request (two seconds at most).
fn reply(from: &std::os::unix::net::UnixStream) -> Result<serde_json::Value, String> {
    use std::io::Read;
    let _ = from.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while line.len() < 64 << 10 {
        match (&*from).read(&mut b) {
            Ok(1) if b[0] == b'\n' => break,
            Ok(1) => line.push(b[0]),
            Ok(_) => return Err("the session closed".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
    let v: serde_json::Value = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
    if v["ok"] == true { Ok(v) } else { Err(v["error"].as_str().unwrap_or("refused").to_string()) }
}

/// Session socket `sock` shared: its output streamed from the oldest it keeps; with
/// `write`, a second connection for what is typed.
fn attach(sock: &Path, write: bool) -> Result<Source, String> {
    use xcomm::ctl::proto::Request;
    let connect = || std::os::unix::net::UnixStream::connect(sock).map_err(|e| format!("{}: {e}", sock.display()));
    let mut out = connect()?;
    request(&mut out, Request::Subscribe { since: Some(0), from_oldest: true }).map_err(|e| e.to_string())?;
    reply(&out)?;
    out.set_nonblocking(true).map_err(|e| e.to_string())?;
    let ctl = if write {
        let mut c = connect()?;
        // A read-only socket (xcomm --ctl ro) says so now, not at the first key.
        request(&mut c, Request::Status).map_err(|e| e.to_string())?;
        reply(&c)?;
        c.set_nonblocking(true).map_err(|e| e.to_string())?;
        Some(c)
    } else {
        None
    };
    FDS.with(|f| f.borrow_mut().push(out.as_raw_fd()));
    Ok(Source::Shared { out, ctl })
}

/// `tty` as /dev names it (`pts/3` for /dev/pts/3).
fn tty_of(real: &Path) -> String {
    real.strip_prefix("/dev").map_or_else(|_| real.display().to_string(), |p| p.display().to_string())
}

impl Channel {
    fn new(tty: String, reopen: PathBuf, source: Source, shared: Option<(PathBuf, String)>, write: bool, settings: LineSettings) -> Channel {
        Channel {
            tty,
            reopen,
            source: Some(source),
            shared,
            write,
            settings,
            screen: vt100::Parser::new(24, 80, SCROLLBACK),
            rx: 0,
            tx: 0,
            link: Link::Open,
            retry: Instant::now(),
            capture: None,
            note: None,
        }
    }

    /// `path` opened here at `settings` (8N1 at 115200 by default).
    pub fn open(path: &Path, settings: LineSettings, env: &dyn Fn(&str) -> Option<String>) -> Result<Channel, String> {
        let source = take(path, &settings, env)?;
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Channel::new(tty_of(&real), xcomm::reconnect::stable_path(path), source, None, true, settings))
    }

    /// `tty`, held by the xcomm session on `sock` (`who` it is), shared with it: watched,
    /// and (`write`) typed into through it.
    pub fn share(tty: &str, sock: &Path, who: String, write: bool) -> Result<Channel, String> {
        let source = attach(sock, write)?;
        Ok(Channel::new(tty.to_string(), Path::new("/dev").join(tty), source, Some((sock.to_path_buf(), who)), write, LineSettings::default()))
    }

    fn drop_port(&mut self, why: String) {
        if let Some(s) = self.source.take() {
            let fd = s.fd();
            FDS.with(|f| f.borrow_mut().retain(|&x| x != fd));
        }
        self.link = Link::Waiting(why);
        self.retry = Instant::now() + RETRY_EVERY;
    }

    /// What the device sent since: onto the screen, into the capture. True when some came.
    pub fn pump(&mut self) -> bool {
        let mut buf = [0u8; 1 << 16];
        let mut got = self.drain_replies();
        for _ in 0..16 {
            let Some(src) = &self.source else { return got };
            // SAFETY: an open descriptor and a buffer of the length given.
            let n = unsafe { libc::read(src.fd(), buf.as_mut_ptr().cast(), buf.len()) };
            match n {
                n if n > 0 => {
                    let b = &buf[..n as usize];
                    self.screen.process(b);
                    self.rx += b.len() as u64;
                    if let Some((_, f)) = &mut self.capture {
                        let _ = f.write_all(b);
                    }
                    got = true;
                }
                0 => {
                    let why = if self.shared.is_some() { "the xcomm session ended" } else { "the device closed" };
                    self.drop_port(why.into());
                    return true;
                }
                _ => {
                    let e = std::io::Error::last_os_error();
                    match e.kind() {
                        std::io::ErrorKind::WouldBlock => return got,
                        std::io::ErrorKind::Interrupted => {}
                        _ => {
                            self.drop_port(format!("{e}: waiting for it to come back"));
                            return true;
                        }
                    }
                }
            }
        }
        got
    }

    /// A shared session's replies to what was sent: a refusal shows in the status.
    fn drain_replies(&mut self) -> bool {
        use std::io::Read;
        let Some(Source::Shared { ctl: Some(c), .. }) = &mut self.source else { return false };
        let mut buf = [0u8; 4096];
        let mut text = String::new();
        while let Ok(n) = c.read(&mut buf) {
            if n == 0 {
                break;
            }
            text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        let refused = text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).filter(|v| v["ok"] == false).next_back();
        if let Some(v) = refused {
            self.note = Some(v["error"].as_str().unwrap_or("refused").to_string());
            return true;
        }
        false
    }

    /// `b` to the device: written to the port, or sent through the session sharing it.
    /// Not on a read-only channel.
    pub fn send(&mut self, mut b: &[u8]) {
        if !self.write {
            self.note = Some("read only: Typing in the side bar turns it on".into());
            return;
        }
        match &mut self.source {
            Some(Source::Own(p, _)) => {
                let fd = p.fd().as_raw_fd();
                let until = Instant::now() + Duration::from_secs(1);
                while !b.is_empty() && Instant::now() < until {
                    // SAFETY: an open descriptor and a buffer of the length given.
                    let n = unsafe { libc::write(fd, b.as_ptr().cast(), b.len()) };
                    if n > 0 {
                        self.tx += n as u64;
                        b = &b[n as usize..];
                    } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                        return;
                    } else {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
            }
            Some(Source::Shared { ctl: Some(c), .. }) => {
                let data_b64 = xcomm::ctl::proto::base64_encode(b);
                if request(c, xcomm::ctl::proto::Request::Send { data_b64 }).is_ok() {
                    self.tx += b.len() as u64;
                }
            }
            _ => {}
        }
    }

    /// A lost port (or a shared session gone) looked for again, its time come: true
    /// when it is back.
    fn retry(&mut self, env: &dyn Fn(&str) -> Option<String>) -> bool {
        if self.source.is_some() || Instant::now() < self.retry {
            return false;
        }
        let back = match &self.shared {
            Some((sock, _)) => attach(sock, self.write).ok(),
            None => take(&self.reopen, &self.settings, env).ok(),
        };
        match back {
            Some(src) => {
                if self.shared.is_none() {
                    self.tty = tty_of(&std::fs::canonicalize(&self.reopen).unwrap_or_else(|_| self.reopen.clone()));
                }
                self.source = Some(src);
                self.link = Link::Open;
                true
            }
            None => {
                self.retry = Instant::now() + RETRY_EVERY;
                false
            }
        }
    }

    /// The modem lines (DTR, RTS) as they are, on a port opened here.
    pub fn lines(&self) -> Option<xcomm::port::ModemLines> {
        match &self.source {
            Some(Source::Own(p, _)) => p.modem_lines().ok(),
            _ => None,
        }
    }

    pub fn act(&mut self, a: Act, root: &Path) -> String {
        use xcomm::ctl::proto::{ModemLine, ModemOp, Request, SettingsPatch};
        let bauds = crate::serial::BAUDS;
        let step = |b: u32, up: bool| {
            let i = bauds.iter().position(|&x| x >= b).unwrap_or(bauds.len() - 1);
            if up { bauds[(i + 1).min(bauds.len() - 1)] } else { bauds[i.saturating_sub(1)] }
        };
        let tty = self.tty.clone();
        let line_change = matches!(a, Act::BaudDown | Act::BaudUp | Act::Dtr | Act::Rts | Act::Break);
        if line_change && !self.write {
            return format!("{tty}: read only (Typing turns writing on)");
        }
        match (a, &mut self.source) {
            (Act::Write, _) => {
                if let Some((sock, _)) = self.shared.clone() {
                    // Shared: writing needs its own connection to the session.
                    match attach(&sock, !self.write) {
                        Ok(src) => {
                            self.drop_port(String::new());
                            self.source = Some(src);
                            self.link = Link::Open;
                        }
                        Err(e) => return format!("{tty}: {e}"),
                    }
                }
                self.write = !self.write;
                self.note = None;
                format!("{tty}: {}", if self.write { "read and write: typing goes to the device" } else { "read only" })
            }
            (Act::BaudDown | Act::BaudUp, src) => {
                let mut s = self.settings;
                s.baud = step(s.baud, a == Act::BaudUp);
                match src {
                    Some(Source::Own(p, _)) => {
                        if let Err(e) = p.configure(&s) {
                            return format!("{tty}: {e}");
                        }
                    }
                    Some(Source::Shared { ctl: Some(c), .. }) => {
                        let patch = SettingsPatch { baud: Some(s.baud), ..SettingsPatch::default() };
                        if let Err(e) = request(c, Request::Set { patch }) {
                            return format!("{tty}: {e}");
                        }
                    }
                    _ => {}
                }
                self.settings = s;
                format!("{tty}: {}", xcomm::port::describe(&s))
            }
            (Act::Dtr | Act::Rts, Some(Source::Own(p, _))) => {
                let Ok(l) = p.modem_lines() else { return format!("{tty}: the modem lines can't be read") };
                let (name, r) = if a == Act::Dtr { ("DTR", p.set_dtr(!l.dtr)) } else { ("RTS", p.set_rts(!l.rts)) };
                let on = if a == Act::Dtr { !l.dtr } else { !l.rts };
                r.map_or_else(|e| format!("{tty}: {name}: {e}"), |()| format!("{tty}: {name} {}", if on { "on" } else { "off" }))
            }
            (Act::Dtr | Act::Rts, Some(Source::Shared { ctl: Some(c), .. })) => {
                let line = if a == Act::Dtr { ModemLine::Dtr } else { ModemLine::Rts };
                request(c, Request::Modem { line, op: ModemOp::Toggle, ms: 0 }).map_or_else(|e| format!("{tty}: {e}"), |()| format!("{tty}: {} toggled", line.name()))
            }
            (Act::Break, Some(Source::Own(p, _))) => p.send_break(250).map_or_else(|e| format!("{tty}: break: {e}"), |()| format!("{tty}: break sent")),
            (Act::Break, Some(Source::Shared { ctl: Some(c), .. })) => request(c, Request::Break { ms: 250 }).map_or_else(|e| format!("{tty}: {e}"), |()| format!("{tty}: break sent")),
            (Act::Capture, _) => match self.capture.take() {
                Some((path, _)) => format!("{tty}: capture stopped ({})", path.display()),
                None => {
                    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
                    let stamp = crate::panel::rename::local_iso(now).replace(':', "-");
                    let path = root.join(format!("serial-{}-{stamp}.log", tty.replace('/', "_")));
                    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                        Ok(f) => {
                            self.capture = Some((path.clone(), f));
                            format!("{tty}: capturing to {}", path.display())
                        }
                        Err(e) => format!("{}: {e}", path.display()),
                    }
                }
            },
            (Act::Close, _) => format!("{tty}: closed"),
            _ => format!("{tty}: not connected"),
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.drop_port(String::new());
    }
}

/// The ports listed (every 2 s while the view shows), lost ports looked for, output read.
/// True when something on screen changed.
pub fn tick(app: &mut App) -> bool {
    let mut changed = false;
    let shown = app.side == crate::panel::Side::Serial && app.side_visible;
    if shown && app.serial.listed.is_none_or(|t| t.elapsed() >= LIST_EVERY) {
        app.serial.listed = Some(Instant::now());
        match xcomm::list::enumerate(&env) {
            Ok(p) => {
                changed |= p.iter().map(|x| (&x.tty, x.lock_pid)).ne(app.serial.ports.iter().map(|x| (&x.tty, x.lock_pid))) || app.serial.problem.is_some();
                (app.serial.ports, app.serial.problem) = (p, None);
            }
            Err(e) => app.serial.problem = Some(e.to_string()),
        }
    }
    for ch in &mut app.serial.channels {
        changed |= ch.retry(&env);
        changed |= ch.pump();
    }
    changed
}

/// Opens `path` as a channel (or shows the one it is) in an editor tab.
pub fn open(app: &mut App, path: &Path) {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let tty = tty_of(&real);
    if !app.serial.channels.iter().any(|c| c.tty == tty) {
        match Channel::open(path, LineSettings::default(), &env) {
            Ok(c) => app.serial.channels.push(c),
            Err(e) => {
                app.msg = e;
                return;
            }
        }
    }
    app.show_tab(app.ag, format!("{PREFIX}{tty}"), false);
    app.focus = Focus::Editor;
}

/// Channel `tty` closed: its port and lock given back.
pub fn close(app: &mut App, tty: &str) {
    app.serial.channels.retain(|c| c.tty != tty);
}

fn channel_mut<'a>(app: &'a mut App, key: &str) -> Option<&'a mut Channel> {
    let tty = key.strip_prefix(PREFIX)?;
    app.serial.channels.iter_mut().find(|c| c.tty == tty)
}

/// The active editor tab's channel key, if a channel's tab is active.
fn active_key(app: &App) -> Option<String> {
    let g = app.groups.get(app.ag)?;
    g.tabs.get(g.active).map(|t| t.key.clone()).filter(|k| k.starts_with(PREFIX))
}

/// A key in a channel's tab: Shift+PgUp/PgDn scroll back, anything else goes to the
/// device as a terminal would send it (an unopened channel: Enter opens it). False when
/// the active tab is not a channel's.
pub fn page_key(app: &mut App, k: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    let Some(key) = active_key(app) else { return false };
    let Some(ch) = channel_mut(app, &key) else {
        if k.code == KeyCode::Enter {
            open(app, &Path::new("/dev").join(&key[PREFIX.len()..]));
        }
        return true;
    };
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::PageUp | KeyCode::PageDown if shift => {
            let rows = usize::from(ch.screen.screen().size().0);
            let off = ch.screen.screen().scrollback();
            ch.screen.screen_mut().set_scrollback(if k.code == KeyCode::PageUp { off + rows } else { off.saturating_sub(rows) });
        }
        _ => {
            ch.screen.screen_mut().set_scrollback(0);
            let app_cursor = ch.screen.screen().application_cursor();
            ch.send(&crate::term::key_bytes(k, app_cursor));
        }
    }
    true
}

/// A key for a channel's tab, as the terminal takes keys: everything but F1, Ctrl+Q,
/// the terminal's key and the tab keys (Ctrl+PgUp/PgDn, Ctrl+Tab) goes to the device.
/// False when the active tab is not a channel's, or the key is termcode's.
pub fn takes(app: &mut App, k: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    if active_key(app).is_none() {
        return false;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let termcodes = matches!(k.code, KeyCode::F(1))
        || (ctrl && matches!(k.code, KeyCode::Char('q' | 'Q') | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Tab | KeyCode::BackTab))
        || crate::term::toggle_key(&k);
    !termcodes && page_key(app, k)
}

/// Pasted text in a channel's tab goes to the device.
pub fn paste(app: &mut App, text: &str) -> bool {
    let Some(key) = active_key(app) else { return false };
    if let Some(ch) = channel_mut(app, &key) {
        ch.send(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
    }
    true
}

/// The wheel over a channel's tab scrolls its output back.
pub fn wheel(app: &mut App, p: Position, d: isize) -> bool {
    let Some(key) = active_key(app) else { return false };
    if !app.ui.groups.get(app.ag).is_some_and(|g| g.text.contains(p)) {
        return false;
    }
    if let Some(ch) = channel_mut(app, &key) {
        let off = ch.screen.screen().scrollback();
        ch.screen.screen_mut().set_scrollback(if d < 0 { off + 3 } else { off.saturating_sub(3) });
    }
    true
}

fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / f64::from(1 << 20)),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / f64::from(1 << 10)),
        n => format!("{n} B"),
    }
}

/// A channel's line: its settings, its state, what went each way.
/// A state's dot: green reads and writes, yellow is on hold or busy (it still reads),
/// red is disconnected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dot {
    Green,
    Yellow,
    Red,
}

impl Dot {
    pub fn color(self) -> Color {
        use crate::theme::P;
        match self {
            Dot::Green => P.git_added,
            Dot::Yellow => P.git_modified,
            Dot::Red => P.error_fg,
        }
    }
}

/// A channel's dot: disconnected, watching only (read only), or reading and writing.
pub fn channel_dot(c: &Channel) -> Dot {
    match (&c.link, c.write) {
        (Link::Waiting(_), _) => Dot::Red,
        (_, false) => Dot::Yellow,
        _ => Dot::Green,
    }
}

/// A port's dot: free to read and write, held (busy: by an xcomm session it can still be
/// watched), or held by something that can't share it.
pub fn port_dot(p: &PortInfo, shareable: bool) -> Dot {
    match p.lock_pid {
        None => Dot::Green,
        Some(pid) if pid == std::process::id() as i32 => Dot::Green,
        Some(_) if shareable => Dot::Yellow,
        Some(_) => Dot::Red,
    }
}

fn status(c: &Channel) -> String {
    let link = match (&c.link, &c.shared, c.write) {
        (Link::Waiting(why), ..) => why.clone(),
        (Link::Open, Some((_, who)), true) => format!("shared with {who}, read & write"),
        (Link::Open, Some((_, who)), false) => format!("watching {who}, read only"),
        (Link::Open, None, true) => "read & write".to_string(),
        (Link::Open, None, false) => "read only".to_string(),
    };
    let lines = c.lines().map_or(String::new(), |l| format!(" · DTR {} RTS {}", if l.dtr { "on" } else { "off" }, if l.rts { "on" } else { "off" }));
    let cap = if c.capture.is_some() { " · capturing" } else { "" };
    let note = c.note.as_ref().map_or(String::new(), |n| format!(" · {n}"));
    format!("{} · {link}{lines} · in {} out {}{cap}{note}", xcomm::port::describe(&c.settings), human(c.rx), human(c.tx))
}

/// A channel's tab: its status on the first row, its screen under it.
pub fn draw_page(app: &mut App, f: &mut Frame, r: Rect, key: &str, focused: bool) {
    use crate::theme::P;
    let tty = &key[PREFIX.len()..];
    let Some(ch) = app.serial.channels.iter_mut().find(|c| c.tty == tty) else {
        let msg = format!(" {tty} is not open here. Enter (or the port in the Serial view) opens it.");
        f.render_widget(ratatui::widgets::Paragraph::new(msg).style(Style::new().fg(P.dim).bg(P.editor_bg)), r);
        return;
    };
    if r.height < 2 {
        return;
    }
    let head = Rect::new(r.x, r.y, r.width, 1);
    let dot = Span::styled(" ● ", Style::new().fg(channel_dot(ch).color()));
    let text = Span::styled(format!("{tty} · {}", status(ch)), Style::new().fg(P.editor_fg));
    f.render_widget(ratatui::widgets::Paragraph::new(Line::from(vec![dot, text])).style(Style::new().bg(P.widget_bg)), head);
    let body = Rect::new(r.x, r.y + 1, r.width, r.height - 1);
    if ch.screen.screen().size() != (body.height.max(2), body.width.max(2)) {
        ch.screen.screen_mut().set_size(body.height.max(2), body.width.max(2));
    }
    let live = ch.screen.screen().scrollback() == 0;
    crate::term::draw_screen(ch.screen.screen(), None, f.buffer_mut(), body, (P.editor_fg, P.editor_bg, P.selection), focused && live);
}

/// The side bar: PORTS, the mapping (each tty, a dot for whether it can be used, its
/// adapter, USB ID and serial number, who holds it), then CHANNELS (each open one: its
/// dot, its state, in and out, its controls).
pub fn draw_side(app: &mut App, f: &mut Frame, r: Rect) {
    use crate::theme::P;
    // In a card as the other views, its padding inside.
    let focused_side = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused_side);
    let r = crate::ui::inset(card, (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1));
    app.ui.side_list = Rect::default();
    if r.width < 8 || r.height < 3 {
        return;
    }
    let ascii = crate::theme::ascii();
    let (tee, elbow, dot_ch) = if ascii { ("|-", "`-", "*") } else { ("├─", "└─", "●") };
    let s = &mut app.serial;
    s.hits.clear();
    let dim = Style::new().fg(P.dim);
    let fg = Style::new().fg(P.sidebar_fg);
    let line = |f: &mut Frame, y: &mut u16, text: Line| {
        if *y < r.bottom() {
            f.render_widget(text, Rect::new(r.x, *y, r.width, 1));
        }
        *y += 1;
    };
    let mut y = r.y;
    let total = s.ports.len() + s.channels.len();
    s.sel = s.sel.min(total.saturating_sub(1));
    let focused = app.focus == Focus::Side;
    let hover = app.hover;
    let sel_bg = |row: usize| if focused && row == s.sel { P.list_active_bg } else { P.sidebar_bg };
    let me = std::process::id() as i32;
    line(f, &mut y, Line::styled(format!("PORTS ({})", s.ports.len()), dim.bold()));
    if let Some(p) = &s.problem {
        line(f, &mut y, Line::styled(p.clone(), Style::new().fg(P.error_fg)));
    } else if s.ports.is_empty() {
        line(f, &mut y, Line::styled("No serial ports. Plug in an adapter.", dim));
    }
    let mut hits = Vec::new();
    for (i, p) in s.ports.iter().enumerate() {
        let top = y;
        let ours = s.channels.iter().find(|c| c.tty == p.tty);
        let shareable = p.lock_pid.is_some_and(|pid| pid != me) && session_socket(&p.tty, &env).is_some();
        let dot = ours.map_or_else(|| port_dot(p, shareable), channel_dot);
        let product = p.product.clone().unwrap_or_else(|| "serial port".into());
        line(f, &mut y, Line::from(vec![
            Span::styled(format!("{dot_ch} "), Style::new().fg(dot.color())),
            Span::styled(p.tty.clone(), fg.bold()),
            Span::styled(" ⇄ ".replace('⇄', if ascii { "<>" } else { "⇄" }), dim),
            Span::styled(product, fg),
        ]).style(Style::new().bg(sel_bg(i))));
        let usb = match (&p.vid_pid, &p.serial) {
            (Some(v), Some(sn)) => format!("USB {v} · sn {sn}"),
            (Some(v), None) => format!("USB {v}"),
            _ => "not USB".into(),
        };
        line(f, &mut y, Line::from(vec![Span::styled(format!("  {tee} "), dim), Span::styled(usb, dim)]));
        let held = match (ours, p.lock_pid, &p.herdr_pane) {
            (Some(c), ..) => format!("this termcode, {}", if c.write { "read & write" } else { "read only" }),
            (None, Some(pid), _) if pid == me => "this termcode".into(),
            (None, Some(pid), Some(pane)) => format!("xcomm in herdr {pane} (pid {pid})"),
            (None, Some(pid), None) if shareable => format!("xcomm (pid {pid})"),
            (None, Some(pid), None) => format!("{} (pid {pid})", std::fs::read_to_string(format!("/proc/{pid}/comm")).map_or_else(|_| "another program".into(), |c| c.trim().to_string())),
            (None, None, _) => "free".into(),
        };
        line(f, &mut y, Line::from(vec![Span::styled(format!("  {elbow} "), dim), Span::styled(held, Style::new().fg(if dot == Dot::Green { P.sidebar_fg } else { dot.color() }))]));
        hits.push((Rect::new(r.x, top, r.width, 3), Hit::Port(i)));
    }
    y += 1;
    line(f, &mut y, Line::styled(format!("CHANNELS ({})", s.channels.len()), dim.bold()));
    if s.channels.is_empty() {
        line(f, &mut y, Line::styled("None open: click a port (or Enter) to open it.", dim));
    }
    for (i, c) in s.channels.iter().enumerate() {
        let top = y;
        let dot = channel_dot(c);
        line(f, &mut y, Line::from(vec![Span::styled(format!("{dot_ch} "), Style::new().fg(dot.color())), Span::styled(c.tty.clone(), fg.bold())]).style(Style::new().bg(sel_bg(s.ports.len() + i))));
        line(f, &mut y, Line::styled(format!("  {}", status(c)), dim));
        hits.push((Rect::new(r.x, top, r.width, 2), Hit::Channel(i)));
        // The controls: typing on or off, the baud's ‹ ›, the modem lines, a break, capture, close.
        if y < r.bottom() {
            let mut x = r.x + 2;
            let lines = c.lines();
            let on = |b: Option<bool>| if b == Some(true) { "●" } else { "○" };
            let btns = [
                (if c.write { " Read & write ".to_string() } else { " Read only ".to_string() }, Act::Write),
                ("‹".to_string(), Act::BaudDown),
                (format!(" {} ", c.settings.baud), Act::BaudUp),
                ("›".to_string(), Act::BaudUp),
                (format!(" DTR {} ", on(lines.map(|l| l.dtr))), Act::Dtr),
                (format!(" RTS {} ", on(lines.map(|l| l.rts))), Act::Rts),
                (" Break ".to_string(), Act::Break),
                (if c.capture.is_some() { " Stop capture ".to_string() } else { " Capture ".to_string() }, Act::Capture),
                (" Close ".to_string(), Act::Close),
            ];
            for (label, a) in btns {
                let w = label.chars().count() as u16;
                if x + w > r.right() {
                    y += 1;
                    x = r.x + 2;
                    if y >= r.bottom() {
                        break;
                    }
                }
                let b = Rect::new(x, y, w, 1);
                let hot = hover.is_some_and(|p| b.contains(p));
                f.render_widget(Span::styled(label, crate::ui::button_style(false, hot)), b);
                hits.push((b, Hit::Act(i, a)));
                x += w + 1;
            }
            y += 2;
        }
    }
    s.hits = hits;
}

/// A click in the side bar: a port opens (or shows) its channel; a channel shows its
/// tab; a control acts on its channel.
pub fn click(app: &mut App, p: Position) -> bool {
    if app.side != crate::panel::Side::Serial || !app.side_visible {
        return false;
    }
    let Some(&(_, hit)) = app.serial.hits.iter().rev().find(|(r, _)| r.contains(p)) else { return false };
    activate(app, hit);
    true
}

fn activate(app: &mut App, hit: Hit) {
    match hit {
        Hit::Port(i) => {
            app.serial.sel = i;
            let Some(p) = app.serial.ports.get(i).cloned() else { return };
            let me = std::process::id() as i32;
            match p.lock_pid.filter(|&pid| pid != me) {
                None => open(app, Path::new(&p.path)),
                Some(_) if app.serial.channels.iter().any(|c| c.tty == p.tty) => open(app, Path::new(&p.path)),
                Some(pid) => app.prompt = Some(held_prompt(&p, pid)),
            }
        }
        Hit::Channel(i) => {
            app.serial.sel = app.serial.ports.len() + i;
            if let Some(tty) = app.serial.channels.get(i).map(|c| c.tty.clone()) {
                app.show_tab(app.ag, format!("{PREFIX}{tty}"), false);
            }
        }
        Hit::Act(i, a) => {
            let root = app.root.clone();
            let Some(c) = app.serial.channels.get_mut(i) else { return };
            app.msg = c.act(a, &root);
            if a == Act::Close {
                let key = format!("{PREFIX}{}", c.tty);
                // Its tabs closed (the last closes the channel), else the channel itself.
                while let Some((g, t)) = app.groups.iter().enumerate().find_map(|(g, grp)| grp.tabs.iter().position(|t| t.key == key).map(|t| (g, t))) {
                    app.close_tab(g, t, true);
                }
                if app.serial.channels.get(i).is_some_and(|c| format!("{PREFIX}{}", c.tty) == key) {
                    app.serial.channels.remove(i);
                }
            }
            app.serial.listed = None; // who holds what changed
        }
    }
}

/// The little window for a port someone holds: an xcomm session's is shared (watched, or
/// typed into through it, with a warning); another program's is explained.
fn held_prompt(p: &PortInfo, pid: i32) -> crate::panel::Prompt {
    use crate::panel::PAct;
    let who = match &p.herdr_pane {
        Some(pane) => format!("xcomm in herdr {pane} (pid {pid})"),
        None => format!("pid {pid}"),
    };
    if session_socket(&p.tty, &env).is_none() {
        let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).map_or_else(|_| "another program".into(), |c| c.trim().to_string());
        return crate::panel::Prompt {
            text: vec![format!("{} is held by {name} (pid {pid}).", p.tty), "It can't be shared: close it there first.".into()],
            buttons: vec![("OK".into(), PAct::Cancel)],
            sel: 0,
        };
    }
    crate::panel::Prompt {
        text: vec![
            format!("⚠ {} is in use by {who}.", p.tty),
            "Read only: watch and capture its output; it is not disturbed.".into(),
            "Read & write: what you type also goes to the board, through that xcomm (shown there as [ctl]).".into(),
        ],
        buttons: vec![
            ("Read only".into(), PAct::Serial(p.tty.clone(), false)),
            ("Read & write".into(), PAct::Serial(p.tty.clone(), true)),
            ("Cancel".into(), PAct::Cancel),
        ],
        sel: 0,
    }
}

/// The answer to `held_prompt`: `tty` shared with its xcomm session, typed into when `write`.
pub fn share(app: &mut App, tty: &str, write: bool) {
    let Some(sock) = session_socket(tty, &env) else {
        app.msg = format!("{tty}: its xcomm session is gone");
        return;
    };
    let who = app.serial.ports.iter().find(|p| p.tty == tty).map_or_else(
        || "xcomm".to_string(),
        |p| match (&p.herdr_pane, p.lock_pid) {
            (Some(pane), _) => format!("xcomm in herdr {pane}"),
            (None, Some(pid)) => format!("xcomm (pid {pid})"),
            _ => "xcomm".into(),
        },
    );
    match Channel::share(tty, &sock, who, write) {
        Ok(c) => {
            app.serial.channels.push(c);
            app.show_tab(app.ag, format!("{PREFIX}{tty}"), false);
            app.focus = Focus::Editor;
        }
        Err(e) => app.msg = format!("{tty}: {e}"),
    }
}

/// Keys in the side bar: ↑ ↓ choose, Enter opens a port or shows a channel.
pub fn key(app: &mut App, k: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::KeyCode;
    let (sel, ports) = (app.serial.sel, app.serial.ports.len());
    let total = ports + app.serial.channels.len();
    match k.code {
        KeyCode::Up => app.serial.sel = sel.saturating_sub(1),
        KeyCode::Down => app.serial.sel = (sel + 1).min(total.saturating_sub(1)),
        KeyCode::Enter if sel < ports => activate(app, Hit::Port(sel)),
        KeyCode::Enter if sel < total => activate(app, Hit::Channel(sel - ports)),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::FromRawFd;

    /// A pty pair: (the "device" end the test plays, the tty termcode opens).
    fn pty() -> (std::fs::File, PathBuf) {
        let (mut m, mut s) = (0, 0);
        // SAFETY: out-parameters for two descriptors; no name, termios or size given.
        assert_eq!(unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) }, 0);
        let path = std::fs::read_link(format!("/proc/self/fd/{s}")).unwrap();
        // SAFETY: just opened, owned here.
        unsafe { libc::close(s) };
        // SAFETY: just opened, owned here.
        (unsafe { std::fs::File::from_raw_fd(m) }, path)
    }

    #[test]
    fn a_channel_shows_what_the_device_sends_and_sends_what_is_typed() {
        let locks = std::env::temp_dir().join(format!("termcode-xcomm-{}", std::process::id()));
        std::fs::create_dir_all(&locks).unwrap();
        let dir = locks.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "XCOMM_LOCK_DIR").then(|| dir.clone());
        let (mut dev, tty) = pty();
        let mut ch = Channel::open(&tty, LineSettings::default(), &env).expect("the pty opens as a port");
        assert!(ch.tty.starts_with("pts/") && ch.link == Link::Open, "{}", ch.tty);
        assert!(std::fs::read_dir(&locks).unwrap().count() == 1, "its UUCP lock taken");
        assert!(Channel::open(&tty, LineSettings::default(), &env).is_err(), "taken once only");
        dev.write_all(b"U-Boot 2024.01\r\n\x1b[32mlogin:\x1b[0m ").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(ch.pump());
        assert!(ch.screen.screen().contents().contains("U-Boot 2024.01\nlogin:"), "{:?}", ch.screen.screen().contents());
        assert_eq!(ch.rx, 32);
        ch.send(b"root\r");
        let mut got = [0u8; 16];
        let n = dev.read(&mut got).unwrap();
        assert_eq!(&got[..n], b"root\r");
        assert_eq!(ch.tx, 5);
        let msg = ch.act(Act::BaudUp, &locks);
        assert!(msg.contains("230400") && ch.settings.baud == 230_400, "{msg}");
        drop(ch);
        assert_eq!(std::fs::read_dir(&locks).unwrap().count(), 0, "its lock given back");
        std::fs::remove_dir_all(&locks).unwrap();
    }

    /// A stand-in for an xcomm session's control socket: it answers each request, streams
    /// `out` to a subscriber, and hands over what `send` requests carried.
    fn fake_session(sock: &Path, out: &'static [u8]) -> std::sync::mpsc::Receiver<serde_json::Value> {
        use std::io::{BufRead, BufReader};
        let listener = std::os::unix::net::UnixListener::bind(sock).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let mut w = conn.try_clone().unwrap();
                    for line in BufReader::new(conn).lines().map_while(Result::ok) {
                        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                        w.write_all(b"{\"ok\":true,\"code\":0}\n").unwrap();
                        if v["cmd"] == "subscribe" {
                            w.write_all(out).unwrap();
                        }
                        let _ = tx.send(v);
                    }
                });
            }
        });
        rx
    }

    #[test]
    fn a_port_another_xcomm_holds_is_watched_or_typed_into_through_it() {
        let dir = std::env::temp_dir().join(format!("termcode-xcomm-share-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("ttyFAKE0.sock");
        let reqs = fake_session(&sock, b"board> ");
        // Read only: its output streams, from the oldest the session keeps; typing is not sent.
        let mut ro = Channel::share("ttyFAKE0", &sock, "xcomm (pid 1)".into(), false).unwrap();
        let first = reqs.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((first["cmd"].as_str(), first["since"].as_u64(), first["from_oldest"].as_bool()), (Some("subscribe"), Some(0), Some(true)));
        std::thread::sleep(Duration::from_millis(50));
        assert!(ro.pump() && ro.screen.screen().contents().contains("board>"));
        assert_eq!(channel_dot(&ro), Dot::Yellow, "on hold, still read");
        ro.send(b"x");
        assert!(ro.note.as_deref().is_some_and(|n| n.starts_with("read only")) && ro.tx == 0);
        assert!(ro.act(Act::Break, &dir).contains("read only"), "no line changes either");
        // Read & write: typing goes as that session's send requests (base64).
        let mut rw = Channel::share("ttyFAKE0", &sock, "xcomm (pid 1)".into(), true).unwrap();
        let cmds: Vec<String> = (0..2).map(|_| reqs.recv_timeout(Duration::from_secs(2)).unwrap()["cmd"].as_str().unwrap().to_string()).collect();
        assert!(cmds.contains(&"subscribe".to_string()) && cmds.contains(&"status".to_string()), "{cmds:?}");
        assert_eq!(channel_dot(&rw), Dot::Green);
        rw.send(b"ls\r");
        let sent = reqs.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((sent["cmd"].as_str(), sent["data_b64"].as_str()), (Some("send"), Some("bHMN")));
        assert!(rw.act(Act::BaudUp, &dir).contains("230400"));
        let set = reqs.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((set["cmd"].as_str(), set["baud"].as_u64()), (Some("set"), Some(230_400)));
        // The session gone: red, waiting for it.
        drop(reqs);
        std::fs::remove_dir_all(&dir).unwrap();
        rw.drop_port("the xcomm session ended".into());
        assert_eq!(channel_dot(&rw), Dot::Red);
    }

    #[test]
    fn a_channels_tab_shows_its_status_and_stream_and_typing_reaches_the_device() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let locks = std::env::temp_dir().join(format!("termcode-xcomm-ui-{}", std::process::id()));
        std::fs::create_dir_all(&locks).unwrap();
        let dir = locks.to_string_lossy().into_owned();
        let env = move |k: &str| (k == "XCOMM_LOCK_DIR").then(|| dir.clone());
        let (mut dev, tty) = pty();
        let root = std::env::temp_dir().join(format!("termcode-xcomm-root-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(root.clone(), None);
        let ch = Channel::open(&tty, LineSettings::default(), &env).unwrap();
        let key = format!("{PREFIX}{}", ch.tty);
        app.serial.channels.push(ch);
        app.show_tab(0, key.clone(), false);
        app.focus = Focus::Editor;
        dev.write_all(b"Linux 6.8 board ttyS0\r\nboard login: ").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(tick(&mut app));
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let text = crate::api::screen_text(t.backend().buffer());
        assert!(text.contains("115200 8N1 · read & write") && text.contains("board login:"), "{text}");
        // Typing in the tab goes to the device, as a terminal sends it.
        for c in "root".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        // (Each key its own write: read until all came.)
        let mut got = Vec::new();
        while got.len() < 5 {
            let mut b = [0u8; 16];
            let n = dev.read(&mut b).unwrap();
            got.extend_from_slice(&b[..n]);
        }
        assert_eq!(got, b"root\r");
        // Its board's line editing gets termcode's own keys (Ctrl+W, Ctrl+H, Ctrl+S).
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        let mut b = [0u8; 4];
        let n = dev.read(&mut b).unwrap();
        assert_eq!(&b[..n], [0x17], "Ctrl+W to the device, the tab still open");
        assert!(!app.serial.channels.is_empty());
        // Ctrl+T w closes it (View: Close Editor): the port and its lock given back.
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert!(app.serial.channels.is_empty() && std::fs::read_dir(&locks).unwrap().count() == 0, "{}", app.msg);
        std::fs::remove_dir_all(&locks).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
