import unittest
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path
import re
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
        return "\n".join(f"{checksum}  {name}" for name in sorted(updater.release_assets(version)))

    @staticmethod
    def formula_assets(version):
        return [f"shoal-v{version}-{platform}{'-app.tar.gz' if platform_os == 'macos' else ''}"
                for _, platform, platform_os, _ in updater.release_targets()]

    @staticmethod
    def asset_names(formula):
        return re.findall(r'^      url "[^"\n]+/([^/"\n]+)"$', formula, re.M)

    def update(self, text, version, revision, source_url="https://github.com/HN05/shoal.git"):
        return updater.update_formula(text, version, revision, self.manifest(version), source_url)

    def test_migrates_source_formula_to_binaries_and_is_idempotent(self):
        result = self.update(self.formula, "0.2.0", "a" * 40)
        self.assertIn("# Release commit: " + "a" * 40, result)
        for name in self.formula_assets("0.2.0"):
            self.assertIn(f'https://github.com/HN05/shoal/releases/download/v0.2.0/{name}', result)
        self.assertEqual(result.count('sha256 "' + "c" * 64 + '"'), len(updater.release_targets()))
        self.assertIn('  version "0.2.0"', result)
        self.assertNotRegex(result, r"\bhead\b")
        self.assertIn('libexec.install "Shoal.app"', result)
        self.assertIn('bin.install_symlink libexec/"Shoal.app/Contents/MacOS/shoal"', result)
        self.assertIn('libexec.install Dir["shoal-v*"].fetch(0) => "shoal"', result)
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

    def test_macos_installs_the_app_archive_and_linux_the_bare_binary(self):
        result = self.update(self.formula, "0.2.0", "a" * 40)
        self.assertEqual(self.asset_names(result), self.formula_assets("0.2.0"))

    def test_upgrades_a_formula_that_installs_archives_of_the_binary(self):
        current = self.update(self.formula, "0.1.0", "a" * 40)
        earlier = current
        for _, platform, _, _ in updater.release_targets():
            name = updater.binary_asset("0.1.0", platform)
            earlier = earlier.replace(f'/{name}"', f'/{name}.tar.gz"')
        self.assertNotEqual(earlier, current)
        self.assertEqual(self.update(earlier, "0.2.0", "b" * 40), self.update(current, "0.2.0", "b" * 40))
        with self.assertRaisesRegex(ValueError, "pinned release checksums"):
            updater.update_formula(earlier, "0.1.0", "a" * 40, self.manifest("0.1.0", "d" * 64))

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
                        manifest.replace("v0.2.0", "v0.1.0"),
                        # Releases no longer publish an archive of the bare binary.
                        re.sub(r"(-(?:linux|macos)-[a-z0-9_]+)$", r"\1.tar.gz", manifest, flags=re.M)):
            with self.subTest(manifest=invalid):
                with self.assertRaises(ValueError):
                    updater.update_formula(self.formula, "0.2.0", "a" * 40, invalid)

    def test_checksums_follow_asset_names_not_manifest_order(self):
        assets = {name: f"{index:064x}"
                  for index, name in enumerate(sorted(updater.release_assets("0.2.0")))}
        manifest = "\n".join(f"{checksum}  {name}"
                             for name, checksum in reversed(list(assets.items())))
        result = updater.update_formula(self.formula, "0.2.0", "a" * 40, manifest)
        for name in self.formula_assets("0.2.0"):
            self.assertIn(f'/{name}"\n      sha256 "{assets[name]}"', result)
        self.assertEqual(updater.update_formula(result, "0.2.0", "a" * 40, manifest), result)

    def test_formula_selectors_and_required_checksums_follow_the_table(self):
        previous = self.update(self.formula, "0.1.0", "b" * 40)
        with tempfile.TemporaryDirectory() as root:
            table = Path(root) / "targets.txt"
            table.write_text("custom-target custom-platform linux arm\n")
            with patch.object(updater, "TARGETS_FILE", table):
                manifest = "c" * 64 + "  shoal-v0.2.0-custom-platform"
                result = updater.update_formula(self.formula, "0.2.0", "a" * 40, manifest)
                self.assertIn('  on_linux do\n    on_arm do\n', result)
                self.assertIn('/shoal-v0.2.0-custom-platform"', result)
                self.assertEqual(result.count('      url "'), 1)
                self.assertNotIn("on_macos", result)
                self.assertEqual(updater.update_formula(result, "0.2.0", "a" * 40, manifest), result)
                self.assertEqual(updater.update_formula(previous, "0.2.0", "a" * 40, manifest), result)
                with self.assertRaisesRegex(ValueError, "pinned release checksums"):
                    self.update(previous, "0.1.0", "b" * 40)
                with self.assertRaisesRegex(ValueError, "every release asset"):
                    updater.release_checksums(manifest.replace("custom-platform", "old-platform"),
                                              updater.release_assets("0.2.0"))


if __name__ == "__main__":
    unittest.main()
