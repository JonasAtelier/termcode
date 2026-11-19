#!/usr/bin/env python3
"""A termcode extension: termcode starts it and talks JSON-RPC on its stdin and stdout,
one message a line. It fills its slots with rows (view.set, view.append) and answers
what the user does there (view.action, view.key). Guide: doc/extensions.md."""
import json
import sys

_next_id = 0
_waiting = []  # what termcode sent while this one waited for a reply


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def call(method, **params):
    """Ask termcode (any API method, view.*); returns the result, raises on an error."""
    global _next_id
    _next_id += 1
    send({"jsonrpc": "2.0", "id": _next_id, "method": method, "params": params})
    while True:
        line = sys.stdin.readline()
        if not line:
            sys.exit(0)  # termcode closed: end
        msg = json.loads(line)
        if msg.get("id") == _next_id:
            if "error" in msg:
                raise RuntimeError(msg["error"]["message"])
            return msg["result"]
        _waiting.append(msg)


def messages():
    """What termcode tells this extension, one at a time, until it closes."""
    while True:
        while _waiting:
            yield _waiting.pop(0)
        line = sys.stdin.readline()
        if not line:
            return
        yield json.loads(line)


count = 0
picked = None


def draw():
    # Side bar: two sections, a line between them (drag it).
    call("view.set", slot="left", section="items", title="Items",
         rows=[{"cells": [{"text": "● " if picked == i else "  ", "fg": "accent"}, f"Item {i}"], "action": f"pick:{i}"}
               for i in range(1, 6)])
    call("view.set", slot="left", section="status", title="Status",
         rows=[[{"text": "● ", "fg": "ok"}, "running"], "",
               [{"text": "Count", "button": True, "action": "count"}]])
    # A tab in the editor area, the panel on the right, the panel at the bottom.
    call("view.set", slot="main", title="Hello",
         rows=[{"text": "Hello from an extension", "bold": True}, "",
               f"Count: {count}", "",
               [{"text": "+1", "button": True, "action": "count"}, " ",
                {"text": "Reset", "button": True, "action": "reset"}]])
    call("view.set", slot="right", title="Details",
         rows=[f"Picked: {picked or '-'}", [{"text": "dim text", "dim": True}]])


root = call("state.get", path="/workspace")["root"]
call("view.set", slot="bottom", title="Log", rows=[f"started in {root}"])
draw()

for msg in messages():
    method, p = msg.get("method"), msg.get("params", {})
    if method == "view.action":
        action = p["action"]
        if action == "count":
            count += 1
        elif action == "reset":
            count = 0
        elif action.startswith("pick:"):
            picked = int(action[5:])
        call("view.append", slot="bottom", rows=[f"{p['slot']}: {action}"])
        draw()
    elif method == "view.key":
        call("view.append", slot="bottom", rows=[f"key {p['key']}" + (" with Ctrl" if p["ctrl"] else "")])
