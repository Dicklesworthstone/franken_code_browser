#!/usr/bin/env python3
"""Inspect a Cargo closure without compiling it.

Cargo metadata is the sole resolver.  It runs locked and offline, so the probe
never downloads, updates, or builds a graph.  If metadata cannot establish a
selected graph, the result is a bounded ``incomplete`` diagnostic rather than
a speculative lockfile interpretation.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import subprocess
import sys
from collections import defaultdict, deque
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = "fcb.closure-probe.v1"
PROBE_VERSION = "0.2.0"
METADATA_TIMEOUT_SECONDS = 120
FORBIDDEN_PACKAGES = frozenset(
    {
        "async-std", "cosmic-text", "electron", "egui", "freefont", "harfbuzz",
        "iced", "metal", "objc2", "rayon", "rayon-core", "serde", "serde_derive",
        "serde_json", "serde_yaml", "skia-safe", "tantivy", "tokio", "tokio-util",
        "tree-sitter", "webview", "wgpu", "winit",
    }
)
RUNTIME_FAMILIES = {"asupersync": "asupersync", "async-std": "async-std", "smol": "smol", "tokio": "tokio"}
DEFAULT_ALLOWED_ORIGINS = (
    "https://github.com/Dicklesworthstone/asupersync",
    "https://github.com/Dicklesworthstone/coding_agent_session_search",
    "https://github.com/Dicklesworthstone/franken_code_browser",
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


def _run_git(root: Path, *args: str) -> str | None:
    try:
        result = subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True, text=True)
    except (OSError, subprocess.CalledProcessError):
        return None
    return result.stdout.strip() or None


def git_receipt(root: Path) -> dict[str, str | None]:
    return {
        "revision": _run_git(root, "rev-parse", "HEAD"),
        "origin": _run_git(root, "config", "--get", "remote.origin.url"),
        "dirty": _run_git(root, "status", "--porcelain", "--untracked-files=all"),
    }


def file_digest(path: Path) -> str | None:
    try:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()
    except OSError:
        return None


def _canonical_origin(value: str) -> str:
    return value.removeprefix("git+").split("?", 1)[0].rstrip("/").removesuffix(".git")


def _origin_allowed(source: str | None, local: bool, allowed_origins: tuple[str, ...]) -> bool:
    if local:
        return True
    return source is not None and any(
        _canonical_origin(source) == _canonical_origin(expected) for expected in allowed_origins
    )


def _is_forbidden_package(name: str) -> bool:
    lowered = name.lower()
    return lowered in FORBIDDEN_PACKAGES or any(
        lowered.startswith(prefix) for prefix in ("serde-", "serde_", "tokio-", "tree-sitter-", "wgpu-", "winit-")
    )


def _shipping_dependency(dependency: dict[str, Any]) -> bool:
    dep_kinds = dependency.get("dep_kinds", [])
    if not isinstance(dep_kinds, list) or not dep_kinds:
        return True
    return any(
        isinstance(dep_kind, dict) and dep_kind.get("kind") in (None, "normal", "build")
        for dep_kind in dep_kinds
    )


def _dependency_kinds(dependency: dict[str, Any]) -> list[str]:
    dep_kinds = dependency.get("dep_kinds", [])
    kinds = ["normal" if dep_kind.get("kind") is None else str(dep_kind["kind"])
             for dep_kind in dep_kinds if isinstance(dep_kind, dict)]
    return kinds or ["normal"]


def cargo_metadata(
    root: Path,
    features: Iterable[str],
    filter_platform: str | None,
    no_default_features: bool,
) -> tuple[dict[str, Any] | None, str | None]:
    command = [
        "cargo", "metadata", "--manifest-path", str(root / "Cargo.toml"),
        "--format-version", "1", "--locked", "--offline",
    ]
    feature_values = tuple(features)
    if feature_values:
        command.extend(["--features", ",".join(feature_values)])
    if no_default_features:
        command.append("--no-default-features")
    if filter_platform:
        command.extend(["--filter-platform", filter_platform])
    try:
        completed = subprocess.run(
            command, check=False, capture_output=True, text=True, timeout=METADATA_TIMEOUT_SECONDS
        )
    except subprocess.TimeoutExpired:
        return None, f"cargo metadata exceeded {METADATA_TIMEOUT_SECONDS}s"
    except OSError as error:
        return None, f"cargo metadata could not start: {error}"
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip() or "no diagnostic"
        return None, f"cargo metadata exited {completed.returncode}: {detail}"
    try:
        document = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        return None, f"cargo metadata returned invalid JSON: {error}"
    if not isinstance(document, dict) or not isinstance(document.get("packages"), list):
        return None, "cargo metadata JSON lacks the package list"
    return document, None


def _metadata_package_map(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {
        str(package["id"]): package for package in document.get("packages", [])
        if isinstance(package, dict) and "id" in package
    }


def inspect_metadata_root(
    root: Path,
    document: dict[str, Any],
    selected_packages: tuple[str, ...],
    features: tuple[str, ...],
    filter_platform: str | None,
    no_default_features: bool,
    allowed_origins: tuple[str, ...],
) -> tuple[dict[str, Any], dict[str, dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]], dict[str, set[str]], dict[str, set[str]]]:
    packages = _metadata_package_map(document)
    workspace_members = [str(value) for value in document.get("workspace_members", [])]
    resolve = document.get("resolve") if isinstance(document.get("resolve"), dict) else {}
    resolve_nodes = {
        str(node["id"]): node for node in resolve.get("nodes", [])
        if isinstance(node, dict) and "id" in node
    }
    unresolved: list[dict[str, Any]] = []
    roots: list[str] = []
    if selected_packages:
        for selector in selected_packages:
            matches = [package_id for package_id, package in packages.items()
                       if package.get("name") == selector or package_id == selector]
            if not matches:
                unresolved.append({"root": str(root), "selector": selector, "reason": "selected package does not exist"})
            roots.extend(matches)
    elif resolve.get("root"):
        roots = [str(resolve["root"])]
    elif len(workspace_members) == 1:
        roots = workspace_members
    else:
        unresolved.append({"root": str(root), "reason": "virtual workspace has multiple members; pass --package"})
    if not roots:
        unresolved.append({"root": str(root), "reason": "selected root set is empty"})

    receipt = git_receipt(root)
    if receipt["revision"] is None:
        unresolved.append({"root": str(root), "reason": "git revision is unavailable"})
    if receipt["origin"] is None:
        unresolved.append({"root": str(root), "reason": "git origin is unavailable"})
    if receipt["dirty"]:
        unresolved.append({"root": str(root), "reason": "selected input has uncommitted or untracked bytes", "status": receipt["dirty"]})

    reachable: set[str] = set()
    queue = deque(roots)
    while queue:
        package_id = queue.popleft()
        if package_id in reachable:
            continue
        package = packages.get(package_id)
        node = resolve_nodes.get(package_id)
        if package is None or node is None:
            unresolved.append({"root": str(root), "package": package_id, "reason": "selected package is absent from resolve graph"})
            continue
        reachable.add(package_id)
        for dependency in node.get("deps", []):
            if isinstance(dependency, dict) and dependency.get("pkg") is not None and _shipping_dependency(dependency):
                queue.append(str(dependency["pkg"]))

    root_result: dict[str, Any] = {
        "path": str(root), "revision": receipt["revision"], "origin": receipt["origin"],
        "dirty": bool(receipt["dirty"]), "manifest_digest": file_digest(root / "Cargo.toml"),
        "lock_digest": file_digest(root / "Cargo.lock"), "manifest_count": 0,
        "lock_package_count": len(reachable), "lock_error": None, "metadata_error": None,
        "metadata_mode": "cargo-metadata", "selection": {
            "packages": list(selected_packages), "features": list(features),
            "filter_platform": filter_platform, "no_default_features": no_default_features,
        }, "invalid_manifests": [], "packages": [],
    }
    nodes: dict[str, dict[str, Any]] = {}
    edges: list[dict[str, Any]] = []
    violations: list[dict[str, Any]] = []
    runtime_versions: dict[str, set[str]] = defaultdict(set)
    runtime_sources: dict[str, set[str]] = defaultdict(set)
    for package_id in sorted(reachable):
        package = packages[package_id]
        manifest_path = Path(str(package.get("manifest_path", "")))
        source = str(package["source"]) if package.get("source") is not None else None
        inside_root = manifest_path.is_file() and manifest_path.is_relative_to(root)
        vendored = any(part.lower() in {"vendor", "third_party", "third-party"} for part in manifest_path.parts)
        local = source is None and inside_root and not vendored
        targets = package.get("targets", [])
        target_kinds = {str(kind) for target in targets if isinstance(target, dict) for kind in target.get("kind", [])}
        license_file = package.get("license_file")
        node = {
            "id": package_id, "name": str(package["name"]), "version": str(package["version"]),
            "source": source, "local": local,
            "manifest": str(manifest_path.relative_to(root)) if local else None,
            "manifest_digest": file_digest(manifest_path) if manifest_path.is_file() else None,
            "license": package.get("license"), "license_file": license_file,
            "kind": "proc-macro" if "proc-macro" in target_kinds else "normal",
            "build_script": "custom-build" in target_kinds, "native_links": package.get("links"),
            "features": list(resolve_nodes[package_id].get("features", [])),
            "allowed_origin": _origin_allowed(source, local, allowed_origins),
        }
        nodes[package_id] = node
        root_result["packages"].append(node)
        root_result["manifest_count"] += 1
        family = RUNTIME_FAMILIES.get(node["name"])
        if family:
            runtime_versions[family].add(node["version"])
            runtime_sources[family].add(source or "local")
        if _is_forbidden_package(node["name"]):
            violations.append({"code": "forbidden_dependency", "package": package_id, "message": f"forbidden dependency {node['name']!r} is in the resolved closure"})
        elif not node["allowed_origin"]:
            violations.append({"code": "unapproved_dependency_origin", "package": package_id, "source": source, "message": "resolved package origin is not local or an explicitly allowed first-party origin"})
        if not node["license"] and not node["license_file"]:
            unresolved.append({"root": str(root), "package": package_id, "reason": "resolved package has no license metadata"})
        if license_file:
            license_path = Path(str(license_file))
            if not license_path.is_absolute():
                license_path = manifest_path.parent / license_path
            if not license_path.is_file():
                violations.append({"code": "missing_license_file", "package": package_id, "license_file": str(license_file), "message": "manifest license-file does not exist in the selected input"})
        for dependency in resolve_nodes[package_id].get("deps", []):
            if not isinstance(dependency, dict) or not _shipping_dependency(dependency):
                continue
            dependency_id = dependency.get("pkg")
            if dependency_id not in reachable:
                unresolved.append({"root": str(root), "from": package_id, "dependency": dependency_id, "reason": "shipping dependency is absent from selected reachability"})
                continue
            edges.append({"from": package_id, "to": str(dependency_id), "kind": _dependency_kinds(dependency), "resolved": True})

    return root_result, nodes, edges, violations, unresolved, runtime_versions, runtime_sources


def inspect_roots(
    roots: Iterable[Path], allowed_origins: Iterable[str] = (), selected_packages: Iterable[str] = (),
    features: Iterable[str] = (), filter_platform: str | None = None, no_default_features: bool = False,
) -> dict[str, Any]:
    root_results: list[dict[str, Any]] = []
    all_violations: list[dict[str, Any]] = []
    all_nodes: dict[str, dict[str, Any]] = {}
    all_edges: list[dict[str, Any]] = []
    unresolved: list[dict[str, Any]] = []
    runtime_versions: dict[str, set[str]] = defaultdict(set)
    runtime_sources: dict[str, set[str]] = defaultdict(set)
    origins = tuple(allowed_origins) or DEFAULT_ALLOWED_ORIGINS
    selected = tuple(selected_packages)
    selected_features = tuple(features)
    for raw_root in roots:
        root = raw_root.resolve()
        receipt = git_receipt(root) if root.is_dir() else {"revision": None, "origin": None, "dirty": None}
        manifest_path = root / "Cargo.toml"
        if not root.is_dir() or not manifest_path.is_file():
            root_results.append({
                "path": str(root), "revision": receipt["revision"], "origin": receipt["origin"],
                "dirty": bool(receipt["dirty"]), "manifest_digest": file_digest(manifest_path),
                "lock_digest": file_digest(root / "Cargo.lock"), "manifest_count": 0,
                "lock_package_count": 0, "lock_error": "Cargo.toml is absent", "metadata_error": None,
                "metadata_mode": "not-cargo", "selection": {"packages": list(selected), "features": list(selected_features), "filter_platform": filter_platform, "no_default_features": no_default_features},
                "invalid_manifests": [], "packages": [],
            })
            all_violations.append({"code": "no_package_manifest", "root": str(root), "message": "input root has no Cargo package manifest"})
            continue
        metadata, metadata_error = cargo_metadata(root, selected_features, filter_platform, no_default_features)
        if metadata is None:
            root_results.append({
                "path": str(root), "revision": receipt["revision"], "origin": receipt["origin"],
                "dirty": bool(receipt["dirty"]), "manifest_digest": file_digest(manifest_path),
                "lock_digest": file_digest(root / "Cargo.lock"), "manifest_count": 0,
                "lock_package_count": 0, "lock_error": None, "metadata_error": metadata_error,
                "metadata_mode": "cargo-metadata", "selection": {"packages": list(selected), "features": list(selected_features), "filter_platform": filter_platform, "no_default_features": no_default_features},
                "invalid_manifests": [], "packages": [],
            })
            unresolved.append({"root": str(root), "reason": metadata_error})
            if receipt["revision"] is None:
                unresolved.append({"root": str(root), "reason": "git revision is unavailable"})
            if receipt["origin"] is None:
                unresolved.append({"root": str(root), "reason": "git origin is unavailable"})
            if receipt["dirty"]:
                unresolved.append({"root": str(root), "reason": "selected input has uncommitted or untracked bytes", "status": receipt["dirty"]})
            continue
        root_result, nodes, edges, violations, metadata_unresolved, metadata_runtimes, metadata_sources = inspect_metadata_root(
            root, metadata, selected, selected_features, filter_platform, no_default_features, origins
        )
        root_results.append(root_result)
        all_nodes.update(nodes)
        all_edges.extend(edges)
        all_violations.extend(violations)
        unresolved.extend(metadata_unresolved)
        for family, versions in metadata_runtimes.items():
            runtime_versions[family].update(versions)
        for family, sources in metadata_sources.items():
            runtime_sources[family].update(sources)

    for family, versions in sorted(runtime_versions.items()):
        if len(versions) > 1:
            all_violations.append({"code": "duplicate_runtime_version", "runtime": family, "versions": sorted(versions), "sources": sorted(runtime_sources[family]), "message": f"runtime family {family!r} resolves to multiple versions"})
    active_families = sorted(family for family, versions in runtime_versions.items() if versions)
    if len(active_families) > 1:
        all_violations.append({"code": "multiple_runtime_families", "runtimes": active_families, "message": "more than one asynchronous runtime family is present"})
    complete = bool(root_results) and all(result["manifest_count"] > 0 for result in root_results) and not unresolved
    qualification = "noncompliant" if all_violations else "qualified" if complete else "incomplete"
    return {
        "schema_version": SCHEMA_VERSION, "probe_version": PROBE_VERSION,
        "qualification": qualification, "complete": complete, "roots": root_results,
        "graph": {"nodes": sorted(all_nodes.values(), key=lambda node: node["id"]), "edges": all_edges, "node_count": len(all_nodes), "edge_count": len(all_edges)},
        "unresolved": unresolved, "violations": all_violations,
        "summary": {"root_count": len(root_results), "package_count": sum(result["manifest_count"] for result in root_results), "resolved_node_count": len(all_nodes), "unresolved_count": len(unresolved), "violation_count": len(all_violations), "runtime_families": active_families},
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", type=Path, required=True)
    parser.add_argument("--allow-origin", action="append", default=[])
    parser.add_argument("--package", action="append", default=[])
    parser.add_argument("--feature", action="append", default=[])
    parser.add_argument("--filter-platform")
    parser.add_argument("--no-default-features", action="store_true")
    parser.add_argument("--pretty", action="store_true")
    parser.add_argument("--extension-ledger")
    parser.add_argument("--extension-repo", action="append", default=[])
    return parser.parse_args(argv)


def _apply_extension_ledger(
    result: dict[str, Any],
    ledger_path: Path,
    repo_specs: list[str],
) -> dict[str, Any]:
    """Consume the upstream extension ledger and let it gate qualification.

    A rejected ledger means a landing receipt violates the closure and
    landing discipline, so a ``qualified`` result is downgraded to
    ``noncompliant``.  An unverifiable ledger degrades ``qualified`` to
    ``incomplete``; it never upgrades a failing result.
    """
    ledger_path = Path(ledger_path)
    module_spec = importlib.util.spec_from_file_location(
        "fcb_extension_ledger", Path(__file__).with_name("extension_ledger.py")
    )
    section: dict[str, Any]
    if module_spec is None or module_spec.loader is None:
        section = {"verdict": "incomplete", "violations": [], "unverified": [], "parse_error": "extension ledger module unavailable"}
    else:
        module = importlib.util.module_from_spec(module_spec)
        sys.modules[module_spec.name] = module
        module_spec.loader.exec_module(module)
        repositories, repo_errors = module.parse_repo_specs(repo_specs)
        for error in repo_errors:
            print(error, file=sys.stderr)
        document, load_error = module.load_ledger(ledger_path)
        if document is None:
            section = {
                "verdict": "incomplete",
                "violations": [],
                "unverified": [],
                "parse_error": load_error,
                "receipt": {"ledger_digest": module.file_digest(ledger_path)},
            }
        else:
            section = module.evaluate(document, repositories)
    result["extension_ledger"] = section
    if section["verdict"] == "rejected" and result["qualification"] == "qualified":
        result["qualification"] = "noncompliant"
    elif section["verdict"] == "incomplete" and result["qualification"] == "qualified":
        result["qualification"] = "incomplete"
    return result



def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    result = inspect_roots(args.root, args.allow_origin, args.package, args.feature, args.filter_platform, args.no_default_features)
    if args.extension_ledger is not None:
        result = _apply_extension_ledger(result, Path(args.extension_ledger), args.extension_repo)
    print(json.dumps(result, indent=2 if args.pretty else None, sort_keys=True))
    return {"qualified": 0, "noncompliant": 2, "incomplete": 3}.get(result["qualification"], 4)


if __name__ == "__main__":
    raise SystemExit(main())
