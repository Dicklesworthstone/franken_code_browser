"""Tests for the upstream extension ledger validator (fcb-zrm.4).

The oracle here is independent of the validator's internals: every fixture
computes its ground truth directly from git (``rev-parse HEAD``,
``hash-object``) or from a literal expected verdict, never by calling
extension_ledger helpers.
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LEDGER_PATH = REPO_ROOT / "scripts" / "extension_ledger.py"
PROBE_PATH = REPO_ROOT / "scripts" / "closure_probe.py"


def _load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


OWNER = "franken_markdown"
ORIGIN = "https://github.com/Dicklesworthstone/franken_markdown"

CARGO_TOML = '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\nlicense = "MIT"\n'


class ExtensionLedgerTests(unittest.TestCase):
    def tempdir(self, prefix: str) -> Path:
        directory = Path(tempfile.mkdtemp(prefix=prefix))
        return directory

    # ------------------------------------------------------------------
    # Fixture builders (ground truth comes from git itself).
    # ------------------------------------------------------------------

    def make_owner_repo(self, prefix: str = "fcb-ledger-owner-") -> tuple[Path, str, str]:
        """Create a real owner repository with one commit.

        Returns ``(path, commit_sha, blob_sha)`` where ``blob_sha`` is the
        hash of a file object that is deliberately not a commit.
        """
        directory = self.tempdir(prefix)
        sample = directory / "src.rs"
        sample.write_text("pub fn flow() -> u32 { 7 }\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q", str(directory)], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "config", "user.email", "ledger@example.invalid"], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "config", "user.name", "FCB ledger probe"], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "add", "."], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "commit", "-qm", "land flow source maps"], check=True, timeout=60)
        commit = subprocess.run(
            ["git", "-C", str(directory), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
            timeout=60,
        ).stdout.strip()
        blob = subprocess.run(
            ["git", "-C", str(directory), "hash-object", str(sample)],
            check=True,
            capture_output=True,
            text=True,
            timeout=60,
        ).stdout.strip()
        return directory, commit, blob

    def make_empty_owner_repo(self) -> Path:
        """A first-party repository that contains no commits."""
        directory = self.tempdir(prefix="fcb-ledger-empty-owner-")
        subprocess.run(["git", "init", "-q", "-b", "main", str(directory)], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "config", "user.email", "ledger@example.invalid"], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "config", "user.name", "FCB ledger probe"], check=True, timeout=60)
        return directory

    def valid_entry(self, commit: str) -> dict[str, object]:
        return {
            "owner": OWNER,
            "origin": ORIGIN,
            "extension": "flow-source-maps",
            "public_api": ["fmd_flow::SourceMap"],
            "source": {"commit": commit},
            "features": ["flow"],
            "closure_delta": {"added_packages": [], "removed_packages": []},
            "upstream_tests": ["fmd_flow::tests::source_map_roundtrip"],
            "fcb_consumer": {
                "route": "probes/headless_consumer",
                "evidence": "consumer_contract::flow_source_maps_render",
            },
            "qualification": "implemented",
        }

    def write_ledger(self, entries: list[dict[str, object]], schema: str = "fcb.extension-ledger.v1") -> Path:
        directory = self.tempdir(prefix="fcb-ledger-doc-")
        path = directory / "ledger.json"
        path.write_text(json.dumps({"schema": schema, "entries": entries}), encoding="utf-8")
        return path

    def run_ledger_cli(
        self,
        ledger: Path,
        repos: dict[str, Path] | None = None,
    ) -> tuple[int, dict[str, object]]:
        command = [sys.executable, str(LEDGER_PATH), "--ledger", str(ledger)]
        for owner, path in (repos or {}).items():
            command += ["--repo", f"{owner}={path}"]
        completed = subprocess.run(command, capture_output=True, text=True, timeout=120)
        return completed.returncode, json.loads(completed.stdout)

    def make_cargo_fixture(self) -> Path:
        """Minimal committed Cargo root, mirroring the closure probe recipe."""
        directory = self.tempdir(prefix="fcb-ledger-cargo-")
        (directory / "Cargo.toml").write_text(CARGO_TOML, encoding="utf-8")
        (directory / "src").mkdir()
        (directory / "src" / "lib.rs").write_text("pub struct Fixture;\n", encoding="utf-8")
        subprocess.run(
            ["cargo", "metadata", "--offline", "--format-version", "1"],
            cwd=directory,
            check=True,
            capture_output=True,
            text=True,
            timeout=180,
        )
        subprocess.run(["git", "init", "-q", str(directory)], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "config", "user.email", "ledger@example.invalid"], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "config", "user.name", "FCB ledger probe"], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "remote", "add", "origin", "https://example.invalid/fixture.git"], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "add", "."], check=True, timeout=60)
        subprocess.run(["git", "-C", str(directory), "commit", "-qm", "fixture"], check=True, timeout=60)
        return directory

    def run_probe_cli(self, root: Path, ledger: Path | None = None, owner_repo: Path | None = None) -> tuple[int, dict[str, object]]:
        command = [sys.executable, str(PROBE_PATH), "--root", str(root)]
        if ledger is not None:
            command += ["--extension-ledger", str(ledger)]
        if owner_repo is not None:
            command += ["--extension-repo", f"{OWNER}={owner_repo}"]
        completed = subprocess.run(command, capture_output=True, text=True, timeout=120)
        return completed.returncode, json.loads(completed.stdout)

    # ------------------------------------------------------------------
    # Acceptance: a real committed input.
    # ------------------------------------------------------------------

    def test_real_committed_input_is_accepted(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        ledger = self.write_ledger([self.valid_entry(commit)])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(0, exit_code)
        self.assertEqual("accepted", result["verdict"])
        self.assertEqual(1, result["entry_count"])
        self.assertEqual([], result["violations"])
        self.assertEqual([], result["unverified"])
        self.assertIsNone(result["parse_error"])
        receipt = result["receipt"]
        assert isinstance(receipt, dict)
        self.assertEqual(commit, (receipt["repositories"] or {}).get(OWNER, {}).get("head"))

    # ------------------------------------------------------------------
    # Named rejections.
    # ------------------------------------------------------------------

    def test_research_blob_id_as_commit_is_rejected(self) -> None:
        owner, _, blob = self.make_owner_repo()
        ledger = self.write_ledger([self.valid_entry(blob)])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        self.assertEqual("rejected", result["verdict"])
        codes = [item["code"] for item in result["violations"]]
        self.assertIn("BLOB_ID_AS_COMMIT", codes)
        kinds = [item["object_kind"] for item in result["violations"] if item["code"] == "BLOB_ID_AS_COMMIT"]
        self.assertEqual(["blob"], kinds)

    def test_moving_branch_pin_is_rejected(self) -> None:
        owner, _, _ = self.make_owner_repo()
        entry = self.valid_entry("main")
        ledger = self.write_ledger([entry])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        codes = [item["code"] for item in result["violations"]]
        self.assertIn("MOVING_BRANCH_PIN", codes)

    def test_path_patch_receipt_is_rejected(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        entry = self.valid_entry(commit)
        entry["origin"] = "../franken_markdown"
        ledger = self.write_ledger([entry])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        codes = [item["code"] for item in result["violations"]]
        self.assertIn("PATH_PATCH_RECEIPT", codes)

    def test_wrong_status_is_rejected(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        entry = self.valid_entry(commit)
        entry["qualification"] = "declared"
        ledger = self.write_ledger([entry])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        codes = [item["code"] for item in result["violations"]]
        self.assertIn("INVALID_STATUS", codes)

    def test_missing_evidence_rows_are_rejected(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        entry = self.valid_entry(commit)
        del entry["upstream_tests"]
        ledger = self.write_ledger([entry])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        fields = [item.get("field") for item in result["violations"] if item["code"] == "MISSING_FIELD"]
        self.assertIn("upstream_tests", fields)

    def test_omitted_consumer_route_is_rejected(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        entry = self.valid_entry(commit)
        entry["fcb_consumer"] = {"evidence": "consumer_contract::flow_source_maps_render"}
        ledger = self.write_ledger([entry])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        fields = [item.get("field") for item in result["violations"] if item["code"] == "MISSING_FIELD"]
        self.assertIn("fcb_consumer.route", fields)

    def test_owner_origin_mismatch_is_rejected(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        entry = self.valid_entry(commit)
        entry["owner"] = "franken_threed"
        ledger = self.write_ledger([entry])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        codes = [item["code"] for item in result["violations"]]
        self.assertIn("OWNER_ORIGIN_MISMATCH", codes)

    # ------------------------------------------------------------------
    # Bounded incompleteness: unverifiable evidence is never a pass.
    # ------------------------------------------------------------------

    def test_commit_absent_from_owner_repo_is_incomplete(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        other = self.make_empty_owner_repo()
        ledger = self.write_ledger([self.valid_entry(commit)])
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: other})
        self.assertEqual(3, exit_code)
        self.assertEqual("incomplete", result["verdict"])
        codes = [item["code"] for item in result["unverified"]]
        self.assertIn("COMMIT_UNVERIFIED", codes)
        self.assertEqual([], result["violations"])

    def test_commit_without_owner_repo_is_incomplete(self) -> None:
        _, commit, _ = self.make_owner_repo()
        ledger = self.write_ledger([self.valid_entry(commit)])
        exit_code, result = self.run_ledger_cli(ledger)
        self.assertEqual(3, exit_code)
        self.assertEqual("incomplete", result["verdict"])

    def test_empty_ledger_gates_nothing_and_is_incomplete(self) -> None:
        ledger = self.write_ledger([])
        exit_code, result = self.run_ledger_cli(ledger)
        self.assertEqual(3, exit_code)
        self.assertEqual("incomplete", result["verdict"])
        self.assertIsNotNone(result["parse_error"])

    def test_wrong_schema_is_rejected(self) -> None:
        owner, commit, _ = self.make_owner_repo()
        ledger = self.write_ledger([self.valid_entry(commit)], schema="fcb.closure-probe.v1")
        exit_code, result = self.run_ledger_cli(ledger, repos={OWNER: owner})
        self.assertEqual(2, exit_code)
        codes = [item["code"] for item in result["violations"]]
        self.assertIn("SCHEMA_MISMATCH", codes)

    def test_unreadable_ledger_is_incomplete(self) -> None:
        missing = self.tempdir(prefix="fcb-ledger-missing-") / "absent.json"
        exit_code, result = self.run_ledger_cli(missing)
        self.assertEqual(3, exit_code)
        self.assertEqual("incomplete", result["verdict"])
        self.assertIsNotNone(result["parse_error"])

    # ------------------------------------------------------------------
    # Consumption by the existing qualification tool.
    # ------------------------------------------------------------------

    def test_probe_without_ledger_keeps_contract(self) -> None:
        root = self.make_cargo_fixture()
        exit_code, result = self.run_probe_cli(root)
        self.assertEqual(0, exit_code)
        self.assertEqual("qualified", result["qualification"])
        self.assertNotIn("extension_ledger", result)

    def test_probe_downgrades_on_rejected_ledger(self) -> None:
        root = self.make_cargo_fixture()
        owner, _, blob = self.make_owner_repo()
        ledger = self.write_ledger([self.valid_entry(blob)])
        exit_code, result = self.run_probe_cli(root, ledger=ledger, owner_repo=owner)
        self.assertEqual(2, exit_code)
        self.assertEqual("noncompliant", result["qualification"])
        section = result["extension_ledger"]
        assert isinstance(section, dict)
        self.assertEqual("rejected", section["verdict"])
        codes = [item["code"] for item in section["violations"]]
        self.assertIn("BLOB_ID_AS_COMMIT", codes)

    def test_probe_downgrades_on_incomplete_ledger(self) -> None:
        root = self.make_cargo_fixture()
        ledger = self.write_ledger([])
        exit_code, result = self.run_probe_cli(root, ledger=ledger)
        self.assertEqual(3, exit_code)
        self.assertEqual("incomplete", result["qualification"])
        section = result["extension_ledger"]
        assert isinstance(section, dict)
        self.assertEqual("incomplete", section["verdict"])

    def test_probe_stays_qualified_with_accepted_ledger(self) -> None:
        root = self.make_cargo_fixture()
        owner, commit, _ = self.make_owner_repo()
        ledger = self.write_ledger([self.valid_entry(commit)])
        exit_code, result = self.run_probe_cli(root, ledger=ledger, owner_repo=owner)
        self.assertEqual(0, exit_code)
        self.assertEqual("qualified", result["qualification"])
        section = result["extension_ledger"]
        assert isinstance(section, dict)
        self.assertEqual("accepted", section["verdict"])


if __name__ == "__main__":
    unittest.main()
