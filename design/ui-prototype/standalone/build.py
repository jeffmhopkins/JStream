#!/usr/bin/env python3
"""Builds ../jstream-prototype.html: every Design Component in this folder's parent, plus
runtime.js and shell.html, in one file that opens offline by double-click.

Run from anywhere: python3 design/ui-prototype/standalone/build.py
"""
import html
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROTO = HERE.parent
OUT = PROTO / "jstream-prototype.html"


def split_dc(path):
    """Returns (markup, logic, props_json) for one .dc.html file."""
    src = path.read_text(encoding="utf-8")
    xdc = re.search(r"<x-dc>.*?</x-dc>", src, re.S)
    if not xdc:
        sys.exit(f"{path.name}: no <x-dc> block")
    markup = xdc.group(0)
    logic_m = re.search(r'<script type="text/x-dc" data-dc-script data-props=\'(.*?)\'>(.*?)</script>', src, re.S)
    logic, props = (logic_m.group(2), logic_m.group(1)) if logic_m else ("class Component extends DCLogic {}", "{}")
    for name, text in (("markup", markup), ("script", logic)):
        if re.search(r"</script", text, re.I):
            sys.exit(f"{path.name}: its {name} contains </script, which would end the inlined block")
    return markup, logic, props


def main():
    canvas = json.loads((PROTO / "canvas.json").read_text(encoding="utf-8"))
    boards = canvas["boards"]
    names = canvas["order"]
    blocks = []
    for file in names:
        name = file.removesuffix(".dc.html")
        markup, logic, props = split_dc(PROTO / file)
        blocks.append(
            f'\n<!-- ===== {html.escape(file)} : {html.escape(boards[file]["title"])} ===== -->\n'
            f'<script type="text/x-dc-markup" data-dc="{name}">\n{markup}\n</script>\n'
            f'<script type="text/x-dc-logic" data-dc="{name}" data-props=\'{props}\'>{logic}</script>\n'
        )
    # The switcher lists the boards a person can open on their own (not Panel, which needs
    # the workspace's props); the interactive one opens first.
    views = [
        {"name": f.removesuffix(".dc.html"), "title": boards[f]["title"], "interactive": bool(boards[f].get("is_interactive"))}
        for f in names
        if f != "Panel.dc.html"
    ]
    views.sort(key=lambda v: not v["interactive"])
    shell = (HERE / "shell.html").read_text(encoding="utf-8")
    runtime = (HERE / "runtime.js").read_text(encoding="utf-8")
    out = (
        shell.replace("/*@RUNTIME@*/", runtime)
        .replace("/*@VIEWS@*/[]", json.dumps(views, ensure_ascii=False))
        .replace("<!--@COMPONENTS@-->", "".join(blocks))
        .replace("@TITLE@", html.escape(canvas.get("title", "JStream UI prototype")))
    )
    OUT.write_text(out, encoding="utf-8")
    print(f"wrote {OUT.relative_to(PROTO.parent.parent)} ({len(out.encode()) // 1024} KB)")


if __name__ == "__main__":
    main()
