"""Release cross-build helpers use Zig's bundled SDK and preserve tool errors."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts"


def release_targets():
    return [line.split() for line in (SCRIPTS / "release-targets.txt").read_text().splitlines()]


class CrossBuildTests(unittest.TestCase):
    def test_workflow_build_stdin_cannot_consume_target_rows(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            source = root / "source"
            (source / "dist").mkdir(parents=True)
            script_dir = source / ".forgejo/scripts"
            script_dir.mkdir(parents=True)
            for directory in (root, script_dir):
                (directory / "release-targets.txt").write_bytes((SCRIPTS / "release-targets.txt").read_bytes())
                helper = directory / "cross-build.sh"
                helper.write_text('#!/bin/sh\nread -r ignored || true\nprintf "%s\\n" "$1"\n')
                helper.chmod(0o755)
            signer = root / "sign-macos.sh"
            signer.write_text('#!/bin/sh\nprintf "signed %s with %s\\n" "$2" "$1"\n')
            signer.chmod(0o755)
            for workflow in ("ci.yml", "release.yml"):
                with self.subTest(workflow=workflow):
                    text = (SCRIPTS.parent / "workflows" / workflow).read_text()
                    loop = re.search(r"          while read.*?\n          done[^\n]*", text, re.S)[0]
                    result = subprocess.run(
                        ["bash", "-eu", "-c", 'tar() { :; }; gzip() { cat; };\n' + loop],
                        cwd=source, input="build input\n", capture_output=True, text=True, check=True,
                        env={**os.environ, "CARGO_TARGET_DIR": str(root / "target"),
                             "RELEASE_TAG": "v0.2.0", "epoch": "0", "pem": "key.pem"},
                    )
                    expected = []
                    for target, *_ in release_targets():
                        expected.append(target)
                        # Only the release signs, and it goes by the target, not the platform.
                        if workflow == "release.yml" and target.endswith("-apple-darwin"):
                            expected.append(f"signed {root}/target/{target}/release/shoal with key.pem")
                    self.assertEqual(result.stdout.splitlines(), expected)

    def test_target_validation_follows_the_table_beside_the_helper(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            script = root / "cross-build.sh"
            script.write_bytes((SCRIPTS / "cross-build.sh").read_bytes())
            table = root / "release-targets.txt"
            table.write_text("custom-target custom-platform linux arm\n")
            for tool, body in (("zig", "exit 0"), ("uname", "echo Linux"),
                               ("cargo", 'printf "%s\\n" "$*"')):
                executable = root / tool
                executable.write_text(f"#!/bin/sh\n{body}\n")
                executable.chmod(0o755)
            env = {**os.environ, "PATH": f"{root}{os.pathsep}{os.environ['PATH']}"}
            result = subprocess.run(["bash", str(script), "custom-target"], env=env,
                                    capture_output=True, text=True, check=True)
            self.assertEqual(result.stdout.splitlines(), [
                "clean --workspace --release --target custom-target",
                "zigbuild --locked --release --target custom-target",
            ])
            table.write_text("replacement-target replacement-platform linux arm\n")
            result = subprocess.run(["bash", str(script), "custom-target"], env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stderr, "Unknown release target: custom-target\n")
            self.assertEqual(result.stdout, "")

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
            for target, _platform, _os, _arch in release_targets():
                with self.subTest(target=target):
                    result = subprocess.run(
                        [str(SCRIPTS / "cross-build.sh"), target], env=env,
                        capture_output=True, text=True, check=True,
                    )
                    clean, build = map(json.loads, result.stdout.splitlines())
                    self.assertEqual(clean["args"], ["clean", "--workspace", "--release", "--target", target])
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
