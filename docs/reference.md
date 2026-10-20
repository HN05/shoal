# Shoal command reference

Start with the [usage guide](../README.md).

Human-readable output uses color when its destination is a terminal. Set a
nonempty `NO_COLOR` or `TERM=dumb` to disable it. Redirected output and `--json`
stay plain; stdout and stderr are detected independently.
Progress feedback shows the pending operation with a spinner and elapsed seconds
after half a second, cleared before the result.
Progress uses terminal stderr only and is suppressed with `--json` or `TERM=dumb`.

- [Installation and upgrades](#homebrew)
- [Daemon](#daemon)
- [Workspaces](#workspaces)
- [Setup and hooks](#workspace-setup-and-hooks)
- [Removal and cleanup](#remove-a-workspace)
- [Notifications](#notifications)
- [Ports](#port-reservations)
- [Shell integration](#shell-navigation)
- [Recovery](#recovery)
- [Simulators](#simulators-macos)
- [Resource pools](#cooperative-resource-pools)
- [Agent skill](#agent-skill-outside-project-repositories)

## Homebrew

The [HN05 tap](https://github.com/HN05/homebrew-tap) installs prebuilt releases
on macOS and Linux (arm64 or x86_64); `--HEAD` builds from source and requires
Rust. Both channels use the same [runtime dependencies](../README.md#runtime-dependencies).

```sh
brew tap hn05/tap
brew install hn05/tap/shoal            # Or: brew install --HEAD hn05/tap/shoal
shoal skill install
shoal install
```

Upgrade with `brew update && brew upgrade hn05/tap/shoal` (`--fetch-HEAD` for
`main`). Managed daemons notice the replacement and restart once daemon operations are
idle and no setup or landing runs; until then, commands of the new release wait
up to ten minutes for it. Followed event and notification streams and
item waits reconnect across the handoff, continuing in the installed release
when its protocol differs; use `shoal daemon restart` to apply an
upgrade immediately. Tracked agents and commands keep running while the daemon
is away and reattach to the next one; see [Daemon restarts](#daemon-restarts). The channels share one installation,
daemon, and skill path; to switch, run `shoal daemon stop`, uninstall, install
the other channel, and `shoal daemon start`. State and installed skills live
outside the package and survive; the next `shoal` command
[updates installed skills](#agent-skill-outside-project-repositories) from
Homebrew's stable `opt` path.
Packagers can set `SHOAL_SKILLS_DIR` to an absolute directory holding every
bundled `<skill>/SKILL.md` at runtime, overriding `SHOAL_BUILD_SKILLS_DIR` at
build time. Without either, an adjacent `shoal-skills` symlink can point through
a stable installation prefix; relative targets resolve lexically against its
directory. Without that link, installation uses the embedded skills.

## Daemon

```sh
cargo install --path .
shoal install --dry-run     # Preview the OS service definition
shoal install               # Register and start the per-user service
shoal daemon status|stop|start|restart|reload
```

macOS uses a launchd LaunchAgent in `~/Library/LaunchAgents` (requires a GUI
login session); Linux uses a systemd user service. Without a service manager,
run `shoal --state-dir /tmp/shoal-dev daemon run` in the foreground and target
it with the same `--state-dir` (or `SHOAL_STATE_DIR`). State defaults to
`~/.local/state/shoal`; runtime state and the Unix socket are private to the
user. Service commands target the one registered per-user service, whose state
directory must match. `daemon status` exits 1 when offline. `daemon start`
without an installed service offers to run `shoal install` in an interactive terminal.

`shoal install` preserves the invoked executable's symlink path and captures the
current `PATH` for the service, so install runtime dependencies and any hook
tools first. It also creates `~/.config/shoal/config.toml` (or
`$XDG_CONFIG_HOME/shoal/config.toml`) from
[configs/default.toml](../configs/default.toml), which states every default,
and installs the [bundled issue template](../src/config/templates/issue.md) and
chat template beside it, never touching an existing file; `--dry-run` writes
nothing. Installation prints
the shell initialization hint only when neither `.bashrc` nor `.zshrc` (under
`ZDOTDIR` when set) contains an active initialization command. `shoal config
install <name>` replaces the entire global file with a packaged template;
`shoal config reset` installs `default`. Both move the current file to
`config.toml.backup` (replacing an older backup) without asking. Templates are
copied, so package upgrades leave installed edits alone. Unknown names fail
without changing either file; installation needs no daemon. `shoal config` edits,
installs and resets apply to a running daemon at once; after editing the file by
hand, run `shoal daemon reload`. Reloading keeps running agents and leases, and an
invalid file leaves the daemon's previous settings in place. The CLI reads agent
settings on each command. Setup preserves compatible
daemons and commands until restart; incompatible daemons restart automatically.
Stop foreground daemons manually. The daemon starts each diagnostic line with its
local time and drops lines it cannot write. macOS diagnostics go to `daemon.log` in the
state directory; Linux uses `journalctl --user -u shoal.service`. Native Linux service integration remains untested.

### Daemon restarts

Stopping, restarting or losing the daemon does not interrupt tracked agents and
commands, nor an agent's wait for its automatic restore after a protection stop.
Their wrappers keep the command and terminal, write nothing while it runs, and
reconnect once a daemon listens again, which continues the wait; until then the command's own
`shoal` calls fail. The new daemon checks the execution record, scope token and
process identities, then restores the agent's name and automatic recovery
setting, so stop, overload protection, exit notifications and
`post_agent_exit_cmd` work as before. A command that exits while the daemon is
away reports its exit code once reattached; after it exits the wrapper prints
that it is waiting and gives up after a minute or on Ctrl-C, leaving the
execution for `shoal doctor`. When the daemon refuses an execution, the wrapper
stops the command and saves what `shoal stop` saves. Waiting executions stay
active for cleanup and removal. Setup, landing and wrappers from releases before
reattachment stop on shutdown as `shoal stop` does, and `shoal daemon restart`
then suggests `shoal resume --all`.

### Overload protection

At the configured memory threshold, the daemon asks the newest connected tracked
agent to stop. Linux measures usage from `MemAvailable`, including reclaimable
cache; macOS uses native critical pressure. The wrapper terminates the process
group through its normal grace period. The monitor waits for the cooldown before
resampling and stopping another agent if pressure persists. It retains workspaces, execution recovery records and resource
leases and records an `agent_stopped` notification.
Ordinary commands, desktop handoffs and disconnected executions are not selected.
Monitoring does not guarantee that the OS will never reach its OOM limit.

When space available to unprivileged processes on a filesystem holding
workspaces or daemon state falls below `disk.cleanup_free_gib`, the daemon
removes the workspaces there that idle cleanup would remove, without waiting for
their idle delay, until enough space is available. A repository with idle
cleanup disabled keeps its workspaces. Removals record `workspace_removed`
notifications and the `disk_space` event cause; a pass that cannot free enough
space repeats at most every 30 seconds. While space stays below
`disk.stop_free_gib` after cleanup, the daemon stops every running agent as
memory protection does, then every other tracked execution as `shoal stop` does,
including ones started later, recording `agent_stopped` notifications, or
`stop_failed` when stopping fails. Agents restore under the recovery rules below
once `disk.cleanup_free_gib` is available; otherwise free disk space, then run
`shoal resume`. A filesystem whose reading fails is skipped, so it removes and
stops nothing, and no agent restores until every reading succeeds.

Before stopping anything, Shoal warns the agents. While memory, CPU or free
disk space is past its `[overload.warning]` threshold and that protection is
enabled, the daemon queues an [agent message](#agent-messages) for each
workspace with a running agent, naming the threshold and asking it to reduce
load. CPU must stay above its warning threshold for `cpu_sustained_seconds`.
Each workspace hears about each signal at most once per `repeat_minutes`, and
agents started later are warned at the next sample. Warnings stop and remove
nothing; a failed reading warns about nothing. Warning thresholds are not checked
against stop thresholds; a warning at or above one arrives with the stop.

Configure machine-wide settings in global TOML and reload the daemon:

```toml
load = "balanced"              # "light", "balanced" or "full"

[overload]
poll_seconds = 2
cooldown_seconds = 5

[overload.memory]
enabled = true                 # Opt out with false
used_percent = 95              # Linux only; 50–99
sustained_seconds = 0           # Stop on the first critical sample

[overload.cpu]
enabled = false                # Opt in with true
used_percent = 90              # Aggregate busy time across all cores; 1–100
sustained_seconds = 300         # Five minutes

[overload.disk]
enabled = true                 # Opt out with false
cleanup_free_gib = 5           # Remove idle cleanup candidates below this
stop_free_gib = 2              # Stop executions below this; 1 to cleanup_free_gib

[overload.warning]
enabled = true                 # Message running agents before a stop
memory_used_percent = 85       # Linux; macOS warns at native warning pressure
cpu_used_percent = 80
cpu_sustained_seconds = 60
disk_free_gib = 10
repeat_minutes = 30            # Least time between repeats per signal and workspace

[overload.recovery]
enabled = true                 # Applies only with a configured resume command
memory_used_percent = 80       # Linux; macOS requires normal pressure
cpu_used_percent = 75
sustained_seconds = 60
```

`load` says how hard Shoal may load the machine, and sets each CPU and memory
percentage the file leaves out; the values above are `balanced` without swap.
Use `light` for a computer you work on and `full` for one that only runs agents.
The warning levels are also where [dynamic admission](#agent-queue) stops
starting agents:

| `load` | CPU warning / stop / recovery | Memory warning / stop / recovery |
|---|---|---|
| `light` | 60 / 80 / 55 | 75 / 90 / 70, with swap 80 / 93 / 75 |
| `balanced` | 80 / 90 / 75 | 85 / 95 / 80, with swap 90 / 97 / 85 |
| `full` | 95 / 100 / 90 | 95 / 98 / 90, with swap 97 / 99 / 93 |

Memory percentages apply on Linux, where configured swap lets memory fill
further before the kernel kills processes.

Durations are seconds, bounded to one day; polling, cooldown and sustained CPU
and recovery durations must be positive. CPU usage measures aggregate busy time
across all cores, excluding I/O wait. A failed reading resets that signal’s timer and cannot authorize a stop;
memory protection remains independent of CPU readings.

Resume commands are argument arrays in `[agent_resume]`, keyed by the tracked
agent name. They use the same workspace substitutions and repository layering as
`[commands]`; they must restore a session without repeating the initial prompt.
Codex and Claude automatically continue the latest session in the workspace by
default, without a session picker, with a prompt to continue their unfinished
assignment on manual or automatic resume. Configured commands receive that
prompt through `{prompt}`; other agents need an entry in `[agent_resume]`.
A failed command lookup warns and
disables automatic recovery for that launch. Overload notifications include the
pressure reason, execution ID, and automatic or manual recovery path. After the
wrapper stops the agent, Shoal also stops processes that carry the execution's
identity outside the agent's process group, such as background shell commands,
as `shoal doctor --stop` does. Shoal saves
the agent identity and pressure reason before signaling the wrapper, so a wrapper killed
under pressure can still be resumed after reconciliation. With one configured, the wrapper
stays connected while the agent is stopped and automatically runs that command
after healthy readings persist for the recovery interval. Recovery requires
headroom below each enabled signal’s recovery threshold, whose thresholds must be
positive and below their stop thresholds, and with disk protection enabled at
least `disk.cleanup_free_gib` available on every monitored filesystem. Agents restore one at a time, each with
a fresh healthy interval, which a reload changing `[overload]` restarts. Missing
readings, manual stop/removal, an exited wrapper, or changed workspace ownership
prevent recovery. Surviving processes Shoal cannot stop, or cannot prove stopped,
delay it: a `recovery_blocked` notification names them, and the wrapper keeps
waiting until they exit. Reloading with recovery disabled
finishes waiting agents at once, keeping their records for `shoal resume`.

`shoal stop [workspace]` stops tracked agents and commands through their wrappers,
which print what stopped them and how to restore them, retaining work and resource leases. Agents save recovery records; commands started
with `exec` or `run` save their arguments, which `shoal resume` reports and never
reruns; arguments that are not UTF-8 are not saved. Setup, landing and other internal commands save nothing.
`--all` stops every ready or failed workspace with a running execution in parallel,
reporting each failure without keeping the others running.
Disconnected executions require `shoal doctor`. Stopping cancels a waiting automatic
restore and requires explicit resume even with `[agent_resume]` configured.
Run stop and resume outside scoped executions.

`shoal resume [workspace]` restores a saved agent recovery record after its
wrapper exits; use `--execution <id>` when several agents stopped in one workspace,
or pick the agent by name, stop time and reason on a terminal. Without a
workspace, resume uses the current workspace when it has stopped work, otherwise
a picker listing every workspace with what it would restore, stopped work first;
with no stopped work anywhere it reports that without a picker. Completion
describes each workspace the same way.
Stopped commands are included in the continuation prompt, asking the agent to
rerun the ones still needed: built-in agents receive it as their prompt argument,
and `[agent_resume]` commands through `{prompt}`. Otherwise, and when no agent was
stopped, resume prints them as `shoal exec` commands. Either way they are reported once.
`--all` resumes every workspace and reports commands without an agent,
skipping agents whose wrapper still waits to restore them. In an
interactive Herdr pane with `herdr.enabled`, it restores each agent in the
pane it stopped in once that pane is back at its prompt. Otherwise it restores a
single agent in the current terminal and starts several in the background, as
`shoal add` does: a tab per agent where `herdr.new_tab` allows, labelled as
`shoal add` labels it, or a detached zmx session per agent on a terminal outside
Herdr. Without a terminal it lists them as `shoal resume` commands to run in
separate terminals.
Use `--discard` to forget the workspace's stopped agents and commands, every
workspace's with `--all`, or the `--execution` one, without launching anything, allowing normal idle cleanup again.
When a stopped agent's execution is unknown, after a daemon restart or a stop
Shoal could not prove, the daemon forgets it once its wrapper has exited and none
of its processes remain, keeping the record for `shoal resume`, which repeats the
check for the execution it selects. When processes survive or Shoal cannot prove
that none do, the execution stays, a `recovery_blocked` notification names the
problem, and the daemon checks again until they exit; inspect it with
`shoal doctor`. An agent still running when its wrapper
reattaches drops a protection stop that never reached it. Unrelated executions may keep running.
The command uses
the current resume configuration; without one, built-in terminal agents continue
the latest session in the workspace. Other agents require a configured restore
command. Resume shows the saved pressure reason. Records
survive daemon restart and suppress idle cleanup until the replacement process
is registered or the workspace is removed. A failed launch retains its records;
a later stop or overload creates a record for the replacement execution. The original
task prompt is never replayed.

## Workspaces

`shoal rename [WORKSPACE] BRANCH` renames a ready workspace's checked-out Git
branch and its derived workspace name while keeping the worktree path and
owned resources and settings. Dirty files are preserved;
Git keeps the branch's upstream and reflog. Remotes and open PR head branches
are unchanged. Names must be available locally and as a derived workspace name;
rename does not choose a suffix. The default branch, branches checked out
elsewhere, and Worktrunk-reserved names are refused. Scoped callers may rename
their own workspace when no other execution is recorded; unscoped renames
require no recorded executions. The caller keeps its original environment, and
new commands receive the renamed identity. Existing PR watches and acknowledgements
block renaming because their recorded remote head remains the original branch.
An interrupted rename requires `shoal doctor --repair` before reuse;
confirmed-deleted worktrees can still be removed through normal cleanup.

`shoal add <link>` recognizes issue, PR and branch URLs. Links select the single
registered repository with the same origin; `--repo` selects one explicitly and
must match the link. An unregistered remote offers interactive registration,
otherwise lookup fails with the `shoal repo add` command to run. Lookups use your
[forge login](#forges).

Issue links and `shoal add <number>` require an open issue and derive
`issue-<number>-<title-slug>`. Numbers use `--repo`, the current registered checkout
or workspace, then an interactive repository picker. An existing association
reopens its workspace; otherwise the derived local branch reopens as `--existing`
would. Lookup and ownership failures create nothing. The command starts
`--agent`, else the configured `default_agent`, else an interactive agent picker.
Choosing “No agent” creates the workspace without a launch. Issue prompts use
`issue-template.md`, substituting `{number}`, `{title}`, `{url}` and `{body}`
once; options after `--` go to the agent. Codex uses CLI mode for issue prompts.

PR links open their head branch against the refreshed remote base, reusing an
owned workspace when present, and link the PR once the workspace is ready; a
refused link only warns. Fork PRs cannot be opened. Branch URLs use
GitHub's `/tree/<branch>`, GitLab's `/-/tree/<branch>`, Bitbucket's
`/branch/<branch>` or Forgejo's `/src/branch/<branch>` route and open the
origin branch, preserving slashes and decoding URL escapes. Setup, hooks and
existing-branch ownership checks apply. These forms launch an agent with `--agent`.
`add <repository> --issue <number-or-url>` remains available with an optional
branch name and launches an agent only with `--agent`.

```sh
shoal repo add /path/to/repo             # Or a Git clone URL; register once
shoal repo add /path/to/repo --name my-project
shoal repo rename my-project new-name
shoal repo list
shoal repo                               # Interactive repository menu
shoal add my-project fix-login
shoal add my-project fix-api --agent codex -- "Fix the API timeout"
shoal add https://forge.example/team/repo/issues/34 -- --model fast
shoal cd                                 # Fuzzy picker, even inside a workspace
shoal cd fix-login                       # Enter through the shell function
shoal cd -                               # Previous directory
shoal exec fix-login -- cargo test
shoal edit fix-login                     # Open in $VISUAL/$EDITOR or the edit command
shoal claude fix-login -- --help
shoal codex                              # Current workspace or picker; default mode
shoal codex fix-login --cli -- --help
shoal codex fix-login --app              # Codex desktop app
shoal happy codex fix-login              # Detached Happy session for the Happy app
shoal t3 fix-login                       # Running T3 Code desktop app
shoal status fix-login                   # Activity, changes, held resources, issue and PR state
shoal status pr 12                       # Workspaces that link PR 12
shoal status resource devices            # Workspaces that hold a devices lease
shoal inspect fix-login
shoal stop fix-login                     # Stop agents and commands for shoal resume
shoal rm fix-login                       # Remove; choose what to keep if work differs
```

Bare `shoal` opens an fzf list of workspaces (`shoal --help`, or bare `shoal`
without a terminal, prints commands grouped by task and a starting workflow;
commands that only Shoal, its agent hooks and integrations run live under the
hidden `shoal internal`).
Its rows, like `shoal ls` and workspace pickers, are aligned and marked ● ready,
◌ in progress or ✗ failed. Each names the workspace, its repository when the
rows span several, its status and the linked issue's title. The status is the
state unless ready; for a ready workspace it is, in order of precedence,
`stopped` when `shoal stop` saved agents or commands for `shoal resume`,
`waiting for input`, `ready for review`, the other [agent states](#agent-state),
`running` while an agent or command runs, `ready for review (outdated)`, or `idle`. Branches and paths are in `shoal status`.
`ls --repo <repository>`, with or without `--ready` or `--queue`, lists only that
registered repository's workspaces and queued agents. `ls --status <status>`,
repeated or comma-separated, lists only rows with one of those statuses, spelled
in kebab case (`waiting-for-input`, `ready-for-review-outdated`, `queued`).
Enter enters the selection; Ctrl-D deletes, Ctrl-E
runs Claude/Codex CLI, starts a Happy session, opens Codex/T3 apps, or runs a shell command, Ctrl-A adds,
Ctrl-O inspects, Ctrl-S stops, Ctrl-F shows the diff. Each action returns to your
shell. Bare `shoal repo` opens the same kind of list over registered repositories
(without a terminal it prints `repo` help): Enter adds a workspace, Ctrl-A
registers a path or URL, Ctrl-R renames, Ctrl-O shows saved config, Ctrl-D deletes.
`repo rename` asks for an omitted name.

Commands acting on one workspace use an explicit target, otherwise the caller's
scoped workspace or the workspace containing the current directory, then an
interactive fzf picker; a deleted current directory provides no workspace context.
Invalid explicit targets fail without fallback; without a
current workspace, noninteractive and JSON calls require a target. Scoped callers
remain confined to their own workspace. Another omitted required argument that
names an existing record opens a picker of those records, under the same
noninteractive rule. Bare `shoal cd` always opens a picker;
all-workspace listings and `--all` retain their scope. A command that accepts
`--all` lists it as the last picker entry, "All workspaces", and as the first
completion of its workspace argument. `config show` checks the
registered checkout before the picker, as described below.
`shoal add` offers repositories in most-recently-used order, then
a new-branch prompt or a picker of existing branches, open issues or open PRs; a
chosen issue or PR opens as its link would. Noninteractive and JSON calls never prompt;
management commands support JSON output, while executed commands keep their
own stdin, stdout, stderr, and exit code.

`shoal status [target]` summarizes one workspace's current state, changes
since its fork point, what it holds, its agent sessions, whether its linked issue is open or closed,
and each watched PR as the forge reports it now: open, merged or closed, merge
conflicts, each CI check's result, and a review state from each reviewer's latest
approval or change request. A failed lookup reports its error on that item
without failing the command. When the
daemon's automatic cleanup is failing or has not finished a pass for 10 minutes,
status says so in `cleanup_error` and `doctor` reports the error. Status also
names the first condition that keeps the workspace from automatic cleanup, holds
and assignment state before commands, leases and Git state, or the idle delay
when nothing does, in `cleanup_reason`. `--json`
returns the same data, with PRs in `prs`, the issue in `issue_status` and sessions
in `sessions`, or a failed `zmx list` in `sessions_error`; `inspect` keeps the raw workspace and
execution records. Workspace records from `ls --json` and `inspect --json` include
`links.issue` and linked PR URLs in `links.prs`; an acknowledgement without a PR
URL leaves `links.prs` empty. An issue or PR URL, `pr` or `issue` with a number or URL, or
`resource` with a pool or member name shows every workspace that links or holds
it instead, as a JSON array; a number must be linked in only one repository.
Scoped callers find only their own workspace.

### Configured commands

Define workspace shortcuts in global or repository TOML:

```toml
[commands]
check = ["cargo", "test"]
```

Run `shoal run check [workspace] -- <extra arguments>`; the shorter `shoal check`
form is equivalent when the name does not collide with a built-in command. Omit
the workspace to use the current one, otherwise the picker, which offers only the
workspaces that define a repository-only command. Unknown
names are rejected with suggestions for similar built-ins. Put
Shoal's global flags before `run` or the shorthand command name. Bare `shoal run`
lists each effective command's argument array, source layer, and whether its bare
name is a built-in; outside a managed workspace it lists only global commands and
built-in defaults.
Each name resolves from saved repository config, worktree config, then global
config and built-in defaults; a higher layer replaces the whole argument array.
The executable is resolved through PATH (or use a path),
with the workspace as working directory. Arguments are passed literally without
shell expansion. Execution preserves terminal I/O, scope, reserved-port variables,
and exit status just like `exec`.

`{workspace}`, `{branch}`, `{path}`, and `{diff_base}` expand once inside
configured arguments. `{diff_base}` resolves lazily through the same daemon
fork-point/merge-base lookup as `shoal diff`; failure prevents launch.
A standalone `{args}` inserts the forwarded arguments there; otherwise they are
appended. Forwarded arguments are never expanded. The CLI agent defaults are:

```toml
[commands]
claude = ["claude"]
codex = ["codex", "{args}", "--sandbox", "danger-full-access", "--ask-for-approval=never"]
```

Override their `[commands]` entries to change the executable or flags; prompt
templates, trust setup, and agent exit notifications still apply.
Claude and Codex shortcuts, including Happy, trust the repository's shared Shoal
workspaces directory before launch, creating the agent's user config if needed and
preserving other settings. An explicitly placed workspace outside that directory
gets its own trust entry.
Claude uses `~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`); Codex CLI
and app use `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`). A trust update
failure warns and still launches the agent. Removing a repository removes its
trust entries from these files while preserving other project settings.

`shoal edit [workspace] -- <args>` runs the `edit` command, such as
`edit = ["zed", "{path}"]`. Without one it runs `$VISUAL`, otherwise `$EDITOR`, split on whitespace,
followed by the workspace path and the forwarded arguments.

For local review with [tuicr](https://github.com/agavra/tuicr), install it on PATH
and configure:

```toml
[commands]
review = ["tuicr", "-r", "{diff_base}..HEAD"]
review-worktree = ["tuicr", "-w"]
```

`shoal review [workspace]` asks whether to review manually or with an agent;
`--manual` or `--agent <name>` skips the question, which is required without a
terminal. Manual review runs the `review` command (`shoal run review` also does);
without one, the agent reviews. Choosing “No agent” returns without starting a reviewer.
The agent is otherwise chosen as for `shoal add` and is
prompted to report findings, not to change files, commit, push, or post.
`shoal review <pr-url>` or `shoal review --pr <number-or-url>` looks up the PR
with your `gh`/`fj` login and reviews it the same way in the workspace that owns
its head branch, fast-forwarded to the pushed head and refused when diverged, or
opens one from origin whose base is the PR's refreshed `origin/<base>`. Either
workspace links the PR as `shoal add` does; fork PRs are refused. `--repo` selects the PR's repository,
which otherwise follows `shoal add`.
`shoal review <issue-url>` or `shoal review --issue <number-or-url>` has an agent
refine the issue before implementation: it checks the issue against the code and
reports what is unclear, missing or already done, open questions and a suggested
approach. It runs in the workspace associated with the issue or on its derived
branch, opening the one `shoal add --issue` continues when none exists; `--manual`
is refused.
An agent reviewing a PR or issue posts its findings there as one comment, without
approving, requesting changes or editing the issue, unless `[review] post` is
false; `--post` or `--no-post` overrides that for one run and selects the agent.

```toml
[review]
post = true # Default: true
```
`shoal review-worktree [workspace]` reviews uncommitted changes. The committed
range excludes working-tree changes that `shoal diff` includes. Export feedback
from tuicr and hand it to your agent explicitly; see
[tuicr's export documentation](https://github.com/agavra/tuicr/blob/main/docs/CLI.md#output-for-scripts-and-agents).
Shoal tracks execution; the review tool owns sessions, exports, and forge access.

### Agents

Interactive `shoal add` selects an agent using `--agent` or an agent picker;
choosing “No agent” continues without a launch. Non-interactive additions launch
only when an agent is named. `add --agent <name>` starts the agent after
worktree creation, setup, and
the post-setup hook succeed; arguments after `--` go to the agent. Shoal refuses
an agent whose executable is not on PATH before creating anything, and leaves
it and its shortcut command out of the agent picker, the workspace menu and tab
completion. CLI agents
run through the tracked execution wrapper and, when run in place, return the
agent's exit code, restoring OS terminal settings even after interruption. When
`TERM` is nonempty and not `dumb`, it also resets emulator input modes for the shell;
the workspace is retained even when launch fails. With shell
integration, your shell enters the new workspace after the agent exits.
`--json` emits the workspace record first, then the agent's unmodified output.

On a terminal, CLI agent launches (`shoal claude`, `shoal codex`, other AI
tools, `add --agent` and `resume`) run in a [zmx](https://github.com/neurosnap/zmx)
session, so the agent keeps running when you detach with Ctrl-\ or close the
terminal. zmx passes the agent's output to your terminal unchanged, so images and
other terminal protocols keep working, and restores the screen when you attach
again. The tracked wrapper runs inside the session, so stop, resume, scope and
cleanup treat the agent as before. When the session returns control, Shoal resets
emulator input modes on the caller's terminal. zmx reports no exit status: these launches
exit 0 once the agent exits or you detach. A launch without a terminal, inside a
zmx session (`ZMX_SESSION` is set), or in a Herdr pane with `herdr.enabled` runs
in place. On a terminal outside zmx, a missing zmx refuses the agent as a missing
agent executable does.

Interactive `shoal add` on a terminal outside Herdr and zmx places its launch
as a Herdr tab would be: an agent that starts with input runs setup and the
agent in a detached zmx session and returns with a `shoal attach` hint, and any
other launch runs here and attaches its agent's session. The top-level `focus`
option overrides the choice, as it does for tabs, and `--here` keeps the launch
in this terminal. A detached session whose start fails keeps its shell open to
show why, and one without an agent opens the workspace shell there, as a tab
does.

`shoal attach [workspace]` reattaches to the workspace's session, with a picker
or `--session <name>` when it has several; `shoal status` lists them. Sessions
are named after the workspace, suffixed `-2`, `-3`, ... when the name is in use,
and labelled `shoal.workspace=<id>`, so `zmx ls` and `zmx attach` work as well.

Inside Herdr (`HERDR_ENV=1`), interactive `shoal add` looks up
the issue and resolves repository, branch, and agent choices in the caller's pane,
then opens a tab in `HERDR_WORKSPACE_ID` and returns once the command is submitted
there. Setup and agent execution run in that tab, which reads the issue again and
is labeled `<repo>#<number>` for an issue (such as `shoal#375`) and with the
allocated workspace branch otherwise, unless `tab_name` supplies a template.
`--here` keeps the command in the current pane; JSON, help, and noninteractive
calls run in place.

The `[herdr]` table in global or repository config uses normal per-option
precedence. New tabs stay in the background by default when launching an agent
with an issue prompt or forwarded arguments, and focus otherwise, so a workspace
shell or an agent waiting for its first prompt is ready to type into; the
top-level `focus` option overrides this for tabs and zmx sessions alike, and
`shoal add --focus` focuses one launch.
`enabled`, `new_tab` and `close_when_done` default to true; with `enabled = false`
Shoal treats a Herdr pane as a plain terminal:

```toml
[herdr]
enabled = true          # Use Herdr tabs and panes
new_tab = true          # Enable the handoff inside Herdr
# tab_name = "{repo}: {branch}" # Customize the tab name
close_when_done = true  # Close after the workspace is completed or removed
```

`tab_name` supports `{repo}`, `{branch}`, `{issue_number}`, and `{issue_title}`,
or a literal name. Issue fields are empty without an issue; unknown placeholders
stay literal and inserted values are never expanded again. `{branch}` uses the
requested branch when the tab opens and the allocated branch after creation.

Failures before agent execution leave the tab open. Agent exits leave the tab
open so work can continue; completion closes it once tracked executions resolve
or Herdr reports that the agent finished its turn, unless stopped work waits for
`shoal resume`, and workspace removal closes it immediately. `add`
without an agent (or choosing “No agent”) opens an interactive shell in the ready
workspace and leaves the tab open until that workspace is completed or removed.

Besides Claude and Codex, Shoal includes [opencode](https://opencode.ai)
(`opencode --prompt {prompt} {args}`), [pi](https://github.com/badlogic/pi-mono)
(`pi`) and [Grok Build](https://docs.x.ai/build/overview) (`grok`); pi and Grok
take the prompt as their first argument. Each runs as `shoal <name>` once its
executable is on PATH. Add another AI tool in global config to run it as an agent:

```toml
default_agent = "droid"

[ai.droid]
command = ["droid", "{args}"]
skill_dir = "~/.factory/skills" # Optional; see agent skills below
```

`shoal droid [workspace] -- <args>` (or `shoal run droid`) then starts it in a
workspace the way `shoal claude` does, and `--agent droid` selects it. `command`
is its launcher at the global layer, so a repository `[commands]` entry with the
same name replaces it; defining both in the global file is an error. Names that
collide with built-in commands are rejected. The picker and completion offer AI
tools only.

For AI tool launches, `{prompt}` combines the rendered agent guidance and
issue prompt, separated by a blank line. Without that placeholder, nonempty context
becomes the first forwarded argument. Substitutions happen once; user arguments
remain literal. Claude and Codex keep their specialized launchers; other tools
receive no tool-specific flags or trust setup. `--agent` and `default_agent` also
accept any `[commands]` name; `shoal run <name>` for a name that is not an AI tool
remains a plain command invocation without agent prompts or exit notifications,
expanding `{prompt}` to an empty string.

`shoal codex` without `--cli`/`--app` uses `codex.default_mode` from the workspace's
repository config or `~/.config/shoal/config.toml` (or
`$XDG_CONFIG_HOME/shoal/config.toml`), read at launch without a daemon restart:

```toml
[codex]
default_mode = "cli" # Or "app"
```

App launches run `codex app <path>` or `t3 app <path>` with any `--` arguments
and preserve the launcher's output and exit code. They add no agent flags and
provide no execution tracking, scope token, or port variables (so automatic
cleanup cannot see their activity); T3's app must already be running.

`shoal happy claude|codex [workspace]` and `add --agent happy-claude|happy-codex`
start a [Happy](https://github.com/slopus/happy) session as Happy's own daemon
would (`happy <agent> --happy-starting-mode remote --started-by daemon`, then
your `--` arguments), so it registers with that daemon and appears in the Happy
app. The session is detached from your terminal (stdin from `/dev/null`, output
appended to `<state>/workspaces/<id>/happy-<agent>-<time>.log`, deleted with the
workspace) but runs through the tracked wrapper in a background `shoal` process:
it gets the scope token and port variables, counts as activity for idle cleanup,
and `stop`, `rm`, PR cleanup, and reconciliation treat it like any other command.
Shoal returns once the daemon records the launch, printing the execution, PID,
and log (`--json` adds `prompt_file`, `prompt_delivered`, `happy_session_id` and
`happy_daemon_recorded`). Without `~/.happy/daemon.state.json` (`$HAPPY_HOME_DIR`
overrides `~/.happy`) Shoal warns that the session will not appear in the app
until `happy daemon start` runs, and launches anyway.

An `--issue` prompt, or `shoal happy … --prompt <text>`, reaches Claude as an
argument. `happy codex` takes none, so Shoal delivers it the way the app does:
with the login in `~/.happy/access.key` it creates the session on Happy's server
(`$HAPPY_SERVER_URL`, the settings file, or Happy's default), encrypted as
happy-cli would encrypt it, starts `happy codex` attached to that session through
Happy's own `HAPPY_RECONNECT_*` variables, waits up to 90 seconds for the session
to report alive, and posts the prompt (`curl`, token on stdin). The prompt is also
saved beside the log; when Happy is not logged in or delivery fails, Shoal warns
and leaves it there for you to send from the app.

### Chats

`shoal chat [repository] [--agent NAME] [--here] [-- ARGS]` starts a new chat
with Claude Code or Codex CLI about a repository, to discuss it, its issues and
plans, or to coordinate work, without changing code. The repository is the named
one, else the current checkout's or workspace's, else a picker. The agent is
`--agent`, which must be `claude` or `codex`, else `default_agent` when it is
one of them, else a picker of the installed ones. The agent's configured launcher
runs in the registered checkout with the rendered
[chat template](#prompt-templates) as native instructions, so it waits for your
first message. The template substitutes `{repo}` and `{path}`; in the launcher,
`{path}` is the checkout and other workspace placeholders stay literal. The checkout is trusted in the agent's
config as workspace launches trust theirs.

Every call starts a new chat and focuses it. Inside Herdr it opens a focused tab
labelled `<repo> chat` regardless of `herdr.focus`, unless `--here` or
`new_tab = false` keeps it in the current pane; on a terminal it runs in a zmx
session labelled `shoal.chat=<repository id>`, which `zmx attach` returns to;
inside a zmx session or a Herdr pane it runs in place. Chats run with your
authority, not a workspace scope, and workspace processes cannot start one. The
daemon does not track them: they are not listed, queued, stopped under pressure
or resumed.

### Agent queue

```sh
shoal config set max_agents dynamic           # Start agents while measured load allows
shoal config set max_agents 3                 # Fixed machine-wide limit
shoal config set max_agents 2 --repo shoal    # And at most two of them in this repository
shoal config set max_agents none --repo docs  # No repository limit
shoal add <issue-url> --agent claude          # Starts now, or queues when no slot is free
shoal add <issue-url> --agent claude --now    # Starts now regardless of the limits
shoal ls --queue                              # Waiting and starting entries
shoal rm issue-612-add-a-queue-cmd            # Removes a waiting entry
```

`max_agents` limits the agents that run at once. It defaults to `dynamic`,
which starts one agent at a time, at least 10 seconds apart, while measured load
leaves room below the [`[overload.warning]`](#overload-protection) CPU, memory
and disk thresholds, which [`load`](#overload-protection) sets. A new agent
mostly reads files at first and compiles later, so for its first three minutes
it counts at the load a settled agent shows: average CPU use over the last five
minutes divided by the agents that have run longer, at least half a core each,
or two cores each before any has. A start must fit the last minute's CPU use
plus every such warming agent. With no agent running, one starts whatever the
load. Critical memory, disk below `overload.disk.stop_free_gib`, or a failed
memory or disk reading starts none; a missing CPU reading starts only that first
agent. Disabled memory or disk protection ignores that signal. Set a positive
number for a fixed limit or `none` for unlimited admission. While a fixed
limit applies, so does the disk rule: below `[overload.warning] disk_free_gib`
an agent starts only when none uses a slot, and below
`overload.disk.stop_free_gib` or without a reading none starts. The global value
counts every agent on the machine; a repository's fixed value (saved config or
`.shoal.toml` in its registered checkout) counts only that repository's agents,
within the global limit, while `dynamic` there adds no repository limit. Each tracked agent uses a slot while it runs, awaits reattachment
after a daemon restart, or waits for `shoal resume`, unless its workspace is
done; `shoal done`, the agent's exit, `resume --discard` and removal free it.

`shoal add` with an agent asks the daemon for a slot after resolving the issue,
branch and agent. When none is free it prints the entry's place and returns
(`--json` emits the entry); the workspace is created only when the entry
starts, from the then-current default branch. The daemon starts waiting entries
in order as slots free, passing over an entry whose repository is full, and
records a `queue_started` notification. An entry queued from Herdr opens a
background tab in the same Herdr workspace; otherwise it runs in a detached zmx
session labelled for `shoal attach`. A start keeps its slot through setup until
its agent registers; a start that fails, or whose launch ends without opening the
workspace for two minutes, records a `queue_failed` notification with the reason
and frees the slot. `--now` starts at once and still takes a slot.

`shoal ls` lists entries after the workspaces as `queued, position N` or
`starting`; `ls --queue` lists only them (`--json` gives the entries, while `ls
--json` stays a list of workspaces). `shoal rm` removes a waiting entry by name
or ID, and only by ID when a workspace has the same name; a starting one belongs
to its workspace. Scoped callers see no entries.

### Agent accounts

`[agent_auth]` selects user-owned executable wrappers for `fj`, `gh`, `glab` and
`tea`, each an absolute or `~/` path, `fj_home`, the HOME the `fj` wrapper runs `fj` with,
and a `git_profile` naming one of the global [Git profiles](#git-profiles), all
defaulting to unset. Values follow repository/global
precedence and appear in `config show`. Tracked agent shortcuts prepend a private
directory containing these tool names to the child's PATH; nested commands inherit
it. Missing or non-executable wrappers fail before the agent starts. The directory
lasts for that execution and is removed on exit; wrappers and credentials remain
user-owned. Issue lookup before launch uses the invoking CLI's login.

Wrappers receive arguments unchanged and must invoke the real tool by absolute
path to avoid recursion. They own credential selection, including overriding
inherited token variables. PATH selection is cooperative: an absolute tool path
or a shell that resets PATH bypasses it.

Issue and PR changes Shoal makes for a workspace use the agent account. On
GitHub they run `gh api` through the `gh` wrapper, and on GitLab `glab api`
through the `glab` wrapper. On Forgejo and Gitea they use
the token the agent's `fj` saved under `fj_home`, read for each request and passed
to curl on stdin; with an `fj` wrapper but no `fj_home` they fail rather than use
your login. For a host the agent's fj has no login for, a `tea` wrapper sends
them through `tea api` with its login for the host. Options left unset use your own logins, as the agent would.

The agent Git profile reaches tracked agents through Git's `GIT_CONFIG_COUNT`
variables, after any inherited entries, so it overrides every Git config file for
the agent and its nested commands in any repository; the worktree's config and
ordinary executions keep their settings. When the profile sets `user.name` or
`user.email`, inherited `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables for that
field are dropped. Push credentials follow profile settings such as
`core.sshCommand`. An undefined profile fails before the agent starts.

For `fj` 0.6, a separate home selects separate credentials on macOS; Linux also
needs a separate XDG data directory. Save this as `~/bin/fj-agent`, substitute
the installed `fj` path, and make it executable:

```sh
#!/bin/sh
agent_home="$HOME/.local/share/shoal-auth/fj"
exec env HOME="$agent_home" XDG_DATA_HOME="$agent_home/.local/share" \
  XDG_CONFIG_HOME="$agent_home/.config" /opt/homebrew/bin/fj "$@"
```

Run `~/bin/fj-agent auth login` (or `auth add-token`) yourself to authenticate
as the agent account, and set `fj_home = "~/.local/share/shoal-auth/fj"` so
Shoal finds that login. Only the `fj` process receives the separate home.

For `gh`, changing `GH_CONFIG_DIR` alone can still reach your system keyring.
A wrapper can instead obtain an agent token from your credential manager and
export `GH_TOKEN` (or `GH_ENTERPRISE_TOKEN` for an enterprise host), failing
if retrieval returns an empty token. For example, with a GitHub.com agent token
stored in a private file, save this as executable `~/bin/gh-agent`, adapting the
real `gh` path:

```sh
#!/bin/sh
unset GITHUB_TOKEN GH_ENTERPRISE_TOKEN GITHUB_ENTERPRISE_TOKEN
GH_TOKEN=$(cat "$HOME/.config/shoal-auth/github.token") || exit 1
: "${GH_TOKEN:?agent token is empty}"
export GH_TOKEN
exec /opt/homebrew/bin/gh "$@"
```

Keep tokens outside repository and Shoal TOML. This example selects credentials
for GitHub.com; configure the corresponding token for each enterprise host you use.

### Agent secrets

`[secrets]` gives tracked agents values that commands fetch from your secret
store. Each entry is named after the variable that carries the value, and its
`command` is an argument array whose standard output is the value:

```toml
[secrets.OPENAI_API_KEY]
command = ["security", "find-generic-password", "-s", "shoal-agent-openai", "-w"]

[secrets.AGENT_BROWSER_ENCRYPTION_KEY]
command = ["op", "read", "op://Agents/agent-browser/key"]
tools = ["agent-browser"]
```

Set it in the global config or the saved repository config; a worktree file
that sets it is rejected, because secret sources belong to the machine. An
entry replaces the same-named entry below it whole. Shoal stores the commands,
never the values.

Commands run without input, and one trailing line break is dropped. Without
`tools`, agent launches run the command before the agent starts and add the
value to the agent's environment, which nested commands inherit; a command
that fails, prints nothing or prints NUL stops the launch. With `tools`, the
agent's environment never holds the value: the launch puts a launcher for
each listed program first on a private PATH, and each run of the program
through it fetches the value and runs the next program of that name on PATH
with the value set, or fails with the command's error. Like the agent account
wrappers, launchers are cooperative: an absolute program path or a reset PATH
bypasses them. Other executions fetch no secrets of their own, and hooks started
beneath an agent drop the inherited values and launchers.

A tool-scoped secret can set `requires_approval = true` and `approval_lifetime`
like a [protected resource](#resource-approvals); an environment secret cannot,
because it is fetched before the agent can ask. The agent requests access with
`shoal acquire secret NAME --reason ...`, and the user decides with `shoal
approvals`. Each run of a listed tool in a workspace asks the daemon about
every secret it serves, under the current policy, and fetches a gated value
only once the workspace holds a grant for the secret's current `command` and
`tools`; otherwise the run fails, naming the pending or denied request, or the
`acquire` command when there is none; a run that cleared `SHOAL_SCOPE_TOKEN`
fails too. A launcher serves the config of its agent launch, so once a
secret's `command` or `tools` change, its runs fail until the agent restarts. `shoal release secret NAME` cancels the request or
ends a lease grant.

### Prompt templates

`issue_template`, `agent_template` and `chat_template` in the saved repository
TOML win over the worktree's TOML values or `issue-template.md`,
`agent-template.md` and `chat-template.md` in the repository root.
The selected repository template is appended after the global TOML value or
corresponding file beside `config.toml`, with a blank line between nonempty
templates. Inline TOML wins over Markdown at each level. An empty repository
value suppresses local additions while preserving global guidance; an empty
global value suppresses its base text. Without a global issue or chat template,
Shoal uses its bundled default as the base. `config show` reports the combined text and the highest
layer that configured it. Unknown placeholders stay literal. Templates are
read at launch without a daemon restart.

Every agent launch receives Shoal's [agent guidance](../src/config/templates/agent.md),
followed by the agent template, which holds only your additions and cannot
replace or suppress the guidance. Both may use `{workspace}`, `{branch}` and
`{path}` from the target worktree, and `{updates}` saying how the launch
receives issue and PR activity: delivered by its hooks for Claude Code and for
Codex in a zmx session, otherwise by running `shoal watch pr`. Claude receives `--append-system-prompt`,
Codex CLI receives a `developer_instructions` config override, and Happy Codex
receives the instructions before its first prompt through Happy's delivery path
(or as the first message when no prompt was supplied). Desktop handoffs do not
consume templates. User prompt arguments are preserved.

### Branch and workspace names
`shoal add <repository> [branch]` creates a literal Git branch;
`--existing <branch|remote/branch>` uses an existing one (incompatible with the
branch argument and `--issue`); its `--base` is the ref `diff` and `{diff_base}`
compare against instead of the default branch, for new worktrees only. The picker
queries remotes for current branches. Local branches take precedence and remain
unchanged; remote selections fetch and create tracking branches, or fast-forward
a matching local tracking branch without discarding ahead commits. Ambiguous
remotes or unrelated local branches fail. A ready Shoal workspace reopens without
setup/hooks or refresh; other checkouts block creation, including checked-out `main`.
Full `refs/heads/...` and `refs/remotes/...` selectors disambiguate names.
The workspace name replaces non-ASCII-alphanumeric/`-_` characters with `-`, drops
leading `-_`, truncates to 64 characters, and falls back to `workspace`. Globally
colliding names fail. Commands use this name or ID. Directories default to the repository's
Shoal workspace directory;
`add --path <dir>` selects an exact new directory for this workspace only (relative
to the current directory; `~/` allowed). Existing paths and overlaps with Shoal
state, checkouts, workspaces, or another repository's reserved directory are refused.
Reopening accepts its current path but cannot relocate it. Custom locations have
the same setup, ownership, and cleanup rules as default locations.

For new branches, names taken by a local branch, known remote branch, branch
namespace, or retained Shoal record get `-2`, `-3`, etc. on the leaf; when an
ancestor blocks it, that component is suffixed (`feature` makes `feature/topic`
into `feature-2/topic`). Names interpreted specially by the Worktrunk adapter
(`HEAD`, `@`, or entirely hexadecimal names of exactly 40 or 64 characters) are
reserved across Git object formats: creation suffixes them, while opening an
existing branch reports incompatibility. Nested components remain literal.
Suffixes never change the derived workspace name.

### Adopting a worktree
`shoal adopt <path>` registers an existing linked worktree in place
and enters it with shell integration. The path is relative to the current directory
(`~/` allowed) and must name its root. `--repo` selects the registered repository;
otherwise it is the one whose checkout owns the worktree, else the only one sharing
its `origin` remote, else one chosen interactively. Its local branch derives the workspace name;
name or ownership conflicts fail. Main checkouts, detached or locked worktrees,
and paths overlapping protected locations are refused. Repeating adoption reopens
a verified ready workspace; it cannot repair a moved or replaced managed worktree.

Adoption preserves commits, dirty files, and Git settings without fetching, setup,
or post-setup hooks. It takes full Shoal ownership with normal cleanup, including
the pre-remove hook. Disable automatic cleanup
in repository configuration before adopting work that should stay indefinitely.
Use `shoal setup` explicitly when setup is wanted.

Pass `--copy` to create a new branch worktree under the repository's Shoal
directory, copy the source worktree's files into it, and leave the source in
place. The copied worktree is adopted without setup and receives the next
available branch suffix when its source branch is already checked out.

### Base branch
New branches start from the repository's default branch: `origin/HEAD`, or the
sole remote's HEAD without `origin` (several remotes without `origin` are
ambiguous). A missing symbolic remote HEAD is discovered with `ls-remote` and
cached; update it with `git remote set-head origin --auto`. Without remotes, the
registered checkout's current branch is used; a detached checkout needs `--base`.

Before branching, Shoal fetches that local branch's upstream and fast-forwards
it, even when the registered checkout is on another branch. A missing branch or
upstream, failed fetch, divergence, or a dirty or managed default-branch
checkout stops creation; an already-ahead branch is preserved. `--base REF`
starts from any locally resolvable commit without refreshing,
unless it names the local default branch; the resolved base is recorded for
`shoal diff`.
Existing-branch workspaces diff against the local default, or their opening
commit if on it or unavailable.

### Stacked workspaces

A stacked change is a chain of workspaces, each on a branch built on the one
below. A `--base` naming another workspace's branch, locally or on a remote,
records that workspace as the new workspace's base workspace.

```sh
shoal base                          # Show the current workspace's base workspace
shoal base set feature-a            # Record it by workspace name or branch
shoal base clear                    # Remove it
shoal base set feature-a --workspace feature-b
```

The base must be another workspace of the same repository and must not create
a cycle. Scoped callers may set their own workspace's base. Removing a base
workspace moves the workspaces stacked on it to its own base workspace, or to
none. `status` shows both directions; `ls --json` and `inspect` give
`base_workspace` (`id`, `name`, `branch`, or null) and `stacked_workspaces`.

Once every watched PR of a base workspace has merged, with its HEAD among the
merged commits, the daemon moves the
workspaces stacked on it to its own base workspace and retargets their linked
PRs that still target its branch to the branch it merged into. Their next
`watch` returns a `base_merged` update with the rebase command; the agent
rebases and force-pushes its branch. The PR is retargeted through the forge's
API with the user's login: `gh api` on GitHub, and on Forgejo or Gitea curl with
the token fj saved for that host, read for the request and passed on stdin, or
`tea api` with tea's login for the host. A missing login or failed request is
reported in the update.

### Repositories

Register a local checkout in place (no remote required) or a clone URL. Each
repository gets `~/shoal/<name>/`, named by `--name` or the source basename without
`.git`, suffixed `-2`, `-3` on conflict with files or recorded paths; its workspaces
default to it and a URL clone lives there as `.checkout`. A local checkout
already at `~/shoal/<name>/<anything>` keeps that directory. `root_dir =
"~/Projects"` in the global config (absolute or `~/` path outside Shoal's state
directory and every checkout; daemon reload required) changes the parent for new
registrations; symlinks and `..` are resolved before checking and creating the root,
even across missing components. Existing registrations keep their paths, as do
workspaces created before this layout. `repo add <url> --path <dir>` clones one repository to an exact new
directory (relative to the current directory; `~/` allowed). Re-registering a URL
with its existing path is fine; a different path is rejected.

Registration is idempotent by normalized `origin` URL (HTTPS/SSH forms and
`.git` suffixes match) or canonical local path, and never fetches. Clone URLs
lose trailing slashes so worktrees get a remote forge CLIs recognize. Names from
`--name` or `repo rename` are unique and work as selectors alongside IDs, paths,
and source URLs; inferred names work when unambiguous. IDs and sources take
precedence over paths. A name still resolves when
an unrelated path has the same spelling in the caller's directory; if that path
is another registered checkout, the selector is ambiguous and lists both matches.

`repo setup [repository] [--agent NAME] [-- ARGS]` starts an agent that writes
the repository's `.shoal.toml`: setup, named ports, commands and resource pools,
checked with `config show` and `setup` and then committed. It runs in the
repository's `shoal-setup` workspace, opening one on that branch when none
exists. The agent is `--agent`, else `default_agent`, else one chosen from the
agent picker; arguments after `--` go to the agent.

### Git profiles

Define named Git settings in the global config, then reload the daemon:

```toml
[git.profiles.work]
user.name = "Your Name"
user.email = "you@company.example"
user.signingKey = "your-key"
commit.gpgsign = true
```

Set `git_profile = "work"` at the top level of repository or global config.
Override it for one creation with `shoal add <repo> --git-profile work`.
Selection follows the flag, saved repository config, the new worktree's config file,
then global config. With no selection, Git settings stay unchanged. Profile
values may be strings, booleans or integers; omitted keys retain Git's normal
inheritance. An unknown flag value is rejected before creation; an unknown
configured profile leaves a failed workspace for inspection or removal.

Settings go into Git's per-worktree config before setup runs; reopening or
preparing a workspace does not reapply them, and an explicit profile flag on
reopen is rejected. Shoal enables the shared
`extensions.worktreeConfig` setting when necessary, refusing repositories with
shared `core.worktree` or `core.bare = true` until those settings are migrated
to the main checkout's `config.worktree`. Profiles cannot set repository layout
or extensions. Other worktrees retain their settings.

### Inspect effective configuration

Run `shoal config show [workspace]` to print every effective repository setting
and the layer that supplied it. With no workspace, Shoal uses the current
workspace, the registered checkout when no workspace exists yet, then the
workspace picker. Named tables
such as commands and ports show one entry per name. `--json` returns `key`, `value`,
and `layer` for each entry.

`shoal config set KEY VALUE [KEY VALUE...]` edits the global file without a daemon,
creating it if absent; `shoal config unset KEY...` removes existing keys or tables
to restore defaults. Keys use TOML dotted syntax (quote a component containing dots); values
use TOML syntax, falling back to a string when they are not TOML values. Quote
arrays for the shell, for example `shoal config set commands.check '["cargo", "test"]'`.
Edits preserve unrelated settings and comments and keep the previous file as
`config.toml.backup`. One command's changes apply in order and are validated once,
so settings that depend on each other change together:

```sh
shoal config set simulators.profiles.phone.device '"iPhone 17"' \
  simulators.profiles.phone.runtime '"iOS 27"' simulators.default phone
```

An invalid result leaves both files unchanged.

### Store repository config outside Git

```sh
shoal repo config my-project --file ~/project-shoal.toml
shoal repo config my-project          # Print the saved TOML
shoal repo config my-project --clear  # Return to worktree config
```

The file uses the `.shoal.toml` format and is validated and copied into Shoal's
database; reimport it after edits, or use `shoal config set KEY VALUE... --repo NAME`
and `shoal config unset KEY... --repo NAME` to edit individual saved keys with the
same syntax and validation as global edits. Repository edits are serialized in
the daemon and leave worktree files untouched. Unsetting removes only the saved
value, allowing lower layers to supply it. Repository options resolve per option, for
every workspace of that repository: a value in the saved config wins, one it
omits comes from the worktree's `.shoal.toml` or `.shoal/config.toml` (both
together is an error), and a named `[ports.<name>]`, `[resources.<name>]` or
`[resource_pools.<name>]` table replaces the one below it as a unit, so a
saved `[ports.web]` supplies every field of `web`. Changes apply on the next
request without a restart. The saved config survives restarts, renames, and
workspace removal; `repo rm` deletes it. `--json` returns `repository_id` and
`toml`. Scoped workspace commands cannot administer it.

### Delete a repository

```sh
shoal repo rm my-project -y
```

Permanently deletes the checkout (including in-place local repositories with
uncommitted or unpushed work), all its workspaces, their ports, simulators, and
leases, and the saved config, stopping managed commands first. Shoal attempts to
delete workspace branches; a branch whose recorded worktree directory is already
missing is retained because its contents cannot be inspected.
Interactive calls identify the resolved repository, source, and checkout path before asking
`Are you sure? [y/N]`; `-y`/`--yes` skips the prompt and is required for scripts
and `--json`. `repo remove` is an alias.
Linked worktrees outside Shoal must be removed first; prunable stale records do
not block, locked worktrees do. Shoal refuses redirected paths and deletions
that would include another registered repository or its own state. If cleanup
fails, completed steps stay done, remaining records are retained, and new
workspace creation is blocked until the same command is retried.

### Merge into your workspace branch

```sh
shoal merge main                           # Any local branch: fast-forwarded from upstream first
shoal merge feature/api                    # Local, or discover a remote-only branch
shoal merge feature/api --local            # Merge the local branch as it is, no refresh
shoal merge feature/api --remote origin    # Fetch explicitly, even if local exists
shoal merge origin/feature/api fix-login   # Qualified source, named destination
```

The destination must be the workspace's recorded branch. Local branches take
precedence. Unless `--local`, a local branch with an upstream is fetched and
fast-forwarded first; a dirty checkout, divergence, or failed fetch blocks the
merge, while an ahead branch is preserved. A local branch without an upstream,
or checked out in a managed workspace, is merged as it is. Otherwise Shoal
queries configured remotes and fetches the branch only when the local ref is
absent; a failed ref or commit lookup stops the merge.
Several matches or an unreachable remote require `--remote`. Qualified remote
sources and full `refs/…` names always fetch fresh data. Git fast-forwards or
creates a merge commit; conflicts stay in the worktree for `git commit` or `git
merge --abort`, and `--json` reports `success`, `exit_code`, commits, and Git
output. Nothing is stashed, reset, or pushed.

### Land into the default branch

`shoal land [workspace]` merges the workspace's recorded branch into the
repository default branch for repositories without a pull-request flow. When the
default branch has an upstream, Shoal fetches and fast-forwards it first,
preserving an ahead branch and refusing divergence or a failed fetch. The default
branch cannot be held by a managed workspace, and any other checkout of it must be
clean. The workspace must be clean and on its recorded branch. Git fast-forwards or
creates a merge commit in the default checkout. A merge that does not apply cleanly
is aborted: run `shoal merge <default>` in the workspace, resolve there, and land
again. Scoped agents cannot land. Landed commits count as pushed for `rm` and
automatic cleanup.

With `[land] push`, or `--push` for one run, Shoal pushes the default branch to
its upstream after merging; `--no-push` keeps one landing local. A default branch without an
upstream is refused before merging, and a failed push keeps the landed merge.

```toml
[land]
push = false # Default: false
```

### Diff

```sh
shoal diff                 # Current workspace, otherwise fzf
```

Runs native `git diff` (your pager and external diff apply) from the fork point
of the base branch recorded at creation, using its reflog with merge-base as a
fallback, so commits added to the base later are excluded even after a rebase.
Fixed-commit bases use that commit; older workspaces without base metadata use
`main`. Committed, staged, and unstaged tracked changes appear; untracked files
follow Git. A missing or unrelated base is an error.

### Conflicts

```sh
shoal conflicts            # Against the base workspace's branch, otherwise the default branch
shoal conflicts release    # Against another branch or commit
```

Reports the paths a merge of the workspace's committed HEAD into the target would
leave conflicted, without touching the worktree, index or refs; uncommitted
changes are not checked. The default target is the local branch, so run
`shoal sync` first to compare with the remote's latest default branch. Exits 1 on
conflicts; `--json` returns `target`, `target_commit`, `head`, `conflicts` and
`files`. Requires Git 2.38 or newer.

### Workspace setup and hooks

Repository config (`.shoal.toml`, `.shoal/config.toml`, or the imported local
config) can name lifecycle executables. Untracked hooks may also be global
defaults in `~/.config/shoal/config.toml`; a repository value replaces the global
command, with saved repository config taking precedence over the worktree file.
`setup_cmd` is repository-only:

```toml
pre_setup_cmd = "scripts/before.sh"   # Before tracked setup, without a terminal
setup_cmd = "scripts/setup.sh"        # Prepares the worktree; must exit 0
post_setup_cmd = "scripts/attach.sh"  # After the workspace is ready, e.g. open tmux
post_agent_exit_cmd = "scripts/exited.sh" # When a tracked agent exits
post_done_cmd = "scripts/done.sh"     # After assignment completion, before cleanup
post_ready_cmd = "scripts/ready.sh"   # After work is marked ready for review
pre_remove_cmd = "scripts/detach.sh"  # Before the worktree is removed, e.g. close it
post_remove_cmd = "scripts/removed.sh" # After removal, from the repository checkout
```

Each value is one executable path, run directly without shell parsing or PATH
lookup. Hooks that run while the worktree exists resolve relative paths against
its root and use it as their working directory. Give scripts a shebang and put
arguments and shell logic inside them.

`pre_setup_cmd` runs in the daemon before tracked setup, after ownership and
execution checks, with a 60-second limit. It works without a `setup_cmd`.
Failure marks the workspace failed and skips setup;
retry with `shoal setup`. Lifecycle and permit changes are rejected while it runs.

`setup_cmd` runs through the tracked execution wrapper with workspace scope and
your CLI environment. `shoal add` waits for it before entering the worktree or
launching an agent; the workspace stays `preparing` until it exits 0 with no
surviving processes. On failure, interactive mode asks whether to delete the
new workspace and branch, ignore the failure and continue, or keep it for
inspection (default). JSON/noninteractive mode returns nonzero, keeps the
workspace, and sends setup output to stderr. Then choose explicitly:

```sh
shoal setup fix-login                      # Rerun setup and the post-setup hook
shoal doctor fix-login --repair             # Ignore the failure after ownership checks
shoal rm fix-login --yes --delete-branch    # Delete this workspace and branch
```

Hooks are untracked: they run as your own processes with `SHOAL_HOOK` (the key
without `_cmd`), `SHOAL_WORKSPACE`, `SHOAL_WORKSPACE_ID`, `SHOAL_WORKSPACE_PATH`,
and `SHOAL_STATE_DIR`, without a scope token or port variables, so whatever they leave running (a tmux
server, say) is not a Shoal execution. `post_setup_cmd` runs from the CLI with your
terminal after `add` or `setup` has a ready workspace and before any `--agent`; a
nonzero exit keeps the workspace, skips the agent, and fails the command.
`pre_remove_cmd` runs inside the daemon for `rm`, `repo rm`, and automatic cleanup,
after the removal checks pass and managed commands stop, without a terminal and with
a 60-second limit; a nonzero exit or timeout retains the workspace with the hook's
stderr as its error. It is skipped when the worktree directory is already gone.

`post_remove_cmd` runs after successful removal from the shared daemon path,
with a 60-second limit and no terminal. Its path and working directory are relative
to the repository checkout, whose copy of the script must exist; `SHOAL_WORKSPACE_PATH`
still names the removed worktree. The command is selected before removal.
Failure cannot undo deletion: removal still succeeds, with a CLI warning,
JSON `hook_error`, and a `hook_failed`
notification. It is skipped for already-missing worktrees and is not replayed
following a daemon restart. Use a pre-remove hook when failure must retain ownership.

### Seed builds

A repository can keep a build of its default branch that new workspaces start
from, so a branch rebuilds only what it changed and shares the rest on disk:

```toml
[seed]
cmd = "scripts/seed.sh"  # In the worktree; builds the default branch and must exit 0
paths = ["target"]       # Build outputs, relative to the worktree, new workspaces start with
```

The daemon keeps a locked, detached worktree at the local default branch's tip in
the repository directory's `.seed`, and runs `seed.cmd` there when that branch or
the seed's settings change. It checks every minute, one repository at a time, and
waits while memory or free disk space is past its `[overload.warning]` threshold;
a build that failed is retried only after such a change or a daemon restart. The
settings come from the repository's saved config and its registered checkout's
file. The command is an executable inside the seed worktree, given as a relative
path and run like `setup_cmd`. It gets the repository's `[env]`, with placeholders
rendered for the seed worktree, but no inherited `SHOAL_*` variables or agent secrets. It has a
two-hour limit, writes its output to `.seed/build.log`, and anything it leaves
running in its process group is stopped when it exits.

A successful build freezes `paths` as a copy-on-write snapshot: APFS clones on
macOS, reflinks on Linux, never a full copy. A new worktree whose own settings
match the snapshot's clones its paths before setup, keeping any path that already
exists. When it clones at least one, it gives every tracked file its commit left
unchanged the seed's modification time, so tools that compare modification times,
such as Cargo and make, rebuild only changed files; next to outputs the worktree
already had, older times could hide changed sources. Cloned files share disk blocks with the snapshot
until rewritten. A failed build or clone records a `seed_failed` notification, and
the workspace starts without the seed.

Removing the repository or its `[seed]` stops a running build and deletes the
seed worktree, snapshot and log, keeping anything else in `.seed`. Shoal marks
`.seed` and the seed worktree's Git metadata with the repository, and neither
builds in nor deletes a seed whose marks are missing; `doctor` reports such a
`.seed/worktree`. Point outputs a tool keeps
outside the worktree at a path inside it, such as
`xcodebuild -derivedDataPath build/DerivedData`, so workspace removal deletes them
too.

### Remove a workspace

Removal retains the default branch unless `--delete-branch`, and every branch while
the default branch is unknown; other clean branches
matching default/upstream or merged into the default are deleted. Otherwise fzf
offers Cancel (default), Keep branch, or Delete branch, then lists uncommitted
changes and untracked files with Git status codes before `Are you sure? [y/N]`.
Large previews show an omitted-entry count; use `git status` for the full list.
Both choices discard uncommitted and untracked files. `--keep-branch`/`--delete-branch`
skips the picker; add `--yes` to skip confirmation. `--yes` alone cannot choose for dirty/differing work.

Manual removal stops tracked commands and verified survivors, leaving unrelated
processes alone. Ownership and removal policy are checked again after removal hooks;
changed HEAD requires an explicit branch choice. Ignored files go; shared caches
stay; Worktrunk hooks are disabled.
Git protects other checkouts. Failed ancestry or exact-ref checks stop removal
with the Git diagnostic, as they do branch selection and refresh; a missing ref
is a negative answer. Shell integration returns to `<root_dir>/<repo>`.

### Assignment completion

`shoal done [workspace]` records that the assignment is finished and notifies the
user. `[done] cleanup` defaults to true; `--keep` and `--cleanup` override it in
either direction and cannot be combined. The setting follows normal repository
and worktree configuration precedence. Scoped agents may mark only their own
workspace done; completion does not verify or assert a merge.

A scoped `--keep` that overrides `[done] cleanup = true` asks the user: it needs
`--reason`, records completion with `keep_request` naming the request, and cleanup
waits for the decision in `shoal approvals` (see
[resource approvals](#resource-approvals)). Approval keeps the workspace; denial
turns the completion into cleanup. The decision covers the completed HEAD:
repeating `done --keep` there reuses it, a new HEAD needs a new request, and
`done` without `--keep` or `undone` cancels a pending one.

Only `done` completes an assignment by default, so issue closure and merged PRs
leave the workspace in place until the agent finishes. `[done] automatic = true`
also records completion when the associated issue closes or every watched PR
merges, using the same configuration precedence.

```toml
[done]
cleanup = true    # Default: true
automatic = false # Default: false
```

`shoal undone [workspace]` withdraws a recorded completion; `--json` returns
`{"withdrawn": <completion>}`, with null when none was recorded. Scoped agents may mark only their own workspace undone.
Issue associations and PR watches remain registered. It does not keep the workspace:
cleanup proceeds as if `done` had not run, and automatic completion can record it
again. A [hold](#workspace-holds) keeps a workspace that has more work.

`post_done_cmd` runs in the daemon after recording completion, before cleanup,
with the worktree as its working directory, no terminal, and a 60-second limit.
It follows normal global and repository configuration precedence and receives the
[hook identity environment](#workspace-setup-and-hooks) plus
`SHOAL_DONE_CHOICE=keep` or `cleanup`. It runs for explicit and automatic
completion even when cleanup retains the workspace; for a keep request it runs
when the user decides, with the decided choice. Failure records
a `hook_failed` notification without undoing completion or blocking cleanup.
Lifecycle and permit changes are rejected while it runs. The hook holds the
completion/PR gate, so it must not call Shoal commands that change completion or
PR registrations, including in other workspaces. Each explicit `done` runs it
again; automatic completion and daemon restart do not replay it.

`post_agent_exit_cmd` runs alongside agent-exit notifications, including configured
agents and disconnects, while the workspace is ready, after acknowledging the
wrapper's exit. It uses the same daemon hook
rules and config precedence as `post_done_cmd`, with `SHOAL_AGENT`,
`SHOAL_AGENT_EXIT_CODE` (empty on disconnect), and `SHOAL_AGENT_EXIT_COMPLETE`
(`true` when no owned processes remain, otherwise `false`). Failure only records
`hook_failed`; agent exit does not mark an assignment done. Removal uses its own
hooks, and plain commands and restart do not produce agent-exit hooks. An exit
hook may signal `done` only when no `post_done_cmd` is configured; otherwise the
completion hook would conflict with its lifecycle/permit guard.

With automatic completion, the daemon polls the saved issue URL of workspaces
opened with an issue link every ~30 seconds using its `gh`/`fj` login
unless waiting for explicit `done`. Confirmed closure records `done` with the
configured default, preserving any existing completion. Issue associations suppress idle cleanup; failed lookups retain the workspace and appear
in `status` and `inspect`. Reopening an issue does not undo completion.

Cleanup runs in the daemon without an idle delay; tracked agent exits trigger a
sweep after their exit hook instead of waiting for the next poll. It stops tracked
commands using normal branch retention and resource release. Without a PR registration,
files must be clean and all commits pushed or on the local default branch; an
existing registration keeps its merge requirements. Files and the completed HEAD
are checked again after stopping and removal hooks. Failure retains ownership;
`status` and `inspect` show the completion choice and cleanup errors. Changed HEAD
requires a new `done` signal before completion cleanup can proceed.

Keeping persists across restarts and suppresses idle and PR cleanup until a new
`done --cleanup` request or explicit removal. It leaves tracked commands running.
When cleanup removes the worktree, shell integration returns to the repository's
`<root_dir>/<repo>` directory; keeping or retained work leaves the current directory.

### Workspace holds

Holds are caller-named claims of workspace use, independent of assignment completion.
They persist across command exits and daemon restarts and block automatic removal
while the worktree exists. Automatic completion still records `done`; releasing
the last hold lets cleanup recheck its usual conditions. Idle cleanup restarts its
timer after release. Explicit removal and deleted-directory cleanup release holds.
Upgrading turns a `shoal continue` from earlier versions into a hold named
`continue` with the reason “Converted from an earlier Shoal version”, which the
next explicit `done` releases.

Reacquiring a name returns the original hold, including its reason. Acquisition
requires a ready, verified worktree and is excluded while a resource hook runs.
Release remains available while the workspace record exists. Scoped callers manage only their own workspace. `status`, `inspect`,
and `ls --json` show holds on the workspace record. `rm` lists holders before
confirmation and includes them in its JSON result; `--yes` skips confirmation.

### Forges

Shoal reads and changes the issues and PRs of the forge serving a repository's
`origin`. `github.com` is GitHub and `bitbucket.org` is Bitbucket Cloud;
`gitlab.com`, a host with a `gitlab` name label and a host `glab` has a login for
are GitLab; any other host is Forgejo or Gitea, which share an API. Lookups use
your login: `gh` on GitHub, `glab` on GitLab, on Forgejo or Gitea the token `fj`
saved for the host, else `tea`'s login through `tea api`, and on Bitbucket the
token `[bitbucket]` configures. Without a login, only public repositories are
readable. Shoal stores no forge credentials. An issue or PR number may be
written `#42`.

Bitbucket has no official CLI, so the global `[bitbucket]` table names a
`token_command`, an argument array whose output is the token; Shoal runs it for
each lookup or change and keeps nothing. The token is an access token, or an
Atlassian API token when `email` names its account:

```toml
[bitbucket]
token_command = ["security", "find-generic-password", "-s", "bitbucket", "-w"]
email = "you@example.com"
```

GitLab projects may sit in subgroups, its merge requests are PRs, and its item
URLs use `/-/issues/` and `/-/merge_requests/`. Reviewers are usernames, and
`merge` uses the project's merge method, squashed with `--method squash`, so
`--method rebase` is refused. CI checks are the latest status of each job and
external check on the head commit, and reviews are reviewers' states and
approvals; approval rules and merge trains are not modeled.

Bitbucket repositories live under a workspace and its PR URLs use
`/pull-requests/`. Its PRs have no labels and it reports no merge conflicts.
Reviewers are Atlassian account IDs, declining a PR closes it and cannot be
undone, and `--method rebase` rebases and fast-forwards. CI checks are the
builds reported on the PR, and reviews are participants' approvals and change
requests. Agents' changes use your Bitbucket credentials.

### Issue and PR actions

```sh
shoal pr open --label area/cli      # push, then link the branch's open PR or open one
shoal pr edit --title "Fix login" --add-label bug --draft
shoal pr comment --body-file notes.md
shoal pr reply 812 --body-file why.md   # in the thread of review comment 812
shoal pr resolve 812 --body-file why.md # reply, then resolve; unresolve reopens
shoal pr close 12                   # reopen works the same way
shoal pr merge --method rebase --delete-branch
shoal issue edit --add-label bug    # comment, close and reopen work too
shoal issue open --title "Flaky watch test" --label bug
```

`shoal pr open` verifies the workspace's worktree identity and that it is on its
recorded branch, then pushes the branch to `origin` without forcing, setting
its upstream, as a tracked command with your Git configuration and hooks and
its output on stderr. The daemon then links the open PR whose head is that
branch in the repository, or opens one and links it. A new PR's title is
`--title`, else the linked issue's title, else the last commit's subject; its
description is the supplied `--body` or `--body-file`, or `Closes #<number>`
when no body is supplied for a linked issue; its base is `--base`, else the base
workspace's branch, else the default branch. It is a draft with `--draft` or
`[pr] draft = true`, which `--no-draft` overrides. Draft state, repeated
`--label` and repeated `--reviewer` apply only to a new PR. Its JSON adds
`created` (false for a PR it found) and `linked`.

```toml
[pr]
draft = false # Default: false
```

`shoal pr` and `shoal issue` change an item of the workspace's repository: the
number or URL given, otherwise the linked issue or the only linked PR. `edit`
takes `--title`, `--body` or `--body-file`, and repeated `--add-label` and
`--remove-label`; PRs also take `--base`, repeated `--add-reviewer` and
`--remove-reviewer`, and `--draft` or `--no-draft`. Edits apply in that order,
and a failed request stops the rest with the forge's message. Labels are
replaced as a set by name; Forgejo's draft state is the title's `WIP: ` prefix,
and GitLab's its `Draft: ` prefix.
`reply`, `resolve` and `unresolve` take the ID of any comment in a review
thread, as `shoal view` shows it, before the PR's number or URL. `reply` takes
`--body` or `--body-file`; `resolve` posts one as a reply first when given.
Resolving a thread that has no reply asks for confirmation; `--yes` skips it.
Forgejo and Gitea have no API for thread resolution, so `resolve` and
`unresolve` fail there without sending anything.
`merge` takes `--method merge|rebase|squash`; `--delete-branch` deletes the
PR's branch on the forge when it is in the repository, never a local branch.
`issue open` takes `--title`, `--body` or `--body-file`, and repeated `--label`;
the new issue is linked only with `--link`, which fails while another issue is
linked. Its JSON adds `created` and `linked`.

The daemon sends each change through the forge's REST API as the workspace's
[agent account](#agent-accounts), except `merge`, which uses your login and is
refused to processes inside a workspace execution. Each change records an
`item_changed` [workspace event](#workspace-events). `--json` returns the item
as the forge then reports it: `kind`, `number`, `url`, `title`, `state` (`open`,
`closed` or `merged`), `labels`, and for a PR, `pr` with `head`, `base` and
`draft`.

### Linked items and cleanup

`shoal link <number-or-url>` associates an issue or PR with the workspace;
`shoal link issue <number-or-url>` and `shoal link pr <number-or-url>` select the
kind explicitly. Commands that select items this way give a number without a
kind the kind of the linked item with that number, otherwise the kind the forge
reports; GitLab and Bitbucket number issues and PRs separately, so there such a
number must be linked. Use `--workspace` to select a workspace instead of the current
context. Numbers resolve against its origin and URLs must match it. An issue
association is idempotent and must be unlinked before linking a different issue.
Linking an issue records its title for workspace lists; when the forge cannot be
read, the link is recorded without one and a warning names the error.
PR links accumulate without duplicates and must name the recorded workspace
branch. Linked items suppress idle cleanup.

`shoal view` shows linked items as the forge reports them now: title, state,
author, labels, description, comments, and reviews with their inline comments; a
PR adds its branches and the merge conflicts, CI checks and review state `status`
reports, plus the last 8 KiB of each failed Forgejo Actions check's log and its
failing step when recorded by the runner. The daemon fetches logs as the
workspace's agent account; successful and pending checks omit log output.
`--check NAME` selects only that failed check's log on one PR. Unavailable logs
report an error; log downloads are limited to 8 MiB.
Kinds and explicit items select as they do for `watch`, and
`--no-comments` leaves out comments and reviews. Each inline comment shows its ID,
the ID of the comment that starts its thread when it is a reply, and whether
the thread is resolved. A failed lookup reports its error on that item,
and the command exits 1 when an item could not be read. `--json` returns an array
of items with failed logs in `check_logs`, or a check log object with `--check`.

`shoal watch` polls all linked items every ~30 seconds, and the daemon queues
what its own polling of a ready workspace's linked items observes for the next
watch, which returns at once; `watch pr` or `watch issue` filters by kind. `watch <number-or-url>`, `watch pr <number-or-url>` and
`watch issue <number-or-url>` select an explicit item in the workspace's repository
without linking it or changing completion policy; explicit PR watches also accept
PRs whose head branch is in a fork. Watches return on comments,
reviews, completed CI checks, new merge conflicts, closure, reopening, merging,
or a merged base workspace, except for comments, review thread replies, closing
and reopening that the workspace itself made through Shoal; the same change made another way, such as
with `fj` or the web interface, is reported.
An unfiltered `shoal watch` without a linked PR also returns when the workspace
branch newly conflicts with the default target of `shoal conflicts`, naming the
branch and the conflicted paths; with nothing linked, it waits for that alone.
A new [agent message](#agent-messages) ends a watch at once, ahead of item
activity, even when the watch would otherwise fail for lack of linked items.
Each update includes its item URL, kind and message; `--json` returns an `updates`
array, and a `messages` array when agent messages arrived. The first watch
reports existing activity; subsequent watches share a
cursor per workspace and item across restarts. Pending updates replay until the
CLI acknowledges successful output. Unlinking discards that item's cursor.
`--timeout <seconds>` bounds the wait to 1–3600 seconds; without it, the wait is
unbounded. Expiration returns an empty array and `timed_out: true`. A newer watch
in the same workspace
supersedes a running one, which returns an empty array and `superseded: true`
without taking updates; updates acknowledged while a newer watch runs stay pending
for it. Scoped callers can watch only in their own
workspace. Activity polling continues when PR cleanup is disabled. Lookup failures report errors or `lookup_failed` updates; a source
that keeps failing with the same error is reported again every 10 minutes. Forgejo
comment and review changes are grouped; CI results are the commit statuses of the
PR's head commit, read with fj's login for the host when it has one.
Failed Forgejo checks include the failing step when the runner records it and
a command to read the log; a log lookup failure stays on that check's update.

Once every linked PR has merged and at least one contains current HEAD, a
workspace marked `done` is cleaned up. With automatic completion, Shoal records
`done` itself, using `[done] cleanup` and preserving
any previously recorded completion choice. A confirmed
set survives restart without completing again. Cleanup uses normal branch retention and resource release, retaining dirty
or newer work. `inspect` shows lookup and removal
errors in `pr_cleanup`. Invalid registrations retain the workspace.

`shoal unlink` removes all associations; `unlink pr` or `unlink issue` removes
one kind, and an explicit number or URL removes just that item. Removing the last
link does not mark done or undo recorded completion; `done --keep` cancels its
cleanup. `[cleanup.pr] enabled = false` pauses PR completion independently of idle
cleanup, globally on reload or per repository immediately; linking and unlinking
remain available. Previous PR registration and wait command spellings remain accepted.

### Ready for review

```sh
shoal ready              # Mark every linked item, or the workspace when none are linked
shoal ready pr 12        # Mark one linked PR; `ready issue` marks the linked issue
shoal unready            # Withdraw all marks; select a kind or item like unlink
shoal unready --draft    # Also return the PRs whose marks are withdrawn to draft
```

A mark says the agent considers the work for a linked issue or PR, or for the
workspace itself when nothing is linked, ready for review at the current HEAD.
Items are selected like `link`, and a selected item must be linked; a kind with
no linked items is an error. Marking again moves the mark to the current HEAD, and
marking linked items replaces a mark made while nothing was linked.
Marks never notify, record completion, or affect cleanup; use `shoal notify` to
ask for attention. New commits make a mark outdated: `ls` shows
`ready for review (outdated)`, and `status`, `inspect` and `ls --json` list
`review` marks with `kind`, `url`, `head`, `created_at` and `stale`.
`shoal ls --ready` lists every mark across repositories, one row per marked
item with its URL and `outdated` when HEAD has moved; with `--json` it lists the
workspaces that have marks. Unlinking an
item withdraws its mark, and removal deletes all marks. Each mark and withdrawal
is a [workspace event](#workspace-events). Scoped callers mark only their own
workspace; `--workspace` selects another one for unscoped callers.

`post_ready_cmd` runs in the daemon after marks are recorded, under the same
rules, precedence and limits as `post_done_cmd`, with `SHOAL_REVIEW_MARKS` holding
the JSON array of marks just recorded. Failure records `hook_failed` and keeps
the marks; every `shoal ready` runs it again.

Before the hook, `shoal ready` takes each marked open PR that is a draft out of
draft, as `shoal pr edit --no-draft` would; `[ready] undraft = false` keeps marks
local. `shoal unready --draft`, or `[unready] draft = true`, returns the open PRs
whose marks it withdraws to draft first; `--no-draft` overrides that setting.
A failed change reports the forge's error and exits non-zero, keeping the marks
in both commands so either can be retried. Marks never change issues, and
unlinking leaves the PR as it is.

```toml
[ready]
undraft = true # Default: true

[unready]
draft = false  # Default: false
```

### Agent state

```sh
shoal internal agent-state working   # Also waiting (for the user mid-turn) or idle (turn finished)
```

Agent hooks report the agent's turn state with this command, and integrations
read it instead of each inferring it from the terminal. Shoal's terminal
Claude Code and Codex launches, including `add --agent` and resumed sessions, register
the hooks with the [message hooks](#agent-messages); Happy sessions do not.
Neither agent has a hook for an interrupted turn: Claude Code shows `working` until
it notifies that its prompt is waiting for input, and Codex until its next turn
ends. Codex has no hook for a permission prompt, so it reports `working`
instead of `waiting`. Other agents can call the command from their own hooks. `ls` and `status` show
`working`, `waiting for input` or `turn finished`; `status`, `inspect` and `ls --json`
give `agent_state` with `state` and the Unix-seconds `since` it began. A state
reported from a tracked execution ends with that execution; one reported from
outside lasts until the next report or removal. Each change, and each end, is
a [workspace event](#workspace-events); repeating the current state is not a
change. Agent state never notifies, records completion, or affects cleanup.
Scoped callers report only for their own workspace; `--workspace` selects
another one for unscoped callers.

### Automatic cleanup

Idle, clean, fully pushed worktrees are removed after 10 minutes by default, or
sooner when [disk space](#overload-protection) runs low.
Deleted directories are forgotten even when disabled, releasing resources and
retaining branches; moved worktrees or recorded commands need manual recovery.
HEAD and commands reset the timer; file changes, including ignored files, restart
it when it expires.
Running/unknown commands, directory users (including shells), dirty/unpushed work,
simulator leases, permits, holds and failed checks block cleanup. Pushed means reachable
from locally known remote branches or the local default branch; no fetch.
Sweeps run about every 30 seconds; a failed step does not skip the others, and
timers reset on restart. The daemon needs `lsof` on PATH.
`shoal cleanup` removes these workspaces now, without their idle delay and also
where idle cleanup is disabled, and reports any it retained; `--dry-run` lists them
instead, with the reason `status` gives for keeping each other workspace. Removals record the `manual` event cause. Workspace processes cannot run it.

```toml
[cleanup.auto]
enabled = false # Default: true
idle_minutes = 10
```

Reload the daemon after changing this globally; the same table in a
repository config applies to that repository's workspaces on the next sweep.
The former `[auto_cleanup]` and `[pr_cleanup]` tables are still read where
`[cleanup.auto]` and `[cleanup.pr]` leave an option unset.

### Workspace events

```sh
shoal internal events --follow --json             # stream lifecycle changes
shoal internal events --follow --json --since 123  # replay after an event ID
```

The daemon retains the newest 1,000 events independently of notifications;
reading them never marks notifications read. Without `--since`, events replay
all retained history; without `--follow`, the command exits after that backlog.
Scoped callers cannot read events.

Each JSON line has `type: "event"`, an increasing `id`, Unix-seconds `created_at`,
workspace and repository UUIDs (`workspace_id`, `repository_id`), `name`, `path`,
`branch`, `kind`, `cause`, and `error`. Kinds are `created`, `ready`, `setup_failed`,
`completed`, `undone`, `removed`, `retained`, `branch_changed`, `review_ready`,
`review_cleared`, `linked`, `unlinked`, `base_changed`, `agent_state`, `item_changed` and `item_activity`; history from earlier versions may also contain `continued`. Review events add a `review` object with the mark's `kind`,
`url` (both null for a workspace mark) and `head`. Link events add a `link`
object with the linked issue or PR's `kind` and canonical `url`. Agent state
events add `agent_state` with the reported `state`, null when its execution ended.
`item_changed` events add an `item` object with the `kind`, `url` and `action`
of a change Shoal made to an issue or PR. `item_activity` events add an
`item_activity` object with the item's `kind` and `url`, and an `update` object
with the observed update's `kind` and `message`. `created` and `base_changed`
events add `base_workspace` with the base's `id`, `name` and `branch`, or null.
Causes are `manual`, `idle`, `issue`, `pr` (including a base workspace's merged
PRs), `completion`, `missing_directory`, `disk_space`, or `removed` for a base workspace's removal, and null
when inapplicable; `error` describes setup or cleanup failures. Branch changes
are observed during daemon sweeps; detached HEAD has a null branch, and the
recorded workspace branch remains unchanged.

A `type: "gap"` line gives `since`, `oldest_id`, and `latest_id` when a cursor
falls outside retained history, then replay continues with retained events.
Resync with `shoal ls --json` after a gap or on first connection: the journal
starts when the daemon upgrades and does not reconstruct older changes.

### Notifications

```sh
shoal notifications            # Oldest new ones first, then marked read; says how many remain
shoal notifications --all      # Recent ones including read (--limit, default 50)
shoal notifications --follow   # Keep printing, and raise terminal notifications
shoal notify "PR #12 is ready to merge" [--workspace W]  # Add one for a workspace
```

The daemon records what happens while you are not looking: a resource or
simulator request that found no capacity (naming the workspaces holding the
pool), a preferred port in use, a tracked agent
exiting (with its code, or a note to run `doctor` when it left processes
behind), [agent queue](#agent-queue) starts and failures, [seed build](#seed-builds) failures, and workspaces it removed or retained on its own through PR, merge, or
idle cleanup, and automatic cleanup that starts failing. `shoal notify` adds a one-line message (at most 512 bytes, without
control characters) for a workspace; agents use it to ask for attention without
marking the assignment done. Each line shows the local time, the workspace, and the message;
`--json` returns records with `kind`, `created_at`, and `read`. On a terminal,
`--follow` also sends each entry as an OSC 9 terminal notification, which iTerm2,
Ghostty, WezTerm, and Kitty show as a desktop notification (tmux needs
`allow-passthrough`); other terminals ignore it. Repeated
identical conflicts, cleanup failures and messages collapse into one entry until read;
every agent exit is listed. `shoal ls` and `daemon status` mention pending
ones. Plain `exec` commands and manual `rm` are your own and record nothing.
Scoped commands cannot read notifications but may notify about their own
workspace. Read entries older than the newest
500 are dropped; unread ones stay.

### Agent messages

```sh
shoal message "Stop the dev server" [--workspace W]  # Queue one for a workspace's agents
shoal messages [workspace]     # New messages, oldest first, shown once
```

`shoal message` queues a one-line message, with the same limits as `notify`, for
the agents working in a workspace. Agents read messages with `shoal messages` or
`shoal watch`, which delete each message once they have printed it.

`shoal claude`, `shoal codex` and their automatic or `shoal resume` restores
register `shoal messages --hook` for the `PostToolUse` and `UserPromptSubmit`
events, so a working agent receives new messages, and the activity on its linked
issues and PRs that the daemon has queued for the next watch, as additional
context after its next tool call or prompt. Delivering through the hook replaces a running watch in
the workspace, which ends with `superseded: true`. Claude Code launches also run
`shoal internal wake` as a background `Stop` hook with `asyncRewake`: after each
turn it waits up to a week for the next message or item activity, prints it and
exits 2, so Claude Code starts the idle agent's next turn with it. Its command
passes Claude Code's process ID as `--agent "$PPID"`, and it exits with that
process: Claude Code runs hooks outside its process group, so stopping the
agent would otherwise leave the hook behind. Codex cannot
be woken by a hook: when a Codex launched in a zmx session reports its turn
finished and has undelivered messages or activity, Shoal types `Shoal: new
messages or activity on this workspace's issues and PRs.` into the session and
submits it, once until Codex starts another turn, and its prompt hook delivers
them. Text typed but not yet sent in that prompt is sent with it. Codex runs the hooks only after you trust them in Codex's
hook review. The hook reads the event from stdin and prints nothing when there
is nothing new; failures go to stderr and never fail the agent. Other agents
whose hooks accept Claude Code's `hookSpecificOutput.additionalContext` output can
run the same command; the rest rely on `shoal messages` and `shoal watch`, which
Shoal's agent guidance mentions.
An identical message that is still unread is not queued again, and a workspace
holds at most 50 unread messages. Scoped commands may read their own workspace's
messages but cannot send any. Removing the workspace deletes its messages.

### Leases

`shoal acquire <kind>` reserves a port, simulator, resource permit, or related
repository for the current workspace, or the one named by `--workspace`, or
requests access to a [secret](#agent-secrets) that requires approval.
Simulator, resource, and repository leases take `--name` (default `default`) to
hold several, `--reason`, and `--queue` to wait, without a time limit, until the
lease is acquired or its approval denied. `acquire repo` selects only `kind = "repo"` members.

Queued requests get capacity in arrival order, per pool and for simulators. While
any are queued, every other request for new capacity is busy, even when capacity is
free; renewing a held lease and requesting approval are not affected. A request
keeps its place while it is in progress or has polled in the last 30 seconds, so
stopping the command gives the place up. The daemon keeps the queue in memory: a
restart loses the order, and commands still polling join again.

`shoal release` releases every lease and active approval request the workspace
holds, in order, and stops at the first failure. A kind narrows it to that kind, a resource or repository name
to its pool, and `--name` or a port name to one lease. Holds are released
separately.

`shoal leases` shows every kind's capacity and leases; a kind shows
only that kind, with repositories listed apart from other resources, and `--all`
covers every workspace. The earlier `acquire`, `release`, and listing forms of
`port`, `sim`, and `resource` remain accepted but are hidden from help.

### Port reservations

```sh
shoal acquire port web --workspace fix-login --reason "Frontend dev server"
shoal acquire port api --port 3001 --env API_PORT --reason "HTTP API"
shoal leases port --workspace fix-login  # Configured names and current leases
shoal leases port --all                 # Every workspace
shoal release port web --workspace fix-login
shoal exec fix-login -- sh -c 'my-server --port "$API_PORT"'
```

Named TCP reservations belong to the worktree, persist across command exits, `stop`,
and restarts, and are released by successful removal. Repeating a name returns the
same port (`--reason` may update it); changing the number or variable requires
release first. Workspace environment exports and tracked commands receive
`SHOAL_PORT_<NAME>` or the `--env` variable; running processes keep their
environment, and nested executions drop the parent's port variables. Automatic
allocation uses 49152–65535, configurable with `[ports]` `start`/`end` in the global
or a repository config; `--port` may name any nonzero port. Shoal probes IPv4/IPv6 availability and
prevents duplicates within the daemon, but reservations are cooperative and
unrelated processes can still bind. UDP is not supported.

Repository defaults, in `.shoal.toml` or the imported config:

```toml
[ports]
on_conflict = "suggest" # or "auto"
start = 3000            # Automatic range for this repository
end = 3999

[ports.web]
port = 3000
env = "PORT"
reason = "Frontend dev server"
```

`on_conflict`, `start` and `end` are keys of the table itself, so no port can
take those names. `shoal acquire port web` allocates on request; CLI flags override. A conflict
suggests a free port: fzf offers to accept it, `--json` returns `reserved:
false` with exit 2, and `--port <suggested>` or `--on-conflict auto` accepts.

### Shell navigation

Add `source <(shoal shell init)` to `.bashrc`/`.zshrc` (or `eval "$(shoal shell
init)"` for Bash without process substitution) and run it in open terminals,
including after upgrades. The function lets `add` and `cd` enter workspaces
and moves you out of a removed one. If cleanup removes your directory while an
agent runs, the shell returns to the nearest surviving parent after exit or at
the next prompt, preserving the exit status. `shoal cd` always opens fzf, `shoal cd
<name>` goes directly, and `shoal cd -` returns to the shell's previous
directory (`OLDPWD`, per shell), refusing a deleted destination. Scoped agents
cannot navigate outside their worktree. Without the function, Shoal prints the
destination and, in an interactive terminal, explains how to load the integration;
`--json` returns the path without requesting navigation.
The wrapper refreshes the Zsh line-editor prompt after navigation; Bash updates
its prompt through `PROMPT_COMMAND`.

### Tab completion

The same shell integration enables Bash and Zsh completion (initializing Zsh's
completion system if needed). Each Tab asks the installed binary for
subcommands (including global configured commands), flags, fixed values, and
paths without a daemon; with one running it also suggests repository-configured
commands, repositories, workspaces, pools, members, lease names, ports,
and simulator leases for the current or named workspace, honoring `--state-dir`
and scope with a 500 ms timeout, never starting a daemon or picker. Targets sort
before flags, also in fzf-tab. `shoal completions <shell>` prints scripts for
Bash, Zsh, Fish, PowerShell, and Elvish.

### Recovery

```sh
shoal doctor fix-login                  # Report only
shoal --json doctor --all
shoal doctor fix-login --repair         # Repair verified state, retain work and leases
shoal doctor fix-login --repair --stop  # Also stop verified surviving commands
shoal doctor fix-login --repair --reclaim  # Re-establish ownership after checking the worktree
```

Exit 2 while findings or incomplete checks remain, 0 when clear. JSON contains
`checks` and `workspaces`. Environment checks cover daemon reachability and
version, executable `git`, `wt`, `lsof`, `zmx`, and `fzf` (for interactive pickers) on
the daemon's PATH, Git worktrees under registered repository roots that Shoal
does not track, and shell integration in the calling shell. They run regardless
of the workspace selection and only diagnose, even with `--repair`. An unavailable or mismatched daemon leaves its
checks marked as skipped (`dependency:<tool>` and `worktrees:*` when repository
names are unavailable); doctor never starts or restarts it.
When current checks find no issues, reports show the recorded failure and repair guidance.
`doctor` is unavailable inside scoped executions. Before accepting requests or
running cleanup, daemon startup atomically marks unfinished simulator cleans
interrupted, transient lifecycle operations failed, and disconnected executions
unknown until their wrappers reattach, then audits worktrees without deleting files or releasing leases. Repair restores verified
worktrees to ready and clears executions proven stopped; connected commands keep
running unless `--stop`. Moved worktrees must return to their recorded path,
and a deleted directory is forgotten by the next
cleanup sweep or `shoal rm`, retaining the branch. Ownership is proven by a
`shoal-workspace` marker in the worktree's Git admin directory, so it survives
reboots, device renumbering and restores. Workspaces recorded before markers
gain one once their recorded filesystem identity verifies at startup or with
`doctor --repair`. When neither proof verifies ownership, check the worktree at
the recorded path yourself and add `--reclaim`: it re-marks a linked worktree of
the recorded repository on the recorded branch that no other workspace owns,
then repairs as usual.

Executions record wrapper and child identities plus a process group. Descendants
inherit `SHOAL_EXECUTION_ID`, which recovery uses with live ancestry to find
survivors; identities are rechecked before signaling, and unverified candidates are
never killed. Stopping gives the entire command process group a shared grace period
before forced termination, even if its leader exits first. Detection is cooperative:
hidden environments, cleared markers, and old records can leave it uncertain.
A reported command exit retains an unknown execution and prevents setup readiness
while child/group survivors or live unreadable environments remain. Exiting
processes do not count, and a process still publishing its environment after exec
is read again for up to two seconds; a settled empty environment is readable and
does not identify an execution. Setup verification failures retain the workspace
and report the command's exit status, blocking PIDs or inspection error, and
recovery commands. `doctor` identifies unreadable PIDs without exposing their
arguments or environments. After checking
yourself that such processes stopped, use `--repair --acknowledge-stopped`; visible live processes still block.

### Scoped workspace commands

PR watches, merge acknowledgements, `notify`, `messages`, `ready`, `unready`
and `internal agent-state` are own-workspace scope exceptions.
Processes carrying a Shoal scope token are confined to their own worktree:
`status`, inspect, execute, `merge`, `diff`, `setup`, and resources. They may read
effective configuration for their own workspace, but cannot change configuration.
They cannot `land`, reach other worktrees, create or remove workspaces, read
notifications, or administer repositories or the daemon service; nested commands
keep the scope. A command whose ancestor carries scope for the same state directory
gets the CLI's scoped refusals even after the token is removed from its
environment. Scope is cooperative and does not restrict direct filesystem or
Git operations.

`shoal env [workspace] --json` returns an object mapping environment variable names
to strings, using the same workspace identity and current port exports as tracked
executions, with a fresh `SHOAL_SCOPE_TOKEN` and no `SHOAL_EXECUTION_ID`.
The `[env]` table adds user-defined variables to tracked executions, lifecycle hooks,
and this exported environment. Values resolve from saved repository config, the
worktree file, global config, then the default, and substitute `{workspace}`,
`{workspace_path}` (also `{path}`), `{repo}`, `{branch}`, and reserved `{port.NAME}`
fields once. Unknown fields remain literal, inserted values are not expanded, and
`SHOAL_*` names are reserved for Shoal. Names must be ASCII identifiers; values
must be strings without NUL. An empty string sets an empty value; inherited
variables and shell expressions are not expanded. Reserved port exports take
precedence over `[env]`. Desktop handoffs may reuse an existing process and do
not receive these variables.
Only unscoped callers can export or revoke tokens, and export requires a ready,
verified worktree. Add the returned variables to the processes your app starts.
Each exported token survives daemon restarts until `shoal env [workspace] --revoke
<token>` or successful workspace removal; revoking one leaves other tokens valid.
Running processes retain their port snapshot. These processes are untracked and
the token provides no cleanup protection.

## Simulators (macOS)

Shoal creates, boots, shares, and deletes its own Xcode simulators. Configure
profiles in the global config and reload the daemon:

```toml
[simulators]
max_booted = 2
max_devices = 4
idle_seconds = 120
allow_any = false
default = "phone"

[simulators.profiles.phone]
device = "iPhone 17"
# runtime = "iOS 26.5"  # Optional pin
```

Omitting `runtime` from a profile or `--runtime` with `--device` selects the latest
installed, available iOS runtime compatible with the device. `shoal sim catalog`
lists installed device types and runtimes; nothing is downloaded.
Repository config may set `[simulators] preferred = ["phone"]`;
flags override it, and `allow_any = true` permits unconfigured `--device`
requests with a `--reason`.

```sh
shoal acquire sim                   # Current worktree, configured preference
shoal acquire sim --profile phone --name tests --queue
shoal leases sim                    # Profiles, capacity, and managed devices; --all for all
shoal release sim --name tests     # Or every simulator lease
```

Acquisition returns a ready device UDID; use it explicitly with `simctl` or
`xcodebuild -destination 'platform=iOS Simulator,id=<UDID>'`. Repeating a lease name
returns the same device. Leases belong to worktrees, survive command exit and
restarts, block automatic removal, and are deleted with the workspace. At capacity,
idle managed devices shut down first; active leases and personal simulators are
never touched, every device not confirmed shut down counts toward the limit, and busy requests
exit 2 unless `--queue`. Released devices keep apps and settings until the
idle timer deletes them (checked every 15 seconds). Failed operations retain records
for retry. Only the default CoreSimulator device set and one daemon are covered.

### Clean devices and audit history

```sh
shoal acquire sim --clean --reason "Verify first-launch permission prompts"
shoal sim history                  # --all, --limit 50, --before <id>
```

Normal handoff preserves device state. `--clean` returns a fresh or erased
device, needs a reason, and cannot erase an active lease. Shoal prefers spare
capacity, otherwise erases or replaces the idle device with the fewest known
user apps. History records workspace, execution, reason, outcome, device,
planned resets, estimated app loss, and whether the erase completed, including
busy and failed attempts; it survives removal and restarts, and scoped commands
see only their own entries. Erasing devices outside Shoal bypasses this audit.

## Cooperative resource pools

Global config defines resources shared across repositories; repository config
defines resources shared across that repository's branches. Both use:

```toml
[resources.signing]            # Standalone mutex; raise capacity for a semaphore
capacity = 1
reason = "Signing service"

[resource_pools.devices]
capacity = 2                   # Total permits across the pool

[resource_pools.devices.resources.alpha]
capacity = 1

[resource_pools.devices.resources.beta]
capacity = 2                   # Within the pool limit
```

Capacities default to 1 (pools to their members' total); every pool needs
members. Names are lowercase letters, digits, `_`, or `-`, starting with a
letter, at most 64 characters; capacities are 1–65535.

```sh
shoal leases resource                        # Capacities and own leases
shoal acquire resource devices               # Any available member
shoal acquire resource devices --member beta --name tests --reason "Integration tests"
shoal acquire resource signing --queue
shoal leases resource --all                  # Every workspace
shoal release resource devices --name tests
```

Each lease takes one slot from the pool and the chosen member; an explicit member
never changes silently. Repeating a pool and lease name (default `default`) returns
the existing permit. `--json` returns the lease or `acquired: false` with exit 2;
`--queue` waits in arrival order. Global names cannot be
redefined by a repository; repository pools are keyed by repository, so equal names
elsewhere are independent. Global changes apply on reload, repository config is read
per request, and conflicting definitions block new claims until they agree or leases
drain. Leases survive command exit and restarts, block automatic cleanup, and are
released by successful removal. Shoal accounts for permits only; stop using a
resource before releasing it.

### Related repository paths

Declare a registered repository as a resource in global or repository config:

```toml
[resources.server]
kind = "repo"
repo = "saldoir-server"        # Registered name or ID, or a remote URL
```

A URL keeps tracked config independent of local registration names. It matches
the registration with the same remote over any transport, and is never cloned:
register it first with `shoal repo add <url>`.

`shoal acquire repo server` returns a read lease with `repository.id` and
`repository.path` in JSON. Scoped callers can acquire it without repository
administration access. New leases default to `read`; other modes are rejected.
Capacity must be 1, and readers share one pool slot. Resource approvals and hooks
apply normally; approvals bind the resolved repository ID and path.

The path is the registered checkout's live working tree, with cooperative read
access. Shoal does not enforce filesystem permissions, fetch, select a ref, or
create a sibling path. Use the returned path in a project's repository override;
use Git to read committed files at a chosen ref. Missing checkouts and repositories
being removed cannot be acquired. Repeated acquisition preserves the recorded
binding even if configuration changes. A lease prevents repository deletion while
another repository's workspace holds it; release and workspace removal only
discard the lease, leaving the related checkout untouched.

### Resource hooks

Global or repository config can name `post_resource_acquire_cmd` and
`pre_resource_release_cmd`, using the [hook path and environment rules](#workspace-setup-and-hooks).
They apply to generic permits, run in the daemon without a terminal, and have a
60-second limit. `SHOAL_RESOURCE_LEASE` contains the lease as JSON, including its
`id`, `scope`, `pool`, `resource`, `name`, `mode`, and `reason`.

Acquisition records the lease before running its hook, including when an existing
lease is returned; busy requests run no hook. A failed acquisition hook returns
an error but retains the permit: repeat acquisition to retry, or release it.
Release runs its hook before freeing capacity; failure retains the lease.
Workspace removal runs release hooks after `pre_remove_cmd`, keeping every lease
until removal succeeds. Hooks are skipped when the worktree is already missing.
Make hooks idempotent: retries can repeat them, and reader hooks run per lease,
not once per shared member. Hooks may inspect state, but overlapping permit or
lifecycle changes in the same workspace fail while a resource hook is running.

### Resource approvals

Set `requires_approval = true` on a resource, named port, simulator profile, or
tool-scoped secret to gate scoped acquisition. `[simulators]` also accepts approval defaults with normal
repository/global precedence. A protected machine profile cannot be relaxed by
repository defaults or by selecting its device/runtime directly; where policies
overlap, lease lifetime is stricter than workspace lifetime.
`approval_lifetime = "lease"` (default) ends approval on release;
`"workspace"` permits later acquisitions of the same member and access settings.
Unscoped acquisition needs no separate approval. Port approvals bind the preferred
port, automatic range, environment variable, and conflict policy; an accepted conflict
suggestion with changed settings requires a new request. Simulator approvals bind
the installed device/runtime and whether a clean device was requested; clean requests
still need a reason and the usual audit before mutations.

Agents use the normal acquire command with `--reason`. `shoal approvals` lists
these requests, keep requests from `shoal done --keep`, and retained workspace grants; `shoal approvals list <workspace>` filters
by workspace. Scoped callers can see only their own requests. An unscoped user
reviews the recorded settings and uses `shoal approvals approve [id]` or
`shoal approvals deny [id]`. Approval reserves no capacity: retry acquisition after
the decision. Pending requests produce a daemon notification, collapsed until read;
a decision queues an [agent message](#agent-messages) for the requesting workspace
naming the request, its target and the decision.

An acquisition waiting for a decision returns exit 2 and JSON
`code: "approval_pending"` with the request; denial returns `"approval_denied"`.
`--queue` waits for pending approvals and capacity, stopping on denial. Retrying the same
name preserves the request; changing its settings or reason requires release first.
Release cancels a pending or denied request even without a lease. Changed effective
settings cannot reuse a grant. Requests survive restart and failed removal and are
deleted with the workspace; they do not prevent idle cleanup.

### Shared readers and exclusive writers

Set `kind = "rwlock"` on a standalone resource or pool member and acquire with
`--mode read` or `--mode write` (new rwlock leases default to write; semaphores
use `permit`). Readers coexist and share one pool slot, freed by the last
release; a writer excludes everyone. Capacity must be 1. Repeating a lease
name returns its mode; changing mode requires release first, and there is no
writer priority. `shoal leases resource` shows reader and writer counts with
separate read/write availability.

## Agent skill outside project repositories

```sh
shoal skill install          # Every tool whose skill directory exists
shoal skill install grok     # One tool, creating its skill directory
```

Installs the bundled skills at user scope with no daemon, as `<skill>/SKILL.md`
in each tool's skill directory: `~/.claude/skills` for Claude (honoring an
absolute `CLAUDE_CONFIG_DIR`), `~/.grok/skills` for Grok, and `~/.agents/skills`
for Codex, opencode and pi, which share one copy. Without a tool name, Shoal
installs only into skill directories that already exist and skips a configured
tool without `skill_dir`; create a directory, or name the tool, to install for
it. Set or override a tool's directory in global Shoal config:

```toml
[ai.droid]
skill_dir = "~/.factory/skills"
```

Names are portable identifiers; `all` is reserved. `skill_dir` must be absolute
or start with `~/`. These machine settings cannot be set per repository;
`command` makes a tool an [agent](#agents). Each `SKILL.md` is a regular copy
of the packaged or embedded skill, since Codex skips a linked one; a linked
skill directory is left to the user. Other files in the skill directory are
preserved. Run it outside scoped executions. `shoal skill` prints the instructions (`--json`
returns a `skill` field).

Shoal records what it installed in `.shoal-skills.json` in each skill directory.
After an upgrade, the next unscoped command updates each directory that still
holds a Shoal skill: it adds new skills, replaces outdated copies and removes
retired skills, and replaces links earlier versions installed with copies. A
skill changed or removed since Shoal installed it is kept and reported once;
`shoal skill install` restores a removed skill and, in a terminal, asks whether
to keep, replace or show the diff of each changed one; `--force` replaces them
without asking. Skills installed before the record existed are treated as
unchanged.
