"""The flaky-test report lists every test nextest failed, and counts a round broken whenever nextest fails for more than its failed tests."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / ".forgejo" / "scripts" / "flaky-report.sh"

START = """\
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.59s
────────────
 Nextest run ID 1fe6c3e1-2425-4cad-9a2a-ecc30c53c078 with nextest profile: ci
    Starting 3 tests across 2 binaries
"""

PASSED = """\
        PASS [   0.017s] (  1/3) a tests::good
        LEAK [   0.120s] (  2/3) a tests::leaky
"""

# The failed test's output holds a helper's libtest lines, which name no test
# nextest ran.
FAILED = """\
        FAIL [   0.017s] (  3/3) b tests::bad
  stdout ───

    running 1 test
    test helper ... FAILED
    test tests::bad ... FAILED

  stderr ───

    thread 'tests::bad' (415442519) panicked at src/lib.rs:6:38:
    nope
    second
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

"""

CRASHED = """\
     SIGABRT [   0.019s] (  3/3) b tests::aborts
  stdout ───

    running 1 test

    (test aborted with signal 6: SIGABRT)

"""

HUNG = """\
        SLOW [>  60.000s] (───) b tests::hangs
 TERMINATING [> 600.000s] (───) b tests::hangs
     TIMEOUT [ 600.005s] (  3/3) b tests::hangs
  stdout ───

    running 1 test

    (test timed out)

"""


def summary(*failures):
    return ("────────────\n     Summary [   4.005s] 3 tests run\n" + "".join(failures)
            + ("error: test run failed\n" if failures else ""))


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
        result = self.report(START + PASSED + summary(), 0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("No test failed.", result.stdout)
        self.assertNotIn("[!WARNING]", result.stdout)

    def test_a_failed_test_is_listed_with_its_panic(self):
        result = self.report(START + PASSED + FAILED + summary(FAILED), 100)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("| `b tests::bad` | 1 of 1 | `src/lib.rs:6:38: nope` |", result.stdout)
        self.assertNotIn("helper", result.stdout)
        self.assertNotIn("[!WARNING]", result.stdout)
        self.assertIn("thread 'tests::bad' (415442519) panicked", result.stderr)

    def test_crashed_and_hung_tests_are_failures(self):
        result = self.report(START + CRASHED + HUNG + summary(CRASHED, HUNG), 100)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("| `b tests::aborts` | 1 of 1 |  |", result.stdout)
        self.assertIn("| `b tests::hangs` | 1 of 1 |  |", result.stdout)
        self.assertNotIn("[!WARNING]", result.stdout)

    def test_a_failure_without_a_failed_test_breaks_the_round(self):
        result = self.report(START + PASSED + summary(), 100)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("1 of 1 rounds failed without a failing test to account for it", result.stdout)
        self.assertIn("No test failed.", result.stdout)

    def test_nextest_killed_after_a_failed_test_breaks_the_round(self):
        result = self.report(START + FAILED, 137)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("| `b tests::bad` | 1 of 1 | `src/lib.rs:6:38: nope` |", result.stdout)
        self.assertIn("round 1 of 1 broke: nextest exited with status 137", result.stderr)

    def test_a_build_error_breaks_the_round_and_prints_the_error(self):
        result = self.report(BUILD_ERROR, 101)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("error[E0425]: cannot find value `missing` in this scope", result.stderr)
        self.assertIn("   Compiling crate v0.1.0", result.stderr)


if __name__ == "__main__":
    unittest.main()
