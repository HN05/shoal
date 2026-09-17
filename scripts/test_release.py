import unittest
from pathlib import Path
import subprocess
import tempfile
from unittest.mock import patch
import release


class ReleaseTests(unittest.TestCase):
    manifest = '[package]\nname = "shoal"\nversion = "0.1.0"\n'
    lock = '[[package]]\nname = "shoal"\nversion = "0.1.0"\n\n[[package]]\nname = "other"\nversion = "2.0.0"\nsource = "registry"\n'

    def test_next_unused_patch_and_only_local_package_changes(self):
        version, manifest, lock = release.bump(self.manifest, self.lock, None, {"v0.1.1"})
        self.assertEqual(version, "0.1.2")
        self.assertEqual(release.versions(manifest, lock), version)
        self.assertIn('name = "other"\nversion = "2.0.0"', lock)

    def test_explicit_version_and_rejections(self):
        self.assertEqual(release.bump(self.manifest, self.lock, "1.0.0", set())[0], "1.0.0")
        for version in ("0.1.0", "0.0.9", "0.1.1", "v1.0.0", "1.0.0-rc1", "01.0.0"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.bump(self.manifest, self.lock, version, {"v0.1.1"})
        with self.assertRaises(ValueError):
            release.bump(self.manifest, self.lock.replace('"0.1.0"', '"0.2.0"'), None, set())


class ReleaseIntegrationTests(unittest.TestCase):
    def test_prepare_and_publish_use_separate_reviewed_commits(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            remote, checkout = root / "remote.git", root / "checkout"

            def command(*args, cwd=None):
                return subprocess.check_output(args, cwd=cwd, text=True, stderr=subprocess.DEVNULL).strip()

            command("git", "init", "--bare", "--initial-branch=main", str(remote))
            command("git", "clone", str(remote), str(checkout))
            command("git", "config", "user.name", "Test", cwd=checkout)
            command("git", "config", "user.email", "test@example.invalid", cwd=checkout)
            command("git", "config", "core.hooksPath", "/dev/null", cwd=checkout)
            (checkout / "Cargo.toml").write_text(ReleaseTests.manifest)
            (checkout / "Cargo.lock").write_text(ReleaseTests.lock)
            command("git", "add", ".", cwd=checkout)
            command("git", "commit", "-m", "Initial", cwd=checkout)
            command("git", "push", "origin", "main", cwd=checkout)
            actual_run = release.run
            forge_calls = []

            def run(*args, **kwargs):
                if args[0] == "fj":
                    forge_calls.append(args)
                    return ""
                return actual_run(*args, **kwargs)

            with patch.object(release, "ROOT", checkout), patch.object(release, "run", run), \
                    patch.object(release, "validate") as validate:
                release.prepare(None, False)
                self.assertEqual(command("git", "branch", "--show-current", cwd=checkout), "release/v0.1.1")
                self.assertTrue(any(args[1:3] == ("pr", "create") for args in forge_calls))
                self.assertEqual(validate.call_count, 1)
                with self.assertRaises(ValueError):
                    release.publish("0.1.1", False)
                # Simulate the separately approved merge of the release PR.
                command("git", "switch", "main", cwd=checkout)
                command("git", "merge", "--ff-only", "release/v0.1.1", cwd=checkout)
                command("git", "push", "origin", "main", cwd=checkout)
                release.publish("0.1.1", False)
                self.assertTrue(any(args[1:3] == ("release", "create") for args in forge_calls))
                self.assertEqual(command("git", "rev-parse", "v0.1.1^{commit}", cwd=remote),
                                 command("git", "rev-parse", "main", cwd=remote))
                self.assertEqual(validate.call_count, 2)


if __name__ == "__main__":
    unittest.main()
