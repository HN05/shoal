"""Release cross-build helpers use Zig's bundled SDK and preserve tool errors."""

import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts"


def write_python_tool(path, source):
    """Writes an executable that runs source with this interpreter.

    macOS's /usr/bin/python3 shim sets SDKROOT again, so a fake tool must not
    start through `env python3`."""
    path.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} -c {shlex.quote(source)} "$@"\n')
    path.chmod(0o755)


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
            signer.write_text('#!/bin/sh\nprintf "signed %s %s into %s with %s\\n" "$2" "$3" "$4" "$1"\n')
            signer.chmod(0o755)
            for workflow in ("cross-build.yml", "release.yml"):
                with self.subTest(workflow=workflow):
                    text = (SCRIPTS.parent / "workflows" / workflow).read_text()
                    # The release defines its archive helper just before the loop.
                    loop = re.search(r"          (?:archive\(\)|while read).*?\n          done[^\n]*",
                                     text, re.S)[0]
                    result = subprocess.run(
                        ["bash", "-eu", "-o", "pipefail", "-c",
                         'tar() { printf "%s\\n" "$*"; }; gzip() { cat; };\n' + loop],
                        cwd=source, input="build input\n", capture_output=True, text=True, check=True,
                        env={**os.environ, "CARGO_TARGET_DIR": str(root / "target"),
                             "RELEASE_TAG": "v0.2.0", "epoch": "0", "pem": "key.pem",
                             "staged": str(root / "staged")},
                    )
                    expected = []
                    for target, *_ in release_targets():
                        expected.append(target)
                        # Only the release signs, and it goes by the target, not the platform.
                        if workflow == "release.yml" and target.endswith("-apple-darwin"):
                            release = f"{root}/target/{target}/release"
                            expected.append(
                                f"signed {release}/shoal 0.2.0 into {release}/app/Shoal.app with key.pem")
                    self.assertEqual(result.stdout.splitlines(), expected)
                    if workflow == "release.yml":
                        self.assert_archive_entries(root, source / "dist")

    def assert_archive_entries(self, root, dist):
        """Each archive holds one copy of Shoal, macOS's Shoal.app in an archive of its own."""
        skills = (f"{root}/staged", "skills")
        expected = {}
        for target, platform, *_ in release_targets():
            release = f"{root}/target/{target}/release"
            expected[f"shoal-v0.2.0-{platform}.tar.gz"] = [(release, "shoal"), skills]
            if target.endswith("-apple-darwin"):
                expected[f"shoal-v0.2.0-{platform}-app.tar.gz"] = [(f"{release}/app", "Shoal.app"), skills]
        # The tar stub writes its arguments into the archive.
        actual = {archive.name: re.findall(r"-C (\S+) (\S+)", archive.read_text())
                  for archive in dist.iterdir()}
        self.assertEqual(actual, expected)

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
            write_python_tool(zig, "import json, sys\nprint(json.dumps(sys.argv[1:]))\n"
                                   "print('tool warning', file=sys.stderr)\nsys.exit(7)\n")
            env = {**os.environ, "CI_ZIG_BINARY": str(zig), "SDKROOT": "/test sdk"}
            for tool in ("cc", "c++", "ar", "version"):
                with self.subTest(tool=tool):
                    args = [tool, "-Wl,-O1", "-O3", "input with spaces.o", "-Wl,-O2", "--sysroot=/test sdk",
                            "-L/test sdk/usr/lib", "-F/test sdk/System/Library/Frameworks",
                            "-L/another library"]
                    result = subprocess.run(
                        [sys.executable, str(SCRIPTS / "zig-wrapper.py"), *args], env=env,
                        capture_output=True, text=True, check=False,
                    )
                    expected = args if tool not in ("cc", "c++") else [
                        tool, "-Wl,-O1", "-O3", "input with spaces.o", "-Wl,-O2", "-L/another library",
                    ]
                    self.assertEqual(json.loads(result.stdout), expected)
                    self.assertEqual(result.stderr, "tool warning\n")
                    self.assertEqual(result.returncode, 7)

    def test_cross_build_links_with_the_sdk_and_header_room_only_for_macos(self):
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
            # `zig env` prints ZON.
            zig.write_text(
                "#!/bin/sh\ncat <<'EOF'\n.{\n"
                f'    .zig_exe = "{zig}",\n    .lib_dir = "{lib}",\n    .std_dir = "{lib}/std",\n'
                "    .env = .{\n        .ZIG_LIB_DIR = null,\n    },\n}\nEOF\n"
            )
            zig.chmod(0o755)
            uname = bin_dir / "uname"
            uname.write_text("#!/bin/sh\necho Linux\n")
            uname.chmod(0o755)
            cargo = bin_dir / "cargo"
            write_python_tool(cargo, "import json, os, sys\n"
                                     "print(json.dumps({'args': sys.argv[1:], 'sdk': os.environ.get('SDKROOT'), "
                                     "'rustflags': os.environ.get('RUSTFLAGS'), "
                                     "'wrapper': os.environ['CARGO_ZIGBUILD_ZIG_PATH']}))\n")
            env = {key: value for key, value in os.environ.items() if key not in ("SDKROOT", "RUSTFLAGS")}
            env.update(PATH=f"{bin_dir}{os.pathsep}{env['PATH']}", CARGO_TARGET_DIR=str(root / "target"))
            # The release sets no RUSTFLAGS; cross-build.yml sets some.
            for target, rustflags in [(row[0], rustflags) for row in release_targets()
                                      for rustflags in (None, "-D warnings")]:
                with self.subTest(target=target, rustflags=rustflags):
                    result = subprocess.run(
                        [str(SCRIPTS / "cross-build.sh"), target],
                        env=env if rustflags is None else {**env, "RUSTFLAGS": rustflags},
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
                        headerpad = "-C link-arg=-Wl,-headerpad_max_install_names"
                        self.assertEqual(build["rustflags"],
                                         headerpad if rustflags is None else f"{rustflags} {headerpad}")
                    else:
                        self.assertIsNone(build["sdk"])
                        self.assertEqual(build["rustflags"], rustflags)
            result = subprocess.run(
                [str(SCRIPTS / "cross-build.sh"), "unsupported"], env=env,
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
