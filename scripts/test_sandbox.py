import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import sandbox


class EnvironmentTests(unittest.TestCase):
    def test_the_sandbox_reaches_none_of_the_callers_shoal_hosts_or_agents(self):
        inherited = {"PATH": "/usr/bin", "SHOAL_STATE_DIR": "/real/state",
                     "SHOAL_SCOPE_TOKEN": "token", "ZMX_SESSION": "agent",
                     "HERDR_ENV": "1", "REX_SESSION": "session",
                     "TMUX": "/tmp/tmux-501/default,1,0", "TMUX_PANE": "%0",
                     "ZELLIJ": "0", "ZELLIJ_SESSION_NAME": "work", "ZELLIJ_PANE_ID": "0",
                     "CODEX_HOME": "/real/codex", "GIT_DIR": "/repo/.git",
                     "TERM": "xterm-256color"}
        with patch.dict("os.environ", inherited, clear=True):
            env = sandbox.environment(Path("/sandbox"), Path("/sandbox/bin"))
        for name in ("SHOAL_STATE_DIR", "ZMX_DIR", "TMUX_TMPDIR", "ZELLIJ_SOCKET_DIR"):
            self.assertEqual(Path(env[name]).parent, sandbox.sockets_directory(Path("/sandbox")))
        self.assertEqual(env["HOME"], "/sandbox/home")
        self.assertEqual(env["PATH"], "/sandbox/bin:/usr/bin")
        self.assertEqual(env["TERM"], "xterm-256color")
        for name in ("SHOAL_SCOPE_TOKEN", "ZMX_SESSION", "HERDR_ENV", "REX_SESSION", "TMUX",
                     "TMUX_PANE", "ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID", "CODEX_HOME",
                     "GIT_DIR"):
            self.assertNotIn(name, env)

    def test_socket_directories_stay_short_under_a_long_sandbox_path(self):
        deep = Path("/") / ("deep-directory/" * 20) / "sandbox"
        env = sandbox.environment(deep, deep / "bin")
        self.assertEqual(len(env["ZMX_DIR"]),
                         len(sandbox.environment(Path("/s"), Path("/s/bin"))["ZMX_DIR"]))
        self.assertNotEqual(sandbox.sockets_directory(deep),
                            sandbox.sockets_directory(Path("/s")))


class SandboxTests(unittest.TestCase):
    def test_the_sandbox_has_the_zmx_lock_debug_builds_open(self):
        with tempfile.TemporaryDirectory() as root, \
                patch.object(sandbox, "SOCKETS_ROOT", Path(root)):
            directory = Path(root, "sandbox")
            sandbox.prepare_directories(directory)
            sandbox.prepare_zmx_lock(directory)
            zmx = sandbox.environment(directory, directory / "bin")["ZMX_DIR"]
            self.assertTrue(Path(zmx + ".shoal-ownership.lock").is_file())

    def test_removing_the_sandbox_removes_its_sockets_directory(self):
        with tempfile.TemporaryDirectory() as root, \
                patch.object(sandbox, "SOCKETS_ROOT", Path(root, "sockets")):
            Path(root, "sockets").mkdir()
            directory = Path(root, "sandbox")
            sandbox.prepare_directories(directory)
            sandbox.remove(directory)
            self.assertFalse(directory.exists())
            self.assertFalse(sandbox.sockets_directory(directory).exists())


if __name__ == "__main__":
    unittest.main()
