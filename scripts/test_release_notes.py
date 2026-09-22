import subprocess
import tempfile
import unittest
from unittest.mock import Mock
from urllib.error import HTTPError

import release_notes


class ReleaseNotesTests(unittest.TestCase):
    def test_github_notes_omit_unmirrored_references_and_keep_public_links(self):
        source = "https://git.henriknordvik.com/HN05/shoal"
        body = ("Install via [Homebrew](https://github.com/HN05/homebrew-tap).\n\n"
                "## Changes since v0.1.0\n\n"
                f"- Fix \\[cleanup\\] ([#2]({source}/pulls/2))"
                f" — Issues: [#12]({source}/issues/12), [#14]({source}/issues/14)\n"
                f"- Keep (parentheses) ([#3]({source}/pulls/3))\n\n"
                f"[Full changes]({source}/compare/v0.1.0...v0.2.0)")
        expected = ("Install via [Homebrew](https://github.com/HN05/homebrew-tap).\n\n"
                    "## Changes since v0.1.0\n\n"
                    "- Fix \\[cleanup\\]\n- Keep (parentheses)\n\n"
                    "[Full changes](https://github.com/HN05/shoal/compare/v0.1.0...v0.2.0)")
        self.assertEqual(release_notes.for_github(body, source, "HN05/shoal"), expected)
        first = "## Changes in this first release\n\nNo merged pull requests in this release range."
        self.assertEqual(release_notes.for_github(first, source, "HN05/shoal"), first)

    def test_published_ancestry_pagination_and_referenced_issues(self):
        with tempfile.TemporaryDirectory() as root:
            def git(*args):
                return subprocess.check_output(["git", "-C", root, *args], text=True,
                                               stderr=subprocess.DEVNULL).strip()

            def commit(tag):
                git("commit", "--allow-empty", "-qm", tag)
                git("tag", tag)
                return git("rev-parse", "HEAD")

            git("init", "-q", "-b", "main")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.invalid")
            git("config", "core.hooksPath", "/dev/null")
            old = commit("v0.1.0")
            included = commit("v0.1.1")
            target = commit("v0.2.0")
            later = commit("v0.3.0")
            git("checkout", "--orphan", "other")
            unrelated = commit("v0.1.2")

            def pull(number, sha, **extra):
                return dict(number=number, title=f"PR {number}", body="", merged=True,
                            base={"ref": "main"}, merge_commit_sha=sha) | extra

            responses = {
                "/releases?limit=50&page=1": [{"tag_name": "v0.1.0"},
                                             {"tag_name": "v0.1.1", "draft": True}],
                "/releases?limit=50&page=2": [{"tag_name": "v0.1.2"},
                                             {"tag_name": "v0.2.0"},
                                             {"tag_name": "v0.3.0"},
                                             {"tag_name": "v0.4.0-rc1"}],
                "/releases?limit=50&page=3": [],
                "/pulls?state=closed&limit=50&page=1": [
                    pull(1, old), pull(2, included, title="Fix [cleanup]",
                                       body="Closes #12; Refs #12, #13 and #404. "
                                            "https://forge.test/owner/repo/issues/14\n```\n#99\n```"),
                    pull(3, later)],
                "/pulls?state=closed&limit=50&page=2": [
                    pull(4, target, body="Refs #12"), pull(5, target, merged=False),
                    pull(6, target, base={"ref": "other"}), pull(7, unrelated),
                    pull(8, target, head={"ref": "release/v0.2.0"}),
                    pull(9, target, head={"ref": "refs/pull/9/head", "label": "release/v0.2.0"}),
                    pull(10, target, head={"ref": "release/notes"})],
                "/pulls?state=closed&limit=50&page=3": [],
                "/issues/12": {"title": "Cleanup bug"},
                "/issues/13": {"pull_request": {}},
                "/issues/14": {"title": "Another bug"},
            }

            def get(path):
                if path == "/issues/404":
                    raise HTTPError(path, 404, "missing", {}, None)
                return responses[path]

            api = Mock(side_effect=get)

            def generate():
                return release_notes.generate("v0.2.0", git, api, "https://forge.test/owner/repo")

            notes = generate()
            self.assertIn("Changes since v0.1.0", notes)
            self.assertIn("- Fix \\[cleanup\\] ([#2](https://forge.test/owner/repo/pulls/2))"
                          " — Issues: [#12](https://forge.test/owner/repo/issues/12), "
                          "[#14](https://forge.test/owner/repo/issues/14)", notes)
            self.assertIn("- PR 4", notes)
            self.assertIn("- PR 10", notes)
            for number in (1, 3, 5, 6, 7, 8, 9):
                self.assertNotIn(f"- PR {number} (", notes)
            self.assertEqual(sum(call.args == ("/issues/12",) for call in api.call_args_list), 1)
            self.assertIn("/compare/v0.1.0...v0.2.0", notes)
            responses["/releases?limit=50&page=1"][1]["draft"] = False
            self.assertIn("Changes since v0.1.1", generate())
            responses["/releases?limit=50&page=1"][1]["prerelease"] = True
            self.assertIn("Changes since v0.1.0", generate())
            responses["/releases?limit=50&page=1"] = []
            first = generate()
            self.assertIn("Changes in this first release", first)
            self.assertIn("- PR 1", first)
            responses["/pulls?state=closed&limit=50&page=1"] = []
            self.assertIn("No merged pull requests", generate())
            failure = HTTPError("url", 503, "unavailable", {}, None)
            self.addCleanup(failure.close)
            api.side_effect = failure
            with self.assertRaises(HTTPError):
                generate()


if __name__ == "__main__":
    unittest.main()
