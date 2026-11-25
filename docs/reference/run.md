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

Agents are not commands: `shoal run <name>` runs only `[commands]` entries,
which may take any name, an agent's included, as a plain command without agent
prompts or exit alerts, expanding `{prompt}` to an empty string. For an agent's
name that no entry defines the error says to start it with `shoal start --in
[workspace] -a <name>`. [Agents](agents.md#launchers-and-trust) says how Shoal
launches them.

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
