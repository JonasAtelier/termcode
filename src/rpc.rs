//! The API's methods (doc/api.md, phase 1): what a JSON-RPC request asks of the App.
//! Positions are as the status bar shows them: line and column from 1, the column in
//! characters.

use crate::editor::{Pos, View};
use crate::panel::{App, Doc};
use serde_json::{json, Value};
use std::time::Duration;

/// A method's answer: now, or (the socket waits for it) the next frame's text, or a
/// terminal printing a match.
pub enum Out {
    Now(Value),
    Screen,
    Output { term: usize, re: fancy_regex::Regex, mark: u64, timeout: Duration },
    /// The connection kept for events on these topics (none: all of them).
    Watch(Vec<String>),
    /// `ui.notify`: shown with the client's name.
    Notify(String),
    /// `ui.prompt`: the user asked in termcode; the button picked is the answer.
    Ask { text: String, buttons: Vec<String>, timeout: Duration },
    /// `ui.input` (no items: a line typed, `value` to start with) or `ui.pick` (one of
    /// `items`: label, detail): the user's answer.
    Input { text: String, value: String, items: Option<Vec<(String, String)>>, timeout: Duration },
}

/// What `events.watch` sends, each as `{"method": "event", "params": {"topic", …}}`.
pub const TOPICS: &[&str] = &["buffer.changed", "file.saved", "file.opened", "file.closed", "tab", "cursor", "diagnostics", "git", "terminal.output", "serial.output"];

/// Where the Serial view's extension is (its manifest in that folder of termcode's source).
pub const XCOMM: &str = "https://github.com/JonasAtelier/termcode#extensions/xcomm";

pub const METHODS: &[&str] = &[
    "hello",
    "command.list",
    "command.run",
    "editor.open",
    "state.get",
    "buffer.edit",
    "buffer.save",
    "terminal.send",
    "events.wait",
    "events.watch",
    "ui.notify",
    "ui.prompt",
    "ui.input",
    "ui.pick",
    "ui.decorate",
    "ui.clear",
    "pager.feed",
];

/// What `method` may do (`api::Gate` asks the user before `Edit` and `Run`): reading
/// is free; opening and changing files is `Edit`; commands, terminals and the screen
/// (which shows them) are `Run`.
pub fn tier(method: &str, p: &Value) -> crate::api::Tier {
    use crate::api::Tier;
    match method {
        "editor.open" | "buffer.edit" | "buffer.save" => Tier::Edit,
        // Showing things in termcode, asking the user: as changing what is open.
        "ui.notify" | "ui.prompt" | "ui.input" | "ui.pick" | "ui.decorate" | "ui.clear" => Tier::Edit,
        // An extension bringing its view to the front (its panes are its own: reading).
        "view.open" => Tier::Edit,
        "command.run" | "terminal.send" | "events.wait" => Tier::Run,
        "state.get" if matches!(p["path"].as_str(), Some("/terminals" | "/ui/screen")) => Tier::Run,
        // Output (a terminal's, a serial port's) is what running reads; the rest is reading.
        "events.watch" => {
            let topics: Vec<&str> = p["topics"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if topics.is_empty() || topics.iter().any(|t| t.ends_with(".output")) { Tier::Run } else { Tier::Read }
        }
        _ => Tier::Read,
    }
}

const STATE: &str = "/workspace, /editors, /buffers/<path>, /diagnostics[/<path>], /git, /terminals, /ui/screen";

pub fn call(app: &mut App, method: &str, p: &Value) -> Result<Out, String> {
    let now = |v: Value| Ok(Out::Now(v));
    match method {
        "hello" => now(json!({
            "api": 1,
            "termcode": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "root": app.root,
            "methods": METHODS.iter().map(|m| m.to_string()).chain(crate::ext::api_methods(app)).collect::<Vec<_>>(),
        })),
        "command.list" => now(Value::Array(commands().map(|(id, title, key)| json!({"id": id, "title": title, "key": key})).collect())),
        "command.run" => {
            let want = p["id"].as_str().or(p["title"].as_str()).ok_or("command.run: give an \"id\" (command.list has them)")?;
            let (id, title, _) = commands().find(|(id, title, _)| id == want || *title == want).ok_or_else(|| format!("no command {want:?} (command.list has them)"))?;
            let cmd = crate::panel::all_commands().find(|(t, _)| *t == title).map(|(_, c)| c).ok_or("gone")?;
            app.run_command(cmd);
            now(json!({"ran": id}))
        }
        "editor.open" => {
            let rel = rel_path(app, p)?;
            let (line, col) = (p["line"].as_u64().unwrap_or(1), p["col"].as_u64().unwrap_or(1));
            app.request(&format!("open:{}:{line}:{col}", app.root.join(&rel).display()));
            now(json!({"opened": rel, "line": line, "col": col}))
        }
        "state.get" => state(app, p["path"].as_str().unwrap_or("/workspace")),
        // git's pager in this termcode's terminal (`termcode pager`): its window.
        "pager.feed" => now(crate::pager::feed(app, p)?),
        "buffer.edit" => edit(app, p).map(Out::Now),
        "buffer.save" => {
            let rel = rel_path(app, p)?;
            let key = format!("file:{rel}");
            app.save_doc(&key)?;
            let dirty = matches!(app.docs.get(&key), Some(Doc::Text(b)) if b.dirty());
            now(json!({"saved": rel, "dirty": dirty}))
        }
        "terminal.send" => {
            let text = p["text"].as_str().ok_or("terminal.send: give \"text\" (end it with \\n to run it)")?;
            if app.terms.is_empty() {
                app.new_terminal();
            }
            let i = p["index"].as_u64().map_or(app.term_at, |i| i as usize);
            let t = app.terms.get_mut(i).ok_or_else(|| format!("no terminal {i}"))?;
            let mark = t.log.end();
            t.send(text.as_bytes());
            now(json!({"index": i, "mark": mark}))
        }
        "events.watch" => {
            let topics: Vec<String> = p["topics"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            if let Some(t) = topics.iter().find(|t| !TOPICS.contains(&t.as_str())) {
                return Err(format!("events.watch: no topic {t:?}; {}", TOPICS.join(", ")));
            }
            Ok(Out::Watch(topics))
        }
        "events.wait" => {
            match p["topic"].as_str() {
                Some("terminal.output") => {}
                t => return Err(format!("events.wait: topic {t:?}: \"terminal.output\" for now")),
            }
            let pat = p["match"].as_str().ok_or("events.wait: give \"match\", a regex")?;
            if pat.len() > 1000 {
                return Err("match: 1000 characters at most".into());
            }
            // Bounded backtracking: a pathological pattern fails, it never stalls the editor.
            let re = fancy_regex::RegexBuilder::new(pat).backtrack_limit(100_000).build().map_err(|e| format!("match: {e}"))?;
            let term = p["index"].as_u64().map_or(app.term_at, |i| i as usize);
            let t = app.terms.get(term).ok_or_else(|| format!("no terminal {term}"))?;
            let mark = p["since"].as_u64().unwrap_or_else(|| t.log.end());
            let timeout = Duration::from_secs_f64(p["timeout"].as_f64().unwrap_or(30.0).clamp(0.0, 3600.0));
            Ok(Out::Output { term, re, mark, timeout })
        }
        "ui.notify" => {
            let text = p["text"].as_str().ok_or("ui.notify: give \"text\"")?;
            Ok(Out::Notify(text.chars().take(200).collect()))
        }
        "ui.prompt" => {
            let text = p["text"].as_str().ok_or("ui.prompt: give \"text\" (and \"buttons\")")?.chars().take(300).collect();
            let mut buttons: Vec<String> = p["buttons"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|b| b.chars().take(24).collect()).take(4).collect();
            if buttons.is_empty() {
                buttons = vec!["OK".into()];
            }
            let timeout = Duration::from_secs_f64(p["timeout"].as_f64().unwrap_or(120.0).clamp(1.0, 3600.0));
            Ok(Out::Ask { text, buttons, timeout })
        }
        "ui.input" | "ui.pick" => {
            let text = p["text"].as_str().ok_or_else(|| format!("{method}: give \"text\" (what is asked)"))?.chars().take(120).collect();
            let timeout = Duration::from_secs_f64(p["timeout"].as_f64().unwrap_or(120.0).clamp(1.0, 3600.0));
            let items = if method == "ui.pick" {
                let items: Vec<(String, String)> = p["items"]
                    .as_array()
                    .ok_or("ui.pick: give \"items\": [\"label\", or {\"label\", \"detail\"}]")?
                    .iter()
                    .take(1000)
                    .filter_map(|v| match v {
                        Value::String(s) => Some((s.chars().take(200).collect(), String::new())),
                        _ => Some((v["label"].as_str()?.chars().take(200).collect(), v["detail"].as_str().unwrap_or("").chars().take(200).collect())),
                    })
                    .collect();
                if items.is_empty() {
                    return Err("ui.pick: no items".into());
                }
                Some(items)
            } else {
                None
            };
            let value = p["value"].as_str().unwrap_or("").chars().take(1000).collect();
            Ok(Out::Input { text, value, items, timeout })
        }
        "ui.decorate" => {
            let rel = rel_path(app, p)?;
            let key = app.file_doc(&rel);
            let Some(Doc::Text(b)) = app.docs.get(&key) else { return Err(format!("{rel} is not text")) };
            let mut marks = Vec::new();
            for m in p["marks"].as_array().ok_or("ui.decorate: give \"marks\": [{line, severity?, note}] ([] clears them)")? {
                let line = m["line"].as_u64().ok_or("a mark needs its \"line\" (from 1)")? as usize;
                let Some(text) = b.lines.get(line.saturating_sub(1)) else { continue };
                let sev = match m["severity"].as_str().unwrap_or("info") {
                    "error" => crate::lint::Sev::Error,
                    "warning" => crate::lint::Sev::Warning,
                    _ => crate::lint::Sev::Info,
                };
                let note = m["note"].as_str().unwrap_or_default().chars().take(300).collect();
                // The whole line's text, its indentation left out.
                let start = text.len() - text.trim_start().len();
                marks.push(crate::lint::Diag::mark(line - 1, start, text.len().max(start + 1), sev, note));
            }
            let n = marks.len();
            if marks.is_empty() {
                app.lints.marks.remove(&key);
            } else {
                app.lints.marks.insert(key, marks);
            }
            now(json!({"path": rel, "marks": n}))
        }
        "ui.clear" => {
            match p["path"].as_str() {
                Some(_) => {
                    let rel = rel_path(app, p)?;
                    app.lints.marks.remove(&format!("file:{rel}"));
                }
                None => app.lints.marks.clear(),
            }
            now(json!({"cleared": true}))
        }
        // Serial ports are the xcomm extension's (extensions/xcomm), when installed.
        m if m.starts_with("serial.") => Err(format!("unknown method {m:?}: serial ports come from the xcomm extension (termcode ext install {XCOMM})")),
        m => Err(format!("unknown method {m:?}: one of {}", METHODS.join(", "))),
    }
}

/// `p["port"]`: a tty (`ttyUSB0`, or `/dev/ttyUSB0`).
/// Every command: its id (from its title: "View: Toggle Minimap" → view.toggleMinimap),
/// its title as F1 shows it, its key.
pub fn commands() -> impl Iterator<Item = (String, &'static str, &'static str)> {
    let keys = |title: &str| crate::panel::COMMANDS.iter().find(|c| c.0 == title).map_or("", |c| c.2);
    crate::panel::all_commands().map(move |(title, _)| (command_id(title), title, keys(title)))
}

// ponytail: ids follow titles; a renamed command changes its id. Keep a table of old ids
// if one is ever renamed after people script against it.
pub fn command_id(title: &str) -> String {
    let (area, rest) = title.split_once(": ").unwrap_or(("", title));
    let words = |s: &str| s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let camel = |ws: Vec<String>| {
        ws.iter()
            .enumerate()
            .map(|(i, w)| {
                let w = w.to_lowercase();
                if i == 0 {
                    w
                } else {
                    w[..1].to_uppercase() + &w[1..]
                }
            })
            .collect::<String>()
    };
    match camel(words(area)) {
        a if a.is_empty() => camel(words(rest)),
        a => format!("{a}.{}", camel(words(rest))),
    }
}

/// `p["path"]`, relative to the workspace or absolute inside it: a file there.
fn rel_path(app: &App, p: &Value) -> Result<String, String> {
    let path = p["path"].as_str().ok_or("give a \"path\"")?;
    let abs = std::fs::canonicalize(app.root.join(path)).map_err(|e| format!("{path}: {e}"))?;
    let rel = abs.strip_prefix(&app.root).map_err(|_| format!("{path} is outside {}", app.root.display()))?;
    if !abs.is_file() {
        return Err(format!("{path} is not a file"));
    }
    Ok(rel.to_string_lossy().into_owned())
}

/// A position {line, col} (from 1, the column in characters) in `lines`, clamped.
fn pos(lines: &[String], v: &Value) -> Pos {
    let l = (v["line"].as_u64().unwrap_or(1).max(1) as usize - 1).min(lines.len().saturating_sub(1));
    let text = lines.get(l).map_or("", String::as_str);
    let c = v["col"].as_u64().unwrap_or(1).max(1) as usize - 1;
    Pos::new(l, text.char_indices().nth(c).map_or(text.len(), |(i, _)| i))
}

fn json_pos(lines: &[String], p: Pos) -> Value {
    let col = lines.get(p.line).map_or(0, |l| l[..p.col.min(l.len())].chars().count());
    json!({"line": p.line + 1, "col": col + 1})
}

/// `buffer.edit {path, version?, changes: [{start, end, text}]}`: one undo step, the
/// file's tab opened behind the active one if it has none.
fn edit(app: &mut App, p: &Value) -> Result<Value, String> {
    let rel = rel_path(app, p)?;
    let changes = p["changes"].as_array().ok_or("buffer.edit: give \"changes\": [{start, end, text}]")?;
    let key = app.file_doc(&rel);
    if !app.groups.iter().any(|g| g.tabs.iter().any(|t| t.key == key)) {
        let (g, active) = (app.ag, app.groups[app.ag].active);
        app.show_tab(g, key.clone(), false);
        (app.ag, app.groups[g].active) = (g, active);
    }
    let Some(Doc::Text(b)) = app.docs.get_mut(&key) else { return Err(format!("{rel} is not text")) };
    if let Some(why) = &b.readonly {
        return Err(format!("{rel} is read-only: {why}"));
    }
    if let Some(v) = p["version"].as_u64() {
        if v != b.version() {
            return Err(format!("stale: {rel} is at version {}, not {v}; read it again", b.version()));
        }
    }
    let mut list = Vec::new();
    for c in changes {
        let start = pos(&b.lines, &c["start"]);
        let end = if c["end"].is_null() { start } else { pos(&b.lines, &c["end"]) };
        list.push((start.min(end), start.max(end), c["text"].as_str().unwrap_or_default().to_string()));
    }
    let mut sorted: Vec<_> = list.iter().map(|c| (c.0, c.1)).collect();
    sorted.sort();
    if sorted.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err("changes overlap".into());
    }
    b.replace_ranges(&mut View::default(), list);
    let (version, dirty) = (b.version(), b.dirty());
    if let Some(t) = app.groups.iter_mut().flat_map(|g| g.tabs.iter_mut()).find(|t| t.key == key) {
        b.clamp_view(&mut t.view);
    }
    Ok(json!({"path": rel, "version": version, "dirty": dirty}))
}

fn state(app: &mut App, path: &str) -> Result<Out, String> {
    let now = |v: Value| Ok(Out::Now(v));
    match path.trim_end_matches('/') {
        "/workspace" | "" => now(json!({"root": app.root, "branch": app.branch, "pid": std::process::id()})),
        "/editors" => {
            let groups: Vec<Value> = app
                .groups
                .iter()
                .map(|g| {
                    let tabs: Vec<Value> = g
                        .tabs
                        .iter()
                        .map(|t| {
                            let mut v = json!({"key": t.key, "preview": t.preview});
                            if let (Some(rel), Some(Doc::Text(b))) = (t.key.strip_prefix("file:"), app.docs.get(&t.key)) {
                                v["path"] = json!(rel);
                                v["dirty"] = json!(b.dirty());
                                v["version"] = json!(b.version());
                                v["cursor"] = json_pos(&b.lines, t.view.cursor);
                            }
                            v
                        })
                        .collect();
                    json!({"active": g.active, "tabs": tabs})
                })
                .collect();
            now(json!({"groups": groups, "activeGroup": app.ag, "focus": format!("{:?}", app.focus)}))
        }
        "/git" => now(json!({
            "branch": app.branch,
            "status": app.status.iter().map(|e| json!({"x": e.x.to_string(), "y": e.y.to_string(), "path": e.path})).collect::<Vec<_>>(),
        })),
        "/terminals" => now(json!({
            "open": app.term_open,
            "active": app.term_at,
            "terminals": app.terms.iter().enumerate().map(|(i, t)| json!({"index": i, "name": t.name(), "running": t.running(), "screen": t.contents()})).collect::<Vec<_>>(),
        })),
        "/ui/screen" => Ok(Out::Screen),
        "/diagnostics" => {
            let keys: Vec<String> = app.docs.keys().filter(|k| k.starts_with("file:")).cloned().collect();
            let mut all = serde_json::Map::new();
            for k in keys {
                let d = diagnostics(app, &k);
                if d.as_array().is_some_and(|a| !a.is_empty()) {
                    all.insert(k["file:".len()..].to_string(), d);
                }
            }
            now(Value::Object(all))
        }
        p if p.starts_with("/diagnostics/") => {
            let rel = rel_path(app, &json!({"path": &p["/diagnostics/".len()..]}))?;
            now(diagnostics(app, &format!("file:{rel}")))
        }
        p if p.starts_with("/buffers/") => {
            let rel = rel_path(app, &json!({"path": &p["/buffers/".len()..]}))?;
            let key = app.file_doc(&rel);
            let Some(Doc::Text(b)) = app.docs.get(&key) else { return Err(format!("{rel} is not text")) };
            now(json!({
                "path": rel,
                "text": b.text(),
                "version": b.version(),
                "dirty": b.dirty(),
                "language": b.lang,
                "readonly": b.readonly,
                "lines": b.lines.len(),
            }))
        }
        p => Err(format!("state.get: no {p:?}; one of {STATE}")),
    }
}

fn diagnostics(app: &App, key: &str) -> Value {
    let lines = match app.docs.get(key) {
        Some(Doc::Text(b)) => &b.lines,
        _ => return json!([]),
    };
    Value::Array(
        app.lints
            .all(key)
            .into_iter()
            .map(|(line, start, end, sev, msg)| {
                json!({
                    "severity": format!("{sev:?}").to_lowercase(),
                    "start": json_pos(lines, Pos::new(line, start)),
                    "end": json_pos(lines, Pos::new(line, end)),
                    "message": msg,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn call_now(app: &mut App, m: &str, p: Value) -> Result<Value, String> {
        match call(app, m, &p)? {
            Out::Now(v) => Ok(v),
            _ => Err("waits".into()),
        }
    }

    #[test]
    fn command_ids_come_from_titles_and_are_unique() {
        assert_eq!(command_id("View: Toggle Minimap"), "view.toggleMinimap");
        assert_eq!(command_id("Preferences: Open Settings (JSON)"), "preferences.openSettingsJson");
        assert_eq!(command_id("Go Back"), "goBack");
        let ids: Vec<String> = commands().map(|c| c.0).collect();
        let mut seen = std::collections::HashSet::new();
        assert!(ids.iter().all(|i| seen.insert(i.clone())), "duplicate ids");
        assert!(ids.contains(&"git.pull".to_string()));
    }

    #[test]
    fn an_edit_is_versioned_undoable_and_saved_through_the_api() {
        let dir = std::env::temp_dir().join(format!("termcode-rpc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "hello\nworld\n").unwrap();
        let mut app = App::new(fs::canonicalize(&dir).unwrap(), None);
        assert_eq!(call_now(&mut app, "hello", json!({})).unwrap()["api"], 1);
        let b = call_now(&mut app, "state.get", json!({"path": "/buffers/a.txt"})).unwrap();
        assert_eq!(b["text"], "hello\nworld\n");
        let v = b["version"].as_u64().unwrap();
        // Two changes at once: the later first, so "hello" keeps its place.
        let r = call_now(&mut app, "buffer.edit", json!({"path": "a.txt", "version": v, "changes": [
            {"start": {"line": 1, "col": 1}, "end": {"line": 1, "col": 6}, "text": "HELLO"},
            {"start": {"line": 2, "col": 6}, "text": "!"},
        ]}))
        .unwrap();
        assert_eq!(r["dirty"], true);
        assert!(app.groups[app.ag].tabs.iter().any(|t| t.key == "file:a.txt"), "a tab for it");
        assert_eq!(call_now(&mut app, "state.get", json!({"path": "/buffers/a.txt"})).unwrap()["text"], "HELLO\nworld!\n");
        let stale = call_now(&mut app, "buffer.edit", json!({"path": "a.txt", "version": v, "changes": [{"start": {"line": 1}, "text": "x"}]}));
        assert!(stale.unwrap_err().starts_with("stale"));
        assert!(call_now(&mut app, "buffer.edit", json!({"path": "a.txt", "changes": [
            {"start": {"line": 1, "col": 1}, "end": {"line": 1, "col": 4}, "text": ""},
            {"start": {"line": 1, "col": 2}, "end": {"line": 1, "col": 3}, "text": ""},
        ]})).unwrap_err().contains("overlap"));
        // One undo step takes both back; redo brings them again.
        let Some(Doc::Text(b)) = app.docs.get_mut("file:a.txt") else { panic!() };
        assert!(b.undo(&mut View::default()));
        assert_eq!(b.text(), "hello\nworld\n");
        assert!(b.redo(&mut View::default()));
        assert_eq!(call_now(&mut app, "buffer.save", json!({"path": "a.txt"})).unwrap()["dirty"], false);
        assert!(fs::read_to_string(dir.join("a.txt")).unwrap().starts_with("HELLO"));
        assert!(call_now(&mut app, "buffer.edit", json!({"path": "../x", "changes": []})).is_err(), "outside the workspace");
        assert!(call_now(&mut app, "nope", json!({})).unwrap_err().starts_with("unknown method"));
        let huge = "a".repeat(1001);
        assert!(call(&mut app, "events.wait", &json!({"topic": "terminal.output", "match": huge})).err().unwrap().contains("1000"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pager_feed_opens_git_logs_window_and_says_when_it_closed() {
        let mut app = App::new(std::env::temp_dir(), None);
        let r = call_now(&mut app, "pager.feed", json!({"title": "git log", "text": "commit 1\n"})).unwrap();
        assert_eq!((r["open"].as_bool(), app.pager.is_some()), (Some(true), true));
        app.pager = None;
        let r = call_now(&mut app, "pager.feed", json!({"id": r["id"], "text": "more\n"})).unwrap();
        assert_eq!(r["open"], false);
    }
}