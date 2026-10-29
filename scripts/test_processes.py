import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

import processes

# A generous hang guard for a child to record its PID.
START_SECONDS = 60


class StopTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp()).resolve()
        self.run_dir = self.root / "run"
        self.elsewhere = self.root / "elsewhere"
        self.run_dir.mkdir()
        self.elsewhere.mkdir()
        self.started = []

    def tearDown(self):
        processes.stop(self.started)
        subprocess.run(["rm", "-rf", str(self.root)], check=True)

    def detached(self, cwd, name):
        """A `sleep` working in `cwd` whose parent has exited."""
        pid_file = self.root / name
        subprocess.run(["sh", "-c", f"sleep 300 & echo $! > {pid_file}.tmp; "
                        f"mv {pid_file}.tmp {pid_file}"],
                       cwd=cwd, env={"PATH": os.environ["PATH"]}, stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        deadline = time.monotonic() + START_SECONDS
        while not pid_file.exists():
            self.assertLess(time.monotonic(), deadline, "the detached process never started")
            time.sleep(0.05)
        process = processes.snapshot()[int(pid_file.read_text())]
        self.started.append(process)
        return process

    def test_a_run_stops_what_it_started_in_its_directory_and_nothing_else(self):
        earlier = self.detached(self.run_dir, "earlier")
        # Start times have whole seconds; begin the run in the next one.
        since = earlier.started + 1
        while time.time() < since:
            time.sleep(0.05)
        left = self.detached(self.run_dir, "left")
        unrelated = self.detached(self.elsewhere, "unrelated")

        found = {process.pid for process in processes.started_by_run(self.run_dir, since)}

        self.assertIn(left.pid, found)
        self.assertNotIn(earlier.pid, found)
        self.assertNotIn(unrelated.pid, found)
        self.assertEqual(processes.stop([left]), [])
        self.assertFalse(processes.alive(left))
        self.assertTrue(processes.alive(earlier))
        self.assertTrue(processes.alive(unrelated))

    def test_an_interrupted_run_leaves_detached_processes_in_its_directory(self):
        left = self.detached(self.run_dir, "left")
        unrelated = self.detached(self.elsewhere, "unrelated")

        found = {process.pid for process in processes.detached_in(self.run_dir)}

        self.assertIn(left.pid, found)
        self.assertNotIn(unrelated.pid, found)

    def test_a_reused_pid_is_not_the_process_it_named(self):
        left = self.detached(self.run_dir, "left")
        replaced = processes.Process(left.pid, left.ppid, left.started - 1, left.cwd)

        self.assertFalse(processes.alive(replaced))
        self.assertEqual(processes.stop([replaced]), [])
        self.assertTrue(processes.alive(left))


if __name__ == "__main__":
    unittest.main()
