import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import isolated_git
import profile_shoal


class SummarizeTests(unittest.TestCase):
    def test_sums_timing_lines_per_subject_costliest_first(self):
        lines = ["profile request 4.0ms list_workspaces",
                 "profile run 10.0ms git status",
                 "profile request 2.0ms list_workspaces",
                 "Daemon listening",
                 "profile run 30.0ms git status"]
        self.assertEqual(profile_shoal.summarize(lines, 2), [
            ("run", "git status", 2, 20.0, 30.0),
            ("request", "list_workspaces", 2, 3.0, 4.0),
        ])


class IsolationTests(unittest.TestCase):
    def test_fixture_leaves_the_invoking_repository_and_settings_alone(self):
        with tempfile.TemporaryDirectory() as root:
            caller, sandbox = Path(root, "caller"), Path(root, "sandbox")
            sandbox.mkdir()
            plain = isolated_git.environment(root)
            subprocess.run(["git", "init", "-q", str(caller)], env=plain, check=True)
            inherited = {"GIT_DIR": str(caller / ".git"), "GIT_WORK_TREE": str(caller),
                         "SHOAL_SCOPE_TOKEN": "caller", "CODEX_HOME": str(caller)}
            with patch.dict(os.environ, inherited):
                env = profile_shoal.environment(sandbox)
            self.assertFalse(set(inherited) & set(env))
            self.assertEqual(env["SHOAL_STATE_DIR"], str(sandbox / "s"))
            repo = profile_shoal.fixture_repository(sandbox, env)
            log = subprocess.run(["git", "-C", str(repo), "log", "--format=%an"], env=env,
                                 capture_output=True, text=True, check=True)
            self.assertEqual(log.stdout, "Shoal Profile\n")
            caller_head = subprocess.run(["git", "-C", str(caller), "rev-parse", "HEAD"],
                                         env=plain, capture_output=True)
            self.assertNotEqual(caller_head.returncode, 0)


if __name__ == "__main__":
    unittest.main()
