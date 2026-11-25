# Configuration

This file says what the `config` commands do: setting up, inspecting and storing
config.

## Set up config

`repo setup [repository] [--here] [--agent NAME] [-- ARGS]` starts an agent in
the built-in `setup` mode that writes the repository's config. It works in the
repository's `shoal-setup` workspace, which Shoal opens on that branch when none
exists, with the worker mode's rules. A setup agent studies how the project or
machine is used, asks you about each setting it would write, reading the options
from [`shoal config reference`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md), and writes only what you agree to.
Before it saves the repository's config, it asks you in a [thread](threads.md)
whether to commit it or save it locally, unless you have said already, and
mentions a committed or local config the repository has.

- **Commit** writes `.shoal.toml`, or `.shoal/config.toml` when that file
  exists, and the scripts it runs, tests the setup script with `setup`,
  commits them and opens a PR.
- **Save locally** writes the repository's
  [local config](#store-repository-config-outside-git) and the scripts it runs
  beside it, named by paths relative to that directory, commits nothing and
  tests the setup script with `setup` in its own worktree. Its `shoal done` removes the workspace and its empty branch.

The agent writes the local config through the repository directory, which
every workspace agent has as another working directory. The agent is
`--agent`, else `agent.default`, else one chosen from the agent picker;
arguments after `--` go to the agent.

`config setup [--here] [--agent NAME] [-- ARGS]` starts a `setup` agent in a
[chat without a repository](chats.md#master-session) named `setup`, with the
`orchestrate` mode's rules and reach, which writes the global `config.toml`,
never a repository's config. It edits the file in place, keeping its comments,
runs `config reload` and tells you when the reload rejected the file. When the file
does not exist, the command first writes the packaged default, as `config
install default` does. Every launch and restore of the agent gets the file's
directory as another working directory; `--here` and `--agent` work as for
`repo setup`.

`repo setup` and `config setup` change the user's config, so a scoped
process runs them only as its `admin` permission allows, `config setup`
without a repository. Both start their agent at once regardless of the
limits, in a tab that takes focus whatever `terminal.focus` says; without a
terminal, or from a scoped process, they start it detached.

## Inspect effective configuration

Run `shoal config show [workspace]` to print every effective repository setting
and the layer that supplied it, such as `shared config` for the shared file. With no workspace, Shoal uses the current
workspace, the registered checkout when no workspace exists yet, then the
workspace picker. `--repo NAME` shows a repository's settings in its registered
checkout from any directory. Named tables
such as commands and ports show one entry per name. `--json` returns `key`, `value`,
and `layer` for each entry, and `file` for a template read from a file.

`shoal config reference` prints the [configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md) as
bundled with the installed release, so agents without network access can read
the options; `--json` returns it as `reference`.

`shoal config set KEY VALUE [KEY VALUE...]` edits the global file without a daemon,
creating it if absent; `shoal config unset KEY...` removes existing keys or tables
to restore defaults. Keys use TOML dotted syntax (quote a component containing dots); values
use TOML syntax, falling back to a string when they are not TOML values. Quote
arrays for the shell, for example `shoal config set commands.check '["cargo", "test"]'`.
Edits preserve unrelated settings and comments and keep the previous file as
`config.toml.backup`. One command's changes apply in order and are validated once,
so settings that depend on each other change together:

```sh
shoal config set machine.simulators.profiles.phone.device '"iPhone 17"' \
  machine.simulators.profiles.phone.runtime '"iOS 27"' machine.simulators.default phone
```

An invalid result leaves both files unchanged.

`--shared` makes `set` and `unset` edit the shared config, `shared.toml` beside
the global file, saving only a result that is valid on its own and under this
machine's global file; edits of the global file check it over the shared config
in turn. Only the user's own terminal edits the shared config.

## Store repository config outside Git

```sh
shoal repo config my-project                                 # Print the local config's path and contents
shoal config set setup.command scripts/setup --repo my-project
shoal config unset agent.default --repo my-project
```

A repository's local config is `.shoal/config.toml` in its own directory, beside
its workspaces and `.checkout` (`~/shoal/my-project/.shoal/config.toml`), for a
repository that won't accept `.shoal.toml` or settings only you want. It is
outside every Git tree, so nothing ignores it and `git clean` never touches it.
It takes the options of a [repository file](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers), with
untracked scripts beside it.

Shoal reads the file in place on every request, so an edit applies to every
workspace at once without a restart; an invalid file fails each command until
it is fixed. `shoal config set KEY VALUE... --repo NAME` and
`shoal config unset KEY... --repo NAME` edit it like global edits, keeping
comments and the previous file as `config.toml.backup` and saving only a valid
result. Its options layer over every workspace's worktree file as
[configuration](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers) describes. A worktree file that the current format rejects, such as
one a branch kept from before a format change, is ignored: the workspace
resolves its options from the other layers, `shoal config show` warns about
it, and the daemon logs the error and notifies you once per error while it
runs, until the file is fixed. Its resource leases are the exception: since the
file may name their hooks, acquiring or releasing one, and removing a
workspace that holds one, fails until it is fixed. `repo move` moves the
file with the repository's directory and `repo rm` deletes it. `--json` returns `path` and `toml`, unset
while the file does not exist. Scoped processes read and change it as their
space's [`admin` permission](modes.md#permitted-actions) allows.
