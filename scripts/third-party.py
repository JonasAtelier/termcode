#!/usr/bin/env python3
"""Writes THIRD-PARTY-LICENSES: every Rust crate built into termcode, its license, and
each license file the crate ships (identical texts printed once, with all their crates).

Usage: cargo metadata --format-version 1 --locked --filter-platform <triple> | third-party.py
Only normal dependencies count: dev- and build-dependencies never reach the binary.
"""
import json
import os
import re
import sys

MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy of this
software and associated documentation files (the "Software"), to deal in the Software
without restriction, including without limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons
to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE
FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE."""

NAMES = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)", re.I)


def main():
    meta = json.load(sys.stdin)
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    seen, todo = set(), [meta["resolve"]["root"]]
    while todo:
        for dep in nodes[todo.pop()]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]) and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                todo.append(dep["pkg"])
    crates = sorted((pkgs[i] for i in seen), key=lambda p: (p["name"], p["version"]))

    texts = {}  # license text -> crates that ship it
    for p in crates:
        label = f"{p['name']} {p['version']}"
        root = os.path.dirname(p["manifest_path"])
        files = sorted(f for f in os.listdir(root) if NAMES.match(f) and os.path.isfile(os.path.join(root, f)))
        found = [open(os.path.join(root, f), encoding="utf-8", errors="replace").read().strip() for f in files]
        if not found:
            # No file in the crate: MIT's text with its authors (the only case today).
            if "MIT" not in (p["license"] or ""):
                sys.exit(f"{label}: no license file and not MIT ({p['license']}); add its text here")
            holders = ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in p["authors"]) or f"the {p['name']} authors"
            found = [f"Copyright (c) {holders}\n\n{MIT}"]
        for t in found:
            texts.setdefault(t, []).append(label)

    out = ["Third-party software in termcode", "=" * 32, "",
           "termcode is built with these Rust crates. Their license texts follow the list;",
           "a crate offering a choice of licenses ships all of them.", ""]
    width = max(len(f"{p['name']} {p['version']}") for p in crates)
    for p in crates:
        out.append(f"{p['name'] + ' ' + p['version']:<{width}}  {p['license'] or 'see its license file'}")
    for t, users in texts.items():
        out += ["", "-" * 80, "Used by: " + ", ".join(users), "-" * 80, "", t]
    sys.stdout.write("\n".join(out) + "\n")


main()
