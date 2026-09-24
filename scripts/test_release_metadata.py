import unittest

import release_metadata as metadata


class ReleaseMetadataTests(unittest.TestCase):
    def test_stable_version_and_tag_round_trip(self):
        for version, parts in (("0.0.0", (0, 0, 0)), ("12.34.56", (12, 34, 56))):
            with self.subTest(version=version):
                self.assertEqual(metadata.version_parts(version), parts)
                self.assertEqual(metadata.version_tag(version), "v" + version)
                self.assertEqual(metadata.tag_version("v" + version), version)
        self.assertLess(metadata.version_parts("1.2.9"), metadata.version_parts("1.2.10"))

    def test_rejects_non_stable_versions_and_tags(self):
        for version in ("", "01.2.3", "1.02.3", "1.2.03", "1.2.3-rc1", "1.2.3+build",
                        "1.2", "1.2.3.4", "-1.2.3", "1.a.3", "1.2.3\n", " 1.2.3", "١.2.3"):
            with self.subTest(version=version):
                for parse in (metadata.version_parts, metadata.version_tag):
                    with self.assertRaises(ValueError):
                        parse(version)
                with self.assertRaises(ValueError):
                    metadata.tag_version("v" + version)
        for tag in ("1.2.3", "vv1.2.3", "refs/tags/v1.2.3"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                metadata.tag_version(tag)
        with self.assertRaises(ValueError):
            metadata.version_parts("v1.2.3")

    def test_commit_ids_require_full_supported_lowercase_hashes(self):
        for length in (40, 64):
            self.assertEqual(metadata.commit_id("a" * length), "a" * length)
        for revision in ("", "a" * 7, "a" * 39, "a" * 41, "a" * 63, "a" * 65,
                         "A" * 40, "g" * 40, "a" * 40 + "\n"):
            with self.subTest(revision=revision), self.assertRaises(ValueError):
                metadata.commit_id(revision)

    def test_remote_tag_identity_for_both_hash_formats(self):
        for length in (40, 64):
            revision, tag_object = "a" * length, "b" * length
            lightweight = f"{revision}\trefs/tags/v1.2.3\n"
            annotated = (f"{tag_object}\trefs/tags/v1.2.3\n"
                         f"{revision}\trefs/tags/v1.2.3^{{}}\n")
            for refs in (lightweight, annotated, "\n".join(reversed(annotated.splitlines()))):
                with self.subTest(length=length, refs=refs):
                    self.assertEqual(metadata.verified_tag_commit(refs, "v1.2.3", revision), revision)
                    with self.assertRaisesRegex(ValueError, "not the released"):
                        metadata.verified_tag_commit(refs, "v1.2.3", "c" * length)
            with self.assertRaises(ValueError):
                metadata.verified_tag_commit(annotated, "v1.2.3", tag_object)
            self.assertIsNone(metadata.verified_tag_commit(lightweight, "v1.2.4", revision))
            self.assertIsNone(metadata.verified_tag_commit("", "v1.2.3", revision))
        for refs, tag, revision in (("", "v01.2.3", "a" * 40), ("", "v1.2.3", "short"),
                                    ("short\trefs/tags/v1.2.3", "v1.2.3", "a" * 40)):
            with self.subTest(refs=refs, tag=tag, revision=revision), self.assertRaises(ValueError):
                metadata.verified_tag_commit(refs, tag, revision)
