from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator


REPO_ROOT = Path(__file__).resolve().parents[1]
PROBE_PATH = REPO_ROOT / "scripts" / "closure_probe.py"
SPEC = importlib.util.spec_from_file_location("closure_probe_adversarial", PROBE_PATH)
assert SPEC is not None and SPEC.loader is not None
closure_probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = closure_probe
SPEC.loader.exec_module(closure_probe)


LOCK_HEADER = "version = 4\n\n"


def _package(name: str, version: str, dependencies: tuple[str, ...] = (), source: str | None = None) -> str:
    lines = ["[[package]]", f'name = "{name}"', f'version = "{version}"']
    if source is not None:
        lines.append(f'source = "{source}"')
    if dependencies:
        rendered = ", ".join(f'"{dependency}"' for dependency in dependencies)
        lines.append(f"dependencies = [{rendered}]")
    return "\n".join(lines) + "\n\n"


@contextmanager
def temporary_repo(files: dict[str, str]) -> Iterator[Path]:
    """Create a real manifest/lock repository without invoking Cargo."""
    root = Path(tempfile.mkdtemp(prefix="fcb-closure-adversarial-"))
    for relative, content in files.items():
        destination = root / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(content, encoding="utf-8")

    subprocess.run(["git", "init", "-q", str(root)], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "probe@example.invalid"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "FCB closure adversarial probe"], check=True)
    subprocess.run(
        ["git", "-C", str(root), "remote", "add", "origin", "https://example.invalid/fcb-closure.git"],
        check=True,
    )
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-qm", "adversarial fixture"], check=True)
    yield root


def _report(root: Path) -> dict[str, object]:
    return closure_probe.inspect_roots([root], allowed_origins=())


def _codes(report: dict[str, object]) -> set[str]:
    return {str(violation["code"]) for violation in report["violations"]}  # type: ignore[index]


def _declared_edges(report: dict[str, object], dependency: str) -> list[dict[str, object]]:
    return [
        edge
        for edge in report["graph"]["edges"]  # type: ignore[index]
        if edge.get("to") == dependency and "declared_version" in edge
    ]


class ClosureProbeAdversarialTests(unittest.TestCase):
    def test_near_identical_first_party_local_closure_is_qualified(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
foundation = { path = "foundation" }
""",
                "foundation/Cargo.toml": """[package]
name = "foundation"
version = "0.1.0"
edition = "2024"
license = "MIT"
""",
                "Cargo.lock": LOCK_HEADER + _package("probe-root", "0.1.0", ("foundation",)) + _package("foundation", "0.1.0"),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "qualified")
        self.assertTrue(report["complete"])
        self.assertEqual(report["violations"], [])
        self.assertEqual(report["graph"]["node_count"], 2)  # type: ignore[index]
        self.assertEqual(len(_declared_edges(report, "foundation")), 1)

    def test_direct_forbidden_normal_edge_is_noncompliant(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
tokio = "1.0.0"
""",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("tokio 1.0.0",))
                + _package("tokio", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _codes(report))
        self.assertEqual(len(_declared_edges(report, "tokio")), 1)

    def test_transitive_forbidden_normal_edge_is_noncompliant(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
foundation = { path = "foundation" }
""",
                "foundation/Cargo.toml": """[package]
name = "foundation"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
tokio = "1.0.0"
""",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("foundation",))
                + _package("foundation", "0.1.0", ("tokio 1.0.0",))
                + _package("tokio", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _codes(report))
        transitive_edges = [
            edge
            for edge in _declared_edges(report, "tokio")
            if edge.get("from") == "foundation@0.1.0"
        ]
        self.assertEqual(len(transitive_edges), 1)

    def test_forbidden_build_dependency_is_noncompliant_and_typed(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[build-dependencies]
tokio = "1.0.0"
""",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("tokio 1.0.0",))
                + _package("tokio", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _codes(report))
        build_edges = [edge for edge in _declared_edges(report, "tokio") if edge.get("kind") == "build"]
        self.assertEqual(len(build_edges), 1)

    def test_forbidden_target_dependency_is_not_omitted(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[target.'cfg(target_os = "macos")'.dependencies]
tokio = "1.0.0"
""",
                "Cargo.lock": LOCK_HEADER + _package("probe-root", "0.1.0"),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", _codes(report))
        target_edges = [edge for edge in _declared_edges(report, "tokio") if edge.get("kind") == "normal"]
        self.assertEqual(len(target_edges), 1)

    def test_virtual_workspace_inherits_package_metadata(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[workspace]
members = ["app"]

[workspace.package]
version = "0.1.0"
license = "MIT"
""",
                "app/Cargo.toml": """[package]
name = "app"
version.workspace = true
license.workspace = true
edition = "2024"
""",
                "Cargo.lock": LOCK_HEADER + _package("app", "0.1.0"),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "qualified")
        self.assertTrue(report["complete"])
        packages = report["roots"][0]["packages"]  # type: ignore[index]
        self.assertEqual(len(packages), 1)
        self.assertEqual(packages[0]["name"], "app")
        self.assertEqual(packages[0]["version"], "0.1.0")
        self.assertEqual(packages[0]["license"], "MIT")
        self.assertEqual(report["graph"]["node_count"], 1)  # type: ignore[index]

    def test_duplicate_asupersync_versions_are_noncompliant(self) -> None:
        with temporary_repo(
            {
                "Cargo.toml": """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
runtime-old = { package = "asupersync", version = "0.4.11" }
runtime-new = { package = "asupersync", version = "0.5.0" }
""",
                "Cargo.lock": LOCK_HEADER
                + _package("probe-root", "0.1.0", ("asupersync 0.4.11", "asupersync 0.5.0"))
                + _package(
                    "asupersync",
                    "0.4.11",
                    source="git+https://github.com/Dicklesworthstone/asupersync",
                )
                + _package(
                    "asupersync",
                    "0.5.0",
                    source="git+https://github.com/Dicklesworthstone/asupersync",
                ),
            }
        ) as root:
            report = _report(root)

        self.assertEqual(report["qualification"], "noncompliant")
        duplicate = [
            violation
            for violation in report["violations"]  # type: ignore[index]
            if violation.get("code") == "duplicate_runtime_version"
        ]
        self.assertEqual(len(duplicate), 1)
        self.assertEqual(duplicate[0]["versions"], ["0.4.11", "0.5.0"])


if __name__ == "__main__":
    unittest.main()
