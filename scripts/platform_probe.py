#!/usr/bin/env python3
"""Capture and validate FCB's selected Rust/Apple headless baseline.

This is intentionally metadata-only: it invokes version and SDK discovery
commands, never Cargo build/test, and never opens a source root or network
connection.  The resulting JSON is a reproducible decision receipt, not native
ABI or GPU qualification.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from dataclasses import dataclass
from typing import Callable, Mapping, Sequence


SCHEMA_VERSION = "fcb.platform-probe.v1"
PROBE_VERSION = "0.1.0"


@dataclass(frozen=True)
class Selection:
    toolchain: str = "nightly-2026-09-07"
    edition: str = "2024"
    target: str = "aarch64-apple-darwin"
    deployment_target: str = "14.0"
    sdk_major_minor: str = "26.1"
    xcode: str = "26.1.1"
    sandbox_model: str = "read-only-root-grants"


SELECTED = Selection()

CONSEQUENCES: Mapping[str, str] = {
    "ipc": "Headless consumers stay inert; an embedding host owns any bounded IPC and runtime.",
    "watcher": "Filesystem watching is opt-in and owned by the host; the baseline consumer performs no scan.",
    "font": "Font fallback and font-file access remain explicit platform services; no font work occurs at construction.",
    "export": "Exports require an explicit destination and are never inferred from a source-root grant.",
    "signing": "Native Mac distribution uses the selected SDK plus explicit signing/notarization; headless probes remain unsigned metadata tools.",
    "sandbox": "Opening source grants read-only confined roots; source content cannot execute commands or trigger network fetches.",
}


CommandRunner = Callable[[Sequence[str]], tuple[int, str, str]]


def run_command(command: Sequence[str]) -> tuple[int, str, str]:
    try:
        completed = subprocess.run(command, check=False, capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as error:
        return 127, "", str(error)
    return completed.returncode, completed.stdout.strip(), completed.stderr.strip()


def _fact(run: CommandRunner, command: Sequence[str]) -> dict[str, object]:
    code, stdout, stderr = run(command)
    return {"command": list(command), "exit_code": code, "stdout": stdout, "stderr": stderr}


def collect_facts(run: CommandRunner = run_command) -> dict[str, object]:
    """Collect only version/path metadata needed to validate ``SELECTED``."""
    commands = {
        "architecture": ("uname", "-m"),
        "os_version": ("sw_vers", "-productVersion"),
        "xcode_path": ("xcode-select", "-p"),
        "xcode_version": ("xcodebuild", "-version"),
        "sdk_path": ("xcrun", "--sdk", "macosx", "--show-sdk-path"),
        "sdk_version": ("xcrun", "--sdk", "macosx", "--show-sdk-version"),
        "clang_path": ("xcrun", "--find", "clang"),
        "metal_path": ("xcrun", "--find", "metal"),
        "rustc": ("rustup", "run", SELECTED.toolchain, "rustc", "--version", "--verbose"),
        "toolchains": ("rustup", "toolchain", "list"),
    }
    return {name: _fact(run, command) for name, command in commands.items()}


def _stdout(facts: Mapping[str, object], key: str) -> str:
    value = facts.get(key)
    return str(value.get("stdout", "")) if isinstance(value, dict) else ""


def validate_facts(facts: Mapping[str, object], selection: Selection = SELECTED) -> list[dict[str, str]]:
    """Return deterministic failures; an empty list means the decision is observed."""
    failures: list[dict[str, str]] = []

    def require(key: str, needle: str, reason: str) -> None:
        if needle not in _stdout(facts, key):
            failures.append({"code": key + "_mismatch", "reason": reason})

    require("architecture", "arm64", "selected target requires Apple Silicon arm64")
    require("sdk_version", selection.sdk_major_minor, "selected Apple SDK is not the observed SDK")
    require("xcode_version", "Xcode " + selection.xcode, "selected Xcode version is not observed")
    require("xcode_path", "/Applications/Xcode.app/Contents/Developer", "Xcode developer directory is not selected")
    require("sdk_path", "MacOSX" + selection.sdk_major_minor.replace(".", ".") + ".sdk", "selected SDK path is not observed")
    require("rustc", "nightly", "selected compiler is not a nightly toolchain")
    require("rustc", "host: " + selection.target, "selected compiler host does not match the deployment target")
    if selection.edition != "2024":
        failures.append({"code": "edition_mismatch", "reason": "FCB authoritative crates require Rust edition 2024"})
    if not re.search(r"commit-date:\s+20\d\d-\d\d-\d\d", _stdout(facts, "rustc")):
        failures.append({"code": "undated_toolchain", "reason": "Rust compiler receipt has no dated commit"})
    if selection.toolchain not in _stdout(facts, "toolchains"):
        failures.append({"code": "toolchain_not_installed", "reason": "selected dated toolchain is not installed"})
    for key in ("xcode_path", "sdk_path", "clang_path", "metal_path"):
        fact = facts.get(key)
        if not isinstance(fact, dict) or int(fact.get("exit_code", 127)) != 0:
            failures.append({"code": key + "_unavailable", "reason": "required Apple metadata command failed"})
    return failures


def decision(facts: Mapping[str, object], selection: Selection = SELECTED) -> dict[str, object]:
    failures = validate_facts(facts, selection)
    return {
        "schema_version": SCHEMA_VERSION,
        "probe_version": PROBE_VERSION,
        "decision": {
            "toolchain": selection.toolchain,
            "edition": selection.edition,
            "target": selection.target,
            "deployment_target": selection.deployment_target,
            "sdk_major_minor": selection.sdk_major_minor,
            "xcode": selection.xcode,
            "sandbox_model": selection.sandbox_model,
            "consequences": dict(CONSEQUENCES),
        },
        "observed": dict(facts),
        "status": "selected" if not failures else "blocked",
        "violations": failures,
        "proof_boundary": "toolchain and platform metadata only; no Cargo compilation, native ABI, GPU, or signing qualification",
    }


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pretty", action="store_true")
    args = parser.parse_args(argv)
    result = decision(collect_facts())
    print(json.dumps(result, indent=2 if args.pretty else None, sort_keys=True))
    return 0 if result["status"] == "selected" else 2


if __name__ == "__main__":
    raise SystemExit(main())
