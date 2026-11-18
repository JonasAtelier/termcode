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
    /// Gone (unplugged, power-cycled): looked for again every second.
    Waiting(String),
}

/// An open port: its device, its lock, its screen.
pub struct Channel {
    /// The name in /dev it has now (`ttyUSB0`, `pts/3`).
    pub tty: String,
    /// What it is opened again by after an unplug (its /dev/serial/by-path link).
    reopen: PathBuf,
    port: Option<(Port, xcomm::lock::UucpLock)>,
    pub settings: LineSettings,
    pub screen: vt100::Parser,
    pub rx: u64,
    pub tx: u64,
    pub link: Link,
    retry: Instant,
    /// Output copied to a file as it comes.
    pub capture: Option<(PathBuf, std::fs::File)>,
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
fn take(path: &Path, settings: &LineSettings, env: &dyn Fn(&str) -> Option<String>) -> Result<(Port, xcomm::lock::UucpLock), String> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = xcomm::lock::lock_name(&real).ok_or_else(|| format!("{}: not a device", real.display()))?;
    let dir = xcomm::lock::default_lock_dir(env).map_err(|e| e.to_string())?;
    let (lock, _) = xcomm::lock::acquire(&dir, &name).map_err(|e| e.to_string())?;
    let (port, _) = Port::open(&real, settings, true).map_err(|e| e.to_string())?;
    FDS.with(|f| f.borrow_mut().push(port.fd().as_raw_fd()));
    Ok((port, lock))
}

/// `tty` as /dev names it (`pts/3` for /dev/pts/3).
fn tty_of(real: &Path) -> String {
    real.strip_prefix("/dev").map_or_else(|_| real.display().to_string(), |p| p.display().to_string())
}

impl Channel {
    /// `path` opened at `settings` (8N1 at 115200 by default).
    pub fn open(path: &Path, settings: LineSettings, env: &dyn Fn(&str) -> Option<String>) -> Result<Channel, String> {
        let port = take(path, &settings, env)?;
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Channel {
            tty: tty_of(&real),
            reopen: xcomm::reconnect::stable_path(path),
            port: Some(port),
            settings,
            screen: vt100::Parser::new(24, 80, SCROLLBACK),
            rx: 0,
            tx: 0,
            link: Link::Open,
            retry: Instant::now(),
            capture: None,
        })
    }

    fn drop_port(&mut self, why: String) {
        if let Some((p, _)) = self.port.take() {
            let fd = p.fd().as_raw_fd();
            FDS.with(|f| f.borrow_mut().retain(|&x| x != fd));
        }
        self.link = Link::Waiting(why);
        self.retry = Instant::now() + RETRY_EVERY;
    }

    /// What the device sent since: onto the screen, into the capture. True when some came.
    pub fn pump(&mut self) -> bool {
        let mut buf = [0u8; 1 << 16];
        let mut got = false;
        for _ in 0..16 {
            let Some((p, _)) = &self.port else { return got };
            // SAFETY: an open descriptor and a buffer of the length given.
            let n = unsafe { libc::read(p.fd().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
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
                    self.drop_port("the device closed".into());
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

    /// `b` to the device (a second at most, if its buffer is full).
    pub fn send(&mut self, mut b: &[u8]) {
        let Some((p, _)) = &self.port else { return };
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

    /// A lost port looked for again (its time come): true when it is back.
    fn retry(&mut self, env: &dyn Fn(&str) -> Option<String>) -> bool {
        if self.port.is_some() || Instant::now() < self.retry {
            return false;
        }
        match take(&self.reopen, &self.settings, env) {
            Ok(p) => {
                self.tty = tty_of(&std::fs::canonicalize(&self.reopen).unwrap_or_else(|_| self.reopen.clone()));
                self.port = Some(p);
                self.link = Link::Open;
                true
            }
            Err(_) => {
                self.retry = Instant::now() + RETRY_EVERY;
                false
            }
        }
    }

    /// The modem lines (DTR, RTS) as they are, when open.
    pub fn lines(&self) -> Option<xcomm::port::ModemLines> {
        self.port.as_ref().and_then(|(p, _)| p.modem_lines().ok())
    }

    pub fn act(&mut self, a: Act, root: &Path) -> String {
        let bauds = crate::serial::BAUDS;
        let step = |b: u32, up: bool| {
            let i = bauds.iter().position(|&x| x >= b).unwrap_or(bauds.len() - 1);
            if up { bauds[(i + 1).min(bauds.len() - 1)] } else { bauds[i.saturating_sub(1)] }
        };
        let tty = self.tty.clone();
        match (a, &self.port) {
            (Act::BaudDown | Act::BaudUp, _) => {
                let mut s = self.settings;
                s.baud = step(s.baud, a == Act::BaudUp);
                if let Some((p, _)) = &self.port {
                    if let Err(e) = p.configure(&s) {
                        return format!("{tty}: {e}");
                    }
                }
                self.settings = s;
                format!("{tty}: {}", xcomm::port::describe(&s))
            }
            (Act::Dtr | Act::Rts, Some((p, _))) => {
                let Ok(l) = p.modem_lines() else { return format!("{tty}: the modem lines can't be read") };
                let (name, r) = if a == Act::Dtr { ("DTR", p.set_dtr(!l.dtr)) } else { ("RTS", p.set_rts(!l.rts)) };
                let on = if a == Act::Dtr { !l.dtr } else { !l.rts };
                r.map_or_else(|e| format!("{tty}: {name}: {e}"), |()| format!("{tty}: {name} {}", if on { "on" } else { "off" }))
            }
            (Act::Break, Some((p, _))) => p.send_break(250).map_or_else(|e| format!("{tty}: break: {e}"), |()| format!("{tty}: break sent")),
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
fn status(c: &Channel) -> String {
    let link = match &c.link {
        Link::Open => "connected".to_string(),
        Link::Waiting(why) => why.clone(),
    };
    let lines = c.lines().map_or(String::new(), |l| format!(" · DTR {} RTS {}", if l.dtr { "on" } else { "off" }, if l.rts { "on" } else { "off" }));
    let cap = if c.capture.is_some() { " · capturing" } else { "" };
    format!("{} · {link}{lines} · in {} out {}{cap}", xcomm::port::describe(&c.settings), human(c.rx), human(c.tx))
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
    let st = if ch.link == Link::Open { Style::new().fg(P.editor_fg).bg(P.widget_bg) } else { Style::new().fg(P.git_modified).bg(P.widget_bg) };
    f.render_widget(ratatui::widgets::Paragraph::new(format!(" {tty} · {}", status(ch))).style(st), head);
    let body = Rect::new(r.x, r.y + 1, r.width, r.height - 1);
    if ch.screen.screen().size() != (body.height.max(2), body.width.max(2)) {
        ch.screen.screen_mut().set_size(body.height.max(2), body.width.max(2));
    }
    let live = ch.screen.screen().scrollback() == 0;
    crate::term::draw_screen(ch.screen.screen(), None, f.buffer_mut(), body, (P.editor_fg, P.editor_bg, P.selection), focused && live);
}

/// The side bar: PORTS (the mapping: each tty, its adapter, who holds it, its by-id
/// link), then CHANNELS (each open one: its state, in and out, its controls).
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
    let s = &mut app.serial;
    s.hits.clear();
    let dim = Style::new().fg(P.dim);
    let fg = Style::new().fg(P.sidebar_fg);
    let mut y = r.y;
    let line = |f: &mut Frame, y: &mut u16, text: Line| {
        if *y < r.bottom() {
            f.render_widget(text, Rect::new(r.x + 1, *y, r.width.saturating_sub(2), 1));
        }
        *y += 1;
    };
    let total = s.ports.len() + s.channels.len();
    s.sel = s.sel.min(total.saturating_sub(1));
    let focused = app.focus == Focus::Side;
    let hover = app.hover;
    let mut btn_hits = Vec::new();
    let sel_bg = |row: usize| if focused && row == s.sel { P.list_active_bg } else { P.sidebar_bg };
    line(f, &mut y, Line::styled(format!("PORTS ({})", s.ports.len()), dim.bold()));
    if let Some(p) = &s.problem {
        line(f, &mut y, Line::styled(p.clone(), Style::new().fg(P.error_fg)));
    } else if s.ports.is_empty() {
        line(f, &mut y, Line::styled("No serial ports. Plug in an adapter.", dim));
    }
    let me = std::process::id() as i32;
    for (i, p) in s.ports.iter().enumerate() {
        let top = y;
        let held = match (p.lock_pid, &p.herdr_pane) {
            (Some(pid), _) if pid == me => "open here".to_string(),
            (Some(pid), Some(pane)) => format!("xcomm in herdr {pane} (pid {pid})"),
            (Some(pid), None) => format!("held by pid {pid}"),
            (None, _) => "free".to_string(),
        };
        let name = Span::styled(format!("{:<9}", p.tty), fg.bold());
        let what = Span::styled(format!(" {}", p.product.clone().unwrap_or_default()), fg);
        line(f, &mut y, Line::from(vec![name, what]).style(Style::new().bg(sel_bg(i))));
        let id = p.vid_pid.clone().unwrap_or_default();
        let by = p.by_id.as_deref().and_then(|b| b.rsplit('/').next()).unwrap_or("");
        line(f, &mut y, Line::styled(format!("  {id} · {held}  {by}"), dim));
        s.hits.push((Rect::new(r.x, top, r.width, 2), Hit::Port(i)));
    }
    y += 1;
    line(f, &mut y, Line::styled(format!("CHANNELS ({})", s.channels.len()), dim.bold()));
    if s.channels.is_empty() {
        line(f, &mut y, Line::styled("None open: click a port (or Enter) to open it.", dim));
    }
    for (i, c) in s.channels.iter().enumerate() {
        let top = y;
        let dot = if c.link == Link::Open { Span::styled("● ", Style::new().fg(P.git_added)) } else { Span::styled("○ ", Style::new().fg(P.git_modified)) };
        line(f, &mut y, Line::from(vec![dot, Span::styled(c.tty.clone(), fg.bold())]).style(Style::new().bg(sel_bg(s.ports.len() + i))));
        line(f, &mut y, Line::styled(format!("  {}", status(c)), dim));
        s.hits.push((Rect::new(r.x, top, r.width, 2), Hit::Channel(i)));
        // The controls: the baud's ‹ ›, the modem lines, a break, capture, close.
        if y < r.bottom() {
            let mut x = r.x + 2;
            let lines = c.lines();
            let on = |b: Option<bool>| if b == Some(true) { "●" } else { "○" };
            let btns = [
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
                btn_hits.push((b, Hit::Act(i, a)));
                x += w + 1;
            }
            y += 2;
        }
    }
    s.hits.extend(btn_hits);
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
            if let Some(path) = app.serial.ports.get(i).map(|p| PathBuf::from(&p.path)) {
                open(app, &path);
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
        assert!(text.contains("115200 8N1 · connected") && text.contains("board login:"), "{text}");
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
