#!/usr/bin/env python3
"""Structural checks for COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md and sibling docs.

Checks documents only. A passing run proves nothing about any implementation.

  - work-package rows: unique, contiguous FCB-001..N, no dangling/self dependencies, acyclic
  - every package reachable from the release-acceptance package FCB-064
  - headless-lane packages do not transitively depend on native window/Metal/app packages
  - citation tags [R*]/[A*]/[B*] are all used and all defined
  - Contents anchors match the H2 headings; subsection numbering is contiguous
  - every §N / §N.M reference names an existing section
  - relative Markdown links in every top-level .md resolve to existing files
  - stated package counts in sibling docs match the plan
"""
from __future__ import annotations

import os
import re
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLAN = os.path.join(ROOT, "COMPREHENSIVE_PLAN_FOR_FRANKEN_CODE_BROWSER.md")
RELEASE = "FCB-064"
# Packages that must stay buildable/testable without AppKit, Metal or the standalone app.
HEADLESS = ["FCB-010", "FCB-013", "FCB-014", "FCB-025", "FCB-026", "FCB-027", "FCB-078", "FCB-084", "FCB-085"]
# Packages that imply a native window, Metal device or the standalone composition.
NATIVE = {"FCB-004", "FCB-005", "FCB-006", "FCB-008", "FCB-018", "FCB-070"}

errors: list[str] = []


def err(msg: str) -> None:
    errors.append(msg)


def main() -> int:
    text = open(PLAN, encoding="utf-8").read()

    rows = re.findall(r"^\| (FCB-\d{3}) \| (.*?) \| (.*?) \| (.*?) \|$", text, re.M)
    ids = [r[0] for r in rows]
    if len(ids) != len(set(ids)):
        err("duplicate work-package rows")
    nums = sorted(int(i[4:]) for i in ids)
    if nums != list(range(1, len(nums) + 1)):
        err(f"package numbers are not contiguous 1..{len(nums)}: {nums}")
    deps = {i: re.findall(r"FCB-\d{3}", d) for i, _, d, _ in rows}
    for i, ds in deps.items():
        for d in ds:
            if d not in deps:
                err(f"{i} depends on unknown {d}")
            if d == i:
                err(f"{i} depends on itself")

    color: dict[str, int] = {}

    def dfs(u: str, path: list[str]) -> None:
        color[u] = 1
        for v in deps.get(u, []):
            if color.get(v) == 1:
                err("dependency cycle: " + " -> ".join(path + [v]))
            elif v not in color and v in deps:
                dfs(v, path + [v])
        color[u] = 2

    for i in ids:
        if i not in color:
            dfs(i, [i])

    def closure(u: str, acc: set[str] | None = None) -> set[str]:
        acc = set() if acc is None else acc
        for v in deps.get(u, []):
            if v not in acc:
                acc.add(v)
                closure(v, acc)
        return acc

    if RELEASE in deps:
        unreachable = set(ids) - closure(RELEASE) - {RELEASE}
        if unreachable:
            err(f"not reachable from {RELEASE}: {sorted(unreachable)}")
    else:
        err(f"release package {RELEASE} missing")

    for h in HEADLESS:
        if h not in deps:
            err(f"headless package {h} missing")
            continue
        bad = sorted(closure(h) & NATIVE)
        if bad:
            err(f"headless package {h} transitively requires native/app packages {bad}")

    used = set(re.findall(r"\[([RAB]\d{1,2})\]", text))
    defined = set(re.findall(r"\*\*\[([RAB]\d{1,2})\]", text)) | set(re.findall(r"^\| (B\d{1,2}) \|", text, re.M))
    if used - defined:
        err(f"citations used but not defined: {sorted(used - defined)}")
    if defined - used:
        err(f"citations defined but never used: {sorted(defined - used)}")

    heads = re.findall(r"^## (\d+)\. (.*)$", text, re.M)

    def anchor(n: str, s: str) -> str:
        s = re.sub(r"[^\w\- ]", "", (n + ". " + s).lower())
        return "#" + s.replace(" ", "-")

    anchors = {anchor(n, s) for n, s in heads}
    toc = re.findall(r"^\d+\. \[(.*?)\]\((#.*?)\)", text, re.M)
    for title, a in toc:
        if a not in anchors:
            err(f"Contents anchor does not match a heading: {title} {a}")
    if len(toc) != len(heads):
        err(f"Contents has {len(toc)} entries but there are {len(heads)} H2 sections")

    subs = re.findall(r"^### (\d+)\.(\d+) ", text, re.M)
    by: dict[int, list[int]] = defaultdict(list)
    for a, b in subs:
        by[int(a)].append(int(b))
    for sec, lst in sorted(by.items()):
        if lst != list(range(1, len(lst) + 1)):
            err(f"subsection numbering in section {sec} is {lst}")

    known = {f"{a}.{b}" for a, b in subs} | {n for n, _ in heads}
    for ref in sorted(set(re.findall(r"§§?\s?(\d+(?:\.\d+)?)", text))):
        if ref not in known:
            err(f"§{ref} does not name an existing section")

    plan_count = len(ids)
    for name in sorted(os.listdir(ROOT)):
        if not name.endswith(".md"):
            continue
        body = open(os.path.join(ROOT, name), encoding="utf-8").read()
        for target in re.findall(r"\]\(((?!https?://|#|mailto:)[^)\s]+)\)", body):
            path = target.split("#", 1)[0]
            if path and not os.path.exists(os.path.join(ROOT, path)):
                err(f"{name}: broken link {target}")
        if name != os.path.basename(PLAN):
            for m in re.finditer(r"(\d+)(?:-package| work packages| planned work packages)", body):
                if int(m.group(1)) != plan_count:
                    err(f"{name}: states {m.group(1)} packages, plan has {plan_count}")
            for m in re.finditer(r"FCB-001`?–`?FCB-(\d{3})", body):
                if int(m.group(1)) != plan_count:
                    err(f"{name}: ID range ends at FCB-{m.group(1)}, plan has {plan_count}")

    if errors:
        for e in errors:
            print("FAIL:", e)
        return 1
    print(f"OK: {plan_count} packages, {len(heads)} sections, {len(used)} citations, "
          f"{len(HEADLESS)} headless packages isolated from {sorted(NATIVE)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
