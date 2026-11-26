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

## Waiting for memory

`shoal run <name>` and `shoal run -- <command>` wait until the memory the
command's earlier runs used is free, so the builds and tests on a machine take
turns instead of running it out of memory together. The
[agent queue](queue.md#commands-and-seed-builds) admits the command at that
estimate. Meanwhile `shoal queue` lists it and the command prints what it
waits for, such as `waits for memory: needs 12.0 GiB, 7.0 GiB left`. `--now`
runs it at once, as the space's [`limits` permission](modes.md#permitted-actions)
allows. A configured `edit` command waits like any other; `shoal edit` with
`$VISUAL` or `$EDITOR` does not wait.

A `shoal run` inside an admitted command, such as one a configured script
runs, starts at once, since the enclosing run's admission counts it. An
interrupt or the end of the terminal's session ends a command that waits.

A configured command's runs share one estimate under its name, whatever
arguments follow it, so `shoal run test -- -p foo` counts at what
`shoal run test` used. Define a second entry to keep them apart. The estimate of
`shoal run -- <command>` goes by its exact arguments. A command that has never
run starts as soon as anything fits, and that run gives it an estimate. Each run
adds a `Command ran` entry to the space's [log](log.md) with its peak memory and
how long it ran.

Under memory pressure, [protection](load.md) stops the newest commands holding
memory before it stops any agent. A stopped command prints
`stopped for critical memory pressure at <time>; runs again once it fits`, waits
for healthy readings and its memory, and runs again, so its caller sees a slow
command rather than a failure. Build tools such as Cargo keep what they
finished, so the rerun redoes only what was being built. After its third stop
the command gives up and exits non-zero, naming the stop.

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
