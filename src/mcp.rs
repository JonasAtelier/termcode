//! `termcode mcp`: termcode's API as an MCP server (JSON-RPC on stdin and stdout, a
//! message a line) for agents that speak MCP: each API method a tool (`state.get` is
//! `state_get`), called on a running termcode picked as `termcode api` picks one (`--to`,
//! $TERMCODE_SOCKET, the one on this folder, the only one). A thin adapter: that
//! termcode's own gate still asks the user before changes and running.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Each tool: the API method it calls, and what it does with which arguments.
const TOOLS: &[(&str, &str)] = &[
    ("hello", "termcode's version, pid, workspace root and methods."),
    ("command.list", "Every command termcode has: id, title (as F1 shows it), key."),
    ("command.run", "Run a command: {id} (from command_list), as F1 would."),
    ("editor.open", "Open a file at a line, showing it to the user: {path, line?, col?} (from 1)."),
    ("state.get", "Read state: {path}: /workspace, /editors, /buffers/<file> (text as the editor has it, version), /diagnostics[/<file>], /git, /terminals, /ui/screen (the screen as text)."),
    ("buffer.edit", "Edit a file as one undo step: {path, version? (from state_get), changes: [{start: {line, col}, end?, text}]} (from 1)."),
    ("buffer.save", "Save a file: {path}."),
    ("terminal.send", "Type into termcode's terminal: {text (end with \\n to run), index?}; returns the output mark."),
    ("events.wait", "Wait for terminal output: {topic: \"terminal.output\", match (regex), timeout? (s), since? (a mark)}."),
    ("serial.ports", "(Serial extension) The serial ports: tty, the user's note, state, adapter, holder, the channel open in termcode."),
    ("serial.note", "(Serial extension) Set the note beside a port: {port, note} (\"\" removes it)."),
    ("serial.monitor", "(Serial extension) Open a serial port in termcode, read only: {port}."),
    ("serial.read", "(Serial extension) A serial channel's output as text: {port, since? (offset)}; returns next."),
    ("serial.wait", "(Serial extension) Wait for a serial channel's output: {port, match (regex), timeout? (s), since?}."),
    ("serial.attach", "(Serial extension) Turn writing on for a serial channel: {port}."),
    ("serial.send", "(Serial extension) Send to a serial device (attached first): {port, text (end with \\r to enter it)}."),
    ("serial.set", "(Serial extension) A serial channel's line settings (attached first): {port, baud?, data_bits?, parity?, stop_bits?, flow?}."),
    ("serial.close", "(Serial extension) Close a serial channel: {port}."),
    ("ui.notify", "Show a message on termcode's status bar: {text}."),
    ("ui.prompt", "Ask the user in termcode: {text, buttons? (up to 4)}; returns the button picked."),
    ("ui.input", "Ask the user for a line of text in termcode: {text, value? (to start with)}; returns the text (null: closed)."),
    ("ui.pick", "Ask the user to pick one in termcode: {text, items: [label or {label, detail}]}; returns index and label (null: closed)."),
    ("ui.decorate", "Mark lines of a file as problems with a note: {path, marks: [{line, severity? (info, warning, error), note}]}."),
    ("ui.clear", "Remove your marks: {path?}."),
];

/// The MCP protocol versions answered (the client's, when known).
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// One message: its reply (None for a notification). `call` runs an API method.
pub fn handle(msg: &Value, call: &mut dyn FnMut(&str, &Value) -> Result<Value, String>) -> Option<Value> {
    let id = msg.get("id").cloned()?; // a notification (initialized, cancelled): no reply
    let ok = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    match msg["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"].as_str().unwrap_or_default();
            let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
            ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "termcode", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Drive the user's termcode editor: read what is open and unsaved, edit as undoable steps, run its commands, use its terminal and serial ports. The user is asked in termcode before changes and running.",
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": TOOLS.iter().map(|(m, d)| json!({
            "name": tool_name(m),
            "description": d,
            "inputSchema": {"type": "object", "additionalProperties": true},
        })).collect::<Vec<_>>()})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let Some((method, _)) = TOOLS.iter().find(|(m, _)| tool_name(m) == name) else {
                return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": format!("no tool {name:?}")}}));
            };
            let args = msg["params"].get("arguments").cloned().unwrap_or_else(|| json!({}));
            // A tool's failure is its result (isError), for the model to read.
            let (text, error) = match call(method, &args) {
                Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), false),
                Err(e) => (e, true),
            };
            ok(json!({"content": [{"type": "text", "text": text}], "isError": error}))
        }
        m => Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {m:?}")}})),
    }
}

/// `state.get` as a tool's name: `state_get` (MCP names have no dots).
fn tool_name(method: &str) -> String {
    method.replace('.', "_")
}

/// `termcode mcp [--to PID|FOLDER]`: serves until stdin closes.
pub fn run(args: &[String]) -> Result<(), String> {
    let mut to = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--to" => to = Some(it.next().ok_or("--to: a pid or a folder")?.clone()),
            "-h" | "--help" => {
                println!("usage: termcode mcp [--to PID|FOLDER]\nAn MCP server (stdio) for a running termcode: each API method a tool.\nIn an agent's MCP settings: {{\"command\": \"termcode\", \"args\": [\"mcp\"]}}");
                return Ok(());
            }
            a => return Err(format!("unknown argument {a:?} (termcode mcp --help)")),
        }
    }
    let mut sock: Option<PathBuf> = None;
    let mut id = 0u64;
    let mut call = |method: &str, params: &Value| -> Result<Value, String> {
        // The termcode picked at the first call (none may run yet when the agent starts).
        let s = match &sock {
            Some(s) => s.clone(),
            None => sock.insert(crate::api::pick(&crate::api::running(), to.as_deref())?).clone(),
        };
        id += 1;
        let reply = crate::api::rpc(&s, &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        match reply.get("error") {
            Some(e) => Err(e["message"].as_str().unwrap_or("failed").to_string()),
            None => Ok(reply["result"].clone()),
        }
    };
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, &mut call),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("not JSON: {e}")}})),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}").and_then(|()| out.flush()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_lists_each_api_method_as_a_tool_and_calls_it() {
        let mut calls = Vec::new();
        let mut call = |m: &str, p: &Value| -> Result<Value, String> {
            calls.push((m.to_string(), p.clone()));
            if m == "buffer.save" { Err("refused: the user said no".into()) } else { Ok(json!({"root": "/w"})) }
        };
        let init = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}), &mut call).unwrap();
        assert_eq!((init["result"]["protocolVersion"].as_str(), init["result"]["serverInfo"]["name"].as_str()), (Some("2024-11-05"), Some("termcode")));
        assert!(handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &mut call).is_none(), "no reply to a notification");
        let list = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &mut call).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"state_get") && names.contains(&"serial_send") && names.iter().all(|n| !n.contains('.')));
        // Every tool is a method the API has, or (serial.*) the Serial extension serves; every
        // method a tool but events.watch and pager.feed (git log's pager, not an agent's).
        for (m, _) in TOOLS {
            assert!(crate::rpc::METHODS.contains(m) || m.starts_with("serial."), "{m}");
        }
        assert_eq!(TOOLS.iter().filter(|(m, _)| !m.starts_with("serial.")).count(), crate::rpc::METHODS.len() - 2);
        assert!(!TOOLS.iter().any(|(m, _)| *m == "pager.feed"));
        let r = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "hello", "arguments": {}}}), &mut call).unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("\"root\""));
        let r = handle(&json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "buffer_save", "arguments": {"path": "a"}}}), &mut call).unwrap();
        assert_eq!((r["result"]["isError"].as_bool(), r["result"]["content"][0]["text"].as_str()), (Some(true), Some("refused: the user said no")));
        assert!(handle(&json!({"jsonrpc": "2.0", "id": 5, "method": "nope"}), &mut call).unwrap()["error"].is_object());
        assert_eq!(calls.last().map(|c| (c.0.as_str(), c.1["path"].as_str())), Some(("buffer.save", Some("a"))));
    }
}
