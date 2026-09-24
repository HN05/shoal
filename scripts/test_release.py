import unittest
import copy
from io import BytesIO
from pathlib import Path
import subprocess
import tempfile
from unittest.mock import MagicMock, patch
from urllib.error import HTTPError
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

    def test_merged_identity_including_deleted_branch(self):
        pr = {"merged": True, "number": 7, "merge_commit_sha": "a" * 40,
              "base": {"ref": "main", "repo": {"full_name": "HN05/shoal"}},
              "head": {"ref": "release/v0.1.1", "repo": {"full_name": "HN05/shoal"}}}
        self.assertEqual(release.merged_release(pr, "HN05/shoal", 7), ("0.1.1", "a" * 40))
        pr["head"].update(ref="refs/pull/7/head", label="release/v0.1.1")
        self.assertEqual(release.merged_release(pr, "HN05/shoal", 7), ("0.1.1", "a" * 40))
        mutations = [
            lambda p: p.update(merged=False),
            lambda p: p.update(number=8),
            lambda p: p.update(merge_commit_sha="not-a-commit"),
            lambda p: p["base"].update(ref="other"),
            lambda p: p["head"]["repo"].update(full_name="fork/shoal"),
            lambda p: p["base"]["repo"].update(full_name="fork/shoal"),
            lambda p: p["head"].update(ref="refs/pull/8/head"),
            lambda p: p["head"].update(ref="feature", label="release/v0.1.1"),
        ]
        for mutate in mutations:
            invalid = copy.deepcopy(pr)
            mutate(invalid)
            with self.assertRaises(ValueError):
                release.merged_release(invalid, "HN05/shoal", 7)

    def test_one_click_release_pins_merge_and_only_publishes_after_success(self):
        with patch.dict(release.os.environ, {"RELEASE_REPOSITORY": "HN05/shoal"}), \
                patch.object(release, "versions", return_value="0.1.0"), \
                patch.object(release, "prepare", return_value={"number": 7}) as prepare, \
                patch.object(release, "git", return_value="a" * 40), \
                patch.object(release, "api", side_effect=[None, {"merged": True}]) as api, \
                patch.object(release, "publish_merged") as publish:
            release.release_all(None)
            prepare.assert_called_once_with(None, False, automated=True)
            self.assertEqual(api.call_args_list[0].args[1], {
                "Do": "rebase", "head_commit_id": "a" * 40, "delete_branch_after_merge": True,
            })
            publish.assert_called_once_with({"merged": True}, "HN05/shoal", 7)
            api.side_effect = ValueError("merge blocked")
            publish.reset_mock()
            with self.assertRaises(ValueError):
                release.release_all("0.2.0")
            publish.assert_not_called()

    def test_one_click_release_resumes_current_explicit_version(self):
        with patch.object(release, "versions", return_value="0.2.0"), \
                patch.object(release, "resume") as resume, \
                patch.object(release, "prepare") as prepare:
            release.release_all("0.2.0")
            resume.assert_called_once_with("0.2.0")
            prepare.assert_not_called()

    def test_resume_publishes_from_the_existing_tag_commit(self):
        def git(*args):
            responses = {
                ("status", "--porcelain"): "",
                ("tag", "--list", "v0.2.0"): "v0.2.0",
                ("rev-parse", "refs/tags/v0.2.0^{commit}"): "a" * 40,
                ("merge-base", "--is-ancestor", "a" * 40, "origin/main"): "",
                ("show", "a" * 40 + ":Cargo.toml"): self.manifest.replace("0.1.0", "0.2.0"),
                ("show", "a" * 40 + ":Cargo.lock"): self.lock.replace("0.1.0", "0.2.0"),
            }
            return responses[args]

        with patch.object(release, "git", side_effect=git), \
                patch.object(release, "run") as run, \
                patch.object(release, "publish") as publish:
            release.resume("0.2.0")
            run.assert_any_call("git", "checkout", "--detach", "a" * 40)
            publish.assert_called_once_with("0.2.0", False, merged_commit="a" * 40)

    def test_resume_refuses_a_dirty_checkout_before_fetching(self):
        with patch.object(release, "git", return_value="dirty"), \
                patch.object(release, "run") as run, self.assertRaisesRegex(
                    ValueError, "clean checkout"):
            release.resume("0.2.0")
        run.assert_not_called()

    def test_resume_requires_an_existing_tag(self):
        with patch.object(release, "git", return_value=""), \
                patch.object(release, "run"), self.assertRaisesRegex(
                    ValueError, "existing release tag"):
            release.resume("0.2.0")

    def test_ci_publishes_with_repository_api_without_cli_login(self):
        with patch.dict(release.os.environ, {"RELEASE_AUTOMATION_TOKEN": "test-token",
                                             "RELEASE_REPOSITORY": "HN05/shoal"}), \
                patch.object(release, "api") as api, patch.object(release, "run") as run, \
                patch.object(release.release_notes, "generate", return_value="Changelog") as notes:
            missing = HTTPError("url", 404, "missing", {}, None)
            self.addCleanup(missing.close)
            api.side_effect = [missing, None, []]
            release.create_release("0.2.0")
            run.assert_not_called()
            self.assertEqual(api.call_args_list[0].args,
                             ("/repos/HN05/shoal/releases/tags/v0.2.0",))
            self.assertEqual(api.call_args_list[0].kwargs, {"authenticated": False})
            endpoint, payload = api.call_args_list[1].args
            self.assertEqual(endpoint, "/repos/HN05/shoal/releases")
            self.assertEqual(payload["tag_name"], "v0.2.0")
            self.assertFalse(payload["draft"])
            self.assertFalse(payload["prerelease"])
            self.assertEqual(payload["body"], "Changelog")
            self.assertEqual(notes.call_args.args[0], "v0.2.0")
            notes.call_args.args[2]("/releases?limit=50&page=1")
            self.assertEqual(api.call_args.args, ("/repos/HN05/shoal/releases?limit=50&page=1",))
            self.assertEqual(api.call_args.kwargs, {"authenticated": False})

    def test_existing_forgejo_release_is_reused_with_or_without_ci_token(self):
        for token in ("test-token", ""):
            with self.subTest(token=bool(token)), \
                    patch.dict(release.os.environ, {"RELEASE_AUTOMATION_TOKEN": token,
                                                    "RELEASE_REPOSITORY": "HN05/shoal"}), \
                    patch.object(release, "api",
                                 return_value={"tag_name": "v0.2.0"}) as api, \
                    patch.object(release, "run") as run, \
                    patch.object(release.release_notes, "generate") as notes:
                release.create_release("0.2.0")
                api.assert_called_once_with("/repos/HN05/shoal/releases/tags/v0.2.0",
                                            authenticated=False)
                run.assert_not_called()
                notes.assert_not_called()

    def test_http_errors_include_the_forge_message(self):
        error = HTTPError("url", 403, "Forbidden", {},
                          BytesIO(b'{"message":"token scope denied"}'))
        self.assertEqual(release.http_error_message(error),
                         "HTTP 403 Forbidden: token scope denied")

    def test_public_api_reads_omit_the_release_token(self):
        response = MagicMock()
        response.__enter__.return_value.read.return_value = b"[]"
        with patch.dict(release.os.environ, {"RELEASE_AUTOMATION_TOKEN": "secret"}), \
                patch.object(release, "urlopen", return_value=response) as urlopen:
            self.assertEqual(release.api("/public", authenticated=False), [])
        request = urlopen.call_args.args[0]
        self.assertIsNone(request.get_header("Authorization"))


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

            def api(path, **_kwargs):
                if "/releases/tags/" in path:
                    raise HTTPError(path, 404, "missing", {}, None)
                return []

            with patch.object(release, "ROOT", checkout), patch.object(release, "run", run), \
                    patch.dict(release.os.environ, {"RELEASE_AUTOMATION_TOKEN": ""}), \
                    patch.object(release, "validate") as validate, \
                    patch.object(release, "api", side_effect=api):
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
