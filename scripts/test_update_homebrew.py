import unittest
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import tempfile
from unittest.mock import patch

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
                         for platform in updater.release_platforms())

    def update(self, text, version, revision, source_url="https://github.com/HN05/shoal.git"):
        return updater.update_formula(text, version, revision, self.manifest(version), source_url)

    def test_migrates_source_formula_to_binaries_and_is_idempotent(self):
        result = self.update(self.formula, "0.2.0", "a" * 40)
        self.assertIn("# Release commit: " + "a" * 40, result)
        for platform in updater.release_platforms():
            self.assertIn(f'https://github.com/HN05/shoal/releases/download/v0.2.0/shoal-v0.2.0-{platform}.tar.gz', result)
        self.assertEqual(result.count('sha256 "' + "c" * 64 + '"'), len(updater.release_platforms()))
        self.assertIn('  version "0.2.0"', result)
        self.assertNotRegex(result, r"\bhead\b")
        self.assertIn('libexec.install "shoal"', result)
        self.assertIn('(libexec/"shoal-skills").make_symlink opt_share/"shoal/skills"', result)
        self.assertIn('bin.install_symlink libexec/"shoal"', result)
        self.assertNotIn("write_env_script", result)
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
        self.assertIn('homepage "https://forgejo.example/HN05/shoal"', result)
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
                  for index, platform in enumerate(updater.release_platforms())}
        manifest = "\n".join(f"{checksum}  {name}"
                             for name, checksum in reversed(list(assets.items())))
        result = updater.update_formula(self.formula, "0.2.0", "a" * 40, manifest)
        for name, checksum in assets.items():
            self.assertIn(f'/{name}"\n      sha256 "{checksum}"', result)
        self.assertEqual(updater.update_formula(result, "0.2.0", "a" * 40, manifest), result)

    def test_formula_selectors_and_required_checksums_follow_the_table(self):
        previous = self.update(self.formula, "0.1.0", "b" * 40)
        with tempfile.TemporaryDirectory() as root:
            table = Path(root) / "targets.txt"
            table.write_text("custom-target custom-platform linux arm\n")
            with patch.object(updater, "TARGETS_FILE", table):
                manifest = "c" * 64 + "  shoal-v0.2.0-custom-platform.tar.gz"
                result = updater.update_formula(self.formula, "0.2.0", "a" * 40, manifest)
                self.assertIn('  on_linux do\n    on_arm do\n', result)
                self.assertIn('/shoal-v0.2.0-custom-platform.tar.gz"', result)
                self.assertEqual(result.count('      url "'), 1)
                self.assertNotIn("on_macos", result)
                self.assertEqual(updater.update_formula(result, "0.2.0", "a" * 40, manifest), result)
                self.assertEqual(updater.update_formula(previous, "0.2.0", "a" * 40, manifest), result)
                with self.assertRaisesRegex(ValueError, "pinned release checksums"):
                    self.update(previous, "0.1.0", "b" * 40)
                with self.assertRaisesRegex(ValueError, "every release platform"):
                    updater.release_checksums(manifest.replace("custom-platform", "old-platform"), "0.2.0")


if __name__ == "__main__":
    unittest.main()
