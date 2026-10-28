import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import demo


class EnvironmentTests(unittest.TestCase):
    def test_the_demo_reaches_none_of_the_callers_shoal_hosts_or_agents(self):
        inherited = {"PATH": "/usr/bin", "SHOAL_STATE_DIR": "/real/state",
                     "SHOAL_SCOPE_TOKEN": "token", "ZMX_SESSION": "agent",
                     "HERDR_ENV": "1", "CODEX_HOME": "/real/codex", "GIT_DIR": "/repo/.git",
                     "TERM": "xterm-256color"}
        with patch.dict("os.environ", inherited, clear=True):
            env = demo.environment(Path("/sandbox"), Path("/sandbox/bin"))
        self.assertEqual(env["SHOAL_STATE_DIR"], "/sandbox/state")
        self.assertEqual(env["ZMX_DIR"], "/sandbox/zmx")
        self.assertEqual(env["HOME"], "/sandbox/home")
        self.assertEqual(env["PATH"], "/sandbox/bin:/usr/bin")
        self.assertEqual(env["TERM"], "xterm-256color")
        for name in ("SHOAL_SCOPE_TOKEN", "ZMX_SESSION", "HERDR_ENV", "CODEX_HOME", "GIT_DIR"):
            self.assertNotIn(name, env)


class SandboxTests(unittest.TestCase):
    def test_the_sandbox_has_the_zmx_lock_debug_builds_open(self):
        with tempfile.TemporaryDirectory() as root:
            sandbox = Path(root)
            demo.prepare_zmx_lock(sandbox)
            self.assertTrue((sandbox / "zmx.shoal-ownership.lock").is_file())


if __name__ == "__main__":
    unittest.main()
