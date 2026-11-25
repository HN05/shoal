import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import profile_tests


def artifact(name, kind, executable, test=True):
    return json.dumps({"reason": "compiler-artifact", "executable": executable,
                       "profile": {"test": test},
                       "target": {"name": name, "kind": [kind]},
                       "manifest_path": f"/repo/{name}/Cargo.toml"})


class ConfigEnvTests(unittest.TestCase):
    def test_reads_the_env_table_and_lets_set_variables_win(self):
        config = '\n'.join(['[build]', 'jobs = "4"', '[env]', '# A comment.',
                            'RUST_MIN_STACK = "8388608"', 'FLAGS = "-DA=0"'])
        env = profile_tests.config_env(config, {"FLAGS": "", "HOME": "/home"})
        self.assertEqual(env, {"RUST_MIN_STACK": "8388608", "FLAGS": "", "HOME": "/home"})


class BuiltBinariesTests(unittest.TestCase):
    def test_names_test_targets_and_skips_other_artifacts(self):
        lines = [artifact("shoal", "lib", "/t/shoal-1"),
                 artifact("workspaces", "test", "/t/workspaces-2"),
                 artifact("macros", "proc-macro", "/t/macros-3"),
                 artifact("shoal", "bin", "/t/shoal", test=False),
                 json.dumps({"reason": "build-finished", "success": True})]
        self.assertEqual(profile_tests.built_binaries(lines), [
            ("shoal (lib)", "/t/shoal-1", "/repo/shoal"),
            ("workspaces", "/t/workspaces-2", "/repo/workspaces"),
        ])



class ReportTests(unittest.TestCase):
    def test_reads_test_names_from_a_terse_listing(self):
        listing = "a::first: test\na::slow: bench\nb::second: test\n"
        self.assertEqual(profile_tests.names_in_listing(listing), ["a::first", "b::second"])

    def test_sums_tests_per_binary_in_first_seen_order(self):
        rows = [("lib", "a", 0, 1.0, 2.0, 3.0, 4, 1), ("workspaces", "b", 0, 1.0, 1.0, 1.0, 0, 0),
                ("lib", "c", 101, 1.0, 0.5, 0.25, 6, 2)]
        self.assertEqual(profile_tests.binary_totals(rows), [
            ("lib", 2, 2.5, 3.25, 10, 3), ("workspaces", 1, 1.0, 1.0, 0, 0)])


class GitCountTests(unittest.TestCase):
    def test_splits_production_from_fixture_processes_by_their_option(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "git"
            records = [["--no-optional-locks", "-C", "/a repo", "rev-parse", "--git-dir"],
                       ["--no-optional-locks", "-C", "/repo", "-c", "core.hooksPath=/dev/null",
                        "rev-parse", "HEAD"],
                       ["--no-optional-locks", "-C", "/repo", "config", "--get", "a.b"],
                       ["-C", "/repo", "commit", "-m", "one\ntwo"]]
            log.write_text("".join("".join(f"{arg}\x1f" for arg in args) + "\0"
                                   for args in records))
            production, fixture, commands = profile_tests.git_counts(log)
        self.assertEqual((production, fixture), (3, 1))
        self.assertEqual(commands, {"rev-parse --git-dir": 1, "rev-parse HEAD": 1,
                                    "config --get": 1})

    def test_names_a_command_by_its_options_without_paths_or_names(self):
        commands = [
            (["-C", "/r", "status", "--porcelain=v2", "-z", "--untracked-files=no"],
             "status --porcelain=v2 -z --untracked-files=no"),
            (["-C", "/r", "worktree", "add", "--no-track", "-b", "fix-login", "--", "/w", "main"],
             "worktree add --no-track -b"),
            (["-C", "/r", "worktree", "list", "--porcelain", "-z"], "worktree list --porcelain -z"),
            (["-C", "/r", "rev-parse", "a" * 40 + "^{tree}"], "rev-parse"),
            (["-C", "/r", "diff", "-z", "--name-only", "HEAD", "--", "HEAD"],
             "diff -z --name-only HEAD"),
            (["-C", "/r", "read-tree", "--index-output=/r/.git/index.tmp"],
             "read-tree --index-output=<path>"),
        ]
        for args, command in commands:
            self.assertEqual(profile_tests.git_command(["--no-optional-locks", *args]), command)

    def test_counts_nothing_without_a_log(self):
        self.assertEqual(profile_tests.git_counts("/nonexistent/git")[:2], (0, 0))

    def test_the_wrapper_logs_its_arguments_and_runs_git(self):
        with tempfile.TemporaryDirectory() as directory:
            developer = Path(directory) / "developer"
            fake = developer / "usr" / "bin" / "git"
            fake.parent.mkdir(parents=True)
            fake.write_text("#!/bin/sh\necho ran \"$@\"\n")
            fake.chmod(0o755)
            wrapper = profile_tests.git_wrapper(Path(directory) / "wrapper", developer)
            log = Path(directory) / "log"
            for git in [wrapper / "git", Path(self.selected_developer(wrapper)) / "usr/bin/git"]:
                output = subprocess.run([git, "--no-optional-locks", "-C", "a dir", "status"],
                                        env={profile_tests.GIT_LOG: str(log)},
                                        stdout=subprocess.PIPE, text=True, check=True).stdout
                self.assertEqual(output, "ran --no-optional-locks -C a dir status\n")
            production, fixture, commands = profile_tests.git_counts(log)
            self.assertEqual((production, fixture, commands), (2, 0, {"status": 2}))

    def selected_developer(self, wrapper):
        """The developer directory the wrapper's `xcode-select -p` names."""
        return subprocess.run([wrapper / "xcode-select", "-p"], stdout=subprocess.PIPE,
                              text=True, check=True).stdout.strip()

if __name__ == "__main__":
    unittest.main()
