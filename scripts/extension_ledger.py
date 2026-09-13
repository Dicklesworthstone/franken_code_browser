#!/usr/bin/env python3
"""Validate the FCB upstream extension ledger without trusting it.

Each ledger entry records one upstream extension landing: the owner
repository, the public API surface, the exact committed revision, selected
features, the dependency closure delta, upstream tests, the FCB consumer
route, and a qualification status.  The ledger is supporting evidence for a
real integration, never a substitute for code.

The validator enforces the plan's landing discipline (plan sections 26.1,
27.1 and 27.12):

- every entry carries a complete evidence row;
- the pinned revision is a real commit object in the owner repository, not a
  research blob hash (plan section 32: reviewed blob hashes are not commit
  hashes);
- the pin is an immutable revision, never a moving branch name;
- release receipts never point at uncommitted working trees or path patches;
- the qualification status distinguishes existing implementation, a correctly
  integrated route, and measured acceleration.

Commit verification is only claimed when the owner repository is supplied via
``--repo``.  A commit that cannot be inspected is a bounded ``incomplete``
diagnostic; a commit object of the wrong kind (blob, tree, tag) is a hard
``rejected`` violation.

Output is a single machine-readable JSON document on stdout; diagnostics go
to stderr.  Exit codes mirror scripts/closure_probe.py: ``accepted`` 0,
``rejected`` 2, ``incomplete`` 3.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

SCHEMA_VERSION = "fcb.extension-ledger.v1"
TOOL_VERSION = "0.1.0"

# Plan section 26.1: the suite pin resolves committed origins, never local
# working trees.  These are the same first-party origins the closure probe
# admits.
ALLOWED_ORIGINS = (
    "https://github.com/Dicklesworthstone/asupersync",
    "https://github.com/Dicklesworthstone/coding_agent_session_search",
    "https://github.com/Dicklesworthstone/franken_code_browser",
    "https://github.com/Dicklesworthstone/franken_macos",
    "https://github.com/Dicklesworthstone/franken_markdown",
    "https://github.com/Dicklesworthstone/franken_manim",
    "https://github.com/Dicklesworthstone/franken_networkx",
    "https://github.com/Dicklesworthstone/franken_numpy",
    "https://github.com/Dicklesworthstone/franken_sqlite",
    "https://github.com/Dicklesworthstone/frankensqlite",
    "https://github.com/Dicklesworthstone/frankenterm",
    "https://github.com/Dicklesworthstone/frankentui",
    "https://github.com/Dicklesworthstone/franken_threed",
)

QUALIFICATION_STATUSES = ("implemented", "integrated_route", "measured_acceleration")

COMMIT_PATTERN = re.compile(r"\A[0-9a-f]{40}\Z")
GIT_TIMEOUT_SECONDS = 30
REQUIRED_ENTRY_FIELDS = (
    "owner",
    "origin",
    "extension",
    "public_api",
    "source",
    "features",
    "closure_delta",
    "upstream_tests",
    "fcb_consumer",
    "qualification",
)

EXIT_CODES = {"accepted": 0, "rejected": 2, "incomplete": 3}


def load_ledger(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        return None, f"ledger unreadable: {error}"
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        return None, f"ledger is not valid JSON: {error}"
    if not isinstance(document, dict):
        return None, "ledger document must be a JSON object"
    return document, None


def file_digest(path: Path) -> str | None:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError:
        return None


def _run_git(root: Path, *args: str) -> tuple[bool, str]:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), *args],
            check=True,
            capture_output=True,
            text=True,
            timeout=GIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.SubprocessError):
        return False, ""
    return True, result.stdout.strip()


def repo_receipt(root: Path) -> dict[str, str | None]:
    ok, head = _run_git(root, "rev-parse", "HEAD")
    return {
        "head": head if ok else None,
        "dirty": _run_git(root, "status", "--porcelain", "--untracked-files=all")[1] or None,
    }


def _is_local_path_origin(origin: str) -> bool:
    if origin.startswith(("file://", "/", "~", "../", "./")):
        return True
    if re.match(r"\A[A-Za-z]:[\\/]", origin):
        return True
    # A relative path-patch such as "../franken_markdown".
    is_bare_relative_path = origin.startswith("..")
    looks_like_url = "://" in origin
    return is_bare_relative_path or ("/" in origin and not looks_like_url)


def commit_object_kind(repository: Path, commit: str) -> str | None:
    """Return the git object kind of ``commit`` inside ``repository``.

    ``None`` means the object could not be inspected (missing repository,
    unknown object); a string result is the authoritative kind.
    """
    inspected, kind = _run_git(repository, "cat-file", "-t", commit)
    return kind if inspected else None


def _entry_violations(entry: dict[str, Any], index: int) -> list[dict[str, Any]]:
    violations: list[dict[str, Any]] = []

    def reject(code: str, detail: str, **extra: Any) -> None:
        item = {"row": index, "code": code, "detail": detail}
        item.update(extra)
        violations.append(item)

    for field in REQUIRED_ENTRY_FIELDS:
        if field not in entry:
            reject("MISSING_FIELD", f"entry is missing required field '{field}'", field=field)
    if violations:
        # Later checks index fields that may not exist; the missing-row
        # violations are already decisive.
        return violations

    for field in ("owner", "origin", "extension", "qualification"):
        if not isinstance(entry[field], str) or not entry[field].strip():
            reject("EMPTY_FIELD", f"'{field}' must be a non-empty string", field=field)
    for field in ("public_api", "upstream_tests", "features"):
        if not isinstance(entry[field], list):
            reject("EMPTY_FIELD", f"'{field}' must be a list", field=field)
        elif field != "features" and not entry[field]:
            reject("EMPTY_FIELD", f"'{field}' must name at least one item", field=field)
    if violations:
        return violations

    if entry["qualification"] not in QUALIFICATION_STATUSES:
        reject(
            "INVALID_STATUS",
            "qualification must be one of " + ", ".join(QUALIFICATION_STATUSES),
            field="qualification",
            seen=entry["qualification"],
        )

    origin = entry["origin"]
    if _is_local_path_origin(origin):
        reject("PATH_PATCH_RECEIPT", "origin must be a committed repository URL, not a local path", origin=origin)
    elif origin not in ALLOWED_ORIGINS:
        reject("DISALLOWED_ORIGIN", "origin is not a first-party allowed origin", origin=origin)
    expected_owner = origin.rsplit("/", 1)[-1]
    if entry["owner"] != expected_owner:
        reject(
            "OWNER_ORIGIN_MISMATCH",
            "owner must name the repository at the origin",
            owner=entry["owner"],
            expected=expected_owner,
        )

    source = entry["source"]
    if not isinstance(source, dict) or "commit" not in source:
        reject("MISSING_FIELD", "source must carry a 'commit' row", field="source.commit")
    else:
        commit = source["commit"]
        if not isinstance(commit, str) or not COMMIT_PATTERN.match(commit):
            reject(
                "MOVING_BRANCH_PIN",
                "commit must be a full 40-hex committed revision, not a branch or short pin",
                field="source.commit",
                seen=commit if isinstance(commit, str) else type(commit).__name__,
            )
    if violations:
        return violations

    closure_delta = entry["closure_delta"]
    if not isinstance(closure_delta, dict):
        reject("EMPTY_FIELD", "closure_delta must be an object with package lists", field="closure_delta")
    else:
        for side in ("added_packages", "removed_packages"):
            if side not in closure_delta or not isinstance(closure_delta[side], list):
                reject(
                    "MISSING_FIELD",
                    f"closure_delta is missing required list '{side}'",
                    field=f"closure_delta.{side}",
                )
    if violations:
        return violations

    consumer = entry["fcb_consumer"]
    if not isinstance(consumer, dict):
        reject("MISSING_FIELD", "fcb_consumer must be an object with route and evidence", field="fcb_consumer")
    else:
        for field in ("route", "evidence"):
            value = consumer.get(field)
            if not isinstance(value, str) or not value.strip():
                reject(
                    "MISSING_FIELD",
                    f"fcb_consumer is missing required row '{field}'",
                    field=f"fcb_consumer.{field}",
                )
    return violations


def verify_commits(
    entries: list[dict[str, Any]],
    repositories: dict[str, Path],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Check each pinned revision against its owner repository.

    Returns ``(violations, unverified)``.  A wrong-kind object is a
    violation; an uninspectable pin is recorded as unverified so the overall
    verdict degrades to ``incomplete`` instead of ``accepted``.
    """
    violations: list[dict[str, Any]] = []
    unverified: list[dict[str, Any]] = []
    for index, entry in enumerate(entries):
        repository = repositories.get(entry["owner"])
        if repository is None:
            unverified.append(
                {
                    "row": index,
                    "code": "COMMIT_UNVERIFIED",
                    "detail": "no owner repository supplied for commit inspection",
                    "owner": entry["owner"],
                }
            )
            continue
        kind = commit_object_kind(repository, entry["source"]["commit"])
        if kind is None:
            unverified.append(
                {
                    "row": index,
                    "code": "COMMIT_UNVERIFIED",
                    "detail": "owner repository does not contain the object (or is unreadable)",
                    "owner": entry["owner"],
                    "commit": entry["source"]["commit"],
                }
            )
        elif kind != "commit":
            violations.append(
                {
                    "row": index,
                    "code": "BLOB_ID_AS_COMMIT",
                    "detail": f"pinned object is a git {kind}, not a commit; research blob hashes are not revision pins",
                    "owner": entry["owner"],
                    "commit": entry["source"]["commit"],
                    "object_kind": kind,
                }
            )
    return violations, unverified


def evaluate(
    document: dict[str, Any],
    repositories: dict[str, Path],
) -> dict[str, Any]:
    violations: list[dict[str, Any]] = []
    unverified: list[dict[str, Any]] = []
    parse_error: str | None = None

    if document.get("schema") != SCHEMA_VERSION:
        violations.append(
            {
                "row": None,
                "code": "SCHEMA_MISMATCH",
                "detail": f"ledger schema must be {SCHEMA_VERSION}",
                "seen": document.get("schema"),
            }
        )
    entries = document.get("entries")
    if not isinstance(entries, list):
        parse_error = "ledger must carry an 'entries' list"
    elif not entries:
        parse_error = "ledger carries no entries; it gates nothing"
    else:
        for index, entry in enumerate(entries):
            if isinstance(entry, dict):
                violations.extend(_entry_violations(entry, index))
            else:
                violations.append(
                    {"row": index, "code": "EMPTY_FIELD", "detail": "entry must be a JSON object"}
                )
        structural = [item for item in violations if item["code"] != "BLOB_ID_AS_COMMIT"]
        if not structural:
            # Commit inspection only runs on structurally valid rows.
            commit_violations, unverified = verify_commits(entries, repositories)
            violations.extend(commit_violations)

    if violations:
        verdict = "rejected"
    elif parse_error is not None or unverified:
        verdict = "incomplete"
    else:
        verdict = "accepted"

    return {
        "schema": SCHEMA_VERSION,
        "tool_version": TOOL_VERSION,
        "verdict": verdict,
        "entry_count": len(entries) if isinstance(entries, list) else 0,
        "violations": violations,
        "unverified": unverified,
        "parse_error": parse_error,
    }


def parse_repo_specs(specs: list[str] | None) -> tuple[dict[str, Path], list[str]]:
    repositories: dict[str, Path] = {}
    errors: list[str] = []
    for spec in specs or ():
        owner, separator, path = spec.partition("=")
        if not separator or not owner or not path:
            errors.append(f"invalid --repo spec (want OWNER=PATH): {spec!r}")
            continue
        repositories[owner] = Path(path)
    return repositories, errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", required=True, help="path to the extension ledger JSON document")
    parser.add_argument(
        "--repo",
        action="append",
        default=[],
        metavar="OWNER=PATH",
        help="local checkout of an owner repository for commit inspection (repeatable)",
    )
    args = parser.parse_args(argv if argv is not None else sys.argv[1:])

    repositories, repo_errors = parse_repo_specs(args.repo)
    for error in repo_errors:
        print(error, file=sys.stderr)

    ledger_path = Path(args.ledger)
    document, load_error = load_ledger(ledger_path)

    receipt = {
        "ledger_digest": file_digest(ledger_path),
        "repositories": {owner: repo_receipt(path) for owner, path in sorted(repositories.items())},
    }

    if document is None:
        result: dict[str, Any] = {
            "schema": SCHEMA_VERSION,
            "tool_version": TOOL_VERSION,
            "verdict": "incomplete",
            "entry_count": 0,
            "violations": [],
            "unverified": [],
            "parse_error": load_error,
            "receipt": receipt,
        }
    else:
        result = evaluate(document, repositories)
        result["receipt"] = receipt

    json.dump(result, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return EXIT_CODES[result["verdict"]]


if __name__ == "__main__":
    raise SystemExit(main())
