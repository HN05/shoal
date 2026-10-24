import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import isolated_git
import test_checkout_release
import test_release
import test_release_notes


class IsolatedGitTests(unittest.TestCase):
    def test_release_fixtures_leave_an_inherited_repository_unchanged(self):
        with tempfile.TemporaryDirectory() as temporary:
            outside = Path(temporary)
            environment = isolated_git.environment(outside)

            def git(*args):
                return subprocess.check_output(["git", "-C", str(outside), *args], text=True,
                                               env=environment).strip()

            git("init", "-q", "-b", "main")
            git("config", "user.name", "Outside")
            git("config", "user.email", "outside@example.invalid")
            git("commit", "--allow-empty", "-qm", "Outside", "--no-gpg-sign")
            (outside / "staged").write_text("user work\n")
            git("add", "staged")
            (outside / ".gitconfig").write_text("[user]\n\tname = Inherited\n")

            def state():
                return (git("for-each-ref"), git("diff", "--cached"),
                        (outside / ".git/config").read_text(), git("stash", "list"))

            before = state()
            loader = unittest.defaultTestLoader
            suite = unittest.TestSuite([
                loader.loadTestsFromTestCase(test_checkout_release.CheckoutReleaseTests),
                loader.loadTestsFromTestCase(test_release.ReleaseIntegrationTests),
                loader.loadTestsFromTestCase(test_release_notes.ReleaseNotesTests),
            ])
            inherited = {"GIT_DIR": str(outside / ".git"), "GIT_WORK_TREE": str(outside),
                         "GIT_INDEX_FILE": str(outside / ".git/index"), "HOME": str(outside),
                         "XDG_CONFIG_HOME": str(outside)}
            with patch.dict(os.environ, inherited):
                result = suite.run(unittest.TestResult())
            self.assertEqual(result.errors + result.failures, [])
            self.assertEqual(result.testsRun, suite.countTestCases())
            self.assertEqual(state(), before)


if __name__ == "__main__":
    unittest.main()
