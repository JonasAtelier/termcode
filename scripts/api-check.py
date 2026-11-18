#!/usr/bin/env python3
"""End-to-end check of termcode's API: starts the termcode given (default: the one on
PATH) in a private tmux session on a scratch git repository, calls every API method
through `termcode api`, then checks its safety on a second termcode (asked before
changes and running, the answers kept, Esc, refusals); prints PASS/FAIL per check, cleans
up. Exit 0 when all pass.

  scripts/api-check.py [TERMCODE]        e.g. scripts/api-check.py target/release/termcode
"""
import json, os, shutil, subprocess, sys, tempfile, time

TC = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else shutil.which("termcode")
SESSION = f"tc-api-check-{os.getpid()}"
# Not this termcode's shell's termcode, nor herdr's: a clean start.
ENV = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_") and k != "TERMCODE_SOCKET"}
results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))


def api_cmd(method, params, to, via=()):
    return list(via) + [TC, "api", "--to", str(to), method] + ([json.dumps(params)] if params is not None else [])


def api(method, params=None, to=None, timeout=20, via=()):
    p = subprocess.run(api_cmd(method, params, to, via), capture_output=True, text=True, env=ENV, timeout=timeout)
    if p.returncode:
        return None, p.stderr.strip()
    return json.loads(p.stdout), ""


def main():
    if not TC or not os.path.exists(TC):
        sys.exit("termcode not found: give its path")
    if not shutil.which("tmux"):
        sys.exit("needs tmux")
    work = tempfile.mkdtemp(prefix="tc-api-check-")
    repo = os.path.join(work, "repo")
    os.makedirs(repo)
    with open(os.path.join(repo, "a.txt"), "w") as f:
        f.write("hello\nworld\n")
    with open(os.path.join(repo, "bad.sh"), "w") as f:
        f.write("#!/bin/sh\necho $1\n")
    subprocess.run(["git", "init", "-q", repo], check=True)
    subprocess.run(["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "init"], check=True)
    try:
        # This one lets the check change and run (as automation would start it).
        pid = start(SESSION, repo, {"TERMCODE_API_ALLOW": "edit,run"})
        hello, _ = api("hello", to=pid)
        check("hello: api 1, this workspace", hello and hello["api"] == 1 and os.path.realpath(hello["root"]) == os.path.realpath(repo), hello)
        run(pid, repo)
        subprocess.run(["tmux", "kill-session", "-t", SESSION], capture_output=True)
        # This one asks, as the user's own does.
        safety(start(SESSION + "-ask", repo, {}), SESSION + "-ask", repo)
    finally:
        for s in (SESSION, SESSION + "-ask"):
            subprocess.run(["tmux", "kill-session", "-t", s], capture_output=True)
        shutil.rmtree(work, ignore_errors=True)
    width = max(len(r[0]) for r in results)
    for name, ok, detail in results:
        print(f"{'PASS' if ok else 'FAIL'}  {name.ljust(width)}" + ("" if ok else f"  {str(detail)[:200]}"))
    failed = sum(1 for r in results if not r[1])
    print(f"\n{len(results) - failed}/{len(results)} passed")
    sys.exit(1 if failed else 0)


def start(session, repo, env):
    subprocess.run(["tmux", "new-session", "-d", "-s", session, "-x", "120", "-y", "40", "-c", repo, f"exec {TC} ."], env=dict(ENV, **env), check=True)
    pid = int(subprocess.run(["tmux", "display", "-p", "-t", session, "#{pane_pid}"], capture_output=True, text=True).stdout.strip())
    for _ in range(100):  # the socket comes up with the window
        if api("hello", to=pid)[0]:
            break
        time.sleep(0.1)
    return pid


def screen(session):
    return subprocess.run(["tmux", "capture-pane", "-p", "-t", session], capture_output=True, text=True).stdout


# A new client each call: a process that stays (not a shell, not an exec like nice).
FRESH = (sys.executable, "-c", "import subprocess, sys; sys.exit(subprocess.call(sys.argv[1:]))")


def asked(method, params, pid, session, keys, via=FRESH):
    """Calls `method` from a fresh client (`via`), waits for termcode's prompt, answers it
    with `keys`; returns (prompt shown, result, error)."""
    p = subprocess.Popen(api_cmd(method, params, pid, via), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=ENV)
    shown = False
    for _ in range(50):
        if "asks to" in screen(session):
            shown = True
            break
        time.sleep(0.1)
    for k in keys:
        subprocess.run(["tmux", "send-keys", "-t", session, k])
        time.sleep(0.2)
    out, err = p.communicate(timeout=30)
    return shown, (json.loads(out) if p.returncode == 0 else None), err.strip()


def safety(pid, session, repo):
    r, err = api("state.get", {"path": "/buffers/a.txt"}, to=pid)
    check("safety: reading needs no answer", r and "text" in r, err)
    shown, r, err = asked("terminal.send", {"text": "echo should-not-run\n"}, pid, session, ["Enter"])
    check("safety: running is asked first; Enter is Deny", shown and r is None and "said no" in err, err)
    shown, r, err = asked("buffer.edit", {"path": "a.txt", "changes": [{"start": {"line": 1}, "text": "ONCE "}]}, pid, session, ["Left", "Enter"])
    check("safety: Allow once lets that request", shown and r and r["dirty"], err)
    shown, r, err = asked("buffer.save", {"path": "a.txt"}, pid, session, ["Escape"])
    check("safety: Esc refuses it", shown and r is None and "did not answer" in err, err)
    # The check itself as the client: no, then no again without asking.
    shown, r, err = asked("command.run", {"id": "view.toggleMinimap"}, pid, session, ["Enter"], via=())
    t = time.time()
    _, err2 = api("command.run", {"id": "view.toggleMinimap"}, to=pid)
    check("safety: a no is kept for that client", shown and "said no" in err and "said no" in err2 and time.time() - t < 5 and "asks to" not in screen(session), (shown, err, err2, round(time.time() - t, 1)))
    shown, r, err = asked("buffer.edit", {"path": "a.txt", "changes": [{"start": {"line": 1}, "text": "AGAIN "}]}, pid, session, ["Left", "Left", "Enter"], via=())
    r2, err2 = api("buffer.edit", {"path": "a.txt", "changes": [{"start": {"line": 1}, "text": "AND "}]}, to=pid)
    check("safety: Allow is kept for that client and tier", shown and r and r2 and "asks to" not in screen(session), err or err2)
    _, err = api("events.wait", {"topic": "terminal.output", "match": "a" * 1001}, to=pid)
    check("safety: an oversized pattern refused", "1000" in err or "said no" in err, err)
    disk = open(os.path.join(repo, "a.txt")).read()
    check("safety: nothing saved without leave", "ONCE" not in disk and "AGAIN" not in disk, disk)


def run(pid, repo):
    cmds, err = api("command.list", to=pid)
    ids = {c["id"] for c in cmds or []}
    check("command.list: ids", {"view.toggleMinimap", "file.save", "git.pull"} <= ids, err or sorted(ids)[:5])

    r, err = api("editor.open", {"path": "a.txt", "line": 2, "col": 3}, to=pid)
    check("editor.open", r and r["opened"] == "a.txt", err)
    ed, err = api("state.get", {"path": "/editors"}, to=pid)
    tabs = [t for g in (ed or {}).get("groups", []) for t in g["tabs"]]
    cur = next((t.get("cursor") for t in tabs if t.get("path") == "a.txt"), None)
    check("state.get /editors: tab and cursor", cur == {"line": 2, "col": 3}, err or tabs)

    buf, err = api("state.get", {"path": "/buffers/a.txt"}, to=pid)
    check("state.get /buffers", buf and buf["text"] == "hello\nworld\n" and not buf["dirty"], err or buf)
    v = (buf or {}).get("version", 0)
    r, err = api("buffer.edit", {"path": "a.txt", "version": v, "changes": [
        {"start": {"line": 1, "col": 1}, "end": {"line": 1, "col": 6}, "text": "HELLO"},
        {"start": {"line": 2, "col": 6}, "text": "!"}]}, to=pid)
    check("buffer.edit: two changes", r and r["dirty"], err)
    buf, err = api("state.get", {"path": "/buffers/a.txt"}, to=pid)
    check("buffer.edit: the text", buf and buf["text"] == "HELLO\nworld!\n", err or buf)
    _, err = api("buffer.edit", {"path": "a.txt", "version": v, "changes": [{"start": {"line": 1}, "text": "x"}]}, to=pid)
    check("buffer.edit: stale version refused", "stale" in err, err)
    _, err = api("buffer.edit", {"path": "../outside", "changes": []}, to=pid)
    check("buffer.edit: outside the workspace refused", err, err)
    r, err = api("buffer.save", {"path": "a.txt"}, to=pid)
    check("buffer.save", r and not r["dirty"] and open(os.path.join(repo, "a.txt")).read() == "HELLO\nworld!\n", err)

    gitst, err = api("state.get", {"path": "/git"}, to=pid)
    check("state.get /git", gitst and "branch" in gitst, err)

    before, _ = api("state.get", {"path": "/editors"}, to=pid)
    r, err = api("command.run", {"id": "view.toggleMinimap"}, to=pid)
    check("command.run", r and r["ran"] == "view.toggleMinimap", err)
    api("command.run", {"id": "view.toggleMinimap"}, to=pid)  # back
    _, err = api("command.run", {"id": "no.suchCommand"}, to=pid)
    check("command.run: unknown refused", err, err)

    scr, err = api("state.get", {"path": "/ui/screen"}, to=pid)
    check("state.get /ui/screen: the frame's text", scr and "a.txt" in scr["text"] and scr["rows"] == 40, err)

    word = f"api-check-{os.getpid()}"
    r, err = api("terminal.send", {"text": f"echo {word}\n"}, to=pid)
    check("terminal.send", r and "mark" in r, err)
    mark = (r or {}).get("mark", 0)
    w, err = api("events.wait", {"topic": "terminal.output", "match": f"^{word}$|\\n{word}", "since": mark, "timeout": 15}, to=pid, timeout=40)
    check("events.wait: the terminal printed it", w and word in w["matched"], err)
    _, err = api("events.wait", {"topic": "terminal.output", "match": "never-printed-xyz", "timeout": 1}, to=pid, timeout=30)
    check("events.wait: times out", "timed out" in err, err)
    terms, err = api("state.get", {"path": "/terminals"}, to=pid)
    check("state.get /terminals", terms and terms["terminals"] and word in terms["terminals"][0]["screen"], err)

    api("editor.open", {"path": "bad.sh"}, to=pid)
    diags = None
    for _ in range(50):  # shellcheck runs after a short pause
        diags, err = api("state.get", {"path": "/diagnostics/bad.sh"}, to=pid)
        if diags:
            break
        time.sleep(0.2)
    if shutil.which("shellcheck"):
        check("state.get /diagnostics: shellcheck's", diags and any("quote" in d["message"].lower() for d in diags), err or diags)

    _, err = api("no.method", {}, to=pid)
    check("unknown method refused", "unknown method" in err, err)


if __name__ == "__main__":
    main()
