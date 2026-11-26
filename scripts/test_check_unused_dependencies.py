from pathlib import Path
import tempfile
import unittest

import check_unused_dependencies as check


def package(name, directory, dependencies, unnamed=()):
    """A `cargo metadata --no-deps` package at *directory* under the
    workspace root, with *dependencies* as (name, rename) pairs."""
    return {
        "name": name,
        "id": f"{name}@0.1.0",
        "manifest_path": f"{directory}/Cargo.toml",
        "dependencies": [{"name": dependency, "rename": rename, "kind": None}
                         for dependency, rename in dependencies],
        "metadata": {"shoal": {check.METADATA_KEY: list(unnamed)}} if unnamed else None,
    }


class CheckUnusedDependenciesTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, path, text):
        path = self.root / path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def problems(self, *packages):
        for entry in packages:
            entry["manifest_path"] = str(self.root / entry["manifest_path"])
        return check.problems({
            "workspace_root": str(self.root),
            "workspace_members": [entry["id"] for entry in packages],
            "packages": list(packages),
        })

    def unnamed(self, source, *dependencies):
        self.write("crates/core/src/lib.rs", source)
        found = self.problems(package("core", "crates/core", [(name, None) for name in dependencies]))
        return [problem.split("`")[1] for _package, problem in found]

    def test_a_path_use_or_extern_crate_names_a_dependency(self):
        source = ("fn main() { serde_json::to_string(&1); }\n"
                  "#[derive(::serde::Serialize)] struct A;\n"
                  "use anyhow;\n"
                  "extern crate libc;\n")
        self.assertEqual(self.unnamed(source, "serde_json", "serde", "anyhow", "libc"), [])

    def test_a_word_or_a_path_through_another_module_names_no_dependency(self):
        source = ("// Speaks hyper, as the hub does.\n"
                  "use crate::ring::Key;\n"
                  "fn f() { proxy::hmac::sign(); }\n")
        self.assertEqual(self.unnamed(source, "hyper", "ring", "hmac"), ["hmac", "hyper", "ring"])

    def test_a_dependency_is_named_by_its_crate_name_or_its_rename(self):
        self.write("crates/core/tests/forge.rs", "use shoal_forge::Forge;\nfn f() { gx::open(); }\n")
        found = self.problems(package("core", "crates/core",
                                      [("shoal-forge", None), ("gix", "gx"), ("toml", None)]))
        self.assertEqual([(package, problem.split("`")[1]) for package, problem in found],
                         [("core", "toml")])

    def test_an_allowed_dependency_passes_and_a_stale_allowance_fails(self):
        self.write("crates/core/src/lib.rs", "")
        found = self.problems(package("core", "crates/core", [("clap", None)],
                                      unnamed=["clap", "rusqlite"]))
        self.assertEqual([package for package, _problem in found], ["core"])
        self.assertIn("`rusqlite`", found[0][1])

    def test_the_root_package_reads_only_its_own_sources(self):
        self.write("crates/core/src/lib.rs", "fn f() { toml::from_str(\"\"); }\n")
        self.write("src/main.rs", "fn main() { shoal_cli::run(); }\n")
        found = self.problems(package("shoal", ".", [("shoal-cli", None), ("toml", None)]),
                              package("core", "crates/core", [("toml", None)]))
        self.assertEqual([(package, problem.split("`")[1]) for package, problem in found],
                         [("shoal", "toml")])


if __name__ == "__main__":
    unittest.main()
