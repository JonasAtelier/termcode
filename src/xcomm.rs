//! The Serial view, on xcomm (crates.io): the ports this machine has and who holds each
//! (the side bar's PORTS, xcomm's `list`), the channels open here with their state and
//! line controls (CHANNELS), and each channel's stream in an editor tab (`serial:<tty>`):
//! the device's output on a terminal screen, the keys typed there sent to it. A port is
//! taken as xcomm takes it (a UUCP lock, then an exclusive open) and, after an unplug,
//! opened again by the USB port it is plugged into when it comes back.
//!
//! A port opened here has xcomm's control socket too (`$XDG_RUNTIME_DIR/xcomm/<tty>.sock`),
//! so `xcomm ctl ttyUSB0 run … --until …`, xcomm's agent skill and scripts drive it as
//! they would an xcomm session; what they send shows on its screen as a dim `[ctl]` line.

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

/// The Serial view: an in-tree driver of the view subsystem (ext/mod.rs).
pub const OPS: crate::ext::Ops = crate::ext::Ops {
    id: "serial",
    name: "Serial",
    title: "SERIAL (XCOMM)",
    about: "Serial ports and their streams, on xcomm",
    hint: "Open a port from the side bar",
    icon: || crate::theme::icons().serial,
    side: draw_side,
    owns: |k| k.starts_with(PREFIX),
    tab: |app, f, _, r, key, focused| {
        draw_page(app, f, r, key, focused);
        crate::ext::Drawn::Whole
    },
    // A channel's last tab closed: its port given back.
    closed: |app, key| close(app, key.strip_prefix(PREFIX).unwrap_or(key)),
    right: draw_settings,
    click: |app, p, _| click(app, p),
    wheel,
    mouse,
    drag_to,
    key,
    tab_key: page_key,
    takes: |app, k| app.focus == crate::panel::Focus::Editor && takes(app, k),
    paste: |app, t| app.focus == crate::panel::Focus::Editor && paste(app, t),
    tick,
    fds,
    overlay: draw_peek,
    ..crate::ext::Ops::NONE
};
/// Lines of a channel's output kept to scroll back through.
const SCROLLBACK: usize = 10_000;
/// How often the ports are listed while the view shows, and a lost port looked for.
const LIST_EVERY: Duration = Duration::from_secs(2);
const RETRY_EVERY: Duration = Duration::from_secs(1);

/// Device output kept for control-socket clients to read back (as xcomm's default).
const RING: usize = 1 << 20;

thread_local! {
    /// The open ports' descriptors: the event loop wakes when one has output.
    static FDS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    /// The control sockets' descriptors (listeners, clients), as the last pass left them.
    static CTL_FDS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// The ports' and control sockets' descriptors, for the event loop's poll.
pub fn fds() -> Vec<i32> {
    let mut v = FDS.with(|f| f.borrow().clone());
    v.extend(CTL_FDS.with(|f| f.borrow().clone()));
    v
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
    /// Selected text on its screen: (row, column) where the drag started and where it is.
    pub sel: Option<((u16, u16), (u16, u16))>,
    /// Output kept for control-socket clients (`xcomm ctl read`, `wait`).
    ring: xcomm::ring::Ring,
    /// xcomm's control socket on a port opened here; what it lets agents do.
    ctl: Option<xcomm::ctl::server::CtlServer>,
    pub agents: xcomm::ctl::CtlMode,
}

/// What a click in the side bar hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Port(usize),
    Channel(usize),
    Act(usize, Act),
    /// A port's note, to write.
    Note(usize),
    /// A port watched here, read only (Monitor).
    Monitor(usize),
}

/// A control as drawn: its label, what it does, whether it is the main one.
type Btn = (String, Act, bool);

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
    /// Notes on ports (`port_id` to text), as saved; loaded on first use.
    notes: Option<std::collections::BTreeMap<String, String>>,
    /// A note being written: its port's id and the text so far.
    pub editing: Option<(String, String)>,
    /// The ports pane's height (dragged at the line between the panes), the line itself.
    pub split: Option<u16>,
    pub sash: Rect,
    /// Each pane's rows scrolled off its top: ports, channels.
    pub scrolls: [u16; 2],
    panes: [Rect; 2],
    /// A selection is being dragged on a channel's screen.
    dragging: bool,
    /// The active channel tab's Write / Read only button, as drawn.
    page_btn: Option<(Rect, String)>,
    /// The settings panel's controls, as drawn.
    panel_hits: Vec<(Rect, Hit)>,
    last_click: Option<(Instant, Position)>,
}

/// Where the notes are kept: termcode's state folder (private).
fn notes_file() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("termcode-serial-notes-{}.json", std::process::id()));
    }
    crate::data_dir(true).join("serial-notes.json")
}

/// The ports for the API: each with its note, its state (as its dot) and who holds it,
/// and the channel open on it here.
pub fn ports_json(app: &mut App) -> serde_json::Value {
    let ports = xcomm::list::enumerate(&env).unwrap_or_default();
    let me = std::process::id() as i32;
    let notes = app.serial.notes().clone();
    let list: Vec<serde_json::Value> = ports
        .iter()
        .map(|p| {
            let ch = app.serial.channels.iter().find(|c| c.tty == p.tty);
            let shareable = p.lock_pid.is_some_and(|pid| pid != me) && session_socket(&p.tty, &env).is_some();
            let dot = ch.map_or_else(|| port_dot(p, shareable), channel_dot);
            serde_json::json!({
                "tty": p.tty,
                "note": notes.get(&port_id(p)),
                "state": format!("{dot:?}").to_lowercase(),
                "adapter": p.product,
                "usb": p.vid_pid,
                "serial": p.serial,
                "by_id": p.by_id,
                "holder": p.lock_pid.map(|pid| serde_json::json!({"pid": pid, "here": pid == me, "herdr_pane": p.herdr_pane, "xcomm": shareable})),
                "channel": ch.map(|c| serde_json::json!({"line": xcomm::port::describe(&c.settings), "write": c.write, "in": c.rx, "out": c.tx, "connected": c.link == Link::Open})),
            })
        })
        .collect();
    serde_json::Value::Array(list)
}

/// A channel's output as plain text (escapes dropped) from `since` (clamped to what the
/// ring keeps), and the offset after it.
pub fn output_since(c: &Channel, since: u64, max: usize) -> (String, u64) {
    let from = since.max(c.ring.oldest());
    let mut bytes = Vec::new();
    let next = c.ring.read(from, max, &mut bytes).unwrap_or(from);
    let mut log = crate::term::OutLog::default();
    log.push(&bytes);
    (log.since(0).to_string(), next)
}

/// The offset after a channel's last byte (`serial.wait` waits from there).
pub fn output_end(c: &Channel) -> u64 {
    c.ring.end()
}

/// The API's `serial.monitor`: `tty` watched here, read only (as Monitor).
pub fn api_monitor(app: &mut App, tty: &str) -> Result<serde_json::Value, String> {
    if !app.serial.channels.iter().any(|c| c.tty == tty) {
        app.serial.ports = xcomm::list::enumerate(&env).map_err(|e| e.to_string())?;
        app.serial.listed = Some(Instant::now());
        let i = app.serial.ports.iter().position(|p| p.tty == tty).ok_or_else(|| format!("no port {tty}"))?;
        monitor(app, i);
    }
    let c = app.serial.channels.iter().find(|c| c.tty == tty).ok_or_else(|| std::mem::take(&mut app.msg))?;
    Ok(serde_json::json!({"port": tty, "write": c.write, "shared": c.shared.is_some(), "status": status(c)}))
}

/// The API's `serial.attach`: typing on. A port someone else holds asks the user in
/// termcode first (as Attach does); a gone one is said.
pub fn api_attach(app: &mut App, tty: &str) -> Result<serde_json::Value, String> {
    let i = app.serial.channels.iter().position(|c| c.tty == tty).ok_or_else(|| format!("{tty} is not open: serial.monitor first"))?;
    let c = &app.serial.channels[i];
    if c.write {
        return Ok(serde_json::json!({"port": tty, "write": true}));
    }
    if c.link != Link::Open {
        return Err(format!("{tty} is not available (unplugged?)"));
    }
    if c.shared.is_some() {
        activate(app, Hit::Act(i, Act::Write));
        return Err(format!("{tty} is held by another xcomm: the user is asked in termcode; serial.ports shows when it is attached"));
    }
    activate(app, Hit::Act(i, Act::Write));
    Ok(serde_json::json!({"port": tty, "write": app.serial.channels[i].write}))
}

/// The API's `serial.send`: `text` to the device, on an attached channel.
pub fn api_send(app: &mut App, tty: &str, text: &str) -> Result<serde_json::Value, String> {
    let c = app.serial.channels.iter_mut().find(|c| c.tty == tty).ok_or_else(|| format!("{tty} is not open: serial.monitor first"))?;
    if !c.write {
        return Err(format!("{tty} is read only: serial.attach first"));
    }
    let mark = c.ring.end();
    c.send(text.as_bytes());
    // Shown as an agent's, as xcomm shows a control client's sends.
    let shown: String = text.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect();
    c.screen.process(format!("\x1b[2m[api] {}\x1b[0m\r\n", shown.trim_end()).as_bytes());
    Ok(serde_json::json!({"port": tty, "sent": text.len(), "mark": mark}))
}

/// The API's `serial.set`: line settings (baud, data_bits, parity, stop_bits, flow), on an
/// attached channel.
pub fn api_set(app: &mut App, tty: &str, p: &serde_json::Value) -> Result<serde_json::Value, String> {
    use xcomm::ctl::proto::{Request, SettingsPatch};
    let c = app.serial.channels.iter_mut().find(|c| c.tty == tty).ok_or_else(|| format!("{tty} is not open: serial.monitor first"))?;
    if !c.write {
        return Err(format!("{tty} is read only: serial.attach first"));
    }
    let patch = SettingsPatch {
        baud: p["baud"].as_u64().map(|b| b as u32),
        data_bits: p["data_bits"].as_u64().map(|b| b as u8),
        parity: p["parity"].as_str().map(str::to_string),
        stop_bits: p["stop_bits"].as_u64().map(|b| b as u8),
        flow: p["flow"].as_str().map(str::to_string),
    };
    let s = patch.apply(c.settings)?;
    match &mut c.source {
        Some(Source::Own(port, _)) => {
            let _applied = port.configure(&s).map_err(|e| e.to_string())?;
        }
        Some(Source::Shared { ctl: Some(ctl), .. }) => request(ctl, Request::Set { patch }).map_err(|e| e.to_string())?,
        _ => return Err(format!("{tty} is not connected")),
    }
    c.settings = s;
    Ok(serde_json::json!({"port": tty, "line": xcomm::port::describe(&s)}))
}

/// The API's `serial.close`: the channel closed, its tabs too.
pub fn api_close(app: &mut App, tty: &str) -> Result<serde_json::Value, String> {
    let i = app.serial.channels.iter().position(|c| c.tty == tty).ok_or_else(|| format!("{tty} is not open"))?;
    activate(app, Hit::Act(i, Act::Close));
    Ok(serde_json::json!({"port": tty, "closed": true}))
}

/// The API's `serial.note`: `note` ("" removes it) on the port named `tty`.
pub fn note_by_tty(app: &mut App, tty: &str, note: &str) -> Result<String, String> {
    let ports = xcomm::list::enumerate(&env).map_err(|e| e.to_string())?;
    let p = ports.iter().find(|p| p.tty == tty || format!("/dev/{}", p.tty) == tty).ok_or_else(|| format!("no port {tty}"))?;
    let id = port_id(p);
    app.serial.set_note(&id, note)?;
    app.serial.ports = ports;
    Ok(id)
}

/// A port's lasting name for its note: its adapter's USB ID and serial number (the
/// note follows the adapter to any USB port), else its USB port (by-path), else its tty.
pub fn port_id(p: &PortInfo) -> String {
    match (&p.vid_pid, &p.serial) {
        (Some(v), Some(sn)) => format!("usb:{v}:{sn}"),
        _ => {
            let stable = xcomm::reconnect::stable_path(Path::new(&p.path));
            if stable.starts_with("/dev/serial/by-path") { format!("path:{}", stable.display()) } else { format!("tty:{}", p.tty) }
        }
    }
}

impl Serial {
    pub fn notes(&mut self) -> &mut std::collections::BTreeMap<String, String> {
        self.notes.get_or_insert_with(|| std::fs::read(notes_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default())
    }

    /// The note on `p`, if any.
    pub fn note_of(&mut self, p: &PortInfo) -> Option<String> {
        self.notes().get(&port_id(p)).cloned()
    }

    /// `text` as the note on port `id` ("" removes it), saved.
    pub fn set_note(&mut self, id: &str, text: &str) -> Result<(), String> {
        let text = text.trim().chars().take(80).collect::<String>();
        let notes = self.notes();
        if text.is_empty() {
            notes.remove(id);
        } else {
            notes.insert(id.to_string(), text);
        }
        let json = serde_json::to_vec_pretty(notes).map_err(|e| e.to_string())?;
        let f = notes_file();
        let tmp = f.with_extension("json.tmp");
        std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &f)).map_err(|e| format!("{}: {e}", f.display()))
    }
}

fn env(k: &str) -> Option<String> {
    // Under test: locks and control sockets in a folder of the test run's own (0700), never
    // the machine's lock directory or the user's runtime one.
    if cfg!(test) && matches!(k, "XCOMM_LOCK_DIR" | "XDG_RUNTIME_DIR") {
        let d = std::env::temp_dir().join(format!("termcode-xcomm-env-{}", std::process::id()));
        let _ = std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700).create(&d);
        return Some(d.display().to_string());
    }
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
            sel: None,
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
    fn ctl_pass(&mut self, polled: &mut Vec<i32>) -> bool {
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

    /// The selection in reading order.
    fn sel_range(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.sel?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text, None when nothing is.
    pub fn selected(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.sel_range()?;
        Some(self.screen.screen().contents_between(r0, c0, r1, c1 + 1)).filter(|t| !t.is_empty())
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
        let bauds = crate::serial::BAUDS;
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

/// The ports listed (every 2 s while the view shows), lost ports looked for, output read.
/// True when something on screen changed.
pub fn tick(app: &mut App) -> bool {
    let mut changed = false;
    let shown = app.side == crate::panel::Side::SERIAL && app.side_visible;
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
    let mut polled = Vec::new();
    for ch in &mut app.serial.channels {
        changed |= ch.retry(&env);
        changed |= ch.pump();
        changed |= ch.ctl_pass(&mut polled);
    }
    CTL_FDS.with(|f| *f.borrow_mut() = polled);
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
    // Ctrl+C with text selected copies it, as in the terminal; else it goes to the device.
    if ctrl && matches!(k.code, KeyCode::Char('c' | 'C')) {
        let key = active_key(app).unwrap_or_default();
        if let Some(text) = channel_mut(app, &key).and_then(|c| {
            let t = c.selected();
            c.sel = None;
            t
        }) {
            app.msg = format!("copied via {}", crate::panel::clipboard_set(&text));
            app.clip = text;
            return true;
        }
    }
    let termcodes = matches!(k.code, KeyCode::F(1))
        || (ctrl && matches!(k.code, KeyCode::Char('q' | 'Q') | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Tab | KeyCode::BackTab))
        || crate::term::toggle_key(&k);
    !termcodes && page_key(app, k)
}

/// A mouse event over a channel's screen: a press starts a selection (a double click, a
/// word), a drag extends it; Ctrl+C then copies it. None when it is not over one.
pub fn mouse(app: &mut App, m: ratatui::crossterm::event::MouseEvent) -> Option<bool> {
    use ratatui::crossterm::event::{MouseButton, MouseEventKind};
    let p = Position::new(m.column, m.row);
    let over = |app: &App| {
        app.ui.groups.iter().enumerate().find_map(|(g, gh)| {
            let t = app.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).filter(|t| t.key.starts_with(PREFIX))?;
            let body = Rect::new(gh.text.x, gh.text.y + 1, gh.text.width, gh.text.height.saturating_sub(1));
            body.contains(p).then(|| (g, t.key.clone(), (p.y - body.y, p.x - body.x)))
        })
    };
    // The tab's Write / Read only button.
    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
        if let Some((_, tty)) = app.serial.page_btn.clone().filter(|(b, _)| b.contains(p)) {
            if let Some(i) = app.serial.channels.iter().position(|c| c.tty == tty) {
                activate(app, Hit::Act(i, Act::Write));
            }
            return Some(true);
        }
    }
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let (g, key, at) = over(app)?;
            (app.ag, app.focus) = (g, Focus::Editor);
            let double = app.serial.last_click.is_some_and(|(t, q)| t.elapsed() < Duration::from_millis(400) && q == p);
            app.serial.last_click = Some((Instant::now(), p));
            let ch = channel_mut(app, &key)?;
            ch.sel = if double { word_at(ch, at) } else { Some((at, at)) };
            app.serial.dragging = !double;
            Some(true)
        }
        MouseEventKind::Drag(MouseButton::Left) if app.serial.dragging => {
            let key = active_key(app)?;
            let gh = app.ui.groups.get(app.ag)?;
            let body = Rect::new(gh.text.x, gh.text.y + 1, gh.text.width, gh.text.height.saturating_sub(1));
            let at = (p.y.clamp(body.y, body.bottom().saturating_sub(1)) - body.y, p.x.clamp(body.x, body.right().saturating_sub(1)) - body.x);
            let ch = channel_mut(app, &key)?;
            ch.sel = ch.sel.map(|(a, _)| (a, at));
            Some(true)
        }
        MouseEventKind::Up(MouseButton::Left) if app.serial.dragging => {
            app.serial.dragging = false;
            let key = active_key(app)?;
            // A click without a drag selects nothing.
            if let Some(ch) = channel_mut(app, &key) {
                if ch.sel.is_some_and(|(a, b)| a == b) {
                    ch.sel = None;
                }
            }
            Some(true)
        }
        _ => None,
    }
}

/// The word (a run of non-blank cells) at `at` on a channel's screen.
fn word_at(ch: &Channel, at: (u16, u16)) -> Option<((u16, u16), (u16, u16))> {
    let s = ch.screen.screen();
    let blank = |c: u16| s.cell(at.0, c).is_none_or(|c| c.contents().trim().is_empty());
    if blank(at.1) {
        return None;
    }
    let cols = s.size().1;
    let a = (0..=at.1).rev().take_while(|&c| !blank(c)).last().unwrap_or(at.1);
    let b = (at.1..cols).take_while(|&c| !blank(c)).last().unwrap_or(at.1);
    Some(((at.0, a), (at.0, b)))
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
    // Over the side bar's blocks: they scroll.
    if app.side == crate::panel::Side::SERIAL && app.side_visible {
        if let Some(k) = app.serial.panes.iter().position(|r| r.contains(p)) {
            let s = &mut app.serial.scrolls[k];
            *s = if d < 0 { s.saturating_sub(3) } else { s.saturating_add(3) };
            return true;
        }
    }
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
        n if n >= 1 << 20 => format!("{:.1}M", n as f64 / f64::from(1 << 20)),
        n if n >= 1 << 10 => format!("{:.1}K", n as f64 / f64::from(1 << 10)),
        n => format!("{n}"),
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

/// How many there are and in what state, without a word: a dot each (up to five), else
/// a count per state (green, yellow, red), each in its colour.
pub fn state_dots(dots: &[Dot], dot_ch: &str) -> Vec<Span<'static>> {
    if dots.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Span> = if dots.len() <= 5 {
        dots.iter().map(|d| Span::styled(dot_ch.to_string(), Style::new().fg(d.color()))).collect()
    } else {
        [Dot::Green, Dot::Yellow, Dot::Red]
            .into_iter()
            .filter_map(|d| {
                let n = dots.iter().filter(|x| **x == d).count();
                (n > 0).then(|| Span::styled(format!("{dot_ch}{n} "), Style::new().fg(d.color())))
            })
            .collect()
    };
    if dots.len() <= 5 {
        out.push(Span::raw(" "));
    }
    out
}

fn status(c: &Channel) -> String {
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

/// A channel's tab: its status on the first row, its screen under it.
pub fn draw_page(app: &mut App, f: &mut Frame, r: Rect, key: &str, focused: bool) {
    use crate::theme::P;
    let tty = &key[PREFIX.len()..];
    app.serial.page_btn = None;
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
    // Its one switch, at the right: Write (typing on) or Read only.
    let label = if ch.write { " Read only " } else { " Attach " };
    let w = label.chars().count() as u16;
    let b = Rect::new(head.right().saturating_sub(w + 1), head.y, w, 1);
    let hot = app.hover.is_some_and(|p| b.contains(p));
    f.render_widget(Span::styled(label, crate::ui::button_style(!ch.write || hot, hot)), b);
    app.serial.page_btn = Some((b, tty.to_string()));
    let body = Rect::new(r.x, r.y + 1, r.width, r.height - 1);
    if ch.screen.screen().size() != (body.height.max(2), body.width.max(2)) {
        ch.screen.screen_mut().set_size(body.height.max(2), body.width.max(2));
    }
    let live = ch.screen.screen().scrollback() == 0;
    crate::term::draw_screen(ch.screen.screen(), ch.sel_range(), f.buffer_mut(), body, (P.editor_fg, P.editor_bg, P.selection), focused && live);
}

/// The side bar: the ports pane (a block per port: its dot and tty, its note or adapter,
/// its USB ID and holder, Monitor; ✎ for the note), the line between (dragged to resize,
/// as GitHub's), the channels pane (a block per open channel: its state, Attach, ✕).
/// Blocks light up under the pointer; each pane scrolls.
pub fn draw_side(app: &mut App, f: &mut Frame, r: Rect) {
    use crate::theme::P;
    // In a card as the other views, its padding inside.
    let focused_side = app.focus == Focus::Side && !app.activity_lit;
    let card = crate::blocks::card(app.settings.blocks, f, r, P.sidebar_bg, focused_side);
    let r = crate::ui::inset(card, (app.settings.spacing.sidebar_padding.saturating_sub(1) / 2).max(1));
    app.ui.side_list = Rect::default();
    if r.width < 12 || r.height < 6 {
        return;
    }
    let notes = app.serial.notes().clone();
    let (hover, focused) = (app.hover, app.focus == Focus::Side);
    let dragging = matches!(app.drag, crate::panel::Drag::View(crate::ext::SERIAL));
    let ascii = crate::theme::ascii();
    let dot_ch = if ascii { "*" } else { "●" };
    let me = std::process::id() as i32;
    let s = &mut app.serial;
    let total = s.ports.len() + s.channels.len();
    s.sel = s.sel.min(total.saturating_sub(1));
    let dim = Style::new().fg(P.dim);
    // The panes: ports above the line, channels under it.
    let top_h = s.split.unwrap_or(r.height * 3 / 5).clamp(3, r.height.saturating_sub(4));
    let panes = [Rect::new(r.x, r.y + 1, r.width, top_h - 1), Rect::new(r.x, r.y + top_h + 1, r.width, r.height - top_h - 1)];
    let sash = Rect::new(r.x, r.y + top_h, r.width, 1);
    f.render_widget(Span::styled(format!("PORTS {}", s.ports.len()), dim.bold()), Rect::new(r.x, r.y, r.width, 1));
    let hot = dragging || hover.is_some_and(|p| sash.contains(p));
    let hl = if ascii { "-" } else { "─" };
    let line_st = Style::new().fg(if hot { P.sash_hover } else { P.group_border });
    let mut label = vec![Span::styled(hl, line_st), Span::styled(" CHANNELS ", dim.bold())];
    label.extend(state_dots(&s.channels.iter().map(channel_dot).collect::<Vec<_>>(), dot_ch));
    let used: usize = label.iter().map(|sp| sp.content.chars().count()).sum();
    label.push(Span::styled(hl.repeat(usize::from(r.width).saturating_sub(used)), line_st));
    f.render_widget(Line::from(label), sash);
    // Each block laid out from its pane's top, drawn where it falls inside after its scroll.
    let at = |pane: Rect, y: i32, h: u16| -> Option<Rect> {
        let top = (i32::from(pane.y) + y).max(i32::from(pane.y));
        let bottom = (i32::from(pane.y) + y + i32::from(h)).min(i32::from(pane.bottom()));
        (bottom > top).then(|| Rect::new(pane.x, top as u16, pane.width, (bottom - top) as u16))
    };
    let block = |f: &mut Frame, a: Rect, title: Line<'static>, lit: bool| -> Rect {
        let b = ratatui::widgets::Block::bordered()
            .border_set(crate::blocks::border_set())
            .border_style(Style::new().fg(if lit { P.focus_border } else { P.group_border }))
            .title(title)
            .style(Style::new().bg(if lit { P.widget_bg } else { P.sidebar_bg }));
        let inner = b.inner(a);
        f.render_widget(b, a);
        inner
    };
    let button = |f: &mut Frame, hits: &mut Vec<(Rect, Hit)>, b: Rect, label: &str, primary: bool, hit: Hit| {
        let hot = hover.is_some_and(|h| b.contains(h));
        f.render_widget(Span::styled(format!(" {label} "), crate::ui::button_style(primary || hot, hot)), b);
        hits.push((b, hit));
    };
    let mut hits = Vec::new();
    // Ports.
    let mut y: i32 = -i32::from(s.scrolls[0]);
    if let Some(p) = &s.problem {
        f.render_widget(Span::styled(p.clone(), Style::new().fg(P.error_fg)), Rect::new(panes[0].x, panes[0].y, panes[0].width, 1));
        y += 1;
    } else if s.ports.is_empty() {
        f.render_widget(Span::styled("None plugged in", dim), Rect::new(panes[0].x, panes[0].y, panes[0].width, 1));
        y += 1;
    }
    for (i, p) in s.ports.iter().enumerate() {
        let ours = s.channels.iter().find(|c| c.tty == p.tty);
        let shareable = p.lock_pid.is_some_and(|pid| pid != me) && session_socket(&p.tty, &env).is_some();
        let dot = ours.map_or_else(|| port_dot(p, shareable), channel_dot);
        if let Some(a) = at(panes[0], y, 4) {
            let lit = (focused && s.sel == i) || hover.is_some_and(|h| a.contains(h));
            let title = Line::from(vec![Span::styled(format!(" {dot_ch} "), Style::new().fg(dot.color())), Span::styled(format!("{} ", p.tty), Style::new().fg(P.sidebar_fg).bold())]);
            let inner = block(f, a, title, lit);
            hits.push((a, Hit::Port(i)));
            let id = port_id(p);
            let product = p.product.clone().unwrap_or_else(|| "serial port".into());
            let first = match s.editing.as_ref().filter(|(e, _)| *e == id) {
                Some((_, text)) => Span::styled(format!("{text}▏"), Style::new().fg(P.sidebar_fg).bg(P.input_bg)),
                None => match notes.get(&id) {
                    Some(n) => Span::styled(n.clone(), Style::new().fg(P.tab_active_fg).bold()),
                    None => Span::styled(product, Style::new().fg(P.sidebar_fg)),
                },
            };
            let held = match (ours, p.lock_pid, &p.herdr_pane) {
                (Some(_), ..) => "here".to_string(),
                (None, Some(pid), _) if pid == me => "here".into(),
                (None, Some(_), Some(pane)) => format!("xcomm {pane}"),
                (None, Some(pid), None) if shareable => format!("xcomm {pid}"),
                (None, Some(pid), None) => std::fs::read_to_string(format!("/proc/{pid}/comm")).map_or_else(|_| format!("pid {pid}"), |c| c.trim().to_string()),
                (None, None, _) => "free".into(),
            };
            let second = Line::styled(format!("{} · {held}", p.vid_pid.as_deref().unwrap_or("not USB")), Style::new().fg(if dot == Dot::Green { P.dim } else { dot.color() }));
            f.render_widget(ratatui::widgets::Paragraph::new(vec![Line::from(first), second]), inner);
            // Monitor (or Show, when open here), right on the second row; none when it can't be.
            let can = ours.is_some() || p.lock_pid.is_none_or(|pid| pid == me) || shareable;
            // The block's main button: filled, with its icon.
            let label = match (ours.is_some(), ascii) {
                (true, _) => "Show",
                (false, true) => "> Monitor",
                (false, false) => "▶ Monitor",
            };
            let w = label.chars().count() as u16 + 2;
            if can && inner.height >= 2 && inner.width > w {
                button(f, &mut hits, Rect::new(inner.right() - w, inner.y + 1, w, 1), label, ours.is_none(), Hit::Monitor(i));
            }
            // ✎ on the top border, right: the note.
            let pen = Rect::new(inner.right().saturating_sub(2), a.y, 2, 1);
            if a.y == (i32::from(panes[0].y) + y) as u16 && s.editing.is_none() {
                let hot = hover.is_some_and(|h| pen.contains(h));
                f.render_widget(Span::styled(if ascii { "e" } else { "✎" }, Style::new().fg(if hot { P.focus_border } else { P.dim })), pen);
                hits.push((pen, Hit::Note(i)));
            }
        }
        y += 4;
    }
    let content = (y + i32::from(s.scrolls[0])).max(0) as u16;
    s.scrolls[0] = s.scrolls[0].min(content.saturating_sub(panes[0].height));
    // Channels.
    let mut y: i32 = -i32::from(s.scrolls[1]);
    if s.channels.is_empty() {
        f.render_widget(Span::styled("Monitor a port to open one", dim), Rect::new(panes[1].x, panes[1].y, panes[1].width, 1));
    }
    for (i, c) in s.channels.iter().enumerate() {
        if let Some(a) = at(panes[1], y, 4) {
            let lit = (focused && s.sel == s.ports.len() + i) || hover.is_some_and(|h| a.contains(h));
            let dot = channel_dot(c);
            let title = Line::from(vec![Span::styled(format!(" {dot_ch} "), Style::new().fg(dot.color())), Span::styled(format!("{} ", c.tty), Style::new().fg(P.sidebar_fg).bold())]);
            let inner = block(f, a, title, lit);
            hits.push((a, Hit::Channel(i)));
            f.render_widget(Span::styled(status(c), dim), Rect::new(inner.x, inner.y, inner.width, 1));
            if inner.height >= 2 {
                let mut x = inner.x;
                for (label, act, primary) in [(if c.write { "Read only" } else { "Attach" }, Act::Write, !c.write), ("✕", Act::Close, false)] {
                    let w = label.chars().count() as u16 + 2;
                    if x + w <= inner.right() {
                        button(f, &mut hits, Rect::new(x, inner.y + 1, w, 1), label, primary, Hit::Act(i, act));
                    }
                    x += w + 1;
                }
            }
        }
        y += 4;
    }
    let content = (y + i32::from(s.scrolls[1])).max(0) as u16;
    s.scrolls[1] = s.scrolls[1].min(content.saturating_sub(panes[1].height));
    (s.hits, s.panes, s.sash) = (hits, panes, sash);
}

/// A channel tab's settings panel, right of the editors where the Outline goes (its
/// button opens it): each setting a row, its value a button that changes it; then the
/// port. False when the active tab is not a channel's.
pub fn draw_settings(app: &mut App, f: &mut Frame, r: Rect) -> bool {
    use crate::theme::P;
    let Some(key) = active_key(app) else { return false };
    let hover = app.hover;
    app.serial.panel_hits.clear();
    crate::ui::fill(f, r, P.sidebar_bg);
    let tty = key[PREFIX.len()..].to_string();
    let Some(i) = app.serial.channels.iter().position(|c| c.tty == tty) else { return true };
    let port = app.serial.ports.iter().find(|p| p.tty == tty).cloned();
    let c = &app.serial.channels[i];
    let inner = Rect::new(r.x + 1, r.y, r.width.saturating_sub(2), r.height);
    let dim = Style::new().fg(P.dim);
    let lines = c.lines();
    let onoff = |b: Option<bool>| if b == Some(true) { "● on" } else { "○ off" }.to_string();
    let rows: Vec<(&str, Vec<Btn>)> = vec![
        ("Mode", vec![(if c.write { "Read only".into() } else { "Attach".into() }, Act::Write, !c.write)]),
        ("Baud", vec![("‹".into(), Act::BaudDown, false), (c.settings.baud.to_string(), Act::BaudUp, false), ("›".into(), Act::BaudUp, false)]),
        ("Data", vec![(data_bits(c.settings.data_bits).to_string(), Act::DataBits, false)]),
        ("Parity", vec![(format!("{:?}", c.settings.parity).to_lowercase(), Act::Parity, false)]),
        ("Stop", vec![(if c.settings.stop_bits == xcomm::cli::StopBits::Two { "2" } else { "1" }.into(), Act::StopBits, false)]),
        ("Flow", vec![(flow_name(c.settings.flow).into(), Act::Flow, false)]),
        ("DTR", vec![(onoff(lines.map(|l| l.dtr)), Act::Dtr, false)]),
        ("RTS", vec![(onoff(lines.map(|l| l.rts)), Act::Rts, false)]),
        ("Break", vec![("Send".into(), Act::Break, false)]),
        ("Record", vec![(if c.capture.is_some() { "■ Stop".into() } else { "● Start".into() }, Act::Capture, false)]),
        ("Agents", vec![(if c.shared.is_some() { "via xcomm".into() } else { agents_name(c.agents).into() }, Act::Agents, false)]),
    ];
    let mut y = inner.y;
    f.render_widget(Span::styled("SETTINGS", dim.bold()), Rect::new(inner.x, y, inner.width, 1));
    y += 2;
    let mut hits = Vec::new();
    for (label, btns) in rows {
        if y >= inner.bottom() {
            break;
        }
        f.render_widget(Span::styled(label, dim), Rect::new(inner.x, y, 8.min(inner.width), 1));
        let mut x = inner.x + 8;
        for (text, a, primary) in btns {
            let w = text.chars().count() as u16 + 2;
            if x + w > inner.right() {
                break;
            }
            let b = Rect::new(x, y, w, 1);
            let hot = hover.is_some_and(|h| b.contains(h));
            f.render_widget(Span::styled(format!(" {text} "), crate::ui::button_style(primary || hot, hot)), b);
            hits.push((b, Hit::Act(i, a)));
            x += w + 1;
        }
        y += 2;
    }
    // The port: what it is, who holds it.
    if let Some(p) = port {
        for (k, v) in [("Adapter", p.product.unwrap_or_default()), ("USB", p.vid_pid.unwrap_or_default()), ("Serial", p.serial.unwrap_or_default())] {
            if y >= inner.bottom() {
                break;
            }
            f.render_widget(Line::from(vec![Span::styled(format!("{k:<8}"), dim), Span::styled(v, Style::new().fg(P.sidebar_fg))]), Rect::new(inner.x, y, inner.width, 1));
            y += 1;
        }
    }
    app.serial.panel_hits = hits;
    true
}

/// The card of the row the pointer rests on (or, with `i`, the selected one): everything
/// about the port at a glance, its channel's line settings and modem lines when open.
pub fn draw_peek(app: &mut App, f: &mut Frame) {
    use crate::theme::P;
    if app.side != crate::panel::Side::SERIAL || !app.side_visible {
        return;
    }
    let s = &app.serial;
    // Only while the pointer rests on a port's block.
    let Some((anchor, hit)) = app.hover.and_then(|p| s.hits.iter().find(|(r, h)| r.contains(p) && matches!(h, Hit::Port(_))).copied()) else { return };
    let (port, chan) = match hit {
        Hit::Port(i) => {
            let p = s.ports.get(i);
            (p, p.and_then(|p| s.channels.iter().find(|c| c.tty == p.tty)))
        }
        Hit::Channel(i) => {
            let c = s.channels.get(i);
            (c.and_then(|c| s.ports.iter().find(|p| p.tty == c.tty)), c)
        }
        _ => return,
    };
    let row = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<9}"), Style::new().fg(P.dim)), Span::styled(v, Style::new().fg(P.editor_fg))]);
    let mut lines = Vec::new();
    let note = port.and_then(|p| app.serial.notes.as_ref().and_then(|n| n.get(&port_id(p)).cloned()));
    let tty = port.map(|p| p.tty.clone()).or(chan.map(|c| c.tty.clone())).unwrap_or_default();
    lines.push(Line::from(vec![Span::styled(tty, Style::new().fg(P.editor_fg).bold()), Span::styled(note.map_or(String::new(), |n| format!("  {n}")), Style::new().fg(P.tab_active_fg))]));
    if let Some(p) = port {
        lines.push(row("Adapter", p.product.clone().unwrap_or_else(|| "-".into())));
        lines.push(row("USB", format!("{}  serial {}", p.vid_pid.as_deref().unwrap_or("-"), p.serial.as_deref().unwrap_or("-"))));
        lines.push(row("by-id", p.by_id.as_deref().and_then(|b| b.rsplit('/').next()).unwrap_or("-").to_string()));
        let by_path = xcomm::reconnect::stable_path(Path::new(&p.path));
        lines.push(row("by-path", if by_path.starts_with("/dev/serial/by-path") { by_path.file_name().map_or("-".into(), |n| n.to_string_lossy().into_owned()) } else { "-".into() }));
        let holder = match (p.lock_pid, &p.herdr_pane) {
            (None, _) => "free".to_string(),
            (Some(pid), _) if pid == std::process::id() as i32 => "this termcode".into(),
            (Some(pid), Some(pane)) => format!("xcomm in herdr {pane} (pid {pid})"),
            (Some(pid), None) => format!("pid {pid}"),
        };
        lines.push(row("Holder", holder));
    }
    if let Some(c) = chan {
        let st = c.settings;
        lines.push(row("Line", xcomm::port::describe(&st)));
        if let Some(l) = c.lines() {
            let o = |b: bool| if b { "on" } else { "off" };
            lines.push(row("Modem", format!("DTR {} RTS {} · CTS {} DSR {} DCD {}", o(l.dtr), o(l.rts), o(l.cts), o(l.dsr), o(l.dcd))));
        }
        lines.push(row("Traffic", format!("in {} · out {}", human(c.rx), human(c.tx))));
        lines.push(row("Mode", match (&c.link, c.write) {
            (Link::Waiting(w), _) => format!("disconnected: {w}"),
            (_, true) => "read & write".into(),
            (_, false) => "read only".into(),
        }));
        if let Some((path, _)) = &c.capture {
            lines.push(row("Capture", path.display().to_string()));
        }
    }
    let area = f.area();
    let w = lines.iter().map(Line::width).max().unwrap_or(10).min(70) as u16 + 4;
    let h = lines.len() as u16 + 2;
    let x = (anchor.right() + 1).min(area.right().saturating_sub(w));
    let y = anchor.y.min(area.bottom().saturating_sub(h));
    let card = Rect::new(x, y, w.min(area.width), h.min(area.height));
    f.render_widget(ratatui::widgets::Clear, card);
    let block = ratatui::widgets::Block::bordered().border_set(crate::blocks::border_set()).border_style(Style::new().fg(P.focus_border)).style(Style::new().bg(P.widget_bg));
    f.render_widget(ratatui::widgets::Paragraph::new(lines).block(block.padding(ratatui::widgets::Padding::horizontal(1))), card);
}

/// A click in the side bar: a port opens (or shows) its channel; a channel shows its
/// tab; a control acts on its channel.
pub fn click(app: &mut App, p: Position) -> bool {
    // The settings panel's controls (it shows beside a channel's tab).
    if let Some(&(_, hit)) = app.serial.panel_hits.iter().find(|(r, _)| r.contains(p)) {
        if active_key(app).is_some() {
            activate(app, hit);
            return true;
        }
    }
    if app.side != crate::panel::Side::SERIAL || !app.side_visible {
        return false;
    }
    if app.serial.sash.contains(p) {
        app.drag = crate::panel::Drag::View(crate::ext::SERIAL);
        return true;
    }
    let Some(&(_, hit)) = app.serial.hits.iter().rev().find(|(r, _)| r.contains(p)) else { return false };
    activate(app, hit);
    true
}

fn activate(app: &mut App, hit: Hit) {
    match hit {
        Hit::Port(i) => app.serial.sel = i,
        Hit::Monitor(i) => monitor(app, i),
        Hit::Note(i) => {
            app.serial.sel = i;
            start_note(app);
        }
        Hit::Channel(i) => {
            app.serial.sel = app.serial.ports.len() + i;
            if let Some(tty) = app.serial.channels.get(i).map(|c| c.tty.clone()) {
                app.show_tab(app.ag, format!("{PREFIX}{tty}"), false);
            }
        }
        Hit::Act(i, Act::Write) if app.serial.channels.get(i).is_some_and(|c| !c.write && c.link != Link::Open) => {
            let tty = app.serial.channels[i].tty.clone();
            app.prompt = Some(crate::panel::Prompt { text: vec![format!("{tty}: not available (unplugged?)")], buttons: vec![("OK".into(), crate::panel::PAct::Cancel)], sel: 0 });
        }
        Hit::Act(i, Act::Write) if app.serial.channels.get(i).is_some_and(|c| c.shared.is_some() && !c.write) => {
            let c = &app.serial.channels[i];
            let who = c.shared.as_ref().map_or_else(String::new, |(_, w)| w.clone());
            app.prompt = Some(crate::panel::Prompt {
                text: vec![format!("⚠ {}: typing goes to the board through {who}.", c.tty)],
                buttons: vec![("Write".into(), crate::panel::PAct::Serial(c.tty.clone(), true)), ("Cancel".into(), crate::panel::PAct::Cancel)],
                sel: 1,
            });
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

/// Monitor on port `i`: watched here, read only, no questions: opened if free, shared
/// with the xcomm holding it, its tab shown if already open; a holder that can't share
/// is said.
fn monitor(app: &mut App, i: usize) {
    app.serial.sel = i;
    let Some(p) = app.serial.ports.get(i).cloned() else { return };
    let me = std::process::id() as i32;
    if app.serial.channels.iter().any(|c| c.tty == p.tty) {
        app.show_tab(app.ag, format!("{PREFIX}{}", p.tty), false);
        return;
    }
    match p.lock_pid.filter(|&pid| pid != me) {
        None => open(app, Path::new(&p.path)),
        Some(_) if session_socket(&p.tty, &env).is_some() => share(app, &p.tty, false),
        Some(pid) => {
            let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).map_or_else(|_| "another program".into(), |c| c.trim().to_string());
            app.msg = format!("{}: held by {name} (pid {pid}), which can't share it", p.tty);
        }
    }
}

/// The answer to `held_prompt`: `tty` shared with its xcomm session, typed into when `write`.
pub fn share(app: &mut App, tty: &str, write: bool) {
    // Already shared here: only writing turned on (after the warning).
    if let Some(i) = app.serial.channels.iter().position(|c| c.tty == tty) {
        let root = app.root.clone();
        if write && !app.serial.channels[i].write {
            app.msg = app.serial.channels[i].act(Act::Write, &root);
        }
        return;
    }
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

/// Writing the selected port's note (`n`, or its ✎): its text so far to start with.
fn start_note(app: &mut App) {
    let Some(p) = app.serial.ports.get(app.serial.sel).cloned() else { return };
    let text = app.serial.note_of(&p).unwrap_or_default();
    app.serial.editing = Some((port_id(&p), text));
    app.focus = Focus::Side;
}

/// The line between the panes dragged to `y`: the ports pane ends there.
pub fn drag_to(app: &mut App, y: u16) {
    // From the view's top (the PORTS heading above the pane): the line lands at `y`.
    let top = app.serial.panes[0].y.saturating_sub(1);
    app.serial.split = Some(y.saturating_sub(top).max(2));
}

/// Keys in the side bar: ↑ ↓ choose, Enter opens a port or shows a channel, `n` writes
/// the port's note (Enter keeps it, Esc drops it), `i` shows the row's card.
pub fn key(app: &mut App, k: ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::KeyCode;
    if let Some((id, mut text)) = app.serial.editing.take() {
        match k.code {
            KeyCode::Enter => {
                if let Err(e) = app.serial.set_note(&id, &text) {
                    app.msg = e;
                }
                return true;
            }
            KeyCode::Esc => return true,
            KeyCode::Backspace => drop(text.pop()),
            KeyCode::Char(c) if !c.is_control() && text.chars().count() < 80 => text.push(c),
            _ => {}
        }
        app.serial.editing = Some((id, text));
        return true;
    }
    if k.code == KeyCode::Char('n') && app.serial.sel < app.serial.ports.len() {
        start_note(app);
        return true;
    }

    let (sel, ports) = (app.serial.sel, app.serial.ports.len());
    let total = ports + app.serial.channels.len();
    match k.code {
        KeyCode::Up => app.serial.sel = sel.saturating_sub(1),
        KeyCode::Down => app.serial.sel = (sel + 1).min(total.saturating_sub(1)),
        KeyCode::Enter if sel < ports => activate(app, Hit::Monitor(sel)),
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
        // Locks and control sockets in the test's own folder.
        let env = move |k: &str| matches!(k, "XCOMM_LOCK_DIR" | "XDG_RUNTIME_DIR").then(|| dir.clone());
        let (mut dev, tty) = pty();
        let mut ch = Channel::open(&tty, LineSettings::default(), &env).expect("the pty opens as a port");
        assert!(!ch.write, "read only until Write");
        ch.send(b"x");
        assert!(ch.tx == 0 && ch.note.is_some(), "nothing typed while read only");
        ch.act(Act::Write, Path::new("/"));
        assert!(ch.tty.starts_with("pts/") && ch.link == Link::Open, "{}", ch.tty);
        let lck = |d: &Path| std::fs::read_dir(d).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("LCK")).count();
        assert!(lck(&locks) == 1, "its UUCP lock taken");
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
        // Data bits, parity, stop bits, flow: each its next, applied to the port.
        for a in [Act::DataBits, Act::Parity, Act::StopBits, Act::Flow] {
            ch.act(a, &locks);
        }
        let msg = xcomm::port::describe(&ch.settings);
        assert_eq!(msg, "230400 7E2 rts/cts", "xcomm names flow control when there is one");
        let (Some(Source::Own(p, _)), true) = (&ch.source, true) else { panic!() };
        let applied = p.read_settings().unwrap();
        // The tty itself changed (a pty keeps no data size or parity: no UART behind it).
        assert_eq!((applied.baud, applied.stop_bits, applied.flow), (230_400, xcomm::cli::StopBits::Two, xcomm::cli::Flow::RtsCts));
        drop(ch);
        assert_eq!(lck(&locks), 0, "its lock given back");
        std::fs::remove_dir_all(&locks).unwrap();
    }

    #[test]
    fn xcomm_ctl_drives_a_port_opened_here_as_it_would_an_xcomm_session() {
        use std::io::{BufRead, BufReader};
        let tmp = std::env::temp_dir().join(format!("termcode-xcomm-ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("locks")).unwrap();
        std::fs::create_dir_all(tmp.join("run")).unwrap();
        std::fs::set_permissions(tmp.join("run"), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let (locks, run) = (tmp.join("locks").display().to_string(), tmp.join("run").display().to_string());
        let env = move |k: &str| match k {
            "XCOMM_LOCK_DIR" => Some(locks.clone()),
            "XDG_RUNTIME_DIR" => Some(run.clone()),
            _ => None,
        };
        let (mut dev, tty) = pty();
        let mut ch = Channel::open(&tty, LineSettings::default(), &env).unwrap();
        ch.write = true;
        assert!(ch.note.is_none(), "{:?}", ch.note);
        let name = xcomm::lock::lock_name(&tty).unwrap();
        let sock = tmp.join("run/xcomm").join(format!("{name}.sock"));
        assert!(sock.exists(), "xcomm's socket for it: {}", sock.display());
        // A client: each request a line, each reply a line, the session turning meanwhile.
        let mut conn = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let mut replies = BufReader::new(conn.try_clone().unwrap());
        let mut ask = |ch: &mut Channel, req: &str| -> serde_json::Value {
            conn.write_all(format!("{req}\n").as_bytes()).unwrap();
            replies.get_ref().set_nonblocking(true).unwrap();
            let mut line = String::new();
            for _ in 0..200 {
                let mut polled = Vec::new();
                ch.pump();
                ch.ctl_pass(&mut polled);
                if replies.read_line(&mut line).is_ok_and(|n| n > 0) {
                    return serde_json::from_str(&line).unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("no reply to {req}");
        };
        let st = ask(&mut ch, r#"{"cmd":"status"}"#);
        assert_eq!((st["ok"].as_bool(), st["connected"].as_bool(), st["settings"]["baud"].as_u64()), (Some(true), Some(true), Some(115_200)), "{st}");
        // send: to the device, and on the screen as a [ctl] line.
        assert_eq!(ask(&mut ch, r#"{"cmd":"send","data_b64":"dW5hbWUgLWEN"}"#)["ok"], true);
        let mut got = Vec::new();
        while got.len() < 9 {
            let mut b = [0u8; 32];
            let n = dev.read(&mut b).unwrap();
            got.extend_from_slice(&b[..n]);
        }
        assert_eq!(got, b"uname -a\r");
        assert!(ch.screen.screen().contents().contains("[ctl] uname -a"));
        // read: what the device said, from the ring.
        dev.write_all(b"Linux board 6.8\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let r = ask(&mut ch, r#"{"cmd":"read","last":64}"#);
        let data = xcomm::ctl::proto::base64_decode(r["data_b64"].as_str().unwrap_or_default()).unwrap_or_default();
        assert!(String::from_utf8_lossy(&data).contains("Linux board 6.8"), "{r}");
        // set: the line changed on the port.
        assert_eq!(ask(&mut ch, r#"{"cmd":"set","baud":57600}"#)["ok"], true);
        assert_eq!(ch.settings.baud, 57_600);
        // Agents off: the socket gone.
        ch.act(Act::Agents, &tmp);
        ch.act(Act::Agents, &tmp);
        assert_eq!(ch.agents, xcomm::ctl::CtlMode::Off);
        assert!(!sock.exists());
        drop(ch);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_channels_settings_show_beside_its_tab_and_change_its_line() {
        let tmp = std::env::temp_dir().join(format!("termcode-xcomm-panel-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let dir = tmp.display().to_string();
        let env = move |k: &str| matches!(k, "XCOMM_LOCK_DIR" | "XDG_RUNTIME_DIR").then(|| dir.clone());
        let (_dev, tty) = pty();
        let mut app = App::new(tmp.clone(), None);
        let ch = Channel::open(&tty, LineSettings::default(), &env).unwrap();
        let key = format!("{PREFIX}{}", ch.tty);
        app.serial.channels.push(ch);
        app.show_tab(app.ag, key, false);
        app.outline_open = true; // the Outline's button: beside a channel, its settings
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 36)).unwrap();
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let text = crate::api::screen_text(t.backend().buffer());
        for want in ["SETTINGS", "Mode", "Attach", "Baud", "115200", "Parity", "Agents"] {
            assert!(text.contains(want), "{want}: {text}");
        }
        let press = |app: &mut App, act: Act| {
            let r = app.serial.panel_hits.iter().find(|(_, h)| *h == Hit::Act(0, act)).unwrap().0;
            assert!(click(app, Position::new(r.x, r.y)));
        };
        // Read only: the line is not changed either; Write first.
        press(&mut app, Act::BaudUp);
        assert_eq!(app.serial.channels[0].settings.baud, 115_200);
        press(&mut app, Act::Write);
        press(&mut app, Act::BaudUp);
        press(&mut app, Act::Parity);
        let c = &app.serial.channels[0];
        assert_eq!((c.settings.baud, c.settings.parity, c.write), (230_400, xcomm::cli::Parity::Even, true));
        drop(app);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn monitor_watches_attach_writes_and_the_line_between_the_panes_drags() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let root = std::env::temp_dir().join(format!("termcode-xcomm-monitor-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(root.clone(), None);
        app.show_side(crate::panel::Side::SERIAL, false);
        app.serial.listed = Some(Instant::now() + Duration::from_secs(3600)); // no real listing
        let (_dev, tty) = pty();
        let name = tty_of(&tty);
        let mut free = fake_port(&name, "PTY0001", None);
        free.path = tty.display().to_string();
        app.serial.ports = vec![free, fake_port("ttyUSB9", "HELD0001", Some(1))];
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        let draw = |app: &mut App, t: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
            t.draw(|f| crate::ui::draw(app, f)).unwrap();
            crate::api::screen_text(t.backend().buffer())
        };
        let text = draw(&mut app, &mut t);
        assert!(text.contains("PORTS 2") && text.contains("CHANNELS ─") && text.contains("Monitor"), "none: no dots {text}");
        assert_eq!(app.serial.hits.iter().filter(|(_, h)| matches!(h, Hit::Monitor(_))).count(), 1, "the one held by a program that can't share: no Monitor");
        // Monitor: open, read only, no window asked.
        let m = app.serial.hits.iter().find(|(_, h)| *h == Hit::Monitor(0)).unwrap().0;
        assert!(click(&mut app, Position::new(m.x + 1, m.y)));
        assert!(app.prompt.is_none() && app.serial.channels.len() == 1 && !app.serial.channels[0].write);
        // Attach on its channel block: writing, its own port asking nothing.
        let text = draw(&mut app, &mut t);
        assert!(text.contains("CHANNELS ● ") && text.contains("Attach"), "one: one dot {text}");
        let a = app.serial.hits.iter().find(|(_, h)| *h == Hit::Act(0, Act::Write)).unwrap().0;
        assert!(click(&mut app, Position::new(a.x + 1, a.y)));
        assert!(app.prompt.is_none() && app.serial.channels[0].write);
        // Gone (unplugged): Attach says so in a small window.
        app.serial.channels[0].write = false;
        app.serial.channels[0].drop_port("gone".into());
        draw(&mut app, &mut t);
        let a = app.serial.hits.iter().find(|(_, h)| *h == Hit::Act(0, Act::Write)).unwrap().0;
        click(&mut app, Position::new(a.x + 1, a.y));
        assert!(app.prompt.as_ref().is_some_and(|p| p.text[0].contains("not available")));
        app.prompt = None;
        // The line between the panes: dragged down, the ports pane grows.
        let before = app.serial.panes[0].height;
        let sash = app.serial.sash;
        let ev = |kind, y: u16| MouseEvent { kind, column: sash.x + 3, row: y, modifiers: KeyModifiers::NONE };
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), sash.y));
        app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), sash.y + 5));
        app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), sash.y + 5));
        draw(&mut app, &mut t);
        assert_eq!(app.serial.panes[0].height, before + 5);
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn channel_dots_are_one_each_up_to_five_then_counts_by_state() {
        let text = |v: Vec<Span>| v.iter().map(|s| s.content.to_string()).collect::<String>();
        assert_eq!(text(state_dots(&[], "●")), "");
        let few = state_dots(&[Dot::Green, Dot::Yellow, Dot::Red], "●");
        assert_eq!(text(few.clone()), "●●● ");
        assert_eq!(few[2].style.fg, Some(Dot::Red.color()), "each in its state's colour");
        let many = [vec![Dot::Green; 4], vec![Dot::Yellow; 2], vec![Dot::Red]].concat();
        assert_eq!(text(state_dots(&many, "●")), "●4 ●2 ●1 ");
        assert_eq!(text(state_dots(&[Dot::Green; 6], "●")), "●6 ", "states with none left out");
    }

    #[test]
    fn termcodes_api_monitors_attaches_sends_reads_and_waits_on_a_port() {
        use std::io::{BufRead, BufReader};
        let tmp = std::env::temp_dir().join(format!("termcode-xcomm-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("run")).unwrap();
        std::fs::set_permissions(tmp.join("run"), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let (mut dev, tty) = pty();
        let mut app = App::new(tmp.clone(), None);
        app.serial.channels.push(Channel::open(&tty, LineSettings::default(), &env).unwrap());
        let port = app.serial.channels[0].tty.clone();
        let call = |app: &mut App, m: &str, p: serde_json::Value| match crate::rpc::call(app, m, &p) {
            Ok(crate::rpc::Out::Now(v)) => Ok(v),
            Ok(_) => Err("waits".to_string()),
            Err(e) => Err(e),
        };
        // Read only: nothing sent until attached.
        let e = call(&mut app, "serial.send", serde_json::json!({"port": port, "text": "ls\r"})).unwrap_err();
        assert!(e.contains("serial.attach first"), "{e}");
        assert_eq!(call(&mut app, "serial.attach", serde_json::json!({"port": port})).unwrap()["write"], true);
        call(&mut app, "serial.send", serde_json::json!({"port": port, "text": "uname -a\r"})).unwrap();
        let mut got = Vec::new();
        while got.len() < 9 {
            let mut b = [0u8; 32];
            let n = dev.read(&mut b).unwrap();
            got.extend_from_slice(&b[..n]);
        }
        assert_eq!(got, b"uname -a\r");
        assert!(app.serial.channels[0].screen.screen().contents().contains("[api] uname -a"), "shown as the agent's");
        // read: the device's answer, as text.
        dev.write_all(b"\x1b[1mLinux\x1b[0m board 6.8\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        tick(&mut app);
        let r = call(&mut app, "serial.read", serde_json::json!({"port": port})).unwrap();
        assert!(r["text"].as_str().unwrap().contains("Linux board 6.8"), "{r}");
        assert_eq!(call(&mut app, "serial.set", serde_json::json!({"port": port, "baud": 57600})).unwrap()["line"], "57600 8N1");
        // wait, through the API's socket: answered when the device prints a match.
        app.api = crate::api::start(&tmp.join("run"));
        let sock = std::fs::read_dir(tmp.join("run")).unwrap().flatten().find(|e| e.file_name().to_string_lossy().ends_with(".sock")).unwrap().path();
        let mut conn = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        conn.write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"serial.wait\",\"params\":{{\"port\":\"{port}\",\"match\":\"login:\",\"timeout\":10}}}}\n").as_bytes()).unwrap();
        let mut replies = BufReader::new(conn.try_clone().unwrap());
        replies.get_ref().set_nonblocking(true).unwrap();
        let mut line = String::new();
        for k in 0..300 {
            if k == 20 {
                dev.write_all(b"board login: ").unwrap();
            }
            crate::api::tick(&mut app);
            tick(&mut app);
            if replies.read_line(&mut line).is_ok_and(|n| n > 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        assert_eq!(v["result"]["matched"], "login:", "{line}");
        assert_eq!(call(&mut app, "serial.close", serde_json::json!({"port": port})).unwrap()["closed"], true);
        assert!(app.serial.channels.is_empty());
        drop(app);
        std::fs::remove_dir_all(&tmp).unwrap();
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

    fn fake_port(tty: &str, serial: &str, held: Option<i32>) -> PortInfo {
        PortInfo {
            tty: tty.into(),
            path: format!("/dev/{tty}"),
            by_id: Some(format!("/dev/serial/by-id/usb-FTDI_FT230X_{serial}-if00-port0")),
            vid_pid: Some("0403:6015".into()),
            product: Some("FT230X Basic UART".into()),
            serial: Some(serial.into()),
            lock_pid: held,
            lock_error: None,
            herdr_pane: held.map(|_| "w8:p1".into()),
        }
    }

    #[test]
    fn a_ports_note_shows_beside_it_and_its_card_tells_the_rest() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let _ = std::fs::remove_file(notes_file());
        let root = std::env::temp_dir().join(format!("termcode-xcomm-notes-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(root.clone(), None);
        app.show_side(crate::panel::Side::SERIAL, false);
        app.side_w = Some(60); // whole rows
        app.serial.listed = Some(Instant::now() + Duration::from_secs(3600)); // no real listing
        app.serial.ports = vec![fake_port("ttyUSB7", "A1B2C3D4", None), fake_port("ttyUSB8", "E5F6G7H8", Some(1))];
        // `n` on the first port: its note written, kept by the adapter's serial number.
        app.focus = Focus::Side;
        app.serial.sel = 0;
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        for c in "router console".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let saved = std::fs::read_to_string(notes_file()).unwrap();
        assert!(saved.contains("usb:0403:6015:A1B2C3D4") && saved.contains("router console"), "{saved}");
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let text = crate::api::screen_text(t.backend().buffer());
        // Blocks: the tty on the border, the note (else the adapter), USB ID and holder.
        assert!(text.contains("● ttyUSB7") && text.contains("│router console") && text.contains("0403:6015 · free"), "{text}");
        assert!(text.contains("│FT230X Basic UART") && text.contains("0403:6015 · xcomm w8:p1"), "the second's holder: {text}");
        // The pointer resting on the port's block: its card, everything at a glance.
        let block = app.serial.hits.iter().find(|(_, h)| *h == Hit::Port(0)).unwrap().0;
        app.hover = Some(Position::new(block.x + 2, block.y + 1));
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let text = crate::api::screen_text(t.backend().buffer());
        for want in ["Adapter  FT230X Basic UART", "USB      0403:6015  serial A1B2C3D4", "by-id    usb-FTDI_FT230X_A1B2C3D4-if00-port0", "Holder   free"] {
            assert!(text.contains(want), "{want}: {text}");
        }
        // The pointer elsewhere: no card.
        app.hover = None;
        t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        assert!(!crate::api::screen_text(t.backend().buffer()).contains("Adapter"));
        // A note removed with "".
        app.serial.set_note("usb:0403:6015:A1B2C3D4", "").unwrap();
        assert!(!std::fs::read_to_string(notes_file()).unwrap().contains("router"));
        let _ = std::fs::remove_file(notes_file());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_channels_tab_shows_its_status_and_stream_and_typing_reaches_the_device() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let locks = std::env::temp_dir().join(format!("termcode-xcomm-ui-{}", std::process::id()));
        std::fs::create_dir_all(&locks).unwrap();
        let dir = locks.to_string_lossy().into_owned();
        // Locks and control sockets in the test's own folder.
        let env = move |k: &str| matches!(k, "XCOMM_LOCK_DIR" | "XDG_RUNTIME_DIR").then(|| dir.clone());
        let (mut dev, tty) = pty();
        let root = std::env::temp_dir().join(format!("termcode-xcomm-root-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(root.clone(), None);
        let mut ch = Channel::open(&tty, LineSettings::default(), &env).unwrap();
        ch.write = true;
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
        assert!(text.contains("115200 8N1 · write") && text.contains("board login:"), "{text}");
        // A drag over its screen selects; Ctrl+C copies that (not sent), then goes again.
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let gh = app.ui.groups[app.ag].text;
        let row = (0..gh.height).find(|&y| crate::api::screen_text(t.backend().buffer()).lines().nth(usize::from(gh.y + y)).is_some_and(|l| l.contains("board login:"))).unwrap();
        let line = crate::api::screen_text(t.backend().buffer()).lines().nth(usize::from(gh.y + row)).unwrap().to_string();
        let col = line[..line.find("board").unwrap()].chars().count() as u16;
        let ev = |kind, x: u16| MouseEvent { kind, column: x, row: gh.y + row, modifiers: KeyModifiers::NONE };
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), col));
        app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), col + 10));
        app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), col + 10));
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.clip, "board login");
        assert!(app.serial.channels[0].sel.is_none());
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
        let lck = std::fs::read_dir(&locks).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("LCK")).count();
        assert!(app.serial.channels.is_empty() && lck == 0, "{}", app.msg);
        std::fs::remove_dir_all(&locks).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
