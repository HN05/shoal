import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
# A generous hang guard for the child to get ready or exit.
WAIT_SECONDS = 60


class TerminationTests(unittest.TestCase):
    def test_termination_signals_exit_through_cleanup(self):
        for number in (signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signal=number), tempfile.TemporaryDirectory() as root:
                cleaned = Path(root) / "cleaned"
                program = ("import record, sys, time\n"
                           "record.unwind_on_termination()\n"
                           "try:\n"
                           "    print('ready', flush=True)\n"
                           "    time.sleep(300)\n"
                           "finally:\n"
                           f"    open({str(cleaned)!r}, 'w').close()\n")
                child = subprocess.Popen([sys.executable, "-c", program], cwd=SCRIPTS,
                                         env={"PATH": os.environ["PATH"]},
                                         stdout=subprocess.PIPE, text=True)
                self.assertEqual(child.stdout.readline(), "ready\n")
                child.send_signal(number)
                status = child.wait(timeout=WAIT_SECONDS)
                child.stdout.close()
                self.assertEqual(status, 128 + number)
                self.assertTrue(cleaned.exists())


if __name__ == "__main__":
    unittest.main()
