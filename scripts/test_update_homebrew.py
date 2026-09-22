import unittest
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path

spec = spec_from_file_location("updater", Path(__file__).with_name("update-homebrew.py"))
updater = module_from_spec(spec)
spec.loader.exec_module(updater)


class FormulaUpdateTests(unittest.TestCase):
    formula = '''class Shoal < Formula
  url "https://github.com/HN05/shoal.git", tag: "v0.1.0"
  version "0.1.0"
  head "https://github.com/HN05/shoal.git", branch: "main"
end
'''

    @staticmethod
    def manifest(version, checksum="c" * 64):
        return "\n".join(f"{checksum}  shoal-v{version}-{platform}.tar.gz"
                         for platform in updater.PLATFORMS)

    def update(self, text, version, revision, source_url="https://github.com/HN05/shoal.git"):
        return updater.update_formula(text, version, revision, self.manifest(version), source_url)

    def test_migrates_source_formula_to_binaries_and_is_idempotent(self):
        result = self.update(self.formula, "0.2.0", "a" * 40)
        self.assertIn("# Release commit: " + "a" * 40, result)
        for platform in updater.PLATFORMS:
            self.assertIn(f'https://github.com/HN05/shoal/releases/download/v0.2.0/shoal-v0.2.0-{platform}.tar.gz', result)
        self.assertEqual(result.count('sha256 "' + "c" * 64 + '"'), 4)
        self.assertIn('  version "0.2.0"', result)
        self.assertIn('head do\n    url "https://github.com/HN05/shoal.git", branch: "main"\n    depends_on "rust" => :build\n  end', result)
        self.assertEqual(result.count('depends_on "rust"'), 1)
        self.assertIn('if build.head?', result)
        self.assertIn('libexec.install "shoal"', result)
        self.assertIn('SHOAL_SKILL_PATH: opt_share/"shoal/skill/SKILL.md"', result)
        upgraded = self.update(result, "0.3.0", "b" * 40)
        self.assertNotIn("v0.2.0", upgraded)
        self.assertIn("v0.3.0", upgraded)
        self.assertEqual(self.update(result, "0.2.0", "a" * 40), result)
        with self.assertRaises(ValueError):
            self.update(result, "0.2.0", "b" * 40)
        with self.assertRaises(ValueError):
            self.update(result, "0.1.0", "a" * 40)

    def test_rejects_unexpected_formula_and_invalid_release(self):
        for text, version, revision in [
            (self.formula, "0.2.0-rc1", "a" * 40),
            (self.formula, "0.2.0", "not-a-commit"),
            (self.formula.replace("HN05/shoal.git", "someone/else.git"), "0.2.0", "a" * 40),
            (self.formula + '  version "1.0.0"\n', "0.2.0", "a" * 40),
        ]:
            with self.subTest(version=version, revision=revision):
                with self.assertRaises(ValueError):
                    self.update(text, version, revision)

    def test_forgejo_formula_preserves_its_source_and_rejects_wrong_host(self):
        formula = self.formula.replace("github.com", "forgejo.example")
        source_url = "https://forgejo.example/HN05/shoal.git"
        result = self.update(formula, "0.2.0", "b" * 40, source_url)
        self.assertIn('url "https://forgejo.example/HN05/shoal/releases/download/v0.2.0/', result)
        self.assertIn('url "https://forgejo.example/HN05/shoal.git", branch: "main"', result)
        self.assertNotIn("github.com", result)
        self.assertEqual(self.update(result, "0.2.0", "b" * 40, source_url), result)
        with self.assertRaises(ValueError):
            self.update(formula, "0.2.0", "b" * 40)
        with self.assertRaises(ValueError):
            self.update(self.formula, "0.2.0", "b" * 40, source_url)

    def test_rejects_changed_pinned_checksums_and_wrong_asset_host(self):
        result = self.update(self.formula, "0.2.0", "a" * 40)
        with self.assertRaisesRegex(ValueError, "pinned release checksums"):
            updater.update_formula(result, "0.2.0", "a" * 40, self.manifest("0.2.0", "d" * 64))
        with self.assertRaisesRegex(ValueError, "asset URLs"):
            self.update(result.replace("github.com", "elsewhere.example"), "0.3.0", "b" * 40)

    def test_legacy_pinned_revision_is_immutable(self):
        formula = self.formula.replace('tag: "v0.1.0"', 'tag: "v0.1.0", revision: "' + "a" * 40 + '"')
        self.update(formula, "0.1.0", "a" * 40)
        with self.assertRaisesRegex(ValueError, "pinned release tag"):
            self.update(formula, "0.1.0", "b" * 40)

    def test_requires_complete_valid_unique_checksums(self):
        manifest = self.manifest("0.2.0")
        for invalid in ("", "\n".join(manifest.splitlines()[:-1]),
                        manifest + "\n" + manifest.splitlines()[0],
                        manifest.replace("c" * 64, "invalid"),
                        manifest.replace("v0.2.0", "v0.1.0")):
            with self.subTest(manifest=invalid):
                with self.assertRaises(ValueError):
                    updater.update_formula(self.formula, "0.2.0", "a" * 40, invalid)

    def test_checksums_follow_asset_names_not_manifest_order(self):
        assets = {f"shoal-v0.2.0-{platform}.tar.gz": f"{index:064x}"
                  for index, platform in enumerate(updater.PLATFORMS)}
        manifest = "\n".join(f"{checksum}  {name}"
                             for name, checksum in reversed(list(assets.items())))
        result = updater.update_formula(self.formula, "0.2.0", "a" * 40, manifest)
        for name, checksum in assets.items():
            self.assertIn(f'/{name}"\n      sha256 "{checksum}"', result)
        self.assertEqual(updater.update_formula(result, "0.2.0", "a" * 40, manifest), result)


if __name__ == "__main__":
    unittest.main()
