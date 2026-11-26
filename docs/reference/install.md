# Installation

This file says how to install and update Shoal, its shell integration and its
skill.

## Homebrew

The [HN05 tap](https://github.com/HN05/homebrew-tap) installs prebuilt releases
on macOS and Linux, for arm64 and x86_64. A Homebrew installation needs the
same [runtime dependencies](../../README.md#runtime-dependencies) as a
downloaded binary.

```sh
brew tap hn05/tap
brew install hn05/tap/shoal
shoal skill install
shoal daemon install
```

Upgrade it with Homebrew:

```sh
brew update && brew upgrade hn05/tap/shoal
```

## Update a downloaded binary

```sh
shoal update                  # The latest release, when it is newer
shoal update --version 0.21.0 # Exactly that release, older or newer
```

`shoal update` installs a GitHub release's binary for this platform. It checks
the download against the release's `SHA256SUMS` and the version the binary
reports. It then replaces the executable file that `shoal` resolves to. On
macOS, when that file sits in a `Shoal.app` bundle, it replaces the bundle
from the release's archive ending in `-app`. It refuses a Homebrew
installation, which Homebrew updates.

## After an update

A managed daemon notices the new binary and restarts on it once no daemon
operation, setup or merge runs. Until then, commands of the new release wait
for it, up to ten minutes. `shoal daemon restart` applies the update at once,
after the [work the daemon runs itself](daemon.md#work-the-daemon-runs-itself).

Tracked agents and commands keep running while the daemon restarts, and
reattach to the next one. Followed event and alert streams and item waits
reconnect too, and continue in the new release when its protocol differs.

The state directory and installed skills live outside the package and survive
an update. The next `shoal` command
[updates the installed skills](#skill-updates). Open terminals need the
[shell integration](#shell-integration) loaded again.

A daemon refuses a state database that a release before v0.21.0 wrote, and
changes nothing in it. To upgrade from such a release, start v0.21.0 once
first.

## Shell integration

Add this line to `.bashrc` or `.zshrc`, and run it in each open terminal:

```sh
source <(shoal shell init)
```

Bash without process substitution takes `eval "$(shoal shell init)"` instead.
The integration defines a `shoal` shell function that does
[navigation](#navigation) and loads [tab completion](#tab-completion). Run the
line again in open terminals after an upgrade.

## Navigation

```sh
shoal cd      # Pick a space, even inside one
shoal cd 3    # Enter space 3
shoal cd -    # Enter the previous space
```

With the shell function, `shoal cd` and `shoal start` move the shell into the
workspace. A command that removes the current workspace moves the shell out
of it.

Cleanup can remove the shell's directory while a command runs. The shell then
moves to the nearest directory above that still exists, after the command or
at the next prompt, and keeps the command's exit status.

Without the shell function, a command prints the destination and leaves the
shell where it is. On a terminal, it also says how to load the integration.
`--json` prints the path and moves the shell the same way.

A scoped process's shell, such as its agent's, never moves.

After a move, the function redraws the Zsh prompt. Bash redraws its prompt
through `PROMPT_COMMAND`.

## Tab completion

The shell integration turns on Bash and Zsh completion, and initializes Zsh's
completion system when it is not. Each Tab asks the installed binary, which
offers subcommands, flags, fixed values and paths without a daemon. The global
config's commands are among the subcommands.

When a daemon runs, completion also offers what it knows: the repository's
configured commands, repositories, workspaces, resource pools and members,
lease names, ports and simulator leases. It reads them for the current or
named workspace, under `--state-dir` and the caller's scope. Each query has a
500 ms limit, and completion never starts a daemon or a picker.

Completion leaves out a space without what the command acts on. It describes
a space the command would refuse with the command's reason. Targets sort
before flags, in fzf-tab too.

`shoal shell completions <shell>` prints the completion script for Bash, Zsh,
Fish, PowerShell or Elvish.

## Agent skill

```sh
shoal skill install          # Every tool whose skill directory exists
shoal skill install grok     # One tool, creating its skill directory
shoal skill                  # Print the skill
```

`shoal skill install` installs the `shoal` skill, which says what Shoal's
commands do. It installs it at user scope, as `shoal/SKILL.md` in each AI
tool's skill directory, and needs no daemon. A scoped process cannot run it.

Each tool has a default skill directory:

| Tool | Skill directory |
| --- | --- |
| Claude Code | `~/.claude/skills`, or `skills` under an absolute `CLAUDE_CONFIG_DIR` |
| Grok | `~/.grok/skills` |
| Codex, opencode and pi | `~/.agents/skills`, one copy for all three |

[`[machine.agents.<name>] skill_dir`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent-tools-on-this-machine)
sets a tool's directory. Without a tool name, `shoal skill install` installs
only into directories that exist, and skips a tool without a directory.
Create the directory, or name the tool, to install for it.

Each `SKILL.md` is a regular copy of the skill built into the binary, as
[skill files](../design/modes.md#skill-files) explains. A linked skill
directory stays as the user made it, and so do other files in a skill
directory.

`shoal skill --json` returns the skill in its `skill` field.

## Skill updates

Shoal records what it installed in `.shoal-skills.json` in each skill
directory. After an upgrade, the next unscoped command updates each directory
that holds a Shoal skill. It adds new skills, replaces outdated copies and
removes retired skills, without printing anything.

Shoal keeps a skill that changed or was removed since it installed it, and
reports it once. It treats a skill the record does not list as changed,
unless it matches this version's.

`shoal skill install` restores a removed skill. On a terminal, it asks whether
to keep, replace or show the diff of each changed skill. `--force` replaces
them without asking.
