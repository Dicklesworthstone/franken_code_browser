from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("platform_probe", ROOT / "scripts" / "platform_probe.py")
assert SPEC is not None and SPEC.loader is not None
platform_probe = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = platform_probe
SPEC.loader.exec_module(platform_probe)

CLOSURE_SPEC = importlib.util.spec_from_file_location("closure_probe", ROOT / "scripts" / "closure_probe.py")
assert CLOSURE_SPEC is not None and CLOSURE_SPEC.loader is not None
closure_probe = importlib.util.module_from_spec(CLOSURE_SPEC)
sys.modules[CLOSURE_SPEC.name] = closure_probe
CLOSURE_SPEC.loader.exec_module(closure_probe)

CONSUMER_ROOT = ROOT / "probes" / "headless_consumer"


def observed_facts() -> dict[str, object]:
    def fake_run(command: tuple[str, ...]) -> tuple[int, str, str]:
        outputs = {
            ("uname", "-m"): "arm64",
            ("sw_vers", "-productVersion"): "26.2",
            ("xcode-select", "-p"): "/Applications/Xcode.app/Contents/Developer",
            ("xcodebuild", "-version"): "Xcode 26.1.1\nBuild version 17B100",
            ("xcrun", "--sdk", "macosx", "--show-sdk-path"): "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX26.1.sdk",
            ("xcrun", "--sdk", "macosx", "--show-sdk-version"): "26.1",
            ("xcrun", "--find", "clang"): "/usr/bin/clang",
            ("xcrun", "--find", "metal"): "/usr/bin/metal",
            ("rustup", "run", "nightly-2026-09-07", "rustc", "--version", "--verbose"): "rustc 1.100.0-nightly (5a2be9f5f 2026-09-06)\nhost: aarch64-apple-darwin\ncommit-date: 2026-09-06",
            ("rustup", "toolchain", "list"): "nightly-2026-09-07-aarch64-apple-darwin",
        }
        return 0, outputs[command], ""

    return platform_probe.collect_facts(fake_run)


class PlatformProbeTests(unittest.TestCase):
    def test_real_headless_consumer_has_a_closed_dependency_free_graph(self) -> None:
        report = closure_probe.inspect_roots([CONSUMER_ROOT])
        self.assertEqual(report["qualification"], "qualified")
        self.assertTrue(report["complete"])
        self.assertEqual(report["violations"], [])
        self.assertEqual([node["name"] for node in report["graph"]["nodes"]], ["fcb-headless-consumer"])

    def test_real_selection_is_observed_by_metadata_control(self) -> None:
        facts = observed_facts()
        result = platform_probe.decision(facts)
        self.assertEqual(result["status"], "selected")
        self.assertEqual(result["decision"]["edition"], "2024")
        self.assertEqual(result["decision"]["target"], "aarch64-apple-darwin")
        self.assertEqual(set(result["decision"]["consequences"]), {"ipc", "watcher", "font", "export", "signing", "sandbox"})

    def test_wrong_host_is_rejected_by_the_same_oracle(self) -> None:
        facts = observed_facts()
        facts["rustc"]["stdout"] = facts["rustc"]["stdout"].replace("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
        result = platform_probe.decision(facts)
        self.assertEqual(result["status"], "blocked")
        self.assertIn("rustc_mismatch", {item["code"] for item in result["violations"]})

    def test_undated_compiler_is_a_planted_negative(self) -> None:
        facts = observed_facts()
        facts["rustc"]["stdout"] = facts["rustc"]["stdout"].replace("commit-date: 2026-09-06", "")
        result = platform_probe.decision(facts)
        self.assertEqual(result["status"], "blocked")
        self.assertIn("undated_toolchain", {item["code"] for item in result["violations"]})


if __name__ == "__main__":
    unittest.main()
