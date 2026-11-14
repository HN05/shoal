import unittest

import check_package_sources as check
from test_affected_packages import WORKSPACE

GENERATED = {"/work/target"}


class CheckPackageSourcesTests(unittest.TestCase):
    def test_reads_the_prerequisites_of_a_dep_info_file(self):
        dep_info = ("target/debug/deps/cli-1.d: crates/cli/src/lib.rs crates/cli/my\\ notes.md\n"
                    "\n"
                    "crates/cli/src/lib.rs:\n"
                    "# env-dep:CARGO_PKG_DESCRIPTION=Local workspaces\n")
        self.assertEqual(check.dep_info_paths(dep_info),
                         {"crates/cli/src/lib.rs", "crates/cli/my notes.md"})

    def test_a_file_whose_change_selects_the_package_is_no_stray(self):
        dep_info = ("out: crates/cli/src/lib.rs crates/cli/src/x/../../../core/configs/a.toml "
                    "Cargo.toml /work/crates/cli/src/b.md /work/target/debug/build/out/gen.rs "
                    "/home/user/.cargo/registry/src/serde/lib.rs /seed/worktree/clippy.toml\n")
        self.assertEqual(check.stray_sources(WORKSPACE, "/work", GENERATED, "cli", dep_info), [])

    def test_a_file_outside_the_package_and_its_dependencies_is_a_stray(self):
        dep_info = ("out: crates/cli/src/lib.rs crates/cli/src/../../../docs/config.md "
                    "/work/crates/daemon/src/fixture.py tests/fixtures/forge.py\n")
        self.assertEqual(check.stray_sources(WORKSPACE, "/work", GENERATED, "cli", dep_info),
                         ["crates/daemon/src/fixture.py", "docs/config.md",
                          "tests/fixtures/forge.py"])


if __name__ == "__main__":
    unittest.main()
