"""The macOS signing helper packages Shoal.app, signs it as Shoal and rejects any other signature."""

import os
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts" / "sign-macos.sh"
REQUIREMENT = re.search(r"^requirement='(.*)'$", SCRIPT.read_text(), re.M)[1]


def signature_info(identifier="com.shoal.cli", flags="CodeSignatureFlags(0x0)",
                   requirement=REQUIREMENT, path="Contents/MacOS/shoal"):
    """A bundle's signature info in rcodesign 0.29's layout, trimmed to what the helper reads."""
    lines = [
        "- path: Contents/Info.plist",
        f"- path: {path}",
        "  entity:",
        "    mach_o:",
        "      signature:",
        "        code_directory:",
        f"          flags: {flags}",
        f"          identifier: {identifier}",
        "          executable_segment_flags: ExecutableSegmentFlags(MAIN_BINARY)",
    ]
    if requirement:
        lines += ["        code_requirements:", f"        - 'designated(3): 0: {requirement};'"]
    return "\n".join(lines) + "\n"


class SignMacosTests(unittest.TestCase):
    def sign(self, info):
        """Run the helper on a stand-in binary; return its result, the rcodesign
        calls and the Info.plist and executable of the bundle it made."""
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            calls = root / "calls"
            (root / "info").write_text(info)
            (root / "shoal").write_text("binary")
            rcodesign = root / "rcodesign"
            rcodesign.write_text(
                f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{calls}"\n'
                f'if [ "$1" = print-signature-info ]; then cat "{root}/info"; fi\n'
            )
            rcodesign.chmod(0o755)
            env = {**os.environ, "PATH": f"{root}{os.pathsep}{os.environ['PATH']}"}
            result = subprocess.run(["bash", str(SCRIPT), "key.pem", "shoal", "1.2.3", "app/Shoal.app"],
                                    cwd=root, env=env, capture_output=True, text=True, check=False)
            contents = root / "app/Shoal.app/Contents"
            plist = plistlib.loads((contents / "Info.plist").read_bytes())
            return result, calls.read_text().splitlines(), plist, (contents / "MacOS/shoal").read_text()

    def test_packages_and_signs_as_shoal_and_accepts_the_expected_signature(self):
        result, calls, plist, executable = self.sign(signature_info())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [
            "sign --pem-file key.pem --timestamp-url none app/Shoal.app",
            "print-signature-info app/Shoal.app",
        ])
        self.assertEqual(executable, "binary")
        self.assertEqual(plist["CFBundleIdentifier"], "com.shoal.cli")
        self.assertEqual(plist["CFBundleExecutable"], "shoal")
        self.assertEqual(plist["CFBundleShortVersionString"], "1.2.3")
        self.assertTrue(plist["LSUIElement"])

    def test_rejects_other_signatures(self):
        other_root = re.sub(r'H"[0-9a-f]+"', 'H"' + "0" * 40 + '"', REQUIREMENT)
        for name, info, message in [
            ("identifier", signature_info(identifier="shoal-0ad4ab3a"), "Expected identifier"),
            ("ad-hoc", signature_info(flags="CodeSignatureFlags(ADHOC | LINKER_SIGNED)"),
             "Expected a certificate signature"),
            ("certificate", signature_info(requirement=other_root), "Expected designated requirement"),
            ("no requirement", signature_info(requirement=None), "found: none"),
            ("no executable", signature_info(path="Contents/MacOS/other"), "Expected a signed"),
        ]:
            with self.subTest(name):
                result, *_ = self.sign(info)
                self.assertEqual(result.returncode, 1)
                self.assertIn(message, result.stderr)


if __name__ == "__main__":
    unittest.main()
