import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / ".forgejo/scripts/checkout-release.sh"


class CheckoutReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.remote = self.root / "upstream.git"
        self.remote.mkdir()
        self.git(self.remote, "init", "--initial-branch=main")
        self.git(self.remote, "config", "user.name", "Release test")
        self.git(self.remote, "config", "user.email", "release@example.test")
        self.git(self.remote, "config", "core.hooksPath", "/dev/null")
        self.git(self.remote, "commit", "--allow-empty", "-m", "Initial", "--no-gpg-sign")
        (self.remote / "payload").write_text("released\n")
        self.git(self.remote, "add", "payload")
        self.git(self.remote, "commit", "-m", "Release", "--no-gpg-sign")
        self.revision = self.git(self.remote, "rev-parse", "HEAD")
        self.git(self.remote, "tag", "--no-sign", "v0.0.0")
        self.git(self.remote, "tag", "--no-sign", "-a", "v12.34.56", "-m", "Release")
        (self.remote / "payload").write_text("later main\n")
        self.git(self.remote, "commit", "-am", "Later main", "--no-gpg-sign")

    def git(self, directory, *args):
        return subprocess.check_output(
            ["git", "-C", str(directory), *args], text=True, stderr=subprocess.PIPE).strip()

    def checkout(self, tag, directory):
        directory.mkdir()
        return subprocess.run(
            ["sh", str(SCRIPT)], cwd=directory, capture_output=True, text=True,
            env={**os.environ, "RELEASE_TAG": tag, "FORGEJO_SERVER": self.root.as_uri() + "/",
                 "REPOSITORY": "upstream"})

    def test_lightweight_and_annotated_tags_check_out_shallow_and_detached(self):
        for tag in ("v0.0.0", "v12.34.56"):
            with self.subTest(tag=tag):
                job = self.root / tag
                result = self.checkout(tag, job)
                self.assertEqual(result.returncode, 0, result.stderr)
                source = job / "source"
                self.assertEqual(self.git(source, "rev-parse", "HEAD"), self.revision)
                self.assertEqual(self.git(source, "rev-parse", "--abbrev-ref", "HEAD"), "HEAD")
                self.assertEqual(self.git(source, "rev-parse", "--is-shallow-repository"), "true")
                self.assertEqual(self.git(source, "rev-list", "--count", "HEAD"), "1")
                self.assertEqual((source / "payload").read_text(), "released\n")
                self.assertFalse((source / ".forgejo/scripts/checkout-release.sh").exists())

    def test_invalid_tags_fail_before_creating_source(self):
        for index, tag in enumerate(("", "1.2.3", "v01.2.3", "v1.02.3", "v1.2.03",
                                     "v1.2.3-rc1", "v1.2.3+build", "v1.2", "v1.2.3\n",
                                     "refs/tags/v1.2.3", "--help", "v1.2.3:refs/heads/main")):
            with self.subTest(tag=tag):
                job = self.root / f"invalid-{index}"
                result = self.checkout(tag, job)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("expected stable vX.Y.Z tag", result.stderr)
                self.assertFalse((job / "source").exists())

    def test_missing_tag_fails_without_checking_out_main(self):
        job = self.root / "missing"
        result = self.checkout("v9.9.9", job)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((job / "source/payload").exists())
        with self.assertRaises(subprocess.CalledProcessError):
            self.git(job / "source", "rev-parse", "--verify", "HEAD")
