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


if __name__ == "__main__":
    unittest.main()
