# Configured commands

This file says what `shoal run`, `shoal edit` and the other configured commands
do.

## Run a command

```sh
shoal run -- cargo test                  # Any command, in the current workspace
shoal run check --workspace 3 -- --quiet # A configured command, with extra arguments
shoal run                                # List the configured commands
```

`shoal run -- <command> <arguments>` runs any command in a workspace.
`shoal run <name>` runs a configured command, a
[`[commands]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#commands)
entry, and appends the arguments after `--`. A configured command runs only
through `shoal run`, even where a built-in command has the same name. An unknown
top-level command, such as `shoal check`, runs nothing and prints a line that
points to `shoal run <name>`.

`--workspace` names the workspace. Without it, the command runs in the current
workspace. Elsewhere, a picker offers the workspaces whose configuration
defines the command. Shoal rejects a name as unknown unless the global
configuration or that workspace defines it, or on a terminal any workspace does.

Bare `shoal run` lists each command in effect with its argument array and the
configuration layer it comes from. Outside a managed workspace it lists only the
global commands and the built-in defaults.

## Execution

A configured command runs as an arbitrary `shoal run` command does. It keeps
terminal input and output, the workspace's scope, its reserved-port variables
and its exit status. Shoal resolves `{diff_base}` only when the command uses it,
through the same daemon lookup as [`shoal diff`](branches.md#diff). When that
lookup fails, the command does not start.

A command, `shoal merge` or `shoal unmerge` can start while an operation holds
its workspace and will return it to ready on its own, such as the update an
agent's start makes. The command waits for that operation to end and then runs.
When the operation leaves the workspace failed, Shoal refuses the command as not
ready. A workspace that is being set up or removed refuses the command at once.

## Agents are not commands

`shoal run <name>` runs only `[commands]` entries, even one named after an
agent. Such an entry runs as a plain command: it gets no agent prompts or exit
alerts, and `{prompt}` expands to an empty string. For an agent's name that no
entry defines, the error says to start the agent with `shoal start --in
[workspace] -a <name>`. [Agents](agents.md#launchers-and-trust) says how Shoal
launches them.

## Edit

`shoal edit [workspace] -- <arguments>` opens a workspace in an editor. It runs
the `edit` command, such as `edit = ["zed", "{path}"]`. Without one, it runs
`$VISUAL`, or else `$EDITOR`, split on whitespace, followed by the workspace path
and the arguments after `--`.

## Review commands

The `review` and `review-worktree`
[commands](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#commands)
run a review tool such as [tuicr](https://github.com/agavra/tuicr), which must be
on PATH.

`shoal review run [space]` asks whether to review with the `review` command or
with an agent, a [local review](reviews.md#local-reviews). `--manual` chooses the
command and `--agent <name>` an agent, without asking. Without a terminal, one
of them is required. Without a `review` command, an agent reviews. A manual
review runs the `review` command, as `shoal run review` does, with the arguments
after `--`.

The `review` command reviews committed changes, so it leaves out the uncommitted
changes that `shoal diff` shows. `shoal run review-worktree [--workspace W]`
reviews those. The review tool keeps its own sessions, exports and forge access.
Export its feedback and hand it to your agent yourself, as
[tuicr's export documentation](https://github.com/agavra/tuicr/blob/main/docs/CLI.md#output-for-scripts-and-agents)
describes.
