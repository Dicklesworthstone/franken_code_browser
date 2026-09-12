from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator


REPO_ROOT = Path(__file__).resolve().parents[1]
PROBE_PATH = REPO_ROOT / "scripts" / "closure_probe.py"
SPEC = importlib.util.spec_from_file_location("closure_probe_adversarial", PROBE_PATH)
assert SPEC is not None and SPEC.loader is not None
closure_probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = closure_probe
SPEC.loader.exec_module(closure_probe)


LOCK_HEADER = "version = 4\n\n"
FIXTURE_ORIGIN = "https://example.invalid/fcb-closure.git"


def _package(name: str, version: str, dependencies: tuple[str, ...] = ()) -> str:
    lines = ["[[package]]", f'name = "{name}"', f'version = "{version}"']
    if dependencies:
        rendered = ", ".join(f'"{dependency}"' for dependency in dependencies)
        lines.append(f"dependencies = [{rendered}]")
    return "\n".join(lines) + "\n\n"


def _manifest(name: str, version: str, extra: str = "") -> str:
    return f"""[package]
name = "{name}"
version = "{version}"
edition = "2024"
license = "MIT"
{extra}"""


@contextmanager
def temporary_repo(files: dict[str, str]) -> Iterator[Path]:
    """Create a retained real Cargo fixture without invoking Cargo here."""
    root = Path(tempfile.mkdtemp(prefix="fcb-closure-adversarial-"))
    for relative, content in files.items():
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(content, encoding="utf-8")

    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "probe@example.invalid"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "FCB closure adversarial probe"], check=True)
    subprocess.run(["git", "-C", str(root), "remote", "add", "origin", FIXTURE_ORIGIN], check=True)
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "adversarial fixture"], check=True)
    yield root


def _report(root: Path, **options: Any) -> dict[str, Any]:
    return closure_probe.inspect_roots([root], allowed_origins=(FIXTURE_ORIGIN,), **options)


def _nodes(report: dict[str, Any]) -> list[dict[str, Any]]:
    return report["graph"]["nodes"]


def _node_named(report: dict[str, Any], name: str, version: str | None = None) -> dict[str, Any]:
    matches = [
        node
        for node in _nodes(report)
        if node["name"] == name and (version is None or node["version"] == version)
    ]
    if len(matches) != 1:
        raise AssertionError(f"expected one {name}@{version} node, found {matches}")
    return matches[0]


def _edges_between(report: dict[str, Any], from_name: str, to_name: str) -> list[dict[str, Any]]:
    from_ids = {node["id"] for node in _nodes(report) if node["name"] == from_name}
    to_ids = {node["id"] for node in _nodes(report) if node["name"] == to_name}
    return [
        edge
        for edge in report["graph"]["edges"]
        if edge["from"] in from_ids and edge["to"] in to_ids
    ]


def _violation_codes(report: dict[str, Any]) -> set[str]:
    return {str(violation["code"]) for violation in report["violations"]}


def _assert_metadata_mode(test: unittest.TestCase, report: dict[str, Any]) -> None:
    test.assertEqual(report["roots"][0]["metadata_mode"], "cargo-metadata")
    test.assertIsNone(report["roots"][0]["metadata_error"])


class ClosureProbeAdversarialTests(unittest.TestCase):
    def test_first_party_local_closure_uses_metadata_ids_and_qualifies(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest("probe-root", "0.1.0", '\n[dependencies]\nfoundation = { path = "foundation" }\n'),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "foundation/Cargo.toml": _manifest("foundation", "0.1.0"),
                "foundation/src/lib.rs": "pub fn foundation_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("foundation",))
                + _package("foundation", "0.1.0"),
            }
        ) as root:
            report = _report(root)

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "qualified")
        self.assertTrue(report["complete"])
        self.assertEqual(report["violations"], [])
        self.assertEqual({node["name"] for node in _nodes(report)}, {"probe-root", "foundation"})
        foundation = _node_named(report, "foundation")
        self.assertTrue(foundation["id"].startswith("path+file://"))
        self.assertTrue(foundation["allowed_origin"])
        edges = _edges_between(report, "probe-root", "foundation")
        self.assertEqual(len(edges), 1)
        self.assertTrue(edges[0]["resolved"])
        self.assertIn("normal", edges[0]["kind"])

    def test_direct_forbidden_normal_path_edge_is_noncompliant(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest("probe-root", "0.1.0", '\n[dependencies]\ntokio = { path = "tokio" }\n'),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "tokio/Cargo.toml": _manifest("tokio", "1.0.0"),
                "tokio/src/lib.rs": "pub fn tokio_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("tokio",))
                + _package("tokio", "1.0.0"),
            }
        ) as root:
            report = _report(root)

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _violation_codes(report))
        tokio = _node_named(report, "tokio", "1.0.0")
        self.assertTrue(tokio["local"])
        edges = _edges_between(report, "probe-root", "tokio")
        self.assertEqual(len(edges), 1)
        self.assertIn("normal", edges[0]["kind"])

    def test_transitive_forbidden_normal_path_edge_is_noncompliant(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest("probe-root", "0.1.0", '\n[dependencies]\nfoundation = { path = "foundation" }\n'),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "foundation/Cargo.toml": _manifest(
                    "foundation",
                    "0.1.0",
                    '\n[dependencies]\ntokio = { path = "../tokio" }\n',
                ),
                "foundation/src/lib.rs": "pub fn foundation_marker() {}\n",
                "tokio/Cargo.toml": _manifest("tokio", "1.0.0"),
                "tokio/src/lib.rs": "pub fn tokio_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("foundation",))
                + _package("foundation", "0.1.0", ("tokio",))
                + _package("tokio", "1.0.0"),
            }
        ) as root:
            report = _report(root)

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _violation_codes(report))
        transitive_edges = _edges_between(report, "foundation", "tokio")
        self.assertEqual(len(transitive_edges), 1)
        self.assertIn("normal", transitive_edges[0]["kind"])

    def test_forbidden_build_path_edge_is_noncompliant_and_typed(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest(
                    "probe-root",
                    "0.1.0",
                    '\n[build-dependencies]\ntokio = { path = "tokio" }\n',
                ),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "tokio/Cargo.toml": _manifest("tokio", "1.0.0"),
                "tokio/src/lib.rs": "pub fn tokio_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("tokio",))
                + _package("tokio", "1.0.0"),
            }
        ) as root:
            report = _report(root)

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _violation_codes(report))
        build_edges = [
            edge for edge in _edges_between(report, "probe-root", "tokio") if "build" in edge["kind"]
        ]
        self.assertEqual(len(build_edges), 1)

    def test_forbidden_target_path_edge_is_not_omitted(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest(
                    "probe-root",
                    "0.1.0",
                    '\n[target.\'cfg(target_os = "macos")\'.dependencies]\ntokio = { path = "tokio" }\n',
                ),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "tokio/Cargo.toml": _manifest("tokio", "1.0.0"),
                "tokio/src/lib.rs": "pub fn tokio_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("tokio",))
                + _package("tokio", "1.0.0"),
            }
        ) as root:
            report = _report(root, filter_platform="aarch64-apple-darwin")

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _violation_codes(report))
        target_edges = _edges_between(report, "probe-root", "tokio")
        self.assertEqual(len(target_edges), 1)
        self.assertIn("normal", target_edges[0]["kind"])

    def test_selected_workspace_member_excludes_dev_only_forbidden_edge(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[workspace]
members = ["app", "foundation"]
resolver = "2"

[workspace.package]
version = "0.1.0"
license = "MIT"
""",
                "app/Cargo.toml": _manifest(
                    "app",
                    "0.1.0",
                    '\n[dependencies]\nfoundation = { path = "../foundation" }\n\n[dev-dependencies]\ntokio = { path = "../tokio" }\n',
                ).replace('version = "0.1.0"', "version.workspace = true").replace('license = "MIT"', "license.workspace = true"),
                "app/src/lib.rs": "pub fn app_marker() {}\n",
                "foundation/Cargo.toml": _manifest("foundation", "0.1.0").replace(
                    'version = "0.1.0"', "version.workspace = true"
                ).replace('license = "MIT"', "license.workspace = true"),
                "foundation/src/lib.rs": "pub fn foundation_marker() {}\n",
                "tokio/Cargo.toml": _manifest("tokio", "1.0.0"),
                "tokio/src/lib.rs": "pub fn tokio_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("app", "0.1.0", ("foundation", "tokio"))
                + _package("foundation", "0.1.0")
                + _package("tokio", "1.0.0"),
            }
        ) as root:
            report = _report(root, selected_packages=("app",))

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "qualified")
        self.assertTrue(report["complete"])
        self.assertEqual({node["name"] for node in _nodes(report)}, {"app", "foundation"})
        self.assertEqual(_edges_between(report, "app", "tokio"), [])
        app = _node_named(report, "app", "0.1.0")
        self.assertEqual(app["license"], "MIT")

    def test_default_features_false_excludes_optional_forbidden_edge(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest("probe-root", "0.1.0", '\n[dependencies]\nfoundation = { path = "foundation", default-features = false }\n'),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "foundation/Cargo.toml": _manifest(
                    "foundation",
                    "0.1.0",
                    '\n[features]\ndefault = ["bad-default"]\nbad-default = ["dep:tokio"]\n\n[dependencies]\ntokio = { path = "../tokio", optional = true }\n',
                ),
                "foundation/src/lib.rs": "pub fn foundation_marker() {}\n",
                "tokio/Cargo.toml": _manifest("tokio", "1.0.0"),
                "tokio/src/lib.rs": "pub fn tokio_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("foundation",))
                + _package("foundation", "0.1.0"),
            }
        ) as root:
            report = _report(root)

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "qualified")
        self.assertEqual({node["name"] for node in _nodes(report)}, {"probe-root", "foundation"})
        self.assertEqual(_edges_between(report, "foundation", "tokio"), [])

    def test_duplicate_asupersync_versions_are_noncompliant(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": _manifest(
                    "probe-root",
                    "0.1.0",
                    '\n[dependencies]\nruntime-old = { package = "asupersync", path = "asupersync-old" }\nruntime-new = { package = "asupersync", path = "asupersync-new" }\n',
                ),
                "src/lib.rs": "pub fn root_marker() {}\n",
                "asupersync-old/Cargo.toml": _manifest("asupersync", "0.4.11"),
                "asupersync-old/src/lib.rs": "pub fn old_marker() {}\n",
                "asupersync-new/Cargo.toml": _manifest("asupersync", "0.5.0"),
                "asupersync-new/src/lib.rs": "pub fn new_marker() {}\n",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("asupersync 0.4.11", "asupersync 0.5.0"))
                + _package("asupersync", "0.4.11")
                + _package("asupersync", "0.5.0"),
            }
        ) as root:
            report = _report(root)

        _assert_metadata_mode(self, report)
        self.assertEqual(report["qualification"], "noncompliant")
        duplicate = [
            violation
            for violation in report["violations"]
            if violation.get("code") == "duplicate_runtime_version"
        ]
        self.assertEqual(len(duplicate), 1)
        self.assertEqual(duplicate[0]["versions"], ["0.4.11", "0.5.0"])
        self.assertEqual(
            {node["version"] for node in _nodes(report) if node["name"] == "asupersync"},
            {"0.4.11", "0.5.0"},
        )


if __name__ == "__main__":
    unittest.main()
