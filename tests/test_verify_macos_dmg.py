"""The DMG release gate must reject any image that is not Developer ID-signed.

GH #1: the v0.1.0 image was notarized and stapled but the container itself was
unsigned while the release notes said otherwise. These tests build throwaway
images locally (nothing is signed with a real identity or sent to Apple) and
check that the gate fails closed on them.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERIFY = ROOT / "scripts" / "verify_macos_dmg.sh"
PACKAGE = ROOT / "scripts" / "package_macos_dmg.sh"


def run(*args: str | Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run([str(a) for a in args], capture_output=True, text=True, timeout=300)


class VerifierArgumentTests(unittest.TestCase):
    def test_usage_errors_exit_2(self) -> None:
        self.assertEqual(run("sh", VERIFY).returncode, 2)
        self.assertEqual(run("sh", VERIFY, "--team").returncode, 2)
        self.assertEqual(run("sh", VERIFY, "--bogus", "x.dmg").returncode, 2)
        self.assertEqual(run("sh", VERIFY, "a.dmg", "b.dmg").returncode, 2)

    def test_missing_file_exits_2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = run("sh", VERIFY, Path(tmp) / "absent.dmg")
        self.assertEqual(result.returncode, 2)
        self.assertIn("not a file", result.stderr)


class PackagerWiringTests(unittest.TestCase):
    def test_packager_signs_the_image_and_runs_the_gate(self) -> None:
        text = PACKAGE.read_text(encoding="utf-8")
        sign = text.index('codesign --timestamp --sign "$identity" --identifier "$bundle_id.dmg" "$output"')
        notarize = text.index("notarization submit")
        staple = text.index('xcrun stapler staple "$output"')
        gate = text.index('verify_macos_dmg.sh" "$output"')
        self.assertLess(sign, notarize)
        self.assertLess(staple, gate)


@unittest.skipUnless(sys.platform == "darwin" and shutil.which("hdiutil"), "needs macOS hdiutil/codesign")
class VerifierRejectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        app = self.tmp / "src" / "FrankenCodeBrowser.app" / "Contents"
        (app / "MacOS").mkdir(parents=True)
        (app / "Info.plist").write_text(
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<plist version="1.0"><dict>'
            "<key>CFBundleIdentifier</key><string>test.fcb.verify</string>"
            "<key>CFBundleExecutable</key><string>fcb</string>"
            "</dict></plist>\n",
            encoding="utf-8",
        )
        exe = app / "MacOS" / "fcb"
        exe.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        exe.chmod(0o755)
        self.dmg = self.tmp / "test.dmg"
        created = run(
            "hdiutil", "create", "-quiet", "-volname", "FrankenCodeBrowser",
            "-srcfolder", self.tmp / "src", "-format", "UDZO", self.dmg,
        )
        if created.returncode != 0:
            self.skipTest(f"hdiutil create unavailable: {created.stderr.strip()}")

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def assert_rejected(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("REJECTED:", result.stderr)
        self.assertNotIn("VERIFIED:", result.stdout)
        self.assertIn("image has no valid Developer ID Application signature", result.stderr)
        self.assertIn("no valid stapled notarization ticket", result.stderr)
        self.assertIn("app has no valid Developer ID Application signature", result.stderr)
        attached = run("hdiutil", "info").stdout
        for path in {str(self.tmp), str(self.tmp.resolve())}:
            self.assertNotIn(path, attached, "image left attached")

    def test_unsigned_image_is_rejected(self) -> None:
        self.assert_rejected(run("sh", VERIFY, self.dmg))

    def test_ad_hoc_signed_image_is_rejected(self) -> None:
        signed = run("codesign", "--sign", "-", "--identifier", "test.fcb.verify.dmg", self.dmg)
        self.assertEqual(signed.returncode, 0, signed.stderr)
        self.assert_rejected(run("sh", VERIFY, "--team", "AU8V2Z6NKY", self.dmg))


if __name__ == "__main__":
    unittest.main()
