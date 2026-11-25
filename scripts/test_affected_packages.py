from pathlib import Path
import subprocess
import tempfile
import unittest

import affected_packages as affected
from affected_packages import ALL, Workspace
import isolated_git


def package(name, directory, *dependencies):
    """A `cargo metadata --no-deps` package at *directory* under /work, with
    (name, kind) path dependencies."""
    return {
        "name": name,
        "id": f"path+file:///work/{directory}#{name}@0.1.0",
        "manifest_path": f"/work/{directory}/Cargo.toml".replace("/./", "/"),
        "dependencies": [{"name": dependency, "kind": kind, "path": f"/work/crates/{dependency}"}
                         for dependency, kind in dependencies]
        + [{"name": "serde", "kind": None}],
    }


PACKAGES = [
    package("shoal", ".", ("cli", None), ("daemon", None), ("core", "dev")),
    package("cli", "crates/cli", ("core", None)),
    package("daemon", "crates/daemon", ("core", None)),
    package("core", "crates/core", ("time", None)),
    package("time", "crates/time"),
    package("proxy", "crates/proxy", ("time", "dev")),
]
WORKSPACE = Workspace.from_metadata({
    "workspace_root": "/work",
    "workspace_members": [package["id"] for package in PACKAGES],
    "packages": PACKAGES,
})


class AffectedPackagesTests(unittest.TestCase):
    def test_a_changed_package_selects_itself_and_every_package_above_it(self):
        self.assertEqual(WORKSPACE.affected(["crates/cli/src/lib.rs"]), ["cli", "shoal"])
        self.assertEqual(WORKSPACE.affected(["crates/core/Cargo.toml"]),
                         ["cli", "core", "daemon", "shoal"])
        # A dev dependency counts: its dependent's tests build it.
        self.assertEqual(WORKSPACE.affected(["crates/proxy/src/lib.rs", "crates/daemon/x.md"]),
                         ["daemon", "proxy", "shoal"])

    def test_the_root_package_owns_only_its_sources_and_tests(self):
        self.assertEqual(WORKSPACE.affected(["src/main.rs"]), ["shoal"])
        self.assertEqual(WORKSPACE.affected(["tests/fixtures/forge.py"]), ["shoal"])
        self.assertEqual(WORKSPACE.affected(["README.md", "docs/reference/agents.md", "scripts/demo.py",
                                             ".forgejo/workflows/flaky.yml", "crates/README"]),
                         [])

    def test_a_workspace_input_selects_every_package(self):
        for path in ("Cargo.lock", ".config/nextest.toml", ".forgejo/workflows/ci.yml",
                     "scripts/affected_packages.py"):
            with self.subTest(path=path):
                self.assertEqual(WORKSPACE.affected(["docs/design.md", path]), ALL)
        self.assertEqual(WORKSPACE.affected([".config.toml"]), [])

    def test_a_change_every_package_depends_on_selects_all(self):
        self.assertEqual(WORKSPACE.affected(["crates/time/src/lib.rs"]), ALL)


class ChangedPathsTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.repository = self.root / "repository"
        self.environment = isolated_git.environment(self.root)
        self.repository.mkdir()
        self.git("init", "-q", "-b", "main")
        for name, value in (("user.name", "Test"), ("user.email", "test@example.com")):
            self.git("config", name, value)

    def git(self, *arguments):
        return subprocess.run(["git", "-C", str(self.repository), *arguments], check=True,
                              env=self.environment, capture_output=True, text=True).stdout

    def commit(self, path, contents="contents\n"):
        (self.repository / path).parent.mkdir(parents=True, exist_ok=True)
        (self.repository / path).write_text(contents)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", f"Change {path}")

    def changed_paths(self, base):
        return affected.changed_paths(base, cwd=self.repository, env=self.environment)

    def test_lists_the_branch_changes_since_the_merge_base_with_both_names_of_a_rename(self):
        self.commit("crates/cli/data.md")
        self.git("checkout", "-q", "-b", "topic")
        (self.repository / "docs").mkdir()
        self.git("mv", "crates/cli/data.md", "docs/data.md")
        self.git("commit", "-q", "-m", "Move the data")
        self.commit("src/main.rs")
        self.git("checkout", "-q", "main")
        self.commit("crates/daemon/src/lib.rs")
        self.git("checkout", "-q", "topic")
        self.assertEqual(sorted(self.changed_paths("main")),
                         ["crates/cli/data.md", "docs/data.md", "src/main.rs"])

    def test_a_base_without_a_merge_base_reports_none(self):
        self.commit("src/main.rs")
        self.assertIsNone(self.changed_paths("0" * 40))


if __name__ == "__main__":
    unittest.main()
