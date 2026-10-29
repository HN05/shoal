"""The macOS signing helper signs as Shoal and rejects any other signature."""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts" / "sign-macos.sh"
REQUIREMENT = re.search(r"^requirement='(.*)'$", SCRIPT.read_text(), re.M)[1]


def signature_info(identifier="com.shoal.cli", flags="CodeSignatureFlags(0x0)",
                   requirement=REQUIREMENT):
    """Signature info in rcodesign 0.29's layout, trimmed to what the helper reads."""
    lines = [
        "- path: shoal",
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
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            calls = root / "calls"
            (root / "info").write_text(info)
            rcodesign = root / "rcodesign"
            rcodesign.write_text(
                f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{calls}"\n'
                f'if [ "$1" = print-signature-info ]; then cat "{root}/info"; fi\n'
            )
            rcodesign.chmod(0o755)
            env = {**os.environ, "PATH": f"{root}{os.pathsep}{os.environ['PATH']}"}
            result = subprocess.run(["bash", str(SCRIPT), "key.pem", "dist/shoal"], env=env,
                                    capture_output=True, text=True, check=False)
            return result, calls.read_text().splitlines()

    def test_signs_as_shoal_and_accepts_the_expected_signature(self):
        result, calls = self.sign(signature_info())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [
            "sign --pem-file key.pem --binary-identifier com.shoal.cli --timestamp-url none dist/shoal",
            "print-signature-info dist/shoal",
        ])

    def test_rejects_other_signatures(self):
        other_root = re.sub(r'H"[0-9a-f]+"', 'H"' + "0" * 40 + '"', REQUIREMENT)
        for name, info, message in [
            ("identifier", signature_info(identifier="shoal-0ad4ab3a"), "Expected identifier"),
            ("ad-hoc", signature_info(flags="CodeSignatureFlags(ADHOC | LINKER_SIGNED)"),
             "Expected a certificate signature"),
            ("certificate", signature_info(requirement=other_root), "Expected designated requirement"),
            ("no requirement", signature_info(requirement=None), "found: none"),
        ]:
            with self.subTest(name):
                result, _ = self.sign(info)
                self.assertEqual(result.returncode, 1)
                self.assertIn(message, result.stderr)


if __name__ == "__main__":
    unittest.main()
