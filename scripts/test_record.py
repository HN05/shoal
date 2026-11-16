import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
# How long the child sleeps: one that a signal fails to unwind exits after
# this, so the test waits for its exit without a deadline of its own.
SLEEP_SECONDS = 300


class TerminationTests(unittest.TestCase):
    def test_termination_signals_exit_through_cleanup(self):
        for number in (signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=number), tempfile.TemporaryDirectory() as root:
                cleaned = Path(root) / "cleaned"
                program = ("import record, sys, time\n"
                           "record.unwind_on_termination()\n"
                           "try:\n"
                           "    print('ready', flush=True)\n"
                           f"    time.sleep({SLEEP_SECONDS})\n"
                           "finally:\n"
                           f"    open({str(cleaned)!r}, 'w').close()\n")
                child = subprocess.Popen([sys.executable, "-c", program], cwd=SCRIPTS,
                                         env={"PATH": os.environ["PATH"]},
                                         stdout=subprocess.PIPE, text=True)
                self.assertEqual(child.stdout.readline(), "ready\n")
                child.send_signal(number)
                status = child.wait()
                child.stdout.close()
                self.assertEqual(status, 128 + number)
                self.assertTrue(cleaned.exists())


if __name__ == "__main__":
    unittest.main()
