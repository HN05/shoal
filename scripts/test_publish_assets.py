import importlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

publish = importlib.import_module("publish-assets")

RELEASE = {"id": 9, "tag_name": "v0.2.0", "draft": False, "body": "## Changes\n- Fix workspace cleanup",
           "assets": [{"id": 41, "name": "old.tar.gz"}, {"id": 42, "name": "other.txt"}]}
COMPLETE = dict(RELEASE, assets=RELEASE["assets"] + [{"id": 43, "name": "SHA256SUMS"}])
SHA = "a" * 40


class PublishAssetsTests(unittest.TestCase):
    def files(self, root, *names):
        paths = []
        for name in names:
            path = Path(root) / name
            path.write_bytes(b"binary " + name.encode())
            paths.append(path)
        return paths

    def test_partial_forgejo_upload_is_replaced_whole_with_manifest_last(self):
        env = {"RELEASE_API_URL": "https://forge/api/v1/", "RELEASE_REPOSITORY": "HN05/shoal",
               "RELEASE_AUTOMATION_TOKEN": "forge-token"}
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "request", side_effect=[RELEASE, None, None, None, None]) as request:
            publish.forgejo("v0.2.0", self.files(root, "SHA256SUMS", "old.tar.gz", "new.tar.gz"))
            calls = request.call_args_list
            self.assertEqual(calls[0].args,
                             ("https://forge/api/v1/repos/HN05/shoal/releases/tags/v0.2.0", "forge-token", "token"))
            self.assertEqual(calls[1].args[0], "https://forge/api/v1/repos/HN05/shoal/releases/9/assets/41")
            self.assertEqual(calls[1].kwargs, {"method": "DELETE"})
            self.assertEqual([c.args[0].rsplit("=", 1)[1] for c in calls[2:]],
                             ["new.tar.gz", "old.tar.gz", "SHA256SUMS"])
            url, token, scheme, body, content_type = calls[2].args
            self.assertTrue(content_type.startswith("multipart/form-data; boundary="))
            self.assertIn(b'name="attachment"; filename="new.tar.gz"', body)
            self.assertIn(b"binary new.tar.gz", body)

    def test_complete_asset_set_is_left_alone_and_manifest_is_required(self):
        env = {"RELEASE_API_URL": "https://forge/api/v1/", "RELEASE_REPOSITORY": "HN05/shoal",
               "RELEASE_AUTOMATION_TOKEN": "forge-token"}
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "request", side_effect=[COMPLETE]) as request:
            publish.forgejo("v0.2.0", self.files(root, "SHA256SUMS", "old.tar.gz", "new.tar.gz"))
            self.assertEqual(request.call_count, 1)
        with self.assertRaises(ValueError):
            publish.plan([Path("shoal.tar.gz")], [])

    def test_github_creates_release_after_mirror_and_uploads(self):
        env = {"RELEASE_REPOSITORY": "HN05/shoal", "RELEASE_TOKEN_GITHUB": "gh-token",
               "RELEASE_API_URL": "https://forge/api/v1", "RELEASE_AUTOMATION_TOKEN": "forge-token"}
        created = dict(RELEASE, assets=[])
        source_body = "## Changes\n- Fix workspace cleanup ([#12](https://forge/HN05/shoal/pulls/12))"
        missing = HTTPError("url", 404, "missing", {}, io.BytesIO())
        self.addCleanup(missing.close)
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "mirrored") as mirrored, \
                patch.object(publish, "request", side_effect=[{"body": source_body}, missing,
                                                                    created, None, None]) as request:
            publish.github("v0.2.0", self.files(root, "shoal.tar.gz", "SHA256SUMS"), SHA)
            mirrored.assert_called_once_with("HN05/shoal", "v0.2.0", SHA)
            source = request.call_args_list[0]
            self.assertEqual(source.args, ("https://forge/api/v1/repos/HN05/shoal/releases/tags/v0.2.0",
                                           "forge-token", "token"))
            create = request.call_args_list[2]
            self.assertEqual(create.args[0], "https://api.github.com/repos/HN05/shoal/releases")
            self.assertEqual(create.args[2], "Bearer")
            self.assertEqual(create.args[3]["tag_name"], "v0.2.0")
            self.assertFalse(create.args[3]["draft"])
            self.assertEqual(create.args[3]["body"], "## Changes\n- Fix workspace cleanup")
            upload = request.call_args_list[3]
            self.assertEqual(upload.args[0],
                             "https://uploads.github.com/repos/HN05/shoal/releases/9/assets?name=shoal.tar.gz")
            self.assertEqual(upload.args[3], b"binary shoal.tar.gz")
            self.assertTrue(request.call_args_list[4].args[0].endswith("name=SHA256SUMS"))

    def test_github_replaces_partial_uploads_reuses_complete_releases_and_rejects_drafts(self):
        env = {"RELEASE_REPOSITORY": "HN05/shoal", "RELEASE_TOKEN_GITHUB": "gh-token",
               "RELEASE_API_URL": "https://forge/api/v1", "RELEASE_AUTOMATION_TOKEN": "forge-token"}
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "mirrored"), \
                patch.object(publish, "request", side_effect=[RELEASE, RELEASE, None, None, None]) as request:
            publish.github("v0.2.0", self.files(root, "old.tar.gz", "SHA256SUMS"), SHA)
            delete = request.call_args_list[2]
            self.assertEqual(delete.args[0], "https://api.github.com/repos/HN05/shoal/releases/assets/41")
            self.assertEqual(delete.kwargs, {"method": "DELETE"})
            self.assertEqual(request.call_count, 5)
            request.side_effect = [RELEASE, COMPLETE]
            publish.github("v0.2.0", self.files(root, "old.tar.gz", "SHA256SUMS"), SHA)
            self.assertEqual(request.call_count, 7)
            request.side_effect = [RELEASE, dict(RELEASE, draft=True)]
            with self.assertRaises(ValueError):
                publish.github("v0.2.0", self.files(root, "old.tar.gz", "SHA256SUMS"), SHA)

    def test_github_rerun_updates_notes_without_touching_complete_assets(self):
        env = {"RELEASE_REPOSITORY": "HN05/shoal", "RELEASE_TOKEN_GITHUB": "gh-token",
               "RELEASE_API_URL": "https://forge/api/v1/", "RELEASE_AUTOMATION_TOKEN": "forge-token"}
        old_body = "- Fix cleanup ([#12](https://forge/HN05/shoal/pulls/12))"
        existing = dict(COMPLETE, body=old_body)
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "mirrored"), \
                patch.object(publish, "request", side_effect=[{"body": old_body}, existing, None]) as request:
            publish.github("v0.2.0", self.files(root, "old.tar.gz", "SHA256SUMS"), SHA)
            update = request.call_args_list[2]
            self.assertEqual(update.args, ("https://api.github.com/repos/HN05/shoal/releases/9",
                                           "gh-token", "Bearer", {"body": "- Fix cleanup"}))
            self.assertEqual(update.kwargs, {"method": "PATCH"})
            self.assertEqual(request.call_count, 3)

    def test_mirror_wait_accepts_only_the_released_commit(self):
        refs = f"{SHA}\trefs/tags/v0.2.0\n"
        with patch.object(publish.subprocess, "check_output", side_effect=["", refs]), \
                patch.object(publish.time, "sleep") as sleep:
            publish.mirrored("HN05/shoal", "v0.2.0", SHA, attempts=3, delay=1)
            sleep.assert_called_once_with(1)
        annotated = f"{'b' * 40}\trefs/tags/v0.2.0\n{SHA}\trefs/tags/v0.2.0^{{}}\n"
        with patch.object(publish.subprocess, "check_output", return_value=annotated):
            publish.mirrored("HN05/shoal", "v0.2.0", SHA, attempts=1)
        with patch.object(publish.subprocess, "check_output", return_value=refs), \
                self.assertRaises(ValueError):
            publish.mirrored("HN05/shoal", "v0.2.0", "c" * 40, attempts=1)
        with patch.object(publish.subprocess, "check_output", return_value=""), \
                patch.object(publish.time, "sleep"), self.assertRaises(ValueError):
            publish.mirrored("HN05/shoal", "v0.2.0", SHA, attempts=2, delay=0)

    def test_tags_must_be_stable_versions(self):
        self.assertEqual(publish.version("v1.2.3"), "1.2.3")
        for tag in ("1.2.3", "v1.2", "v1.2.3-rc1", "v01.2.3"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                publish.version(tag)


if __name__ == "__main__":
    unittest.main()
