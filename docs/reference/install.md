# Installation

This file says how to install and update Shoal, its shell integration and its
skill.

## Homebrew

The [HN05 tap](https://github.com/HN05/homebrew-tap) installs prebuilt releases
on macOS and Linux (arm64 or x86_64) with the same
[runtime dependencies](../../README.md#runtime-dependencies) as a downloaded binary.

```sh
brew tap hn05/tap
brew install hn05/tap/shoal
shoal skill install
shoal daemon install
```

Upgrade with `brew update && brew upgrade hn05/tap/shoal`. An installation from
the former `--HEAD` channel moves to releases with `brew reinstall
hn05/tap/shoal`. `shoal update` refuses Homebrew installations; for a downloaded binary,
it installs the latest GitHub release's binary for this platform when that
release is newer, or with `--version` exactly that release, older or newer,
after checking it against the release's `SHA256SUMS` and its
reported version, by replacing the resolved executable file, or on macOS the
`Shoal.app` bundle holding it, which comes from the platform's archive ending
in `-app`. Managed daemons notice the replacement and restart once daemon operations are
idle and no setup or merge runs; until then, commands of the new release wait
up to ten minutes for it. Followed event and alert streams and
item waits reconnect across the handoff, continuing in the installed release
when its protocol differs; use `shoal daemon restart` to apply an
upgrade immediately. Tracked agents and commands keep running while the daemon
is away and reattach to the next one; see [Daemon restarts](daemon.md#daemon-restarts). State and installed skills live
outside the package and survive; the next `shoal` command
[updates installed skills](#agent-skill-outside-project-repositories) to the
ones built into the new binary. An upgrade from before v0.21.0 starts v0.21.0
once on the way: later daemons refuse, unchanged, a state database an older
version wrote, naming that release.

## Shell navigation

Add `source <(shoal shell init)` to `.bashrc`/`.zshrc` (or `eval "$(shoal shell
init)"` for Bash without process substitution) and run it in open terminals,
including after upgrades. The function lets `start` and `cd` enter workspaces
and moves you out of a removed one. If cleanup removes your directory while an
agent runs, the shell returns to the nearest surviving parent after exit or at
the next prompt, preserving the exit status. `shoal cd` always opens fzf, `shoal cd
<space>` goes directly, and `shoal cd -` switches back to the workspace it last
left or entered. Scoped agents cannot navigate outside their worktree. Without
the function, Shoal prints the destination and, in an interactive terminal,
explains how to load the integration; `--json` prints the path and navigates
alike. A scoped process's shell, its agent's, never moves.
The wrapper refreshes the Zsh line-editor prompt after navigation; Bash updates
its prompt through `PROMPT_COMMAND`.

## Tab completion

The same shell integration enables Bash and Zsh completion (initializing Zsh's
completion system if needed). Each Tab asks the installed binary for
subcommands (including global configured commands), flags, fixed values, and
paths without a daemon; with one running it also suggests repository-configured
commands, repositories, workspaces, pools, members, lease names, ports,
and simulator leases for the current or named workspace, honoring `--state-dir`
and scope with a 500 ms timeout, never starting a daemon or picker. A space
the command would refuse is described by the reason the command would give;
one without what the command acts on is left out. Targets sort before flags,
also in fzf-tab. `shoal shell completions <shell>` prints scripts for
Bash, Zsh, Fish, PowerShell, and Elvish.

## Agent skill outside project repositories

```sh
shoal skill install          # Every tool whose skill directory exists
shoal skill install grok     # One tool, creating its skill directory
```

Installs the bundled `shoal` skill, which describes what Shoal's commands do
for workspaces, chats and sessions outside Shoal, at user scope with no daemon,
as `shoal/SKILL.md` in each tool's skill directory: `~/.claude/skills` for Claude (honoring an
absolute `CLAUDE_CONFIG_DIR`), `~/.grok/skills` for Grok, and `~/.agents/skills`
for Codex, opencode and pi, which share one copy. Without a tool name, Shoal
installs only into skill directories that already exist and skips a configured
tool without `skill_dir`; create a directory, or name the tool, to install for
it. [`[machine.agents.<name>] skill_dir`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent-tools-on-this-machine)
sets or overrides a tool's directory. Names are portable identifiers; `all` is
reserved. Each `SKILL.md` is a regular copy
of the skill built into the binary, since Codex skips a linked one; a linked
skill directory is left to the user. Other files in the skill directory are
preserved. Run it outside scoped executions. `shoal skill` prints the skill (`--json`
returns a `skill` field).

Shoal records what it installed in `.shoal-skills.json` in each skill directory.
After an upgrade, the next unscoped command updates each directory that still
holds a Shoal skill without printing anything: it adds new skills, replaces
outdated copies and removes retired skills. A
skill changed or removed since Shoal installed it is kept and reported once;
`shoal skill install` restores a removed skill and, in a terminal, asks whether
to keep, replace or show the diff of each changed one; `--force` replaces them
without asking. A skill a directory's record does not list is treated as
changed unless it is this version's. Debug builds neither install nor update skills unless
`SHOAL_DEV_USER_FILES=1`.
