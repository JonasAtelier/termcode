//! termcode's Serial view, out of tree: a termcode extension (doc/extensions.md) on the
//! xcomm crate. termcode draws; this fills its slots:
//!
//!   left    PORTS (each with its state dot, adapter, note, Monitor) and CHANNELS
//!   main    a tab per channel: its line, then its screen
//!   right   the channel in front's settings, or the port picked's details
//!
//! and takes what the user does there: clicks, keys and pastes (to the device), the
//! tab in front, its size, its closing. It serves termcode's `serial.*` API methods for
//! other clients (agents), and publishes `serial.output`. JSON-RPC on stdin and stdout,
//! a message a line.

mod channel;

use channel::{env, session_socket, status, Act, Channel, Link};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use xcomm::cli::LineSettings;
use xcomm::list::PortInfo;

/// Ports listed this often while the view shows.
const LIST_EVERY: Duration = Duration::from_secs(2);
/// A channel's screen sent at most this often (output coalesced in between).
const FRAME: Duration = Duration::from_millis(33);

/// What a reply answers.
enum Asked {
    /// `ui.prompt` before typing into a shared port: Attach, or not.
    Attach(String),
    /// `ui.input` for a port's note (its id).
    Note(String),
    /// `ui.pick` of a port to open (the ttys offered).
    Open(Vec<String>),
    /// `serial.attach` from an API client (its call's id) on a shared port: the user's
    /// answer is the call's.
    ApiAttach(Value, String),
}

/// A `serial.wait` waiting: its call, the channel, what to match from where, until when.
struct ApiWait {
    id: Value,
    tty: String,
    re: regex::Regex,
    mark: u64,
    until: Instant,
}

struct View {
    next: u64,
    asked: HashMap<u64, Asked>,
    visible: bool,
    ports: Vec<PortInfo>,
    listed: Option<Instant>,
    channels: Vec<Channel>,
    /// The port picked in the side bar; the channel tab in front.
    picked: Option<String>,
    front: Option<String>,
    /// Channels whose screen changed since sent; when each was last sent.
    dirty: Vec<String>,
    sent: HashMap<String, Instant>,
    side: bool,
    right: bool,
    notes: BTreeMap<String, String>,
    waits: Vec<ApiWait>,
    /// Each channel's output published so far (`serial.output`), by offset.
    emitted: HashMap<String, u64>,
}

fn main() {
    let rx = stdin_lines();
    let mut v = View { next: 0, asked: HashMap::new(), visible: false, ports: Vec::new(), listed: None, channels: Vec::new(), picked: None, front: None, dirty: Vec::new(), sent: HashMap::new(), side: true, right: true, notes: load_notes(), waits: Vec::new(), emitted: HashMap::new() };
    loop {
        let wait = if v.channels.is_empty() { Duration::from_millis(500) } else { Duration::from_millis(10) };
        match rx.recv_timeout(wait) {
            Ok(msg) => {
                v.message(&msg);
                while let Ok(msg) = rx.try_recv() {
                    v.message(&msg);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return, // termcode closed our stdin: end
        }
        v.work();
        v.draw();
    }
}

/// termcode's messages, a JSON value a line, from a thread.
fn stdin_lines() -> Receiver<Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

impl View {
    /// A request to termcode; its reply comes later (`message`).
    fn call(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        let line = json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}").and_then(|()| out.flush());
        self.next
    }

    fn notify(&mut self, text: String) {
        self.call("ui.notify", json!({"text": text}));
    }

    /// The answer to an API call termcode passed on.
    fn reply(&mut self, id: &Value, r: Result<Value, String>) {
        let msg = match r {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e}}),
        };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{msg}").and_then(|()| out.flush());
    }

    fn message(&mut self, m: &Value) {
        if m["method"] == "api.call" {
            let id = m["id"].clone();
            let p = &m["params"];
            if let Some(r) = self.api(&id, p["method"].as_str().unwrap_or(""), &p["params"]) {
                self.reply(&id, r);
            }
            return;
        }
        if let Some(id) = m["id"].as_u64() {
            if let Some(e) = m.get("error") {
                eprintln!("termcode refused request {id}: {e}");
            }
            if let Some(a) = self.asked.remove(&id) {
                self.answered(a, &m["result"]);
            }
            return;
        }
        let p = &m["params"];
        let tab = p["tab"].as_str().unwrap_or("").to_string();
        match m["method"].as_str().unwrap_or("") {
            "view.shown" => {
                self.visible = p["visible"].as_bool().unwrap_or(false);
                self.listed = None;
            }
            "view.action" => self.action(p["action"].as_str().unwrap_or("")),
            "view.key" => {
                let bytes = key_bytes(p["key"].as_str().unwrap_or(""), p["ctrl"] == true, p["alt"] == true, p["shift"] == true);
                if let (Some(b), Some(c)) = (bytes, self.channels.iter_mut().find(|c| c.tty == tab)) {
                    // Typing goes back to the bottom, as in a terminal.
                    c.screen.screen_mut().set_scrollback(0);
                    c.send(&b);
                    self.dirty.push(tab);
                }
            }
            "view.paste" => {
                if let Some(c) = self.channels.iter_mut().find(|c| c.tty == tab) {
                    let text = p["text"].as_str().unwrap_or("").replace("\r\n", "\r").replace('\n', "\r");
                    c.send(text.as_bytes());
                }
            }
            "view.resize" => {
                let (w, h) = (p["width"].as_u64().unwrap_or(80) as u16, p["height"].as_u64().unwrap_or(25) as u16);
                if let Some(c) = self.channels.iter_mut().find(|c| c.tty == tab) {
                    // The first row is the channel's line.
                    c.screen.screen_mut().set_size(h.saturating_sub(1).max(1), w.max(1));
                    self.dirty.push(tab);
                }
            }
            // The wheel over a channel's tab: through its scrollback.
            "view.wheel" => {
                if let Some(c) = self.channels.iter_mut().find(|c| c.tty == tab) {
                    let now = c.screen.screen().scrollback() as i64;
                    let to = (now - p["lines"].as_i64().unwrap_or(0)).max(0) as usize;
                    c.screen.screen_mut().set_scrollback(to);
                    self.dirty.push(tab);
                }
            }
            "view.tab" => {
                self.front = Some(tab);
                self.right = true;
            }
            "view.closed" => self.close(&tab),
            "view.command" => match p["id"].as_str().unwrap_or("") {
                "open" => {
                    let ttys: Vec<String> = self.ports.iter().map(|p| p.tty.clone()).collect();
                    let items: Vec<Value> = self.ports.iter().map(|p| json!({"label": p.tty, "detail": p.product.clone().unwrap_or_default()})).collect();
                    if items.is_empty() {
                        self.notify("No serial ports".into());
                    } else {
                        let id = self.call("ui.pick", json!({"text": "Open a port", "items": items}));
                        self.asked.insert(id, Asked::Open(ttys));
                    }
                }
                "close-all" => {
                    for tty in self.channels.iter().map(|c| c.tty.clone()).collect::<Vec<_>>() {
                        self.close(&tty);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn answered(&mut self, a: Asked, r: &Value) {
        match a {
            Asked::Attach(tty) if r["button"] == "Attach" => self.act(&tty, Act::Write),
            Asked::Attach(_) => {}
            Asked::Note(id) => {
                if let Some(text) = r["text"].as_str() {
                    let text: String = text.trim().chars().take(80).collect();
                    if text.is_empty() {
                        self.notes.remove(&id);
                    } else {
                        self.notes.insert(id, text);
                    }
                    save_notes(&self.notes);
                    self.side = true;
                    self.right = true;
                }
            }
            Asked::ApiAttach(id, tty) => {
                let r = if r["button"] == "Attach" {
                    self.act(&tty, Act::Write);
                    Ok(json!({"port": tty, "write": true}))
                } else {
                    Err(format!("serial.attach {tty}: the user said no"))
                };
                self.reply(&id, r);
            }
            Asked::Open(ttys) => {
                if let Some(tty) = r["index"].as_u64().and_then(|i| ttys.get(i as usize)).cloned() {
                    self.monitor(&tty);
                }
            }
        }
    }

    fn action(&mut self, a: &str) {
        let (verb, arg) = a.split_once(':').unwrap_or((a, ""));
        let arg = arg.to_string();
        match verb {
            "pick" => {
                self.picked = Some(arg);
                self.right = true;
                self.side = true;
            }
            "monitor" => self.monitor(&arg),
            "show" => {
                self.call("view.open", json!({"tab": arg}));
            }
            "note" => {
                let Some(p) = self.ports.iter().find(|p| p.tty == arg) else { return };
                let id = port_id(p);
                let value = self.notes.get(&id).cloned().unwrap_or_default();
                let n = self.call("ui.input", json!({"text": format!("Note beside {arg}"), "value": value}));
                self.asked.insert(n, Asked::Note(id));
            }
            "attach" => {
                let Some(c) = self.channels.iter().find(|c| c.tty == arg) else { return };
                match (&c.shared, c.write) {
                    (Some((_, who)), false) => {
                        let text = format!("{arg} is held by {who}: what you type goes through that session.");
                        let n = self.call("ui.prompt", json!({"text": text, "buttons": ["Attach", "Cancel"]}));
                        self.asked.insert(n, Asked::Attach(arg));
                    }
                    _ => self.act(&arg, Act::Write),
                }
            }
            "close" => self.close(&arg),
            "act" => {
                let Some(tty) = self.front.clone().filter(|t| self.channels.iter().any(|c| &c.tty == t)) else { return };
                let a = match arg.as_str() {
                    "baud-" => Act::BaudDown,
                    "baud+" => Act::BaudUp,
                    "bits" => Act::DataBits,
                    "parity" => Act::Parity,
                    "stop" => Act::StopBits,
                    "flow" => Act::Flow,
                    "dtr" => Act::Dtr,
                    "rts" => Act::Rts,
                    "break" => Act::Break,
                    "capture" => Act::Capture,
                    "agents" => Act::Agents,
                    _ => return,
                };
                self.act(&tty, a);
            }
            _ => {}
        }
    }

    fn act(&mut self, tty: &str, a: Act) {
        let root = std::env::var_os("TERMCODE_WORKSPACE").map_or_else(|| PathBuf::from("."), PathBuf::from);
        let Some(c) = self.channels.iter_mut().find(|c| c.tty == tty) else { return };
        let said = c.act(a, &root);
        self.side = true;
        self.right = true;
        self.dirty.push(tty.to_string());
        self.notify(said);
    }

    /// `tty` opened here (free), shared with its xcomm session (held by one), or not.
    fn monitor(&mut self, tty: &str) {
        if let Err(e) = self.open(tty) {
            self.notify(format!("{tty}: {e}"));
        }
    }

    fn open(&mut self, tty: &str) -> Result<(), String> {
        if !self.channels.iter().any(|c| c.tty == tty) {
            let Some(p) = self.ports.iter().find(|p| p.tty == tty).cloned() else { return Err("no such port".into()) };
            let me = std::process::id() as i32;
            let opened = match p.lock_pid.filter(|&pid| pid != me) {
                None => Channel::open(Path::new(&p.path), LineSettings::default(), &env),
                Some(pid) => match session_socket(tty, &env) {
                    Some(sock) => Channel::share(tty, &sock, p.herdr_pane.as_ref().map_or_else(|| format!("xcomm (pid {pid})"), |pane| format!("xcomm in herdr {pane}")), false),
                    None => Err(format!("held by {} (pid {pid}), which can't share it", comm(pid))),
                },
            };
            self.channels.push(opened?);
        }
        self.front = Some(tty.to_string());
        self.dirty.push(tty.to_string());
        self.draw();
        self.call("view.open", json!({"tab": tty}));
        self.side = true;
        Ok(())
    }

    fn list_ports(&mut self) {
        let mut ports = xcomm::list::enumerate(&env).unwrap_or_default();
        ports.extend(extra_ports());
        if ports.iter().map(|p| (&p.tty, p.lock_pid)).ne(self.ports.iter().map(|p| (&p.tty, p.lock_pid))) {
            self.ports = ports;
            self.side = true;
            self.right = true;
        }
    }

    /// An API call (termcode's `serial.*`): its answer, or None while it waits (a match,
    /// the user's answer).
    fn api(&mut self, id: &Value, method: &str, q: &Value) -> Option<Result<Value, String>> {
        let port = q["port"].as_str().map(|s| s.trim_start_matches("/dev/").to_string());
        let need = || format!("{method}: give \"port\" (ttyUSB0)");
        let not_open = |t: &str| format!("{t} is not open: serial.monitor first");
        Some(match method {
            "serial.ports" => {
                self.list_ports();
                Ok(self.ports_json())
            }
            "serial.note" => (|| {
                let tty = port.ok_or_else(need)?;
                self.list_ports();
                let p = self.ports.iter().find(|p| p.tty == tty).ok_or_else(|| format!("no port {tty}"))?;
                let pid = port_id(p);
                let note: String = q["note"].as_str().unwrap_or("").trim().chars().take(80).collect();
                if note.is_empty() {
                    self.notes.remove(&pid);
                } else {
                    self.notes.insert(pid.clone(), note);
                }
                save_notes(&self.notes);
                self.side = true;
                Ok(json!({"port": tty, "id": pid}))
            })(),
            "serial.monitor" => (|| {
                let tty = port.ok_or_else(need)?;
                self.list_ports();
                self.open(&tty)?;
                let c = self.channels.iter().find(|c| c.tty == tty).ok_or("gone")?;
                Ok(json!({"port": tty, "write": c.write, "shared": c.shared.is_some(), "status": status(c)}))
            })(),
            "serial.read" => (|| {
                let tty = port.ok_or_else(need)?;
                let c = self.channels.iter().find(|c| c.tty == tty).ok_or_else(|| not_open(&tty))?;
                let since = q["since"].as_u64().unwrap_or_else(|| c.output_end().saturating_sub(4096));
                let (text, next) = c.output_since(since, 1 << 20);
                Ok(json!({"port": tty, "text": text, "next": next}))
            })(),
            "serial.wait" => {
                let r = (|| {
                    let tty = port.ok_or_else(need)?;
                    let pat = q["match"].as_str().ok_or("serial.wait: give \"match\", a regex")?;
                    if pat.len() > 1000 {
                        return Err("match: 1000 characters at most".to_string());
                    }
                    let re = regex::RegexBuilder::new(pat).size_limit(1 << 20).build().map_err(|e| format!("match: {e}"))?;
                    let c = self.channels.iter().find(|c| c.tty == tty).ok_or_else(|| not_open(&tty))?;
                    let mark = q["since"].as_u64().unwrap_or_else(|| c.output_end());
                    let secs = q["timeout"].as_f64().unwrap_or(30.0).clamp(0.0, 3600.0);
                    Ok(ApiWait { id: id.clone(), tty, re, mark, until: Instant::now() + Duration::from_secs_f64(secs) })
                })();
                match r {
                    Ok(w) => {
                        self.waits.push(w);
                        return None;
                    }
                    Err(e) => Err(e),
                }
            }
            "serial.attach" => {
                let Some(tty) = port else { return Some(Err(need())) };
                let Some(c) = self.channels.iter().find(|c| c.tty == tty) else { return Some(Err(not_open(&tty))) };
                if c.write {
                    Ok(json!({"port": tty, "write": true}))
                } else if c.link != Link::Open {
                    Err(format!("{tty} is not available (unplugged?)"))
                } else if let Some((_, who)) = &c.shared {
                    // Someone else's port: the user says, in termcode.
                    let text = format!("An agent asks to type into {tty}, held by {who}.");
                    let n = self.call("ui.prompt", json!({"text": text, "buttons": ["Attach", "Cancel"]}));
                    self.asked.insert(n, Asked::ApiAttach(id.clone(), tty));
                    return None;
                } else {
                    self.act(&tty, Act::Write);
                    Ok(json!({"port": tty, "write": true}))
                }
            }
            "serial.send" => (|| {
                let tty = port.ok_or_else(need)?;
                let text = q["text"].as_str().ok_or("serial.send: give \"text\" (end it with \\r to enter it)")?;
                let c = self.channels.iter_mut().find(|c| c.tty == tty).ok_or_else(|| not_open(&tty))?;
                if !c.write {
                    return Err(format!("{tty} is read only: serial.attach first"));
                }
                let mark = c.output_end();
                c.send(text.as_bytes());
                // Shown as an agent's, as xcomm shows a control client's sends.
                let shown: String = text.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect();
                c.screen.process(format!("\x1b[2m[api] {}\x1b[0m\r\n", shown.trim_end()).as_bytes());
                self.dirty.push(tty.clone());
                Ok(json!({"port": tty, "sent": text.len(), "mark": mark}))
            })(),
            "serial.set" => (|| {
                let tty = port.ok_or_else(need)?;
                let c = self.channels.iter_mut().find(|c| c.tty == tty).ok_or_else(|| not_open(&tty))?;
                let line = c.set_line(q)?;
                self.right = true;
                Ok(json!({"port": tty, "line": line}))
            })(),
            "serial.close" => (|| {
                let tty = port.ok_or_else(need)?;
                if !self.channels.iter().any(|c| c.tty == tty) {
                    return Err(format!("{tty} is not open"));
                }
                self.close(&tty);
                Ok(json!({"port": tty, "closed": true}))
            })(),
            m => Err(format!("unknown method {m:?}")),
        })
    }

    /// The ports for the API: each with its note, its state (as its dot), who holds it,
    /// and the channel open on it here.
    fn ports_json(&self) -> Value {
        let me = std::process::id() as i32;
        let list: Vec<Value> = self
            .ports
            .iter()
            .map(|p| {
                let ch = self.channels.iter().find(|c| c.tty == p.tty);
                let shareable = p.lock_pid.is_some_and(|pid| pid != me) && session_socket(&p.tty, &env).is_some();
                let state = match (ch, p.lock_pid) {
                    (Some(c), _) if c.link != Link::Open => "red",
                    (Some(c), _) if !c.write => "yellow",
                    (Some(_), _) | (None, None) => "green",
                    (None, Some(pid)) if pid == me => "green",
                    (None, _) if shareable => "yellow",
                    (None, _) => "red",
                };
                json!({
                    "tty": p.tty,
                    "note": self.notes.get(&port_id(p)),
                    "state": state,
                    "adapter": p.product,
                    "usb": p.vid_pid,
                    "serial": p.serial,
                    "by_id": p.by_id,
                    "holder": p.lock_pid.map(|pid| json!({"pid": pid, "here": pid == me, "herdr_pane": p.herdr_pane, "xcomm": shareable})),
                    "channel": ch.map(|c| json!({"line": xcomm::port::describe(&c.settings), "write": c.write, "in": c.rx, "out": c.tx, "connected": c.link == Link::Open})),
                })
            })
            .collect();
        Value::Array(list)
    }

    /// `serial.wait`s matched, or out of time: answered.
    fn answer_waits(&mut self) {
        let now = Instant::now();
        let mut done = Vec::new();
        self.waits.retain(|w| {
            let Some(c) = self.channels.iter().find(|c| c.tty == w.tty) else {
                done.push((w.id.clone(), Err(format!("{} was closed", w.tty))));
                return false;
            };
            let (text, next) = c.output_since(w.mark, 1 << 20);
            if let Some(m) = w.re.find(&text) {
                let tail: String = text.chars().rev().take(4096).collect::<Vec<_>>().into_iter().rev().collect();
                done.push((w.id.clone(), Ok(json!({"matched": m.as_str(), "output": tail, "next": next}))));
                return false;
            }
            if now >= w.until {
                done.push((w.id.clone(), Err("timed out".into())));
                return false;
            }
            true
        });
        for (id, r) in done {
            self.reply(&id, r);
        }
    }

    fn close(&mut self, tty: &str) {
        let before = self.channels.len();
        self.channels.retain(|c| c.tty != tty);
        // A channel opened again starts its output from 0: nothing of this one's kept.
        self.emitted.remove(tty);
        self.sent.remove(tty);
        if self.channels.len() != before {
            self.call("view.clear", json!({"slot": "main", "tab": tty}));
            self.side = true;
            self.right = true;
        }
    }

    /// Ports listed (while shown), lost ports looked for, output read.
    fn work(&mut self) {
        if self.visible && self.listed.is_none_or(|t| t.elapsed() >= LIST_EVERY) {
            self.listed = Some(Instant::now());
            self.list_ports();
        }
        let mut polled = Vec::new();
        for c in &mut self.channels {
            let back = c.retry(&env);
            let got = c.pump();
            let ctl = c.ctl_pass(&mut polled);
            if back || got || ctl {
                self.dirty.push(c.tty.clone());
                self.side |= back;
            }
        }
        if !self.waits.is_empty() {
            self.answer_waits();
        }
    }

    /// What changed, to termcode: the side bar, the right panel, each dirty channel's tab
    /// (at most every FRAME).
    fn draw(&mut self) {
        if std::mem::take(&mut self.side) {
            let (ports, chans) = (self.ports_rows(), self.channel_rows());
            self.call("view.set", json!({"slot": "left", "section": "ports", "title": "Ports", "rows": ports}));
            self.call("view.set", json!({"slot": "left", "section": "channels", "title": "Channels", "rows": chans}));
        }
        if std::mem::take(&mut self.right) {
            let (title, rows) = self.right_rows();
            self.call("view.set", json!({"slot": "right", "title": title, "rows": rows}));
        }
        self.dirty.sort();
        self.dirty.dedup();
        let now = Instant::now();
        let due: Vec<String> = self.dirty.iter().filter(|t| self.sent.get(*t).is_none_or(|s| now.duration_since(*s) >= FRAME)).cloned().collect();
        for tty in due {
            self.dirty.retain(|t| *t != tty);
            let Some(c) = self.channels.iter().find(|c| c.tty == tty) else { continue };
            let back = c.screen.screen().scrollback();
            let line = if back > 0 { format!("{} · ↑ {back} lines back (type to return)", status(c)) } else { status(c) };
            let mut rows = vec![json!({"text": line, "dim": true})];
            rows.extend(screen_rows(c, back == 0 && c.write && self.front.as_deref() == Some(&tty)));
            // What came since last time, to whoever watches serial.output.
            let from = *self.emitted.get(&tty).unwrap_or(&c.output_end());
            let (text, next) = c.output_since(from, 64 << 10);
            self.emitted.insert(tty.clone(), next);
            self.sent.insert(tty.clone(), now);
            self.call("view.set", json!({"slot": "main", "tab": tty, "title": tty, "rows": rows, "scroll": "self"}));
            if !text.is_empty() {
                self.call("events.emit", json!({"topic": "serial.output", "port": tty, "text": text}));
            }
        }
    }

    fn ports_rows(&self) -> Vec<Value> {
        if self.ports.is_empty() {
            return vec![json!({"text": "No serial ports", "dim": true})];
        }
        let me = std::process::id() as i32;
        let mut rows = Vec::new();
        for p in &self.ports {
            let dot = match p.lock_pid {
                None => "ok",
                Some(pid) if pid == me => "ok",
                Some(_) if session_socket(&p.tty, &env).is_some() => "warning",
                Some(_) => "error",
            };
            let picked = self.picked.as_deref() == Some(&p.tty);
            let what = p.product.clone().or_else(|| p.vid_pid.clone()).unwrap_or_default();
            rows.push(json!({"cells": [{"text": "● ", "fg": dot}, {"text": p.tty, "bold": true, "fg": if picked { "accent" } else { "fg" }}, {"text": format!("  {what}"), "dim": true}], "action": format!("pick:{}", p.tty), "hint": self.card(p)}));
            let mut line = vec![json!("  "), json!({"text": "Monitor", "button": true, "action": format!("monitor:{}", p.tty)}), json!(" ")];
            match self.notes.get(&port_id(p)) {
                Some(n) => line.push(json!({"text": n, "fg": "info", "action": format!("note:{}", p.tty)})),
                None => line.push(json!({"text": "✎ note", "dim": true, "action": format!("note:{}", p.tty)})),
            }
            rows.push(Value::Array(line));
        }
        rows
    }

    /// A port's card on hover: what it is, who holds it, its note.
    fn card(&self, p: &PortInfo) -> String {
        let mut l = vec![format!("{}  {}", p.tty, p.path)];
        let mut add = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                l.push(format!("{k:<8} {v}"));
            }
        };
        add("Adapter", p.product.clone());
        add("USB ID", p.vid_pid.clone());
        add("Serial", p.serial.clone());
        add("By id", p.by_id.clone());
        add("Held by", p.lock_pid.map(|pid| format!("{} (pid {pid})", comm(pid))));
        add("Note", self.notes.get(&port_id(p)).cloned());
        l.join("\n")
    }

    fn channel_rows(&self) -> Vec<Value> {
        if self.channels.is_empty() {
            return vec![json!({"text": "Monitor a port to open one", "dim": true})];
        }
        let mut rows = Vec::new();
        for c in &self.channels {
            let (dot, mode) = match (&c.link, c.write) {
                (Link::Waiting(_), _) => ("error", "disconnected"),
                (_, false) => ("warning", "read only"),
                _ => ("ok", "read & write"),
            };
            rows.push(json!({"cells": [{"text": "● ", "fg": dot}, {"text": c.tty, "bold": true}, {"text": format!("  {mode}"), "dim": true}], "action": format!("show:{}", c.tty)}));
            let attach = if c.write { "Read only" } else { "Attach" };
            rows.push(json!(["  ", {"text": attach, "button": true, "action": format!("attach:{}", c.tty)}, " ", {"text": "Close", "button": true, "action": format!("close:{}", c.tty)}]));
        }
        rows
    }

    /// The channel in front's settings; else the picked port's details.
    fn right_rows(&self) -> (String, Vec<Value>) {
        let b = |t: &str, a: &str| json!({"text": t, "button": true, "action": format!("act:{a}")});
        if let Some(c) = self.front.as_ref().and_then(|t| self.channels.iter().find(|c| &c.tty == t)) {
            let s = &c.settings;
            let lines = c.lines();
            let on = |x: Option<bool>| match x {
                Some(true) => "on",
                Some(false) => "off",
                None => "–",
            };
            let rows = vec![
                json!([b("−", "baud-"), format!(" {} baud ", s.baud), b("+", "baud+")]),
                json!(""),
                json!([b(&format!("{} data bits", data_bits(s)), "bits"), " ", b(&format!("parity {}", format!("{:?}", s.parity).to_lowercase()), "parity")]),
                json!([b(&format!("{} stop", if format!("{:?}", s.stop_bits) == "Two" { 2 } else { 1 }), "stop"), " ", b(&format!("flow {}", channel::flow_name(s.flow)), "flow")]),
                json!(""),
                json!([b(&format!("DTR {}", on(lines.map(|l| l.dtr))), "dtr"), " ", b(&format!("RTS {}", on(lines.map(|l| l.rts))), "rts"), " ", b("Break", "break")]),
                json!([b(if c.capture.is_some() { "Stop capture" } else { "Capture" }, "capture")]),
                json!([b(&format!("Agents: {}", channel::agents_name(c.agents)), "agents")]),
                json!(""),
                json!({"text": status(c), "dim": true}),
            ];
            return (format!("{} settings", c.tty), rows);
        }
        let Some(p) = self.picked.as_ref().and_then(|t| self.ports.iter().find(|p| &p.tty == t)) else {
            return ("Serial".into(), vec![json!({"text": "Pick a port, or Monitor one", "dim": true})]);
        };
        let field = |k: &str, v: Option<&String>| json!([{"text": format!("{k:<8}"), "dim": true}, v.cloned().unwrap_or_else(|| "–".into())]);
        let holder = p.lock_pid.map(|pid| format!("{} (pid {pid})", comm(pid)));
        let rows = vec![
            field("Adapter", p.product.as_ref()),
            field("USB ID", p.vid_pid.as_ref()),
            field("Serial", p.serial.as_ref()),
            field("By id", p.by_id.as_ref()),
            field("Held by", holder.as_ref()),
            field("Note", self.notes.get(&port_id(p))),
            json!(""),
            json!([{"text": "Monitor", "button": true, "action": format!("monitor:{}", p.tty)}, " ", {"text": "Note…", "button": true, "action": format!("note:{}", p.tty)}]),
        ];
        (p.tty.clone(), rows)
    }
}

fn data_bits(s: &LineSettings) -> u8 {
    match format!("{:?}", s.data_bits).as_str() {
        "Five" => 5,
        "Six" => 6,
        "Seven" => 7,
        _ => 8,
    }
}

fn comm(pid: i32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).map_or_else(|_| "another program".into(), |c| c.trim().to_string())
}

/// A port's identity for its note: its USB ID and serial number, else its by-path link,
/// else its name (as termcode's built-in Serial view keeps them).
fn port_id(p: &PortInfo) -> String {
    match (&p.vid_pid, &p.serial) {
        (Some(v), Some(sn)) => format!("usb:{v}:{sn}"),
        _ => {
            let stable = xcomm::reconnect::stable_path(Path::new(&p.path));
            if stable.starts_with("/dev/serial/by-path") { format!("path:{}", stable.display()) } else { format!("tty:{}", p.tty) }
        }
    }
}

/// The notes file termcode's built-in view keeps too.
fn notes_file() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")));
    base.unwrap_or_else(std::env::temp_dir).join("termcode/serial-notes.json")
}

fn load_notes() -> BTreeMap<String, String> {
    std::fs::read(notes_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_notes(n: &BTreeMap<String, String>) {
    let f = notes_file();
    let tmp = f.with_extension("json.tmp");
    if let Ok(j) = serde_json::to_vec_pretty(n) {
        let _ = std::fs::write(&tmp, j).and_then(|()| std::fs::rename(&tmp, &f));
    }
}

/// Devices listed besides the usual ones ($TERMCODE_SERIAL_EXTRA, colon-separated: a
/// pseudo-terminal to try it on).
fn extra_ports() -> Vec<PortInfo> {
    let paths = std::env::var("TERMCODE_SERIAL_EXTRA").unwrap_or_default();
    paths
        .split(':')
        .filter(|p| !p.is_empty())
        .filter_map(|p| {
            let real = std::fs::canonicalize(p).ok()?;
            let tty = real.strip_prefix("/dev").map_or_else(|_| real.display().to_string(), |r| r.display().to_string());
            let lock_pid = xcomm::lock::lock_name(&real).and_then(|name| {
                let dir = xcomm::lock::default_lock_dir(&env).ok()?;
                let pid: i32 = std::fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()?;
                Path::new(&format!("/proc/{pid}")).exists().then_some(pid)
            });
            Some(PortInfo { tty, path: real.display().to_string(), by_id: None, vid_pid: None, product: Some("pseudo-terminal".into()), serial: None, lock_pid, lock_error: None, herdr_pane: None })
        })
        .collect()
}

/// What a key sends to a device, as a terminal sends it.
fn key_bytes(key: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Vec<u8>> {
    let seq = |s: &str| s.as_bytes().to_vec();
    let mut b = match key {
        "Enter" => seq("\r"),
        "Backspace" => vec![0x7f],
        "Tab" if shift => seq("\x1b[Z"),
        "Tab" => seq("\t"),
        "Esc" => vec![0x1b],
        "Up" => seq("\x1b[A"),
        "Down" => seq("\x1b[B"),
        "Right" => seq("\x1b[C"),
        "Left" => seq("\x1b[D"),
        "Home" => seq("\x1b[H"),
        "End" => seq("\x1b[F"),
        "Insert" => seq("\x1b[2~"),
        "Delete" => seq("\x1b[3~"),
        "PageUp" => seq("\x1b[5~"),
        "PageDown" => seq("\x1b[6~"),
        "F1" => seq("\x1bOP"),
        "F2" => seq("\x1bOQ"),
        "F3" => seq("\x1bOR"),
        "F4" => seq("\x1bOS"),
        f if f.starts_with('F') && f.len() <= 3 => {
            let n: u8 = f[1..].parse().ok()?;
            let code = [15, 17, 18, 19, 20, 21, 23, 24].get(usize::from(n.checked_sub(5)?))?;
            seq(&format!("\x1b[{code}~"))
        }
        k if k.chars().count() == 1 => {
            let c = k.chars().next()?;
            match c {
                c if ctrl && c.is_ascii_alphabetic() => vec![c.to_ascii_lowercase() as u8 - b'a' + 1],
                ' ' if ctrl => vec![0],
                '[' if ctrl => vec![0x1b],
                '\\' if ctrl => vec![0x1c],
                ']' if ctrl => vec![0x1d],
                c => c.to_string().into_bytes(),
            }
        }
        _ => return None,
    };
    if alt {
        b.insert(0, 0x1b);
    }
    Some(b)
}

/// A screen as termcode's rows: runs of one style each, colours as #rrggbb (or the
/// theme's fg and bg), the cursor shown where typing goes.
fn screen_rows(c: &Channel, cursor: bool) -> Vec<Value> {
    let s = c.screen.screen();
    let (rows, cols) = s.size();
    let (cr, cc) = s.cursor_position();
    let mut out = Vec::with_capacity(usize::from(rows));
    for r in 0..rows {
        let mut cells: Vec<Value> = Vec::new();
        let mut run = String::new();
        let mut style: Option<(Option<String>, Option<String>, bool, bool)> = None;
        for col in 0..cols {
            let Some(cell) = s.cell(r, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let (mut fg, mut bg) = (color(cell.fgcolor()), color(cell.bgcolor()));
            if cell.inverse() != (cursor && !s.hide_cursor() && r == cr && col == cc) {
                (fg, bg) = (bg.or_else(|| Some("bg".into())), fg.or_else(|| Some("fg".into())));
            }
            let st = (fg, bg, cell.bold(), cell.italic());
            if style.as_ref() != Some(&st) {
                if let Some(prev) = style.take() {
                    cells.push(cell_json(std::mem::take(&mut run), prev));
                }
                style = Some(st);
            }
            run.push_str(if cell.has_contents() { cell.contents() } else { " " });
        }
        if let Some(prev) = style {
            let plain = prev == (None, None, false, false);
            let text = if plain { run.trim_end().to_string() } else { run };
            if !text.is_empty() {
                cells.push(cell_json(text, prev));
            }
        }
        out.push(Value::Array(cells));
    }
    out
}

fn cell_json(text: String, (fg, bg, bold, italic): (Option<String>, Option<String>, bool, bool)) -> Value {
    let mut v = json!({"text": text});
    if let Some(f) = fg {
        v["fg"] = json!(f);
    }
    if let Some(b) = bg {
        v["bg"] = json!(b);
    }
    if bold {
        v["bold"] = json!(true);
    }
    if italic {
        v["italic"] = json!(true);
    }
    v
}

/// The 256-colour palette (xterm's), as #rrggbb.
fn color(c: vt100::Color) -> Option<String> {
    const BASE: [(u8, u8, u8); 16] = [(0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229), (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255)];
    let (r, g, b) = match c {
        vt100::Color::Default => return None,
        vt100::Color::Rgb(r, g, b) => (r, g, b),
        vt100::Color::Idx(i) if i < 16 => BASE[usize::from(i)],
        vt100::Color::Idx(i) if i < 232 => {
            let i = i - 16;
            let v = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
            (v(i / 36), v((i / 6) % 6), v(i % 6))
        }
        vt100::Color::Idx(i) => {
            let g = 8 + (i - 232) * 10;
            (g, g, g)
        }
    };
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_send_what_a_terminal_sends() {
        assert_eq!(key_bytes("c", true, false, false), Some(vec![3]));
        assert_eq!(key_bytes("Enter", false, false, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("Up", false, false, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes("x", false, true, false), Some(b"\x1bx".to_vec()));
        assert_eq!(key_bytes("F5", false, false, false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(key_bytes("F13", false, false, false), None);
        assert_eq!(color(vt100::Color::Idx(1)).as_deref(), Some("#cd3131"));
        assert_eq!(color(vt100::Color::Idx(196)).as_deref(), Some("#ff0000"));
    }
}
