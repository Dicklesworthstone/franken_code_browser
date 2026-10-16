#!/usr/bin/env python3
"""Black-box packaging contracts, NOT native build/signing qualification.

Executes the real shell scripts with isolated fake cargo, swiftc, codesign and
copy tools. Checks artifact selection, byte-for-byte CLI inclusion, signing
order, and preservation of existing destinations. No GUI or compiler is run.
Fixtures/logs are retained; set FCB_PACKAGING_TEST_OUTPUT to a NEW absolute
output directory to choose their location. Never points tools at an installed app.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1]
SCRIPTS = ("scripts/build_macos_app.sh", "scripts/install_macos_app.sh",
           "native/macos/scripts/make_app_bundle.sh")
STUB = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["FCB_TOOL_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps([name, args]) + "\n")
if name == "uname":
    print("Darwin" if args == ["-s"] else "arm64")
elif name == "cargo":
    assert args[0] == "build" and "--locked" in args
    packages = [args[i + 1] for i, arg in enumerate(args) if arg == "-p"]
    assert set(packages) == {"fcb-app", "fcb-bridge"}, packages
    target = args[args.index("--target") + 1]
    output = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", "target")) / target
    output /= "release" if "--release" in args else "debug"
    output.mkdir(parents=True, exist_ok=True)
    (output / "libfcb_bridge.a").write_bytes(b"FAKE BRIDGE: not a native artifact")
    (output / "fcb").write_bytes(b"FAKE CLI: exact artifact bytes\x00\xff")
    (output / "fcb").chmod(0o755)
elif name == "xcrun":
    assert args[0] == "swiftc"
    output = pathlib.Path(args[args.index("-o") + 1])
    output.write_bytes(b"FAKE NATIVE APP: never execute")
    output.chmod(0o755)
elif name == "ditto":
    shutil.copytree(args[0], args[1], dirs_exist_ok=True)
elif name == "codesign":
    assert pathlib.Path(args[-1]).exists(), args
elif name != "sbh":
    raise SystemExit("unexpected fixture tool " + name)
'''


class NativeLaunchPackaging(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        requested = os.environ.get("FCB_PACKAGING_TEST_OUTPUT")
        if requested:
            cls.evidence = Path(requested)
            if not cls.evidence.is_absolute():
                raise ValueError("FCB_PACKAGING_TEST_OUTPUT must be absolute")
            cls.evidence.mkdir(parents=True, exist_ok=False)
        else:
            cls.evidence = Path(tempfile.mkdtemp(prefix="fcb-launch-packaging-"))
        print(f"Packaging fixture evidence retained: {cls.evidence}", flush=True)

    def setUp(self) -> None:
        self.root = self.evidence / self._testMethodName
        self.root.mkdir()
        for relative in SCRIPTS:
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SOURCE / relative, target)
            target.chmod(0o755)
        resources = self.root / "native/macos/swiftui/Resources"
        resources.mkdir(parents=True)
        (resources / "AppIcon.icns").write_bytes(b"FAKE ICON")
        tools = self.root / "fixture-tools"
        tools.mkdir()
        for name in ("cargo", "xcrun", "codesign", "uname", "ditto", "sbh"):
            tool = tools / name
            tool.write_text(STUB, encoding="utf-8")
            tool.chmod(0o755)
        self.log = self.root / "tools.jsonl"
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("FCB_") and key != "CARGO_TARGET_DIR"}
        self.env.update(PATH=str(tools) + os.pathsep + os.environ.get("PATH", ""),
                        HOME=str(self.root / "home"), FCB_TOOL_LOG=str(self.log))
        self.app = self.root / "output with spaces/My Browser.app"
        self.app.parent.mkdir()
        self.bridge = self.root / "bridge.a"
        self.bridge.write_bytes(b"FAKE BRIDGE")
        self.cli = self.root / "cli with spaces"
        self.cli.write_bytes(b"FAKE CLI\x00\xff")
        self.cli.chmod(0o755)

    def run_script(self, relative: str, *args: str, **environment: str) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(["/bin/sh", str(self.root / relative), *args], cwd=self.root,
                                env=self.env | environment, capture_output=True, text=True,
                                timeout=20, check=False)
        return result

    def bundle(self, **environment: str) -> subprocess.CompletedProcess[str]:
        return self.run_script(SCRIPTS[2], FCB_BRIDGE_LIB=str(self.bridge),
                               FCB_APP_OUTPUT=str(self.app), **environment)

    def calls(self) -> list[list[object]]:
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_release_build_embeds_matching_cli_and_signs_it_before_bundle(self) -> None:
        result = self.run_script(SCRIPTS[0], "--output", str(self.app))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(self.app))
        original = self.root / "target/aarch64-apple-darwin/release/fcb"
        copied = self.app / "Contents/MacOS/fcb"
        self.assertEqual(copied.read_bytes(), original.read_bytes())
        self.assertTrue(copied.stat().st_mode & 0o111)
        self.assertEqual((self.app / "Contents/Resources/fcb-launch-project-v1").read_bytes(), b"")
        self.assertFalse((self.app / "Contents/Resources/fcb-launch-picker-v1").exists())
        signs = [args for name, args in self.calls() if name == "codesign"]
        self.assertEqual(signs, [["--force", "--sign", "-", str(copied)],
                                 ["--force", "--sign", "-", str(self.app)],
                                 ["--verify", "--deep", "--strict", str(self.app)]])

    def test_debug_build_preserves_relative_target_directory(self) -> None:
        result = self.run_script(SCRIPTS[0], "--debug", "--output", str(self.app), CARGO_TARGET_DIR="build cache")
        self.assertEqual(result.returncode, 0, result.stderr)
        artifact = self.root / "build cache/aarch64-apple-darwin/debug/fcb"
        self.assertEqual((self.app / "Contents/MacOS/fcb").read_bytes(), artifact.read_bytes())
        cargo = next(args for name, args in self.calls() if name == "cargo")
        self.assertNotIn("--release", cargo)

    def test_invalid_explicit_cli_fails_before_creating_destination(self) -> None:
        for path in (str(self.root / "absent-cli"), "relative-cli"):
            result = self.bundle(FCB_CLI_BIN=path)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.app.exists())
        self.assertFalse(any(name == "xcrun" for name, _ in self.calls()))

    def test_lower_level_native_only_bundle_remains_supported(self) -> None:
        result = self.bundle()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.app / "Contents/MacOS/FrankenCodeBrowser").exists())
        self.assertFalse((self.app / "Contents/MacOS/fcb").exists())
        self.assertTrue((self.app / "Contents/Resources/fcb-launch-project-v1").exists())

    def test_existing_output_is_not_modified(self) -> None:
        self.app.mkdir()
        sentinel = self.app / "keep"
        sentinel.write_bytes(b"previous app")
        for result in (self.bundle(FCB_CLI_BIN=str(self.cli)),
                       self.run_script(SCRIPTS[0], "--output", str(self.app))):
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(sentinel.read_bytes(), b"previous app")
        self.assertFalse(any(name in ("cargo", "xcrun") for name, _ in self.calls()))

    def test_dangling_output_symlink_is_not_followed(self) -> None:
        target = self.root / "must-not-create"
        self.app.symlink_to(target, target_is_directory=True)
        for result in (self.bundle(FCB_CLI_BIN=str(self.cli)),
                       self.run_script(SCRIPTS[0], "--output", str(self.app))):
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(self.app.is_symlink())
            self.assertFalse(target.exists())

    def test_installer_copies_cli_without_changing_path_or_replacing_existing_app(self) -> None:
        built = self.bundle(FCB_CLI_BIN=str(self.cli))
        self.assertEqual(built.returncode, 0, built.stderr)
        installed = self.run_script(SCRIPTS[1], str(self.app))
        self.assertEqual(installed.returncode, 0, installed.stderr)
        destination = Path(self.env["HOME"]) / "Applications/FrankenCodeBrowser.app"
        self.assertEqual((destination / "Contents/MacOS/fcb").read_bytes(), self.cli.read_bytes())
        self.assertIn("CLI: " + str(destination / "Contents/MacOS/fcb"), installed.stdout)
        again = self.run_script(SCRIPTS[1], str(self.app))
        self.assertNotEqual(again.returncode, 0)
        self.assertEqual((destination / "Contents/MacOS/fcb").read_bytes(), self.cli.read_bytes())


if __name__ == "__main__":
    unittest.main()
