from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
PROBE_PATH = REPO_ROOT / "scripts" / "closure_probe.py"
SPEC = importlib.util.spec_from_file_location("closure_probe", PROBE_PATH)
assert SPEC is not None and SPEC.loader is not None
closure_probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = closure_probe
SPEC.loader.exec_module(closure_probe)


class ClosureProbeTests(unittest.TestCase):
    def make_repo(
        self,
        cargo_toml: str,
        lock: str | None = None,
        extra: dict[str, str] | None = None,
        generate_lock: bool = True,
    ) -> Path:
        directory = Path(tempfile.mkdtemp(prefix="fcb-closure-probe-"))
        (directory / "Cargo.toml").write_text(cargo_toml, encoding="utf-8")
        if lock is not None:
            (directory / "Cargo.lock").write_text(lock, encoding="utf-8")
        for relative, content in (extra or {}).items():
            destination = directory / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(content, encoding="utf-8")
        if generate_lock:
            completed = subprocess.run(
                ["cargo", "metadata", "--offline", "--format-version", "1"],
                cwd=directory,
                check=False,
                capture_output=True,
                text=True,
            )
            if completed.returncode != 0:
                self.fail(f"fixture cargo metadata failed: {completed.stderr.strip()}")
        subprocess.run(["git", "init", "-q", str(directory)], check=True)
        subprocess.run(["git", "-C", str(directory), "config", "user.email", "probe@example.invalid"], check=True)
        subprocess.run(["git", "-C", str(directory), "config", "user.name", "FCB closure probe"], check=True)
        subprocess.run(["git", "-C", str(directory), "remote", "add", "origin", "https://example.invalid/probe.git"], check=True)
        subprocess.run(["git", "-C", str(directory), "add", "."], check=True)
        subprocess.run(["git", "-C", str(directory), "commit", "-qm", "fixture"], check=True)
        return directory

    def run_cli(self, root: Path) -> tuple[int, dict[str, object]]:
        completed = subprocess.run(
            ["python3", str(PROBE_PATH), "--root", str(root)],
            check=False,
            capture_output=True,
            text=True,
        )
        return completed.returncode, json.loads(completed.stdout)

    def test_closed_local_graph_is_qualified(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"
build = "build.rs"
links = "probe-native"

[dependencies]
foundation = { path = "foundation" }
""",
            extra={
                "foundation/Cargo.toml": """[package]
name = "foundation"
version = "0.1.0"
edition = "2024"
license = "MIT"

[lib]
proc-macro = true
""",
                "build.rs": "fn main() {}\n",
                "src/lib.rs": "pub fn root() {}\n",
                "foundation/src/lib.rs": "extern crate proc_macro;\n",
            },
        )
        result = closure_probe.inspect_roots([root])
        self.assertEqual(result["qualification"], "qualified")
        self.assertTrue(result["complete"])
        self.assertEqual(result["roots"][0]["metadata_mode"], "cargo-metadata")
        self.assertEqual(result["violations"], [])
        self.assertEqual(result["summary"]["resolved_node_count"], 2)
        self.assertIsNotNone(result["roots"][0]["revision"])
        packages = {package["name"]: package for package in result["roots"][0]["packages"]}
        self.assertTrue(packages["probe-root"]["build_script"])
        self.assertEqual(packages["probe-root"]["native_links"], "probe-native")
        self.assertEqual(packages["foundation"]["kind"], "proc-macro")

    def test_cli_returns_machine_result_and_qualified_exit(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"
""",
            extra={"src/lib.rs": "pub fn root() {}\n"},
        )
        exit_code, result = self.run_cli(root)
        self.assertEqual(exit_code, 0)
        self.assertEqual(result["schema_version"], "fcb.closure-probe.v1")
        self.assertEqual(result["qualification"], "qualified")

    def test_dirty_manifest_bytes_prevent_qualification(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"
""",
            extra={"src/lib.rs": "pub fn root() {}\n"},
        )
        (root / "Cargo.toml").write_text(
            (root / "Cargo.toml").read_text(encoding="utf-8") + "\n# changed after capture\n",
            encoding="utf-8",
        )
        result = closure_probe.inspect_roots([root])
        self.assertEqual(result["qualification"], "incomplete")
        self.assertFalse(result["complete"])
        self.assertTrue(any("uncommitted" in item["reason"] for item in result["unresolved"]))

    def test_forbidden_registry_edge_is_reported(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"

[dependencies]
tokio = { path = "tokio" }
""",
            extra={
                "src/lib.rs": "pub fn root() {}\n",
                "tokio/Cargo.toml": """[package]
name = "tokio"
version = "1.0.0"
edition = "2024"
license = "MIT"
""",
                "tokio/src/lib.rs": "pub fn forbidden() {}\n",
            },
        )
        result = closure_probe.inspect_roots([root])
        codes = {violation["code"] for violation in result["violations"]}
        self.assertEqual(result["qualification"], "noncompliant")
        self.assertIn("forbidden_dependency", codes)
        self.assertTrue(any(node["name"] == "tokio" for node in result["graph"]["nodes"]))

    def test_dev_only_forbidden_edge_is_not_shipping_reachability(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dev-dependencies]
tokio = { path = "tokio" }
""",
            extra={
                "src/lib.rs": "pub fn root() {}\n",
                "tokio/Cargo.toml": """[package]
name = "tokio"
version = "1.0.0"
edition = "2024"
license = "MIT"
""",
                "tokio/src/lib.rs": "pub fn forbidden() {}\n",
            },
        )
        result = closure_probe.inspect_roots([root])
        self.assertEqual(result["qualification"], "qualified")
        self.assertTrue(result["complete"])
        self.assertFalse(any(node["name"] == "tokio" for node in result["graph"]["nodes"]))

    def test_workspace_defaults_and_target_profile_edges_are_inspected(self) -> None:
        root = self.make_repo(
            """[workspace]
members = ["member"]
resolver = "3"

[workspace.package]
version = "0.1.0"
license = "MIT"
""",
            extra={
                "member/Cargo.toml": """[package]
name = "member"
version.workspace = true
license.workspace = true
edition = "2024"

[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { path = "../objc2" }
""",
                "member/src/lib.rs": "pub fn member() {}\n",
                "objc2/Cargo.toml": """[package]
name = "objc2"
version = "0.6.0"
edition = "2024"
license = "MIT"
""",
                "objc2/src/lib.rs": "pub fn forbidden() {}\n",
            },
        )
        result = closure_probe.inspect_roots(
            [root], selected_packages=["member"], filter_platform="aarch64-apple-darwin"
        )
        codes = {violation["code"] for violation in result["violations"]}
        member = next(package for package in result["roots"][0]["packages"] if package["name"] == "member")
        target_edges = [
            edge for edge in result["graph"]["edges"]
            if edge["from"] == member["id"] and any(
                node["id"] == edge["to"] and node["name"] == "objc2" for node in result["graph"]["nodes"]
            )
        ]
        self.assertEqual(member["license"], "MIT")
        self.assertEqual([edge["kind"] for edge in target_edges], [["normal"]])
        self.assertIn("forbidden_dependency", codes)
        self.assertEqual(result["qualification"], "noncompliant")

    def test_duplicate_asupersync_versions_are_not_green(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"

[dependencies]
runtime-old = { package = "asupersync", path = "runtime-old" }
runtime-new = { package = "asupersync", path = "runtime-new" }
""",
            extra={
                "src/lib.rs": "pub fn root() {}\n",
                "runtime-old/Cargo.toml": """[package]
name = "asupersync"
version = "0.4.11"
edition = "2024"
license = "MIT"
""",
                "runtime-old/src/lib.rs": "pub fn old() {}\n",
                "runtime-new/Cargo.toml": """[package]
name = "asupersync"
version = "0.5.0"
edition = "2024"
license = "MIT"
""",
                "runtime-new/src/lib.rs": "pub fn new() {}\n",
            },
        )
        result = closure_probe.inspect_roots([root])
        duplicate = [violation for violation in result["violations"] if violation["code"] == "duplicate_runtime_version"]
        self.assertEqual(result["qualification"], "noncompliant")
        self.assertEqual(len(duplicate), 1)
        self.assertEqual(duplicate[0]["versions"], ["0.4.11", "0.5.0"])

    def test_missing_lock_is_incomplete_not_qualified(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"

[dependencies]
foundation = "0.1.0"
""",
            generate_lock=False,
        )
        result = closure_probe.inspect_roots([root])
        self.assertEqual(result["qualification"], "incomplete")
        self.assertFalse(result["complete"])
        self.assertGreater(result["summary"]["unresolved_count"], 0)

    def test_unmatched_package_selector_is_incomplete(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"
""",
            extra={"src/lib.rs": "pub fn root() {}\n"},
        )
        result = closure_probe.inspect_roots([root], selected_packages=["missing-package"])
        self.assertEqual(result["qualification"], "incomplete")
        self.assertFalse(result["complete"])
        self.assertTrue(any(item.get("selector") == "missing-package" for item in result["unresolved"]))

    def test_unrelated_lock_entries_do_not_make_an_empty_graph_complete(self) -> None:
        root = self.make_repo(
            """[package]
name = "probe-root"
version = "0.1.0"
edition = "2024"
license = "MIT"
""",
            """version = 4

[[package]]
name = "unrelated"
version = "9.9.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
""",
            extra={"src/lib.rs": "pub fn root() {}\n"},
            generate_lock=False,
        )
        result = closure_probe.inspect_roots([root])
        self.assertEqual(result["summary"]["resolved_node_count"], 0)
        self.assertFalse(result["complete"])
        self.assertEqual(result["qualification"], "incomplete")
        self.assertTrue(any("metadata" in item["reason"] for item in result["unresolved"]))

    def test_origin_matching_is_exact(self) -> None:
        self.assertFalse(
            closure_probe._origin_allowed(
                "git+https://github.com/Dicklesworthstone/franken_markdown-evil?rev=deadbeef",
                False,
                ("https://github.com/Dicklesworthstone/franken_markdown",),
            )
        )
