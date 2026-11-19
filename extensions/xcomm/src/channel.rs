//! A serial channel: a port opened here (its UUCP lock held) or shared with the xcomm
//! session holding it, its screen (vt100), xcomm's control socket on it for agents.
//! Moved out of termcode as is (termcode/src/xcomm.rs), less its drawing.

use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use xcomm::cli::LineSettings;
use xcomm::port::Port;

/// Lines of a channel's output kept to scroll back through.
pub const SCROLLBACK: usize = 10_000;
const RETRY_EVERY: Duration = Duration::from_secs(1);
/// Device output kept for control-socket clients to read back (as xcomm's default).
const RING: usize = 1 << 20;
/// The speeds the baud buttons step through.
const BAUDS: [u32; 13] = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1000000, 2000000];

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
    pub shared: Option<(PathBuf, String)>,
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
    /// Output kept for control-socket clients (`xcomm ctl read`, `wait`).
    ring: xcomm::ring::Ring,
    /// xcomm's control socket on a port opened here; what it lets agents do.
    ctl: Option<xcomm::ctl::server::CtlServer>,
    pub agents: xcomm::ctl::CtlMode,
}


/// A channel's controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    BaudDown,
    BaudUp,
    /// The next data bits (8, 7, 6, 5), parity (none, even, odd), stop bits, flow control.
    DataBits,
    Parity,
    StopBits,
    Flow,
    Dtr,
    Rts,
    Break,
    Capture,
    /// Typing on or off (read and write, or read only).
    Write,
    /// What agents may do through the control socket: read & write, read only, nothing.
    Agents,
}


pub fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}


/// `path` taken: its UUCP lock, then the port opened at `settings`.
fn take(path: &Path, settings: &LineSettings, env: &dyn Fn(&str) -> Option<String>) -> Result<Source, String> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = xcomm::lock::lock_name(&real).ok_or_else(|| format!("{}: not a device", real.display()))?;
    let dir = xcomm::lock::default_lock_dir(env).map_err(|e| e.to_string())?;
    let (lock, _) = xcomm::lock::acquire(&dir, &name).map_err(|e| e.to_string())?;
    let (port, _) = Port::open(&real, settings, true).map_err(|e| e.to_string())?;
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
    Ok(Source::Shared { out, ctl })
}

fn data_bits(d: xcomm::cli::DataBits) -> u8 {
    use xcomm::cli::DataBits;
    match d {
        DataBits::Five => 5,
        DataBits::Six => 6,
        DataBits::Seven => 7,
        DataBits::Eight => 8,
    }
}

/// What a channel's control socket allows, as its button says.
pub fn agents_name(m: xcomm::ctl::CtlMode) -> &'static str {
    match m {
        xcomm::ctl::CtlMode::ReadWrite => "read & write",
        xcomm::ctl::CtlMode::ReadOnly => "read only",
        xcomm::ctl::CtlMode::Off => "off",
    }
}

/// Flow control as xcomm names it.
pub fn flow_name(f: xcomm::cli::Flow) -> &'static str {
    match f {
        xcomm::cli::Flow::None => "none",
        xcomm::cli::Flow::RtsCts => "rts",
        xcomm::cli::Flow::XonXoff => "xon",
    }
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
            ring: xcomm::ring::Ring::new(RING),
            ctl: None,
            agents: xcomm::ctl::CtlMode::ReadWrite,
        }
    }

    /// xcomm's control socket for this port opened here, in `mode` (none for Off, or for
    /// a shared channel: its session has one).
    fn serve_agents(&mut self, mode: xcomm::ctl::CtlMode, env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
        self.ctl = None;
        self.agents = mode;
        if self.shared.is_some() || mode == xcomm::ctl::CtlMode::Off {
            return Ok(());
        }
        let name = xcomm::lock::lock_name(&Path::new("/dev").join(&self.tty)).ok_or("no tty name")?;
        let dir = xcomm::ctl::path::make_socket_dir(env).map_err(|e| e.0)?;
        self.ctl = Some(xcomm::ctl::server::CtlServer::bind(&dir, &name, mode).map_err(|e| e.0)?);
        Ok(())
    }

    /// The control socket's turn: clients served, their requests on the line (a break,
    /// the modem lines, settings, capture) done. Its descriptors kept for the next poll.
    pub fn ctl_pass(&mut self, polled: &mut Vec<i32>) -> bool {
        let Some(mut srv) = self.ctl.take() else { return false };
        let mut fds = Vec::new();
        srv.poll_fds(self.ring.end(), &mut fds);
        // SAFETY: valid pollfds, as many as given; no wait.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 0) };
        let mut busy = n > 0;
        srv.serve(&fds, self);
        while let Some((id, a)) = srv.next_action() {
            let r = self.ctl_action(a);
            srv.finish_action(id, r);
            busy = true;
        }
        polled.extend(fds.iter().map(|p| p.fd).filter(|&fd| fd >= 0));
        self.ctl = Some(srv);
        busy
    }

    /// A control client's request on the line, done on the port.
    fn ctl_action(&mut self, a: xcomm::ctl::server::Action) -> Result<serde_json::Value, (u8, String)> {
        use xcomm::ctl::code;
        use xcomm::ctl::proto::{ModemLine, ModemOp};
        use xcomm::ctl::server::Action;
        let Some(Source::Own(p, _)) = &self.source else { return Err((code::DISCONNECTED, "the device is disconnected".into())) };
        let fail = |e: std::io::Error| (code::FAILED, e.to_string());
        match a {
            Action::Break { ms } => p.send_break(ms).map_err(fail)?,
            Action::Modem { line, op, ms } => {
                let set = |on: bool| if line == ModemLine::Dtr { p.set_dtr(on) } else { p.set_rts(on) };
                let now = p.modem_lines().map(|l| if line == ModemLine::Dtr { l.dtr } else { l.rts }).map_err(fail)?;
                match op {
                    ModemOp::On => set(true).map_err(fail)?,
                    ModemOp::Off => set(false).map_err(fail)?,
                    ModemOp::Toggle => set(!now).map_err(fail)?,
                    ModemOp::Pulse => {
                        set(!now).map_err(fail)?;
                        std::thread::sleep(Duration::from_millis(u64::from(ms.min(5000))));
                        set(now).map_err(fail)?;
                    }
                }
            }
            Action::Set(patch) => {
                let s = patch.apply(self.settings).map_err(|e| (code::USAGE, e))?;
                // (What the driver made of it: the settings read back show that.)
                let _applied = p.configure(&s).map_err(|e| (code::FAILED, e.to_string()))?;
                self.settings = s;
            }
            Action::CaptureStart(path) => {
                let f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(fail)?;
                self.capture = Some((path, f));
            }
            Action::CaptureStop => self.capture = None,
            Action::Xfer { .. } => return Err((code::FAILED, "file transfers are not in termcode yet: use xcomm for them".into())),
        }
        Ok(serde_json::json!({}))
    }

    /// `path` opened here at `settings` (8N1 at 115200 by default), with xcomm's control
    /// socket on it (none if that can't be made: the channel works without).
    pub fn open(path: &Path, settings: LineSettings, env: &dyn Fn(&str) -> Option<String>) -> Result<Channel, String> {
        let source = take(path, &settings, env)?;
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        // Read only until Write: a glance never types into a board by accident.
        let mut c = Channel::new(tty_of(&real), xcomm::reconnect::stable_path(path), source, None, false, settings);
        if let Err(e) = c.serve_agents(xcomm::ctl::CtlMode::ReadWrite, env) {
            c.note = Some(format!("no control socket: {e}"));
        }
        Ok(c)
    }

    /// `tty`, held by the xcomm session on `sock` (`who` it is), shared with it: watched,
    /// and (`write`) typed into through it.
    pub fn share(tty: &str, sock: &Path, who: String, write: bool) -> Result<Channel, String> {
        let source = attach(sock, write)?;
        Ok(Channel::new(tty.to_string(), Path::new("/dev").join(tty), source, Some((sock.to_path_buf(), who)), write, LineSettings::default()))
    }

    fn drop_port(&mut self, why: String) {
        if let Some(s) = self.source.take() {
            drop(s);
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
                    self.ring.push(b);
                    if let Some(srv) = &mut self.ctl {
                        srv.device_output(&self.ring);
                    }
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
        let refused = text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).rfind(|v| v["ok"] == false);
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
            self.note = Some("read only: Attach to type".into());
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
    pub fn retry(&mut self, env: &dyn Fn(&str) -> Option<String>) -> bool {
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
                    if let (Some(srv), Some(name)) = (&mut self.ctl, xcomm::lock::lock_name(&Path::new("/dev").join(&self.tty))) {
                        let _ = srv.rename(&name);
                    }
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
        let bauds = BAUDS;
        let step = |b: u32, up: bool| {
            let i = bauds.iter().position(|&x| x >= b).unwrap_or(bauds.len() - 1);
            if up { bauds[(i + 1).min(bauds.len() - 1)] } else { bauds[i.saturating_sub(1)] }
        };
        let tty = self.tty.clone();
        let line_change = matches!(a, Act::BaudDown | Act::BaudUp | Act::DataBits | Act::Parity | Act::StopBits | Act::Flow | Act::Dtr | Act::Rts | Act::Break);
        if line_change && !self.write {
            return format!("{tty}: read only: Attach first");
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
            (Act::BaudDown | Act::BaudUp | Act::DataBits | Act::Parity | Act::StopBits | Act::Flow, src) => {
                use xcomm::cli::{DataBits, Flow, Parity, StopBits};
                let mut s = self.settings;
                match a {
                    Act::DataBits => s.data_bits = match s.data_bits { DataBits::Eight => DataBits::Seven, DataBits::Seven => DataBits::Six, DataBits::Six => DataBits::Five, DataBits::Five => DataBits::Eight },
                    Act::Parity => s.parity = match s.parity { Parity::None => Parity::Even, Parity::Even => Parity::Odd, Parity::Odd => Parity::None },
                    Act::StopBits => s.stop_bits = match s.stop_bits { StopBits::One => StopBits::Two, StopBits::Two => StopBits::One },
                    Act::Flow => s.flow = match s.flow { Flow::None => Flow::RtsCts, Flow::RtsCts => Flow::XonXoff, Flow::XonXoff => Flow::None },
                    _ => s.baud = step(s.baud, a == Act::BaudUp),
                }
                match src {
                    Some(Source::Own(p, _)) => {
                        if let Err(e) = p.configure(&s) {
                            return format!("{tty}: {e}");
                        }
                    }
                    Some(Source::Shared { ctl: Some(c), .. }) => {
                        let patch = SettingsPatch {
                            baud: Some(s.baud),
                            data_bits: Some(data_bits(s.data_bits)),
                            parity: Some(format!("{:?}", s.parity).to_lowercase()),
                            stop_bits: Some(if s.stop_bits == StopBits::Two { 2 } else { 1 }),
                            flow: Some(flow_name(s.flow).into()),
                        };
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
            (Act::Agents, _) => {
                use xcomm::ctl::CtlMode;
                if self.shared.is_some() {
                    return format!("{tty}: agents reach the xcomm session sharing it");
                }
                let next = match self.agents {
                    CtlMode::ReadWrite => CtlMode::ReadOnly,
                    CtlMode::ReadOnly => CtlMode::Off,
                    CtlMode::Off => CtlMode::ReadWrite,
                };
                match self.serve_agents(next, &env) {
                    Ok(()) => format!("{tty}: agents {}", agents_name(next)),
                    Err(e) => format!("{tty}: {e}"),
                }
            }
            (Act::Capture, _) => match self.capture.take() {
                Some((path, _)) => format!("{tty}: capture stopped ({})", path.display()),
                None => {
                    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
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
            _ => format!("{tty}: not connected"),
        }
    }
}


impl xcomm::ctl::server::Session for Channel {
    fn status(&self) -> xcomm::ctl::proto::Status {
        use xcomm::ctl::proto::{ModemView, SettingsView, Status};
        let open = matches!(self.source, Some(Source::Own(..)));
        Status {
            device: format!("/dev/{}", self.tty),
            tty: xcomm::lock::lock_name(&Path::new("/dev").join(&self.tty)).unwrap_or_default(),
            stable: self.reopen.display().to_string(),
            settings: open.then(|| SettingsView::of(&self.settings)),
            modem: self.lines().map(|l| ModemView { dtr: l.dtr, rts: l.rts, cts: l.cts, dsr: l.dsr, dcd: l.dcd, ri: l.ri }),
            connected: open,
            offset: self.ring.end(),
            oldest: self.ring.oldest(),
            capture: self.capture.as_ref().map(|(p, _)| p.display().to_string()),
            transfer: false,
            ctl: String::new(),
            herdr_pane: None,
            pid: std::process::id(),
        }
    }

    fn ring(&self) -> &xcomm::ring::Ring {
        &self.ring
    }

    /// What went to the device so far, from the keys and from clients.
    fn written(&self) -> u64 {
        self.tx
    }

    fn send(&mut self, bytes: &[u8]) -> Result<usize, (u8, String)> {
        let Some(Source::Own(p, _)) = &self.source else { return Err((xcomm::ctl::code::DISCONNECTED, "the device is disconnected".into())) };
        // SAFETY: an open descriptor and a buffer of the length given.
        let n = unsafe { libc::write(p.fd().as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        match n {
            n if n >= 0 => Ok(n as usize),
            _ if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock => Ok(0),
            _ => Err((xcomm::ctl::code::FAILED, std::io::Error::last_os_error().to_string())),
        }
    }

    /// What a client sent shows on the screen as a dim `[ctl]` line, as in xcomm.
    fn sent(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.tx += bytes.len() as u64;
        let text: String = String::from_utf8_lossy(bytes).chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        self.screen.process(format!("\r\n\x1b[2m[ctl] {}\x1b[0m\r\n", text.trim_end()).as_bytes());
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.drop_port(String::new());
    }
}


fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1}M", n as f64 / f64::from(1 << 20)),
        n if n >= 1 << 10 => format!("{:.1}K", n as f64 / f64::from(1 << 10)),
        n => format!("{n}"),
    }
}


pub fn status(c: &Channel) -> String {
    let link = match (&c.link, &c.shared, c.write) {
        (Link::Waiting(why), ..) => why.clone(),
        (Link::Open, Some((_, who)), true) => format!("write, via {who}"),
        (Link::Open, Some((_, who)), false) => format!("read only, via {who}"),
        (Link::Open, None, true) => "write".to_string(),
        (Link::Open, None, false) => "read only".to_string(),
    };
    let cap = if c.capture.is_some() { " · ● rec" } else { "" };
    let note = c.note.as_ref().map_or(String::new(), |n| format!(" · {n}"));
    format!("{} · {link} · ↓{} ↑{}{cap}{note}", xcomm::port::describe(&c.settings), human(c.rx), human(c.tx))
}

