#!/usr/bin/env python3
"""Audit GitHub Actions workflows for self-hosted jobs reachable by fork PRs.

A fork PR runs fork-controlled code. On a self-hosted runner that is arbitrary
code execution on the runner fleet — a persistent host with the fleet's network
position, toolchain, and filesystem. The guard that prevents it is a job-level
condition:

    if: github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository

This script finds every self-hosted job a `pull_request` event can reach without
that guard, and reports which of them reference secrets.

On the secret flag: GitHub does not pass secrets to fork-triggered workflows, so
a fork PR sees an empty string and cannot read them. That is GitHub's protection,
not the guard's. The flag is a blast-radius hint — `pull_request_target` *does*
expose secrets to fork code, running in the base repo's context — not evidence
that a fork could have leaked the secret.

Usage:
    check-fork-guards.py                 # scan $PWD
    check-fork-guards.py --root /opt     # scan every git repo under a directory
    check-fork-guards.py --root /opt --json

Exit status:
    0  no unguarded pull_request-triggered self-hosted jobs
    1  at least one found
    2  a workflow could not be parsed (the scan is incomplete, so "clean" is
       not a claim this run can make)

Notes:
  - A job with no `pull_request` trigger is correctly unguarded: nothing to
    guard. Those are reported separately as `push_only`, not as findings.
  - `pull_request_target` is treated as fork-reachable and is worse than
    `pull_request`, because it runs in the base repo's context with secrets
    available. It is called out separately.
  - Only job-level `if:` counts. A step-level guard leaves earlier steps
    exposed, so it is not accepted as a substitute.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError:  # pragma: no cover
    sys.stderr.write("error: PyYAML required (pip install pyyaml)\n")
    raise SystemExit(2)

GUARD_TOKEN = "head.repo.full_name"
PR_EVENTS = ("pull_request", "pull_request_target")


def triggers(doc: dict[str, Any]) -> set[str]:
    """Normalise the `on:` block, which YAML may parse as str, list or dict.

    `on` is a YAML 1.1 boolean, so PyYAML hands back the key as True.
    """
    raw = doc.get(True, doc.get("on"))
    if raw is None:
        return set()
    if isinstance(raw, str):
        return {raw}
    if isinstance(raw, list):
        return {str(x) for x in raw}
    if isinstance(raw, dict):
        return {str(k) for k in raw}
    return set()


def scan_workflow(path: Path) -> tuple[dict[str, Any], str | None]:
    try:
        doc = yaml.safe_load(path.read_text(encoding="utf-8", errors="replace"))
    except Exception as exc:  # noqa: BLE001 - any parse failure is a scan gap
        return {}, f"{type(exc).__name__}: {exc}"
    return (doc or {}), None


def audit(root: Path, label: str) -> dict[str, Any]:
    findings: list[dict[str, Any]] = []
    push_only: list[str] = []
    unreadable: list[dict[str, str]] = []
    jobs_seen = 0
    guarded = 0

    wf_dir = root / ".github" / "workflows"
    if not wf_dir.is_dir():
        return {"repo": label, "skipped": "no workflows directory"}

    for wf in sorted(list(wf_dir.glob("*.yml")) + list(wf_dir.glob("*.yaml"))):
        doc, err = scan_workflow(wf)
        if err is not None:
            unreadable.append({"file": str(wf), "error": err})
            continue
        evs = triggers(doc)
        fork_reachable = [e for e in PR_EVENTS if e in evs]
        for name, job in (doc.get("jobs") or {}).items():
            if not isinstance(job, dict):
                continue
            if "self-hosted" not in str(job.get("runs-on", "")):
                continue
            jobs_seen += 1
            cond = str(job.get("if") or "")
            has_guard = GUARD_TOKEN in cond
            if has_guard:
                guarded += 1
            body_text = json.dumps(job)  # catches env:, run:, with:, services:
            entry = {
                "repo": label,
                "file": wf.name,
                "job": name,
                "events": fork_reachable,
                "uses_secrets": "secrets." in body_text,
            }
            if fork_reachable and not has_guard:
                findings.append(entry)
            elif not fork_reachable:
                push_only.append(f"{label}/{wf.name}/{name}")

    return {
        "repo": label,
        "jobs_seen": jobs_seen,
        "guarded": guarded,
        "findings": findings,
        "push_only": push_only,
        "unreadable": unreadable,
    }


def repos_under(root: Path) -> list[tuple[Path, str]]:
    """Pair each repo directory with a readable label.

    The label is the remote's `owner/name` when there is a remote, because a
    local directory name says nothing about which repo a finding belongs to —
    and scratch clones (`agentctl-wt-pr25`, `m10check`) point at the same
    canonical repo, so the remote is what makes a finding actionable.
    """
    out = []
    for d in sorted(root.iterdir()):
        if not (d.is_dir() and (d / ".git").exists() and (d / ".github" / "workflows").is_dir()):
            continue
        remote = ""
        try:
            remote = subprocess.run(
                ["git", "-C", str(d), "remote", "get-url", "origin"],
                capture_output=True, text=True, timeout=5,
            ).stdout.strip()
        except Exception:  # noqa: BLE001
            pass
        label = d.name
        if remote:
            m = re.search(r"[:/]([^/:]+/[^/]+?)(?:\.git)?/?$", remote)
            label = f"{m.group(1) if m else remote} (local: {d.name})"
        out.append((d, label))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", default=".", help="repo root, or a directory of repos")
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    args = ap.parse_args()
    root = Path(args.root).resolve()

    if (root / ".github" / "workflows").is_dir():
        results = [audit(root, str(root))]
    else:
        results = [audit(p, label) for p, label in repos_under(root)]

    findings = [f for r in results for f in r.get("findings", [])]
    unreadable = [u for r in results for u in r.get("unreadable", [])]

    if args.json:
        print(json.dumps({"results": results, "findings": findings}, indent=2))
    else:
        for r in results:
            if r.get("skipped"):
                continue
            bad = r.get("findings", [])
            if not bad and not r.get("unreadable"):
                continue
            print(f"\n{r['repo']}")
            for f in bad:
                secret = "  ** USES SECRETS **" if f["uses_secrets"] else ""
                print(f"  {f['file']:24} {f['job']:24} events={','.join(f['events'])}{secret}")
            for u in r.get("unreadable", []):
                print(f"  UNREADABLE {u['file']}: {u['error']}")
        if not findings and not unreadable:
            print("clean: no unguarded fork-reachable self-hosted jobs")
        else:
            print(f"\n{len(findings)} unguarded fork-reachable self-hosted job(s)")
            with_secrets = [f for f in findings if f["uses_secrets"]]
            if with_secrets:
                print(f"{len(with_secrets)} of them carry secrets:")
                for f in with_secrets:
                    print(f"  {f['repo']}/{f['file']}/{f['job']}")

    if unreadable:
        return 2
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
