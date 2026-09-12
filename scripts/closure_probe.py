#!/usr/bin/env python3
"""Inspect a Cargo closure without compiling it.

The probe deliberately works from Cargo manifests and an existing Cargo.lock.
It never resolves versions, downloads crates, or runs a build.  A missing lock
file is therefore an honest ``incomplete`` result, not an inferred green.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from collections import defaultdict, deque
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = "fcb.closure-probe.v1"
PROBE_VERSION = "0.1.0"

FORBIDDEN_PACKAGES = frozenset(
    {
        "async-std",
        "cosmic-text",
        "electron",
        "egui",
        "freefont",
        "harfbuzz",
        "iced",
        "metal",
        "objc2",
        "rayon",
        "rayon-core",
        "serde",
        "serde_derive",
        "serde_json",
        "serde_yaml",
        "skia-safe",
        "tantivy",
        "tokio",
        "tokio-util",
        "tree-sitter",
        "webview",
        "wgpu",
        "winit",
    }
)

RUNTIME_FAMILIES = {
    "asupersync": "asupersync",
    "async-std": "async-std",
    "smol": "smol",
    "tokio": "tokio",
}

VERSION_RE = re.compile(r"^(?P<name>[^ ]+)(?: (?P<version>[^ ]+))?(?: \(.+\))?$")
DEFAULT_ALLOWED_ORIGINS = (
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


@dataclass(frozen=True)
class DependencyDecl:
    name: str
    kind: str
    requested_version: str | None
    path: str | None
    git: str | None
    features: tuple[str, ...]
    default_features: bool


@dataclass
class ManifestInfo:
    path: Path
    package_name: str
    package_version: str
    license: str | None
    license_file: str | None
    proc_macro: bool
    has_build_script: bool
    native_links: str | None
    dependencies: list[DependencyDecl] = field(default_factory=list)

    @property
    def package_key(self) -> tuple[str, str]:
        return self.package_name, self.package_version


@dataclass(frozen=True)
class LockPackage:
    name: str
    version: str
    source: str | None
    dependencies: tuple[str, ...]

    @property
    def key(self) -> str:
        return package_key(self.name, self.version, self.source)


def package_key(name: str, version: str, source: str | None) -> str:
    return f"{name}@{version}@{source or 'local'}"


def _run_git(root: Path, *args: str) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), *args],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    value = result.stdout.strip()
    return value or None


def git_receipt(root: Path) -> dict[str, str | None]:
    return {
        "revision": _run_git(root, "rev-parse", "HEAD"),
        "origin": _run_git(root, "config", "--get", "remote.origin.url"),
        "dirty": _run_git(root, "status", "--porcelain", "--untracked-files=all"),
    }


def _iter_dependency_tables(document: dict[str, Any]) -> Iterable[tuple[str, dict[str, Any]]]:
    for kind, key in (
        ("normal", "dependencies"),
        ("build", "build-dependencies"),
        ("dev", "dev-dependencies"),
    ):
        table = document.get(key, {})
        if isinstance(table, dict):
            yield kind, table

    target_tables = document.get("target", {})
    if not isinstance(target_tables, dict):
        return
    for target_table in target_tables.values():
        if not isinstance(target_table, dict):
            continue
        for kind, key in (
            ("normal", "dependencies"),
            ("build", "build-dependencies"),
            ("dev", "dev-dependencies"),
        ):
            table = target_table.get(key, {})
            if isinstance(table, dict):
                yield kind, table


def _dependency_decl(name: str, raw: Any, kind: str) -> DependencyDecl:
    if isinstance(raw, str):
        return DependencyDecl(name, kind, raw, None, None, (), True)
    if not isinstance(raw, dict):
        return DependencyDecl(name, kind, None, None, None, (), True)
    package_name = str(raw.get("package", name))
    features = raw.get("features", ())
    if not isinstance(features, list):
        features = ()
    return DependencyDecl(
        package_name,
        kind,
        str(raw["version"]) if "version" in raw else None,
        str(raw["path"]) if "path" in raw else None,
        str(raw["git"]) if "git" in raw else None,
        tuple(str(feature) for feature in features),
        bool(raw.get("default-features", True)),
    )


def read_manifest(path: Path, workspace_defaults: dict[str, str] | None = None) -> ManifestInfo | None:
    try:
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError):
        return None

    package = document.get("package")
    if not isinstance(package, dict) or "name" not in package:
        return None
    version_value = package.get("version")
    if isinstance(version_value, dict) and version_value.get("workspace"):
        version_value = (workspace_defaults or {}).get("version")
    if version_value is None:
        return None
    license_value = package.get("license")
    if isinstance(license_value, dict) and license_value.get("workspace"):
        license_value = (workspace_defaults or {}).get("license")
    lib = document.get("lib", {})
    proc_macro = isinstance(lib, dict) and bool(lib.get("proc-macro", False))
    build_value = package.get("build")
    has_build_script = bool(build_value) or (path.parent / "build.rs").is_file()
    dependencies = [
        _dependency_decl(name, raw, kind)
        for kind, table in _iter_dependency_tables(document)
        for name, raw in table.items()
    ]
    return ManifestInfo(
        path=path,
        package_name=str(package["name"]),
        package_version=str(version_value),
        license=str(license_value) if license_value is not None else None,
        license_file=str(package["license-file"]) if "license-file" in package else None,
        proc_macro=proc_macro,
        has_build_script=has_build_script,
        native_links=str(package["links"]) if "links" in package else None,
        dependencies=dependencies,
    )


def discover_manifests(root: Path) -> list[ManifestInfo]:
    paths = sorted(
        path
        for path in root.rglob("Cargo.toml")
        if ".git" not in path.parts and "target" not in path.parts
    )
    manifests = []
    for path in paths:
        manifest = read_manifest(path)
        if manifest is not None:
            manifests.append(manifest)
    return manifests


def read_lock(root: Path) -> tuple[list[LockPackage], str | None]:
    path = root / "Cargo.lock"
    if not path.is_file():
        return [], "Cargo.lock is absent; registry and git dependency resolution is unknown"
    try:
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as error:
        return [], f"Cargo.lock cannot be parsed: {error}"
    packages = []
    for raw in document.get("package", []):
        if not isinstance(raw, dict) or "name" not in raw or "version" not in raw:
            return [], "Cargo.lock contains a package entry without name/version"
        dependencies = raw.get("dependencies", [])
        if not isinstance(dependencies, list):
            return [], f"Cargo.lock dependencies for {raw['name']} are not a list"
        packages.append(
            LockPackage(
                name=str(raw["name"]),
                version=str(raw["version"]),
                source=str(raw["source"]) if "source" in raw else None,
                dependencies=tuple(str(value) for value in dependencies),
            )
        )
    if not packages:
        return [], "Cargo.lock contains no package entries"
    return packages, None


def parse_lock_dependency(value: str) -> tuple[str, str | None]:
    match = VERSION_RE.match(value)
    if match is None:
        return value, None
    return match.group("name"), match.group("version")


def _manifest_by_key(manifests: Iterable[ManifestInfo]) -> dict[tuple[str, str], ManifestInfo]:
    return {manifest.package_key: manifest for manifest in manifests}


def _local_dependency_key(
    manifest: ManifestInfo, dependency: DependencyDecl, manifests: dict[tuple[str, str], ManifestInfo]
) -> tuple[str, str] | None:
    if dependency.path is None:
        return None
    candidate_root = (manifest.path.parent / dependency.path).resolve()
    candidate = candidate_root / "Cargo.toml"
    target = read_manifest(candidate)
    if target is None:
        return None
    return target.package_key if target.package_key in manifests else None


def _origin_allowed(source: str | None, local: bool, allowed_origins: tuple[str, ...]) -> bool:
    if local:
        return True
    if source is None:
        return False
    def canonical(value: str) -> str:
        value = value.removeprefix("git+").split("?", 1)[0].rstrip("/")
        return value.removesuffix(".git")

    actual = canonical(source)
    return any(actual == canonical(prefix) for prefix in allowed_origins)


def file_digest(path: Path) -> str | None:
    try:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        return digest.hexdigest()
    except OSError:
        return None


def _is_forbidden_package(name: str) -> bool:
    lowered = name.lower()
    return (
        lowered in FORBIDDEN_PACKAGES
        or lowered.startswith("serde-")
        or lowered.startswith("serde_")
        or lowered.startswith("tokio-")
        or lowered.startswith("tree-sitter-")
        or lowered.startswith("wgpu-")
        or lowered.startswith("winit-")
    )


def cargo_metadata(
    root: Path,
    features: Iterable[str],
    filter_platform: str | None,
) -> tuple[dict[str, Any] | None, str | None]:
    command = [
        "cargo",
        "metadata",
        "--manifest-path",
        str(root / "Cargo.toml"),
        "--format-version",
        "1",
        "--locked",
        "--offline",
    ]
    feature_values = tuple(features)
    if feature_values:
        command.extend(["--features", ",".join(feature_values)])
    if filter_platform:
        command.extend(["--filter-platform", filter_platform])
    try:
        completed = subprocess.run(command, check=False, capture_output=True, text=True)
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
        str(package["id"]): package
        for package in document.get("packages", [])
        if isinstance(package, dict) and "id" in package
    }


def inspect_metadata_root(
    root: Path,
    document: dict[str, Any],
    selected_packages: tuple[str, ...],
    features: tuple[str, ...],
    filter_platform: str | None,
    allowed_origins: tuple[str, ...],
) -> tuple[dict[str, Any], dict[str, dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]], dict[str, set[str]], dict[str, set[str]]]:
    packages = _metadata_package_map(document)
    workspace_members = [str(value) for value in document.get("workspace_members", [])]
    resolve = document.get("resolve") if isinstance(document.get("resolve"), dict) else {}
    resolve_nodes = {
        str(node["id"]): node
        for node in resolve.get("nodes", [])
        if isinstance(node, dict) and "id" in node
    }
    chosen_ids = [str(value) for value in selected_packages]
    if chosen_ids:
        roots = [
            package_id
            for package_id, package in packages.items()
            if package.get("name") in chosen_ids or package_id in chosen_ids
        ]
    elif resolve.get("root"):
        roots = [str(resolve["root"])]
    elif len(workspace_members) == 1:
        roots = workspace_members
    else:
        roots = workspace_members

    unresolved: list[dict[str, Any]] = []
    if not chosen_ids and len(workspace_members) > 1 and not resolve.get("root"):
        unresolved.append(
            {
                "root": str(root),
                "reason": "virtual workspace has multiple members; pass --package to select a shipping root",
            }
        )
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
        node = resolve_nodes.get(package_id)
        package = packages.get(package_id)
        if node is None or package is None:
            unresolved.append({"root": str(root), "package": package_id, "reason": "selected package is absent from resolve graph"})
            continue
        reachable.add(package_id)
        for dependency in node.get("deps", []):
            if isinstance(dependency, dict) and dependency.get("pkg") is not None:
                queue.append(str(dependency["pkg"]))

    manifest_paths = [
        Path(str(package["manifest_path"]))
        for package_id in reachable
        if (package := packages.get(package_id)) and package.get("manifest_path")
    ]
    root_result: dict[str, Any] = {
        "path": str(root),
        "revision": receipt["revision"],
        "origin": receipt["origin"],
        "dirty": bool(receipt["dirty"]),
        "manifest_digest": file_digest(root / "Cargo.toml"),
        "lock_digest": file_digest(root / "Cargo.lock"),
        "manifest_count": len(manifest_paths),
        "lock_package_count": len(reachable),
        "lock_error": None,
        "metadata_mode": "cargo-metadata",
        "selection": {
            "packages": list(selected_packages),
            "features": list(features),
            "filter_platform": filter_platform,
        },
        "invalid_manifests": [],
        "packages": [],
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
        local = source is None and manifest_path.is_relative_to(root)
        targets = package.get("targets", [])
        target_kinds = {
            str(kind)
            for target in targets
            if isinstance(target, dict)
            for kind in target.get("kind", [])
        }
        node_record = resolve_nodes[package_id]
        node = {
            "id": package_id,
            "name": str(package["name"]),
            "version": str(package["version"]),
            "source": source,
            "local": local,
            "manifest": str(manifest_path.relative_to(root)) if local else None,
            "manifest_digest": file_digest(manifest_path) if manifest_path.is_file() else None,
            "license": package.get("license"),
            "license_file": package.get("license_file"),
            "kind": "proc-macro" if "proc-macro" in target_kinds else "normal",
            "build_script": "custom-build" in target_kinds,
            "native_links": package.get("links"),
            "features": list(node_record.get("features", [])),
            "allowed_origin": _origin_allowed(source, local, allowed_origins),
        }
        nodes[package_id] = node
        root_result["packages"].append(node)
        family = RUNTIME_FAMILIES.get(str(package["name"]))
        if family:
            runtime_versions[family].add(str(package["version"]))
            runtime_sources[family].add(source or "local")
        if _is_forbidden_package(str(package["name"])):
            violations.append({"code": "forbidden_dependency", "package": package_id, "message": f"forbidden dependency {package['name']!r} is in the resolved closure"})
        elif not node["allowed_origin"]:
            violations.append({"code": "unapproved_dependency_origin", "package": package_id, "source": source, "message": "resolved package origin is not local or an explicitly allowed first-party origin"})
        if not node["license"] and not node["license_file"]:
            unresolved.append({"root": str(root), "package": package_id, "reason": "resolved package has no license metadata"})
        for dependency in node_record.get("deps", []):
            if not isinstance(dependency, dict) or dependency.get("pkg") not in reachable:
                continue
            dep_kinds = dependency.get("dep_kinds", [])
            kinds = [str(kind.get("kind")) for kind in dep_kinds if isinstance(kind, dict)] or ["normal"]
            edges.append({"from": package_id, "to": str(dependency["pkg"]), "kind": kinds, "resolved": True})

    return root_result, nodes, edges, violations, unresolved, runtime_versions, runtime_sources


def inspect_roots(
    roots: Iterable[Path],
    allowed_origins: Iterable[str] = (),
    selected_packages: Iterable[str] = (),
    features: Iterable[str] = (),
    filter_platform: str | None = None,
) -> dict[str, Any]:
    root_results: list[dict[str, Any]] = []
    all_violations: list[dict[str, Any]] = []
    all_nodes: dict[str, dict[str, Any]] = {}
    all_edges: list[dict[str, Any]] = []
    unresolved: list[dict[str, Any]] = []
    runtime_versions: dict[str, set[str]] = defaultdict(set)
    runtime_sources: dict[str, set[str]] = defaultdict(set)
    allowed_origin_prefixes = tuple(allowed_origins) or DEFAULT_ALLOWED_ORIGINS
    selected_package_names = tuple(selected_packages)
    selected_features = tuple(features)

    for raw_root in roots:
        root = raw_root.resolve()
        metadata_error: str | None = None
        if root.is_dir() and (root / "Cargo.toml").is_file():
            metadata, metadata_error = cargo_metadata(root, selected_features, filter_platform)
            if metadata is not None:
                root_result, nodes, edges, violations, metadata_unresolved, metadata_runtimes, metadata_sources = inspect_metadata_root(
                    root,
                    metadata,
                    selected_package_names,
                    selected_features,
                    filter_platform,
                    allowed_origin_prefixes,
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
                continue
        manifest_paths = sorted(
            path
            for path in root.rglob("Cargo.toml")
            if ".git" not in path.parts and "target" not in path.parts
        ) if root.is_dir() else []
        workspace_defaults: dict[str, str] = {}
        root_document: dict[str, Any] = {}
        if root.is_dir() and (root / "Cargo.toml").is_file():
            try:
                with (root / "Cargo.toml").open("rb") as handle:
                    root_document = tomllib.load(handle)
                workspace_package = root_document.get("workspace", {}).get("package", {})
                if isinstance(workspace_package, dict):
                    for key in ("version", "license"):
                        if key in workspace_package and isinstance(workspace_package[key], str):
                            workspace_defaults[key] = workspace_package[key]
            except (OSError, tomllib.TOMLDecodeError):
                root_document = {}
        manifests: list[ManifestInfo] = []
        invalid_manifests: list[str] = []
        for path in manifest_paths:
            manifest = read_manifest(path, workspace_defaults)
            if manifest is None:
                try:
                    with path.open("rb") as handle:
                        document = tomllib.load(handle)
                except (OSError, tomllib.TOMLDecodeError):
                    invalid_manifests.append(str(path.relative_to(root)))
                else:
                    # A virtual workspace manifest is valid input, not a package.
                    if "package" in document:
                        invalid_manifests.append(str(path.relative_to(root)))
            else:
                manifests.append(manifest)
        lock_packages, lock_error = read_lock(root) if root.is_dir() else ([], "root is not a directory")
        receipt = git_receipt(root) if root.is_dir() else {"revision": None, "origin": None, "dirty": None}
        root_result = {
            "path": str(root),
            "revision": receipt["revision"],
            "origin": receipt["origin"],
            "dirty": bool(receipt["dirty"]),
            "manifest_digest": file_digest(root / "Cargo.toml") if root.is_dir() else None,
            "lock_digest": file_digest(root / "Cargo.lock") if root.is_dir() else None,
            "manifest_count": len(manifests),
            "lock_package_count": len(lock_packages),
            "lock_error": lock_error,
            "metadata_error": metadata_error if root.is_dir() and (root / "Cargo.toml").is_file() else None,
            "metadata_mode": "lockfile-fallback",
            "selection": {
                "packages": list(selected_package_names),
                "features": list(selected_features),
                "filter_platform": filter_platform,
            },
            "invalid_manifests": invalid_manifests,
            "packages": [],
        }
        root_results.append(root_result)

        if not manifests:
            all_violations.append(
                {
                    "code": "no_package_manifest",
                    "root": str(root),
                    "message": "input root has no parseable Cargo package manifest",
                }
            )
            continue
        if receipt["revision"] is None:
            unresolved.append({"root": str(root), "reason": "git revision is unavailable"})
        if receipt["origin"] is None:
            unresolved.append({"root": str(root), "reason": "git origin is unavailable"})
        if receipt["dirty"]:
            unresolved.append({"root": str(root), "reason": "selected input has uncommitted or untracked bytes", "status": receipt["dirty"]})
        if metadata_error is not None:
            unresolved.append({"root": str(root), "reason": metadata_error})
        if invalid_manifests:
            all_violations.append(
                {
                    "code": "invalid_manifest",
                    "root": str(root),
                    "message": "one or more Cargo.toml files could not be parsed",
                    "manifests": invalid_manifests,
                }
            )

        manifest_map = _manifest_by_key(manifests)
        lock_map = {package.key: package for package in lock_packages}
        lock_by_name: dict[str, list[LockPackage]] = defaultdict(list)
        for package in lock_packages:
            lock_by_name[package.name].append(package)

        for manifest in manifests:
            if manifest.license is None and manifest.license_file is None:
                unresolved.append(
                    {
                        "root": str(root),
                        "package": f"{manifest.package_name}@{manifest.package_version}",
                        "reason": "package has no license or license-file declaration",
                    }
                )
            elif manifest.license_file and not (manifest.path.parent / manifest.license_file).is_file():
                all_violations.append(
                    {
                        "code": "missing_license_file",
                        "package": f"{manifest.package_name}@{manifest.package_version}",
                        "license_file": manifest.license_file,
                        "message": "manifest license-file does not exist in the selected input",
                    }
                )
            package_entry = {
                "name": manifest.package_name,
                "version": manifest.package_version,
                "manifest": str(manifest.path.relative_to(root)),
                "manifest_digest": file_digest(manifest.path),
                "license": manifest.license,
                "license_file": manifest.license_file,
                "kind": "proc-macro" if manifest.proc_macro else "normal",
                "build_script": manifest.has_build_script,
                "native_links": manifest.native_links,
                "dependencies": [
                    {
                        "name": dependency.name,
                        "kind": dependency.kind,
                        "version": dependency.requested_version,
                        "path": dependency.path,
                        "git": dependency.git,
                        "features": list(dependency.features),
                        "default_features": dependency.default_features,
                    }
                    for dependency in manifest.dependencies
                ],
            }
            root_result["packages"].append(package_entry)
            for dependency in manifest.dependencies:
                if dependency.kind == "dev":
                    continue
                if _is_forbidden_package(dependency.name):
                    all_violations.append(
                        {
                            "code": "forbidden_dependency",
                            "from": manifest.package_name,
                            "dependency": dependency.name,
                            "message": f"forbidden dependency {dependency.name!r} is declared in the shipping graph",
                        }
                    )
                family = RUNTIME_FAMILIES.get(dependency.name)
                if family and dependency.requested_version:
                    runtime_versions[family].add(dependency.requested_version)
                    runtime_sources[family].add(dependency.git or "declared")
                local_key = _local_dependency_key(manifest, dependency, manifest_map)
                if local_key is None and dependency.path is not None:
                    unresolved.append(
                        {
                            "root": str(root),
                            "from": manifest.package_name,
                            "dependency": dependency.name,
                            "reason": "path dependency manifest is missing or outside discovered inputs",
                        }
                    )
                if local_key is None and dependency.path is None and not _is_forbidden_package(dependency.name):
                    unresolved.append(
                        {
                            "root": str(root),
                            "from": manifest.package_name,
                            "dependency": dependency.name,
                            "reason": "non-path dependency requires an existing lock graph",
                        }
                    )
                all_edges.append(
                    {
                        "from": f"{manifest.package_name}@{manifest.package_version}",
                        "to": dependency.name,
                        "kind": dependency.kind,
                        "declared_version": dependency.requested_version,
                        "features": list(dependency.features),
                        "default_features": dependency.default_features,
                        "resolved": local_key is not None,
                    }
                )

        selected_manifest_keys = {
            manifest.package_key
            for manifest in manifests
            if selected_package_names and manifest.package_name in selected_package_names
        }
        if not selected_manifest_keys:
            root_manifest = manifest_map.get((str(root_document.get("package", {}).get("name")), str(root_document.get("package", {}).get("version")))) if isinstance(root_document.get("package"), dict) else None
            if root_manifest is not None:
                selected_manifest_keys.add(root_manifest.package_key)
        if not selected_manifest_keys:
            selected_manifest_keys = {manifest.package_key for manifest in manifests}
            if len(selected_manifest_keys) > 1 and not selected_package_names:
                unresolved.append({"root": str(root), "reason": "fallback scan selected multiple workspace packages without --package"})
        root_package_keys = selected_manifest_keys
        if lock_packages:
            lock_roots = [
                package
                for package in lock_packages
                if (package.name, package.version) in root_package_keys
            ]
            reachable: dict[str, LockPackage] = {}
            queue = deque(package.key for package in lock_roots)
            while queue:
                key = queue.popleft()
                if key in reachable:
                    continue
                package = lock_map.get(key)
                if package is None:
                    unresolved.append({"root": str(root), "package": key, "reason": "lock package key missing"})
                    continue
                reachable[key] = package
                for dependency_value in package.dependencies:
                    dependency_name, dependency_version = parse_lock_dependency(dependency_value)
                    candidates = lock_by_name.get(dependency_name, [])
                    if dependency_version is not None:
                        candidates = [candidate for candidate in candidates if candidate.version == dependency_version]
                    if len(candidates) != 1:
                        unresolved.append(
                            {
                                "root": str(root),
                                "from": package.key,
                                "dependency": dependency_value,
                                "reason": "lock dependency is ambiguous or absent",
                            }
                        )
                        continue
                    queue.append(candidates[0].key)

            for package in reachable.values():
                manifest = manifest_map.get((package.name, package.version))
                local = manifest is not None and not any(
                    part.lower() in {"vendor", "third_party", "third-party"}
                    for part in manifest.path.parts
                )
                node = {
                    "id": package.key,
                    "name": package.name,
                    "version": package.version,
                    "source": package.source,
                    "local": local,
                    "license": manifest.license if manifest else None,
                    "kind": "proc-macro" if manifest and manifest.proc_macro else "normal",
                    "build_script": manifest.has_build_script if manifest else None,
                    "native_links": manifest.native_links if manifest else None,
                    "allowed_origin": _origin_allowed(package.source, local, allowed_origin_prefixes),
                }
                all_nodes[package.key] = node
                family = RUNTIME_FAMILIES.get(package.name)
                if family:
                    runtime_versions[family].add(package.version)
                    runtime_sources[family].add(package.source or "local")
                if _is_forbidden_package(package.name):
                    all_violations.append(
                        {
                            "code": "forbidden_dependency",
                            "package": package.key,
                            "message": f"forbidden dependency {package.name!r} is in the resolved closure",
                        }
                    )
                elif not node["allowed_origin"]:
                    all_violations.append(
                        {
                            "code": "unapproved_dependency_origin",
                            "package": package.key,
                            "source": package.source,
                            "message": "resolved package origin is not local or an explicitly allowed first-party origin",
                        }
                    )
                if not local:
                    unresolved.append(
                        {
                            "root": str(root),
                            "package": package.key,
                            "reason": "package manifest/license evidence is outside the selected inputs",
                        }
                    )
                for dependency_value in package.dependencies:
                    dependency_name, dependency_version = parse_lock_dependency(dependency_value)
                    candidates = lock_by_name.get(dependency_name, [])
                    if dependency_version is not None:
                        candidates = [candidate for candidate in candidates if candidate.version == dependency_version]
                    if len(candidates) == 1:
                        all_edges.append(
                            {
                                "from": package.key,
                                "to": candidates[0].key,
                                "kind": "resolved",
                                "declared_version": dependency_version,
                                "resolved": True,
                            }
                        )

    for family, versions in sorted(runtime_versions.items()):
        if len(versions) > 1:
            all_violations.append(
                {
                    "code": "duplicate_runtime_version",
                    "runtime": family,
                    "versions": sorted(versions),
                    "sources": sorted(runtime_sources[family]),
                    "message": f"runtime family {family!r} resolves to multiple versions",
                }
            )
    active_families = sorted(family for family, versions in runtime_versions.items() if versions)
    if len(active_families) > 1:
        all_violations.append(
            {
                "code": "multiple_runtime_families",
                "runtimes": active_families,
                "message": "more than one asynchronous runtime family is present",
            }
        )

    complete = bool(root_results) and all(result["manifest_count"] > 0 for result in root_results) and not unresolved and not any(
        result["lock_error"] is not None for result in root_results if result["manifest_count"]
    )
    if all_violations:
        qualification = "noncompliant"
    elif not complete:
        qualification = "incomplete"
    else:
        qualification = "qualified"

    return {
        "schema_version": SCHEMA_VERSION,
        "probe_version": PROBE_VERSION,
        "qualification": qualification,
        "complete": complete,
        "roots": root_results,
        "graph": {
            "nodes": sorted(all_nodes.values(), key=lambda node: node["id"]),
            "edges": all_edges,
            "node_count": len(all_nodes),
            "edge_count": len(all_edges),
        },
        "unresolved": unresolved,
        "violations": all_violations,
        "summary": {
            "root_count": len(root_results),
            "package_count": sum(result["manifest_count"] for result in root_results),
            "resolved_node_count": len(all_nodes),
            "unresolved_count": len(unresolved),
            "violation_count": len(all_violations),
            "runtime_families": active_families,
        },
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", type=Path, required=True, help="Cargo repository root to inspect")
    parser.add_argument(
        "--allow-origin",
        action="append",
        default=[],
        help="additional exact source-origin prefix accepted for resolved packages",
    )
    parser.add_argument(
        "--package",
        action="append",
        default=[],
        help="selected package name or package-id in a virtual/workspace root (repeatable)",
    )
    parser.add_argument("--feature", action="append", default=[], help="feature passed to cargo metadata (repeatable)")
    parser.add_argument("--filter-platform", help="platform passed to cargo metadata, such as aarch64-apple-darwin")
    parser.add_argument("--pretty", action="store_true", help="indent JSON output")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    result = inspect_roots(
        args.root,
        args.allow_origin,
        args.package,
        args.feature,
        args.filter_platform,
    )
    print(json.dumps(result, indent=2 if args.pretty else None, sort_keys=True))
    return {"qualified": 0, "noncompliant": 2, "incomplete": 3}.get(result["qualification"], 4)


if __name__ == "__main__":
    raise SystemExit(main())
