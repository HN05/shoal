"""Environment for test fixtures that run Git or release helpers that do."""

import os


def environment(home, **extra):
    """Return os.environ without inherited Git or XDG settings, with HOME at `home`.

    An inherited GIT_DIR or GIT_WORK_TREE would point fixture commands at the
    invoking repository despite `-C`, and user or system configuration could
    sign, hook or rewrite them.
    """
    inherited = {name: value for name, value in os.environ.items()
                 if not name.startswith(("GIT_", "XDG_"))}
    return {**inherited, "HOME": str(home), "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1", **extra}
