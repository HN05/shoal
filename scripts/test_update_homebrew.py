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

    def test_pins_release_preserves_head_and_is_idempotent(self):
        result = updater.update_formula(self.formula, "0.2.0", "a" * 40)
        self.assertIn('tag: "v0.2.0", revision: "' + "a" * 40 + '"', result)
        self.assertIn('  version "0.2.0"', result)
        self.assertEqual(result.splitlines()[3], self.formula.splitlines()[3])
        self.assertEqual(updater.update_formula(result, "0.2.0", "a" * 40), result)
        with self.assertRaises(ValueError):
            updater.update_formula(result, "0.2.0", "b" * 40)
        with self.assertRaises(ValueError):
            updater.update_formula(result, "0.1.0", "a" * 40)

    def test_rejects_unexpected_formula_and_invalid_release(self):
        for text, version, revision in [
            (self.formula, "0.2.0-rc1", "a" * 40),
            (self.formula, "0.2.0", "not-a-commit"),
            (self.formula.replace("HN05/shoal.git", "someone/else.git"), "0.2.0", "a" * 40),
            (self.formula + '  version "1.0.0"\n', "0.2.0", "a" * 40),
        ]:
            with self.subTest(version=version, revision=revision):
                with self.assertRaises(ValueError):
                    updater.update_formula(text, version, revision)

    def test_forgejo_formula_preserves_its_source_and_rejects_wrong_host(self):
        formula = self.formula.replace("github.com", "forgejo.example")
        source_url = "https://forgejo.example/HN05/shoal.git"
        result = updater.update_formula(formula, "0.2.0", "b" * 40, source_url)
        self.assertIn('url "https://forgejo.example/HN05/shoal.git", tag: "v0.2.0"', result)
        self.assertEqual(result.splitlines()[3], formula.splitlines()[3])
        self.assertNotIn("github.com", result)
        self.assertEqual(updater.update_formula(result, "0.2.0", "b" * 40, source_url), result)
        with self.assertRaises(ValueError):
            updater.update_formula(formula, "0.2.0", "b" * 40)
        with self.assertRaises(ValueError):
            updater.update_formula(self.formula, "0.2.0", "b" * 40, source_url)


if __name__ == "__main__":
    unittest.main()
