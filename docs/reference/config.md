# Configuration

This file says what the commands that set up, show and edit Shoal's config do.
The [configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md)
describes the files, their layers and every option.

## Set up config

`repo setup` and `config setup` start an agent that writes config with you. It
runs in the built-in [`setup` mode](modes.md#the-setup-mode). The agent studies how the
project or machine is used and works out which settings would help. It asks
you about each setting before it writes it, and writes only what you agree to.
It reads the options from `shoal config reference`.

Both commands take the same launch options:

- `--agent NAME` picks the agent tool. Without it, Shoal uses `agent.default`,
  else the agent picker.
- `--here` runs the agent in this terminal, and `--model` and `--effort`
  override its configured model and effort, as [Agents](agents.md) describes.
- Arguments after `--` go to the agent.

A setup agent starts at once, whatever the limits. On a terminal it opens in a tab that takes focus, whatever `terminal.focus`
says. Without a terminal, or from a scoped process, it starts detached. A
scoped process runs either command only as its space's
[`admin` permission](modes.md#permitted-actions) allows.

### Repository config

```sh
shoal repo setup my-project
```

`repo setup [repository]` starts the agent in the repository's `shoal-setup`
workspace. Shoal opens that workspace on the `shoal-setup` branch when none
exists.

Before the agent saves the config, it asks you in a [thread](threads.md)
whether to commit it or save it locally. It skips the question when you have
said already. It also tells you about a committed or local config the
repository already has.

- **Commit**: the agent writes `.shoal.toml`, or `.shoal/config.toml` when that
  file exists, and the scripts the config runs. It tests the setup script with
  `shoal setup`, commits the files and opens a PR.
- **Save locally**: the agent writes the repository's
  [local config](#store-repository-config-outside-git) and saves the scripts
  it runs beside it, named by paths relative to that directory. It commits
  nothing and tests the setup script with `shoal setup` in its own worktree.
  Its `shoal done` removes the workspace and its empty branch.

### Global config

```sh
shoal config setup
```

`config setup` starts the agent in a
[chat without a repository](chats.md#master-session) named `setup`. The agent
writes the global `config.toml` and never a repository's config. It edits the
file in place and keeps its comments. It then runs `config reload` and tells
you when the reload rejected the file. When the file does not exist, the
command first writes the packaged default, as `config install default` does.

## Inspect effective configuration

```sh
shoal config show 3
shoal config show --repo my-project
```

`config show [workspace]` prints every effective repository setting and the
layer that set it, such as `shared config` for the shared file. A named table,
such as commands or ports, shows one entry per name.

For a mode's instructions and a rule's text, it shows the combined text, the
highest layer that configured it and the file that layer read. It warns about
each layer whose rule repeats the opening of a space's facts. The daemon alerts
about such a rule in the global config when it starts or reloads its config.

Without a workspace, it shows the current workspace's settings. Outside a
workspace, it shows those of the registered checkout you are in, and otherwise
opens the workspace picker. `--repo NAME` shows a
repository's settings in its registered checkout, from any directory.

`--json` returns `key`, `value` and `layer` for each entry, and `file` for a
template read from a file.

## Read the configuration reference

`config reference` prints the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md)
of the installed release. An agent without network access reads the options
there. `--json` returns the text as `reference`.

## Edit the global config

```sh
shoal config set agent.default codex
shoal config set commands.check '["cargo", "test"]'
shoal config unset agent.default
```

`config set KEY VALUE [KEY VALUE...]` sets keys in the global file and creates
the file when it is absent. `config unset KEY...` removes keys or tables, so
lower layers or the defaults apply again. Neither needs a daemon.

A key uses TOML's dotted syntax. Quote a component that contains a dot. A value
uses TOML syntax, and one that is not a TOML value is a string. Quote an array
for the shell, as above.

An edit keeps unrelated settings and comments. It keeps the previous file as
`config.toml.backup`. A running daemon applies the edit at once.

One command applies its changes in order and validates the result once. Set
settings that depend on each other in one command:

```sh
shoal config set machine.simulators.profiles.phone.device '"iPhone 17"' \
  machine.simulators.profiles.phone.runtime '"iOS 27"' machine.simulators.default phone
```

An invalid result leaves both files unchanged. A scoped process edits the
config as its [`admin` permission](modes.md#permitted-actions) allows.

### Shared config

`--shared` makes `set` and `unset` edit the shared config, `shared.toml` beside
the global file. Shoal saves the result only when it is valid on its own and
under this machine's global file. An edit of the global file checks it over the
shared config in turn. Only the user's own terminal edits the shared config.

## Replace or reload the global config

`config install <name>` replaces the whole global file with a packaged
template. `config reset` installs `default`. Both move the current file to
`config.toml.backup` without asking, replacing an older backup. An unknown name
fails and changes neither file. Shoal copies a template, so a package upgrade
leaves an installed file's edits alone. Neither command needs a daemon, and a
running daemon applies the new file at once.

After you edit the global or shared file by hand, run `config reload`. The
daemon then rereads both files and keeps its running agents and leases. An
invalid file leaves its previous settings in place. Scoped processes may run
`config reload`.

## Store repository config outside Git

```sh
shoal repo config my-project                                 # Print the local config's path and contents
shoal config set setup.command scripts/setup --repo my-project
shoal config unset agent.default --repo my-project
```

A repository's local config holds its settings outside every Git tree, for a
repository that won't accept `.shoal.toml` or for settings only you want. The
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers)
says where it lives and how it layers over each workspace's file.

`repo config [repository]` prints the file's path and contents. `--json`
returns `path` and `toml`, which is unset while the file does not exist.

`config set --repo NAME` and `config unset --repo NAME` edit the local config
as they edit the global file. Scoped processes read and change the
local config as their space's [`admin` permission](modes.md#permitted-actions)
allows.
