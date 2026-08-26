#!/usr/bin/env python3
"""Regenerate docs/src/assets/cli-demo.cast and cli-demo.svg for ARES v0.10.0.

Runs the real ares-server binary in fresh temp directories, captures ANSI
output, and emits an asciinema v2 cast organized as four numbered Acts:
Bootstrap, Inspect, Guardrails, Operate. Renders the SVG with svg-term@2.0.3
and self-checks glyph-text coverage before finishing.

Usage: python3 docs/tools/gen_demo.py [--skip-svg]
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BIN = REPO / "target/debug/ares-server"
CAST_OUT = REPO / "docs/src/assets/cli-demo.cast"
SVG_OUT = REPO / "docs/src/assets/cli-demo.svg"
SVGTERM = Path("/tmp/svgterm")

WIDTH, HEIGHT = 100, 34
FPS = 12            # render frame rate
IDLE_MS = 900       # collapse idle gaps longer than this at render time

CYAN = "\x1b[1;36m"
GREEN = "\x1b[1;32m"
DIM = "\x1b[2m"
RESET = "\x1b[0m"

events = []


def emit(text, delay):
    events.append((delay, text))


def card(title):
    """Emit a visible Act title card on a cleared screen."""
    emit("\x1b[H\x1b[2J\x1b[3J", 0.35)
    emit(f"{CYAN}── {title} ──{RESET}\r\n\r\n", 0.9)


def prompt(display):
    display = display.replace("\n", "\r\n")
    emit(f"{CYAN}${RESET} {display}\r\n", 0.55)


def note(text):
    emit(f"{DIM}   → {text}{RESET}\r\n", 0.85)


def clean_output(raw, max_cols=96):
    """Strip the ASCII-art boot banner, keep the version line onward."""
    lines = raw.splitlines()
    start = 0
    for i, ln in enumerate(lines):
        if "Agentic Runtime Extensible Server" in ln:
            start = i
            break
    lines = lines[start:]
    out = []
    for ln in lines:
        if len(ln) > max_cols:
            ln = ln[: max_cols - 1] + "…"
        out.append(ln)
    while out and out[-1].strip() == "":
        out.pop()
    return out


def run(cmd, cwd, timeout=20):
    p = subprocess.run(cmd, cwd=cwd, env=dict(os.environ),
                       capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout + p.stderr


def scene(display, cmd, cwd, keep_lines=None, out_delay=1.1,
          exit_note=None, raw_display=None):
    prompt(raw_display if raw_display else display)
    code, output = run(cmd, cwd)
    lines = clean_output(output)
    if not lines:
        lines = ["(no output)"]
    if keep_lines is not None:
        lines = lines[:keep_lines]
    emit("".join(ln + "\r\n" for ln in lines), out_delay)
    if exit_note:
        note(exit_note(code) if callable(exit_note) else exit_note)
    return code


def write_cast():
    CAST_OUT.parent.mkdir(parents=True, exist_ok=True)
    with CAST_OUT.open("w") as f:
        f.write(json.dumps({
            "version": 2, "width": WIDTH, "height": HEIGHT,
            "timestamp": int(time.time()),
            "env": {"SHELL": None, "TERM": "dumb"},
        }) + "\n")
        clock = 0.4
        for delay, text in events:
            clock += delay
            f.write(json.dumps([round(clock, 6), "o", text]) + "\n")
    return clock


def ensure_svgterm():
    if (SVGTERM / "node_modules/svg-term/package.json").exists():
        return
    SVGTERM.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["npm", "install", "--prefix", str(SVGTERM),
         "svg-term@2.0.3", "@emotion/styled@10", "@emotion/core@10"],
        check=True)


RENDER_JS = """
const fs = require('fs');
const { render } = require('svg-term');
const [castPath, svgPath, fpsStr, idleStr] = process.argv.slice(1);
const svg = render(fs.readFileSync(castPath, 'utf8'), {
    fps: parseInt(fpsStr, 10),
    idle: parseInt(idleStr, 10),
    window: true,
});
fs.writeFileSync(svgPath, svg);
"""


def render_svg():
    ensure_svgterm()
    subprocess.run(
        ["node", "-e", RENDER_JS, str(CAST_OUT), str(SVG_OUT),
         str(FPS), str(IDLE_MS)],
        cwd=str(SVGTERM), check=True)


TEXT_RE = re.compile(r"<text[^>]*>([^<]*)</text>")


def check_coverage():
    svg = SVG_OUT.read_text(encoding="utf-8")
    joined = "".join(TEXT_RE.findall(svg))
    compact = re.sub(r"\s+", "", joined)
    needles = ["0.10.0",
               "Act1·Bootstrap",
               "Act2·Inspect",
               "Act3·Guardrails",
               "Act4·Operate",
               "supervise"]
    missing = [n for n in needles if n not in compact]
    if missing:
        print("coverage check FAILED, missing:", missing, file=sys.stderr)
        sys.exit(1)
    print(f"coverage check passed: {needles}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--skip-svg", action="store_true")
    args = ap.parse_args()

    if not BIN.exists():
        print(f"binary not found: {BIN}", file=sys.stderr)
        sys.exit(1)

    tmp = Path(tempfile.mkdtemp(prefix="ares-cast-"))

    # ── Act 1 · Bootstrap ──────────────────────────────────────────────────
    fresh = tmp / "fresh"
    fresh.mkdir()

    card("Act 1 · Bootstrap")
    scene("ares-server --version", [str(BIN), "--version"], fresh,
          out_delay=0.8,
          exit_note="single static binary, no runtime dependencies")
    scene("ares-server init --minimal", [str(BIN), "init", "--minimal"], fresh,
          out_delay=2.2, keep_lines=24,
          exit_note="one command scaffolds config, agents, models, tools")
    scene("find . -type f | sort", ["/bin/sh", "-c", "cd fresh && find . -type f | sort"], tmp,
          out_delay=1.0, keep_lines=10, raw_display="find . -type f | sort",
          exit_note="agents and models land as TOON files under config/")

    # ── Act 2 · Inspect ────────────────────────────────────────────────────
    card("Act 2 · Inspect")
    scene("ares-server config --validate", [str(BIN), "config", "--validate"], fresh,
          out_delay=1.5, keep_lines=17,
          exit_note="every provider, model, tool reference resolves before boot")
    scene("ares-server agent list", [str(BIN), "agent", "list"], fresh,
          out_delay=1.2, keep_lines=10)
    scene("ares-server agent show orchestrator",
          [str(BIN), "agent", "show", "orchestrator"], fresh,
          out_delay=1.5, keep_lines=13,
          exit_note="system prompt included — inspect what will really run")

    # ── Act 3 · Guardrails ─────────────────────────────────────────────────
    card("Act 3 · Guardrails")
    scene("ares-server init --minimal   # again — guarded",
          [str(BIN), "init", "--minimal"], fresh,
          out_delay=0.9, keep_lines=6,
          exit_note="refuses to clobber an existing project")

    scene("ares-server rag ingest-dir …   # bad path",
          [str(BIN), "rag", "ingest-dir", "--collection", "knowledge",
           "--docs-path", "./missing-docs", "--user", "demo@example.com",
           "--password", "demo-pass"],
          fresh, out_delay=0.9, keep_lines=3, raw_display="ares-server rag ingest-dir --collection knowledge \\\n    --docs-path ./missing-docs …   # bad path",
          exit_note="bad input refused before any network call")

    scene("ares-server rag search …   # unauthenticated",
          [str(BIN), "rag", "search", "--collection", "knowledge",
           "--query", "rust agentic", "--user", "demo@example.com",
           "--password", "demo-pass"],
          fresh, out_delay=1.2, keep_lines=3, raw_display="ares-server rag search --collection knowledge \\\n    --query \"rust agentic\" …   # unauthenticated",
          exit_note="fails closed — HTTP 401, nothing half-served")

    broken = fresh / "broken.toml"
    broken.write_text((fresh / "ares.toml").read_text().replace(
        'port = 3000', 'port = "not-a-number"'))
    scene("ares-server config --validate -c broken.toml",
          [str(BIN), "config", "--validate", "-c", "broken.toml"], fresh,
          out_delay=1.3, keep_lines=4,
          raw_display="sed 's/port = 3000/port = \"not-a-number\"/' ares.toml > broken.toml\n$ ares-server config --validate -c broken.toml",
          exit_note=None)

    # ── Act 4 · Operate ────────────────────────────────────────────────────
    card("Act 4 · Operate")
    scene("ares-server --help", [str(BIN), "--help"], fresh,
          out_delay=1.6, keep_lines=36, raw_display="ares-server --help   # capability breadth, supervision built in",
          exit_note="--supervise: respawn on hot-restart exits (51), stop on clean exits, "
                    "surface boot failures (53) non-zero")

    # Closing card: headline v0.10 kernel capabilities.
    emit("\x1b[H\x1b[2J\x1b[3J", 0.4)
    emit(f"{GREEN}═══ ARES 0.10 · kernel highlights ═══{RESET}\r\n", 1.1)
    emit(f"{DIM}intercept meta-events · readiness barriers{RESET}\r\n", 1.1)
    emit(f"{DIM}name-keyed accessors · identity-preserving entry moves{RESET}\r\n", 1.1)
    emit(f"{CYAN}ares-server init · config · agent · rag · --supervise{RESET}\r\n", 1.3)

    total = write_cast()
    print(f"wrote {CAST_OUT}: {len(events)} events, {total:.1f}s")

    if not args.skip_svg:
        render_svg()
        print(f"wrote {SVG_OUT}")
        check_coverage()


if __name__ == "__main__":
    main()
