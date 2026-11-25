# Configured commands

This file says what `shoal run`, `shoal edit` and the other configured commands
do.

Workspace shortcuts are [`[commands]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#commands) entries. Use `shoal run -- <command> <arguments>` for arbitrary commands and
`shoal run check [--workspace W] -- <extra arguments>` for configured ones, whatever
built-in command shares the name; `shoal check` runs nothing, and an unknown
top-level name prints a line pointing to `shoal run <name>`. Omit
the workspace to use the current one, otherwise the picker, which offers only the
workspaces that define a repository-only command. Unless the global
configuration, the current or named workspace, or on a terminal any workspace
defines a name, it is rejected as unknown. Bare `shoal run`
lists each effective command's argument array and source layer; outside a
managed workspace it lists only global commands and built-in defaults.
Execution preserves terminal I/O, scope, reserved-port variables, and exit status
just like arbitrary `run` commands. `{diff_base}` resolves lazily through the same
daemon fork-point/merge-base lookup as `shoal diff`; failure prevents launch.
A command, `shoal merge` or `shoal unmerge` that starts while an operation
that returns its workspace to ready on its own holds it, such as the update an
agent's start makes, waits for that to end and then runs, or is refused as not
ready when it leaves the workspace failed; a workspace being set up or removed
refuses it at once.

Agents' launchers are the [`command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) of their AI tool.
Overriding the `command` of Claude or Codex changes the executable or flags;
prompt templates, trust setup, and agent exit alerts still apply. Agents
are not commands: `shoal run <name>` runs only `[commands]` entries, which may
take any name, an agent's included, and for an agent's name that no entry
defines the error says to start it with `shoal start --in [workspace] -a
<name>`. Both run with their own sandbox and approval settings.
Every Codex launch, chats included, adds
`-c sandbox_workspace_write.network_access=true`, since Codex's workspace-write
sandbox otherwise blocks the daemon's socket and allows a single socket only
along with the network.
Claude and Codex launches, including Happy's, trust the repository's checkout and
shared Shoal workspaces directory before launch, or for a chat without a repository
`root_dir`, whose entry stays, creating the agent's user config if needed and
preserving other settings. Claude Code looks up a worktree's trust under
the checkout, not under parent directories above the worktree. An explicitly placed
workspace outside the workspaces directory gets its own trust entry, and so, for
Claude, which asks about each, does every other directory the launch passes with
`--add-dir` that none of those covers, such as an `agent.dirs` entry; one outside
the repository stays when the repository is removed.
Claude uses `~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`); Codex CLI
and app use `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`). For tracked
agents, a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` set in the space's `[env]` takes
precedence over Shoal's environment; desktop apps use Shoal's. One set
in the tool's [`[agents.<name>]` `env`](agents.md) takes precedence over both. A trust
update failure warns and still launches the agent. Removing a repository, by any
client or as a [contribution](repo.md#contributions), removes its trust entries from the
default files and from those its agents, chats and app handoffs ran with, including
a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` they inherited, while preserving other project
settings; moving it carries each entry to the new path. An entry Shoal cannot
change is left in place with a `trust_left` alert, and the removal or move still
completes.

`shoal edit [workspace] -- <args>` runs the `edit` command, such as
`edit = ["zed", "{path}"]`. Without one it runs `$VISUAL`, otherwise `$EDITOR`, split on whitespace,
followed by the workspace path and the forwarded arguments.

For local review with [tuicr](https://github.com/agavra/tuicr), install it on PATH
and set the `review` and `review-worktree` [commands](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#commands).

`shoal review run [space]` asks whether to review with the `review` command or
an agent (see [local reviews](reviews.md#local-reviews)); `--manual` or `--agent <name>` skips
the question, which is required without a terminal. Manual review runs the `review`
command (`shoal run review` also does) with the arguments after `--`; without one,
the agent reviews.
`shoal run review-worktree [--workspace W]` reviews uncommitted changes. The committed
range excludes working-tree changes that `shoal diff` includes. Export feedback
from tuicr and hand it to your agent explicitly; see
[tuicr's export documentation](https://github.com/agavra/tuicr/blob/main/docs/CLI.md#output-for-scripts-and-agents).
Shoal tracks execution; the review tool owns sessions, exports, and forge access.
