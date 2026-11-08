import shutil
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
        for name in ("HOME", "SHOAL_STATE_DIR", "ZMX_DIR", "TMUX_TMPDIR", "ZELLIJ_SOCKET_DIR"):
            self.assertEqual(Path(env[name]).parent, sandbox.outside_directory(Path("/sandbox")))
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
        self.assertNotEqual(sandbox.outside_directory(deep),
                            sandbox.outside_directory(Path("/s")))


class SandboxTests(unittest.TestCase):
    def test_the_sandbox_has_the_zmx_lock_debug_builds_open(self):
        with tempfile.TemporaryDirectory() as root, \
                patch.object(sandbox, "OUTSIDE_ROOT", Path(root)):
            directory = Path(root, "sandbox")
            sandbox.prepare_directories(directory)
            sandbox.prepare_zmx_lock(directory)
            zmx = sandbox.environment(directory, directory / "bin")["ZMX_DIR"]
            self.assertTrue(Path(zmx + ".shoal-ownership.lock").is_file())

    def test_a_sandbox_in_a_checkout_has_its_home_outside_it(self):
        with tempfile.TemporaryDirectory() as root, \
                patch.object(sandbox, "OUTSIDE_ROOT", Path(root, "outside")):
            Path(root, "outside").mkdir()
            checkout = Path(root, "checkout")
            (checkout / ".git").mkdir(parents=True)
            directory = checkout / "target" / "rec" / "sandbox"
            sandbox.prepare_directories(directory)
            home = Path(sandbox.environment(directory, directory / "bin")["HOME"])
            self.assertTrue(home.is_dir())
            self.assertFalse(any((parent / ".git").exists() for parent in home.parents))

    def test_clearing_a_sandbox_whose_home_is_gone_removes_it(self):
        for removed in ("outside directory", "home"):
            with self.subTest(removed=removed), tempfile.TemporaryDirectory() as root, \
                    patch.object(sandbox, "OUTSIDE_ROOT", Path(root, "outside")):
                Path(root, "outside").mkdir()
                directory = Path(root, "sandbox")
                (directory / "bin").mkdir(parents=True)
                (directory / "bin" / "shoal").symlink_to(shutil.which("true"))
                if removed == "home":
                    sandbox.outside_directory(directory).mkdir()
                sandbox.Sandbox(directory).clear()
                self.assertFalse(directory.exists())
                self.assertFalse(sandbox.outside_directory(directory).exists())

    def test_removing_the_sandbox_removes_its_outside_directory(self):
        with tempfile.TemporaryDirectory() as root, \
                patch.object(sandbox, "OUTSIDE_ROOT", Path(root, "outside")):
            Path(root, "outside").mkdir()
            directory = Path(root, "sandbox")
            sandbox.prepare_directories(directory)
            sandbox.remove(directory)
            self.assertFalse(directory.exists())
            self.assertFalse(sandbox.outside_directory(directory).exists())


if __name__ == "__main__":
    unittest.main()
