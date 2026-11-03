"""The flaky-test report counts a round broken whenever cargo fails for more than its failed tests."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts" / "flaky-report.sh"

PASSING = """\
     Running tests/a.rs (target/debug/deps/a-9233771de7c2fbe8)

running 1 test
test ok ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

"""

CRASHED = """\
     Running tests/b.rs (target/debug/deps/b-df9440f7b507a560)

running 1 test
error: test failed, to rerun pass `--test b`

Caused by:
  process didn't exit successfully: `target/debug/deps/b-df9440f7b507a560` (signal: 6, SIGABRT: process abort signal)
"""

FAILED = """\
     Running tests/c.rs (target/debug/deps/c-d6d98225591a985e)

running 1 test
test bad ... FAILED

failures:

---- bad stdout ----

thread 'bad' (165994152) panicked at tests/c.rs:1:20:
nope
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    bad

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

error: test failed, to rerun pass `--test c`
"""

BUILD_ERROR = """\
   Compiling crate v0.1.0
error[E0425]: cannot find value `missing` in this scope
 --> src/lib.rs:1:14
error: could not compile `crate` (lib test) due to 1 previous error
"""


class FlakyReportTests(unittest.TestCase):
    def report(self, log, status):
        """Runs one round against a cargo that prints `log` and exits with `status`."""
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "round.log").write_text(log)
            cargo = root / "cargo"
            cargo.write_text(f'#!/bin/sh\ncat "{root}/round.log"\nexit {status}\n')
            cargo.chmod(0o755)
            env = {**os.environ, "PATH": f"{root}{os.pathsep}{os.environ['PATH']}"}
            return subprocess.run(["sh", str(SCRIPT), "1"], cwd=root, env=env,
                                  capture_output=True, text=True, check=False)

    def test_a_clean_round_passes(self):
        result = self.report(PASSING, 0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No test failed.", result.stdout)
        self.assertNotIn("[!WARNING]", result.stdout)

    def test_a_failed_test_accounts_for_cargo_failing(self):
        result = self.report(PASSING + FAILED, 101)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("| `bad` | 1 of 1 | `tests/c.rs:1:20: nope` |", result.stdout)
        self.assertNotIn("[!WARNING]", result.stdout)

    def test_a_crashed_binary_beside_passing_ones_breaks_the_round(self):
        result = self.report(PASSING + CRASHED, 101)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("1 of 1 rounds failed without a failing test to account for it", result.stdout)
        self.assertIn("No test failed.", result.stdout)
        self.assertIn("(signal: 6, SIGABRT: process abort signal)", result.stderr)

    def test_a_crashed_binary_beside_a_failed_test_breaks_the_round(self):
        result = self.report(PASSING + CRASHED + FAILED, 101)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("1 of 1 rounds failed without a failing test to account for it", result.stdout)
        self.assertIn("| `bad` | 1 of 1 |", result.stdout)
        self.assertIn("(signal: 6, SIGABRT: process abort signal)", result.stderr)
        # Only the crashed binary's part of the log, not the ones that reported results.
        self.assertNotIn("Running tests/a.rs", result.stderr)
        self.assertNotIn("Running tests/c.rs", result.stderr)

    def test_cargo_killed_after_passing_binaries_breaks_the_round(self):
        result = self.report(PASSING + "     Running tests/d.rs (target/debug/deps/d)\n\nrunning 3 tests\n", 137)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("round 1 of 1 broke: cargo exited with status 137", result.stderr)
        self.assertIn("running 3 tests", result.stderr)

    def test_cargo_killed_after_a_failed_test_breaks_the_round(self):
        result = self.report(FAILED + "     Running tests/d.rs (target/debug/deps/d)\n\nrunning 3 tests\n", 137)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("| `bad` | 1 of 1 |", result.stdout)
        self.assertIn("round 1 of 1 broke: cargo exited with status 137", result.stderr)
        self.assertIn("running 3 tests", result.stderr)

    def test_a_build_error_breaks_the_round_and_prints_the_error(self):
        result = self.report(BUILD_ERROR, 101)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("error[E0425]: cannot find value `missing` in this scope", result.stderr)
        self.assertIn("   Compiling crate v0.1.0", result.stderr)


if __name__ == "__main__":
    unittest.main()
