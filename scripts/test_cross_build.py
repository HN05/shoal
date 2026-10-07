"""Release cross-build helpers use Zig's bundled SDK and preserve tool errors."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts"


class CrossBuildTests(unittest.TestCase):
    def test_zig_wrapper_preserves_arguments_output_and_exit_status(self):
        with tempfile.TemporaryDirectory() as root:
            zig = Path(root) / "zig"
            zig.write_text(
                "#!/usr/bin/env python3\nimport json, sys\n"
                "print(json.dumps(sys.argv[1:]))\n"
                "print('tool warning', file=sys.stderr)\nsys.exit(7)\n"
            )
            zig.chmod(0o755)
            env = {**os.environ, "CI_ZIG_BINARY": str(zig), "SDKROOT": "/test sdk"}
            for tool in ("cc", "c++", "ar", "version"):
                with self.subTest(tool=tool):
                    args = [tool, "-Wl,-O1", "-O3", "input with spaces.o", "-Wl,-O2", "--sysroot=/test sdk",
                            "-L/test sdk/usr/lib", "-F/test sdk/System/Library/Frameworks",
                            "-L/another library"]
                    result = subprocess.run(
                        [str(SCRIPTS / "zig-wrapper.py"), *args], env=env,
                        capture_output=True, text=True, check=False,
                    )
                    expected = args if tool not in ("cc", "c++") else [
                        tool, "-O3", "input with spaces.o", "-Wl,-O2", "-L/another library",
                    ]
                    self.assertEqual(json.loads(result.stdout), expected)
                    self.assertEqual(result.stderr, "tool warning\n")
                    self.assertEqual(result.returncode, 7)

    def test_cross_build_exposes_a_valid_sdk_only_for_macos(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            lib = root / "zig lib"
            headers = lib / "libc/include/any-darwin-any"
            headers.mkdir(parents=True)
            (headers / "stdio.h").write_text("bundled header")
            darwin = lib / "libc/darwin"
            darwin.mkdir()
            (darwin / "libSystem.tbd").write_text("bundled stub")
            (darwin / "SDKSettings.json").write_text('{"MinimalDisplayName":"15.1"}')
            zig = bin_dir / "zig"
            zig.write_text("#!/bin/sh\nprintf '%s\\n' '" + json.dumps({"lib_dir": str(lib)}) + "'\n")
            zig.chmod(0o755)
            uname = bin_dir / "uname"
            uname.write_text("#!/bin/sh\necho Linux\n")
            uname.chmod(0o755)
            cargo = bin_dir / "cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\nimport json, os, sys\n"
                "print(json.dumps({'args': sys.argv[1:], 'sdk': os.environ.get('SDKROOT'), "
                "'wrapper': os.environ['CARGO_ZIGBUILD_ZIG_PATH']}))\n"
            )
            cargo.chmod(0o755)
            env = {key: value for key, value in os.environ.items() if key != "SDKROOT"}
            env.update(PATH=f"{bin_dir}{os.pathsep}{env['PATH']}", CARGO_TARGET_DIR=str(root / "target"))
            for target in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl",
                           "x86_64-apple-darwin", "aarch64-apple-darwin"):
                with self.subTest(target=target):
                    result = subprocess.run(
                        [str(SCRIPTS / "cross-build.sh"), target], env=env,
                        capture_output=True, text=True, check=True,
                    )
                    clean, build = map(json.loads, result.stdout.splitlines())
                    self.assertEqual(clean["args"], ["clean", "-p", "shoal", "--release", "--target", target])
                    self.assertEqual(build["args"], ["zigbuild", "--locked", "--release", "--target", target])
                    self.assertTrue(os.access(build["wrapper"], os.X_OK))
                    if target.endswith("apple-darwin"):
                        sdk = Path(build["sdk"])
                        self.assertEqual((sdk / "usr/include/stdio.h").read_text(), "bundled header")
                        self.assertEqual((sdk / "usr/lib/libSystem.tbd").read_text(), "bundled stub")
                        self.assertEqual(json.loads((sdk / "SDKSettings.json").read_text())["MinimalDisplayName"], "15.1")
                    else:
                        self.assertIsNone(build["sdk"])
            result = subprocess.run(
                [str(SCRIPTS / "cross-build.sh"), "unsupported"], env=env,
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
