import json
import unittest

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
        rows = [("lib", "a", 0, 1.0, 2.0, 3.0), ("workspaces", "b", 0, 1.0, 1.0, 1.0),
                ("lib", "c", 101, 1.0, 0.5, 0.25)]
        self.assertEqual(profile_tests.binary_totals(rows), [
            ("lib", 2, 2.5, 3.25), ("workspaces", 1, 1.0, 1.0)])

if __name__ == "__main__":
    unittest.main()
