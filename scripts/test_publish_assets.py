import importlib
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

publish = importlib.import_module("publish-assets")

RELEASE = {"id": 9, "tag_name": "v0.2.0", "draft": False, "assets": [{"name": "old.tar.gz"}]}
SHA = "a" * 40


class PublishAssetsTests(unittest.TestCase):
    def files(self, root, *names):
        paths = []
        for name in names:
            path = Path(root) / name
            path.write_bytes(b"binary " + name.encode())
            paths.append(path)
        return paths

    def test_forgejo_uploads_only_missing_files_as_multipart(self):
        env = {"RELEASE_API_URL": "https://forge/api/v1/", "RELEASE_REPOSITORY": "HN05/shoal",
               "RELEASE_AUTOMATION_TOKEN": "forge-token"}
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "request", side_effect=[RELEASE, None]) as request:
            publish.forgejo("v0.2.0", self.files(root, "old.tar.gz", "new.tar.gz"))
            self.assertEqual(request.call_count, 2)
            self.assertEqual(request.call_args_list[0].args,
                             ("https://forge/api/v1/repos/HN05/shoal/releases/tags/v0.2.0", "forge-token", "token"))
            url, token, scheme, body, content_type = request.call_args_list[1].args
            self.assertEqual(url, "https://forge/api/v1/repos/HN05/shoal/releases/9/assets?name=new.tar.gz")
            self.assertTrue(content_type.startswith("multipart/form-data; boundary="))
            self.assertIn(b'name="attachment"; filename="new.tar.gz"', body)
            self.assertIn(b"binary new.tar.gz", body)

    def test_github_creates_release_after_mirror_and_uploads(self):
        env = {"RELEASE_REPOSITORY": "HN05/shoal", "GITHUB_RELEASE_TOKEN": "gh-token"}
        created = dict(RELEASE, assets=[])
        missing = HTTPError("url", 404, "missing", {}, io.BytesIO())
        self.addCleanup(missing.close)
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "mirrored") as mirrored, \
                patch.object(publish, "request", side_effect=[missing, created, None]) as request:
            publish.github("v0.2.0", self.files(root, "shoal.tar.gz"), SHA)
            mirrored.assert_called_once_with("HN05/shoal", "v0.2.0", SHA)
            create = request.call_args_list[1]
            self.assertEqual(create.args[0], "https://api.github.com/repos/HN05/shoal/releases")
            self.assertEqual(create.args[2], "Bearer")
            self.assertEqual(create.args[3]["tag_name"], "v0.2.0")
            self.assertFalse(create.args[3]["draft"])
            upload = request.call_args_list[2]
            self.assertEqual(upload.args[0],
                             "https://uploads.github.com/repos/HN05/shoal/releases/9/assets?name=shoal.tar.gz")
            self.assertEqual(upload.args[3], b"binary shoal.tar.gz")

    def test_github_reuses_existing_release_and_rejects_drafts(self):
        env = {"RELEASE_REPOSITORY": "HN05/shoal", "GITHUB_RELEASE_TOKEN": "gh-token"}
        with tempfile.TemporaryDirectory() as root, patch.dict(publish.os.environ, env), \
                patch.object(publish, "mirrored"), \
                patch.object(publish, "request", side_effect=[RELEASE]) as request:
            publish.github("v0.2.0", self.files(root, "old.tar.gz"), SHA)
            self.assertEqual(request.call_count, 1)
            request.side_effect = [dict(RELEASE, draft=True)]
            with self.assertRaises(ValueError):
                publish.github("v0.2.0", self.files(root, "old.tar.gz"), SHA)

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
