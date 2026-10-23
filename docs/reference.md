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
`main`). `shoal update` refuses Homebrew installations; for a downloaded binary,
it installs the latest GitHub release's archive for this platform when that
release is newer, after checking it against the release's `SHA256SUMS` and its
reported version, by replacing the resolved executable file. Managed daemons notice the replacement and restart once daemon operations are
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
deletes the Xcode data there that removing a workspace of a repository using
the [`xcode` preset](#build-tool-presets) deletes, for every worktree in the
repository's directory that no longer exists,
removes the workspaces there that idle cleanup would remove, without waiting for
their idle delay, then empties [shared caches](#shared-caches) there nothing is
using, until enough space is available. A repository with idle
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
Without one, an AI tool with resume arguments (`resume` and `resume_latest`
under `[ai.<name>]`, built in for Claude and Codex) continues the session its
hooks last reported, otherwise the latest session in the workspace, without a
session picker, with a prompt to continue its unfinished assignment on manual or
automatic resume. Configured commands receive that prompt through `{prompt}`;
other agents need an entry in `[agent_resume]`.
A failed command lookup warns and
disables automatic recovery for that launch. Overload notifications include the
pressure reason, execution ID, and automatic or manual recovery path. After the
wrapper stops the agent, Shoal also stops processes that carry the execution's
identity outside the agent's process group, such as background shell commands,
as `shoal doctor --stop` does. Shoal saves
the agent identity and pressure reason before signaling the wrapper, so a wrapper killed
under pressure can still be resumed after reconciliation. With one configured, the wrapper
stays connected while the agent is stopped and automatically runs that command
once healthy readings persist for the recovery interval and the
[agent queue](#agent-queue) admits the restore. Recovery requires
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
workspace, resume uses the current workspace when it has something to resume,
otherwise a picker listing every workspace with what it would restore, stopped
work first; with nothing to resume anywhere it reports that without a picker.
Completion describes each workspace the same way.
A workspace without stopped work resumes its last agent that exited on its
own, such as one the user quit, unless the assignment is done; the next agent
started there or `shoal done` replaces it, and `--discard` forgets it. Unlike
stopped work it uses no agent slot, never delays cleanup and is not part of `--all`.
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
the saved session. Other agents require a configured restore
command. Resume shows the saved pressure reason. Records
survive daemon restart and suppress idle cleanup until the replacement process
is registered; removal keeps them in its [removal record](#checkpoints-and-restore). A failed launch retains its records;
a later stop or overload creates a record for the replacement execution. The original
task prompt is never replayed.

### Keeping the machine awake

```toml
keep_awake = true              # Opt in; global config only
```

With `keep_awake` set, the daemon keeps the machine from sleeping while an
[agent's turn](#agent-state) is working, or while an idle agent receives the
activity of a [watched](#linked-items-and-cleanup) PR not yet seen merged or
closed, from its own workspace or a [child](#child-workspaces) without an agent,
since that activity starts its next turn. Closing a laptop's lid or leaving it idle
then does not suspend the work; once no agent has either, the machine may sleep
again. Linux holds a `systemd-inhibit` lock on sleep, idle and the lid switch,
which logind and idle daemons that honour its inhibitors obey; macOS runs
`caffeinate -i -s`, which prevents idle sleep and, on AC power, system sleep,
but a closed lid still sleeps a Mac that is not in closed-display mode. The
inhibitor ends with the daemon. When it cannot start or ends on its own, such as
when polkit refuses the lock, the daemon records a `keep_awake_failed`
notification and tries again only once agents have no work or the config reloads.

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
`<number>-<words>` from up to three title words, skipping filler such as “a” and
“the”, within 32 characters; a title without ASCII words gives `issue-<number>`.
Numbers use `--repo`, the current registered checkout or workspace, then an
interactive repository picker. An existing association
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

Bare `shoal` opens an fzf picker on the [inbox](#inbox) when something needs you,
searching for the current workspace's items when it has any, otherwise on the
list of workspaces; `shoal inbox` and `shoal work` open it on
their view on a terminal. Tab cycles the inbox, the workspaces and recent
notifications, which it marks read as `shoal notifications` does; there
Enter enters the notification's workspace (`shoal --help`, or bare `shoal`
without a terminal, prints commands grouped by task and a starting workflow;
commands that only Shoal, its agent hooks and integrations run live under the
hidden `shoal internal`). Workspace processes see only the workspaces.
Its rows, like `shoal work` and workspace pickers, are aligned and marked ● ready,
◌ in progress or ✗ failed. Each names the workspace, its repository when the
rows span several, its status and the linked issue's title, cut with `…` to fit
a terminal's width. The status is the
state unless ready; for a ready workspace it is, in order of precedence,
`stopped` when `shoal stop` saved agents or commands for `shoal resume`,
`needs input`, `ready`, the other [agent states](#agent-state),
`running` while an agent or command runs, `ready (outdated)`, or `inactive`. Branches and paths are in `shoal status`.
The header names the keys the list binds, most used first, on as many lines as
the terminal needs. Enter enters the selection; Ctrl-T attaches, Ctrl-F shows
the diff, Ctrl-A adds, Ctrl-E runs Claude/Codex CLI, starts a Happy session,
opens Codex/T3 apps, or runs a shell command, Ctrl-O inspects, Ctrl-S stops,
Ctrl-D deletes. Each action returns to your shell.
`shoal work --print`, like a filter, `--json` or output that is not a terminal,
prints the list instead: your own workspaces first, then each running [chat](#chats)
marked ◆ with its agent, followed by the workspaces it opened; `work --chats`
lists only the chats. `work --repo <repository>`, with or without
`--queue` or `--chats`, lists only that registered repository's workspaces,
chats and queued agents. `work --status <status>`,
repeated or comma-separated, lists only rows with one of those statuses, spelled
in kebab case (`needs-input`, `ready-outdated`, `queued`);
a chat's status is `running`.
<a id="inbox"></a>The inbox lists what needs you, each with its workspace: open questions,
pending approval requests that no coordinating agent decides, including issue
[proposals](#approval-for-agent-issues-and-prs) whose workspace is gone, workspaces whose
agent needs input, failed
workspaces, and [ready marks](#ready-for-review) at the current HEAD, one row per
marked item. There Enter answers a question, approves or denies a request after
asking which, refusing one that replaced the request listed, attaches to the agent that needs input, inspects a failed
workspace, and shows a ready PR or issue with `shoal view` (`shoal status` for a
mark on the workspace itself); Ctrl-T, Ctrl-O and Ctrl-F act on the row's
workspace and Ctrl-A adds one. `shoal inbox --print`, like `--repo <repository>`,
`--json` or output that is not a terminal, prints it; `--json` gives each item's
`kind` (`question`, `approval`, `needs_input`, `failed` or `ready`) with its
`question`, `request` or `workspace` and a ready item's `mark`. A workspace
process gets only its own items, printed, and none after clearing its scope
token.
Bare `shoal repo` opens the same kind of list over registered repositories
(without a terminal it prints `repo` help): Enter adds a workspace, Ctrl-A
registers a path or URL, Ctrl-R renames, Ctrl-O shows saved config, Ctrl-D deletes.
`repo rename` asks for an omitted name.

Commands acting on one workspace use an explicit target, otherwise the caller's
scoped workspace or the workspace containing the current directory, then an
interactive fzf picker; a deleted current directory provides no workspace context.
The picker leaves out workspaces without what the command acts on, such as an
agent session for `attach` or a running agent or command for `stop`, and reports
when none is left. Workspaces the command would refuse for their state, or for
`setup` a running execution, come last with the reason in place of their status.
`cd`, `attach` and the commands that open, show or run something in one workspace
remember, across terminals, the workspaces each acted on, and take `-` as the
target for the latest one other than the current workspace; `cd` also remembers
the workspace it leaves. Scoped callers keep no such history.
Invalid explicit targets and other invalid arguments fail without fallback,
before any prompt for the omitted ones; without a
current workspace, noninteractive and JSON calls require a target. Scoped callers
remain confined to their own workspace and the [child workspaces](#child-workspaces)
split from it: without a target, a scoped command acts on the child whose
directory it runs in, otherwise on the caller's own workspace, and agent hooks
always report for the caller's own. Another omitted required argument that
names an existing record opens a picker of those records, under the same
noninteractive rule. Leaving a picker without a choice exits with status 130
and no message. Bare `shoal cd` always opens a picker;
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
names the first condition that keeps the workspace from automatic cleanup, holds,
open questions, a pending PR proposal, child workspaces, queued agent launches and assignment state before commands, leases and Git state, or the idle delay
when nothing does, in `cleanup_reason`. `--json`
returns the same data, with PRs in `prs`, the issue in `issue_status` and sessions
in `sessions`, or a failed `zmx list` in `sessions_error`; `inspect` keeps the raw workspace and
execution records. Workspace records from `work --json` and `inspect --json` include
the assigned issue in `links.issue`, assigned PR URLs in `links.prs`, and items
recorded as opened or related in `links.opened` and `links.related`, each with
`kind`, `url` and `title` when known; an acknowledgement without a PR URL leaves
`links.prs` empty. `status` lists opened and related items too. An issue or PR URL, `pr` or `issue` with a number or URL, or
`resource` with a pool or member name shows every workspace that links or holds
it instead, as a JSON array; a number must be linked in only one repository.
Scoped callers find only their own workspace and its child workspaces.

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
Claude and Codex launches, including Happy's, trust the repository's checkout and
shared Shoal workspaces directory before launch, creating the agent's user config if
needed and preserving other settings. Claude Code looks up a worktree's trust under
the checkout, not under parent directories above the worktree. An explicitly placed
workspace outside the workspaces directory gets its own trust entry.
Claude uses `~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`); Codex CLI
and app use `~/.codex/config.toml` (or `$CODEX_HOME/config.toml`). For tracked
agents, a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` set in the workspace's `[env]` takes
precedence over Shoal's environment; chats and desktop apps use Shoal's. A trust
update failure warns and still launches the agent. Removing a repository removes
its trust entries from these files, and from those its workspaces' `[env]` selects,
while preserving other project settings. When the daemon removes a
[contribution](#contributions), Shoal's environment is the daemon's.

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
in this terminal. A detached session ends with its agent, whatever its exit
status; one whose start fails opens your login shell (`$SHELL -l`) there to show
why, and one without an agent opens the workspace shell there, as a tab does.

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
`shoal add --focus` focuses one launch and starts it at once, as `--now` does.
`enabled`, `new_tab` and `close_when_done` default to true; with `enabled = false`
Shoal treats a Herdr pane as a plain terminal:

```toml
[herdr]
enabled = true          # Use Herdr tabs and panes
new_tab = true          # Enable the handoff inside Herdr
# tab_name = "{repo}: {branch}" # Customize the tab name
close_when_done = true  # Close after done keeps the workspace, or at removal
```

`tab_name` supports `{repo}`, `{branch}`, `{issue_number}`, and `{issue_title}`,
or a literal name. Issue fields are empty without an issue; unknown placeholders
stay literal and inserted values are never expanded again. `{branch}` uses the
requested branch when the tab opens and the allocated branch after creation.

Failures before agent execution leave the tab open. Agent exits leave the tab
open so work can continue, and with shell integration its shell enters the
workspace at its next prompt; completion closes it once tracked executions resolve
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
instructions = ["--system-prompt", "{instructions}"] # Optional
resume = ["--resume", "{session}"]                    # Optional
resume_latest = ["--continue"]                        # Optional
```

`shoal droid [workspace] -- <args>` (or `shoal run droid`) then starts it in a
workspace the way `shoal claude` does, and `--agent droid` selects it. `command`
is its launcher at the global layer, so a repository `[commands]` entry with the
same name replaces it; defining both in the global file is an error. Names that
collide with built-in commands are rejected. The picker and completion offer AI
tools only.

`instructions` passes the rendered agent guidance apart from the first message,
with `{instructions}`, or `{instructions_toml}` for a TOML string literal;
`resume` and `resume_latest` restore a stopped agent's session (see `[agent_resume]`).
Each entry replaces the built-in one for Claude, Codex and the other built-in tools.
Otherwise, for AI tool launches, `{prompt}` combines the rendered agent guidance and
issue prompt, separated by a blank line. Without that placeholder, nonempty context
becomes the first forwarded argument. Substitutions happen once; user arguments
remain literal. Claude and Codex also get Shoal's hooks and trust entries, which
other tools do without. `--agent` and `default_agent` also
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

Happy sessions are an optional integration: off until `happy.enabled = true` in
global or repository config, and absent from builds without the default `happy`
Cargo feature (`cargo build --no-default-features`), which also drops its
encryption dependencies. While off, pickers and completion leave Happy out and
launches are refused before any work starts.

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
with an agent about a repository, to discuss it, its issues and plans, or to
coordinate work, without changing code. The repository is the named one, else the
current checkout's or workspace's, else a picker. The agent is `--agent`, else
`default_agent`, else a picker of the installed ones; it must take instructions
apart from its first message (an `instructions` template, built in for Claude
Code and Codex CLI). The agent's configured launcher runs in the registered
checkout with the rendered [chat template](#prompt-templates) as those
instructions, so it waits for your
first message. The template substitutes `{repo}` and `{path}`; in the launcher,
`{path}` is the checkout and other workspace placeholders stay literal. The checkout is trusted in the agent's
config as workspace launches trust theirs.

Every call starts a new chat and focuses it. Inside Herdr it opens a focused tab
labelled `<repo> chat` regardless of `herdr.focus`, unless `--here` or
`new_tab = false` keeps it in the current pane; on a terminal it runs in a zmx
session labelled `shoal.chat=<repository id>`, which `zmx attach` returns to;
inside a zmx session or a Herdr pane it runs in place. Chats run with your
authority, not a workspace scope, and workspace processes cannot start one.
Chats are not tracked executions, so they are never queued, stopped under
pressure or resumed. The daemon records each chat while its agent process runs,
numbering a repository's running chats from 1; `shoal work` lists it with the
workspaces it opened, and [workspace events](#workspace-events) report its
start and end. A workspace
belongs to the chat whose agent last opened it with `shoal add`: its `chat`
field and `shoal status` name that chat until the chat ends or the workspace is opened outside a
chat, which returns it to you. While it runs, the chat
[coordinates](#child-workspaces) the workspaces it owns; one that ends before
receiving their events gives you the notifications they stand for.

### Agent queue

```sh
shoal config set max_agents dynamic           # Start agents while measured load allows
shoal config set max_agents 3                 # Fixed machine-wide limit
shoal config set max_agents 2 --repo shoal    # And at most two of them in this repository
shoal config set max_agents none --repo docs  # No repository limit
shoal add <issue-url> --agent claude          # Starts now, or queues when no slot is free
shoal add <issue-url> --agent claude --now    # Starts now regardless of the limits
shoal add <issue-url> --agent claude --after api --after 12  # Starts once api and #12 are done
shoal work --queue                            # Waiting and starting entries
shoal rm 612-add-queue-cmd                    # Removes a waiting launch
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
and frees the slot. `--now` starts at once and still takes a slot, and so does
`--focus`, since the user waits for a focused launch.

An agent's automatic restore after a protection stop waits in the same queue,
in order with launches, from when its processes stop. It keeps the agent's slot,
so a fixed limit lets it take that slot back on a full machine, while `dynamic`
still needs room for its load; a lowered limit holds it until other agents
leave. It starts only after the recovery interval's healthy readings, one
restore per interval, and until they arrive nothing queued behind it starts; a
stop, reload or unhealthy reading before the restored agent starts returns it to
its place. The restored
agent takes the slot over once it runs. Stopping the workspace or a failed
restore removes the entry and keeps the recovery record. A daemon restart or a
lost connection keeps the order; an entry whose waiting wrapper has not
reattached within two minutes is removed, leaving its record to `shoal resume`.

`--after` names work the launch waits for: a workspace or queued agent of the
repository, or an issue number or URL; repeat it for more. An issue launch also
waits for the open issues its forge records as blocking it: Forgejo and GitHub
issue dependencies and GitLab "is blocked by" links; when the forge cannot list
them, such as with dependencies turned off, Shoal warns and the agent does not
wait for them. Such a launch queues even without a limit and starts once every
workspace it waits for is done at its current HEAD, every queued agent has
opened its workspace and that workspace is, and every issue is closed or done in
a workspace of the repository that links it; a removed workspace or queued agent
leaves nothing to wait for. A commit after `shoal done` makes the workspace wait
again; issue state is read every 30 seconds. A new branch then starts from the
done dependency's branch and is stacked on that workspace, as with `--base`, so
the agent builds on the finished work before it merges. With several done
dependencies it takes the one whose HEAD contains the others, and while they are
on separate branches it waits until all but one have merged and their workspaces
are gone. `--base` and `--existing` keep their own starting point, and `--now`
and `--focus` cannot wait for work.

`shoal work` lists entries after the workspaces as `queued, position N` or
`starting`, followed by the work a waiting launch is after, a restore with its
agent; `work --queue` lists only them (`--json` gives the entries with their
`kind`, `launch` or `restore`, `waiting_for` while dependencies remain and
`base` once a launch starts on a dependency's branch, while `work --json` stays
a list of workspaces). `shoal rm` removes a waiting launch by name or ID, and
only by ID when a workspace has the same name; a starting one belongs to its
workspace. Scoped callers see no entries.

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
none. `status` shows both directions; `work --json` and `inspect` give
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

### Child workspaces

```sh
shoal split fix-typo                 # A child of the current workspace, from the default branch
shoal split --issue 42               # Branch named after the issue, which it links
shoal split part-two --base fix-api  # Stacked on the parent's branch
shoal split fix-typo --workspace fix-api
shoal split --issue 43 --agent claude                  # A child with an agent of its own
shoal split api-part --agent codex -- "Move the API client into its own crate"
```

`shoal split` creates a workspace on a new branch in the parent's repository,
runs its setup, and records the parent, so work that needs its own PR gets its
own branch and worktree. The parent must be ready. `--base` works as for
`shoal add`; naming the parent's branch also [stacks](#stacked-workspaces) the
child on it. Children can be split from children. Workspace processes may split
from their own workspace and act on every workspace below it, which is the only
workspace creation scope allows; other creation and removal stay with the user.

The parent's agent receives the issue and PR activity of its children that have
no agent of their own, and of their children in turn, through its hooks and an
unfiltered or kind-filtered `watch`; each such update names its workspace in
`workspace`. A child with an agent of its own, one that reports its turn state or
any agent Shoal tracks there, receives its own activity
and its subtree's instead. A child keeps its parent from every unattended removal,
completion and PR cleanup included, with `cleanup_reason` `children`, until the
child is removed; each child completes on its own with `shoal done`. Removing a
parent moves its children to its own parent, or to none, and records a
`parent_changed` event. `work` lists children indented below their parent;
`status` shows both directions, and `work --json` and `inspect` give
`parent_workspace` (`id`, `name`, `branch`, or null) and `child_workspaces`.

`--agent` starts an agent of its own in the ready child, prompted with the
issue or the arguments after `--`. It goes through the [agent queue](#agent-queue)
like any launch, but the daemon always starts it, detached in a background
Herdr tab or zmx session, so the agent gets the child's own scope rather than
the caller's; Codex runs as its CLI. The agent and that launcher, zmx outside a
Herdr that opens tabs, are checked before the child is created, and the launch
reopens only the child, failing if it was removed meanwhile. `--json` adds the
queue entry as `agent`.
Workspace processes may queue agents only in the workspaces below their own.

A workspace's coordinator is the agent of its nearest ancestor that has one,
or the running [chat](#chats) that owns the workspace or an ancestor nearer than
that agent. The coordinator receives, as messages naming the workspace, what
would otherwise notify the user. That covers the workspace agent's messages from
`shoal notify`, its questions, its completion, and its ready-for-review marks,
which never notify the user otherwise. It also covers the daemon's reports that
the workspace's agent exited or was stopped. Without a coordinator, or when its unread messages are full,
the user receives them as before. A workspace without an agent of its own is its
coordinator's own work, so what a process does there reaches the user. A
coordinator answers a question asked below it with
`shoal answer <number> <answer>`, or passes it up: an agent with its own
`shoal ask`, a chat by asking the user in the chat.

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
with its existing path is fine; a different path is rejected. Registering, and
creating a workspace, fail when the daemon cannot create a file in the directory
it would write within 30 seconds, as on a volume macOS has not given it access to.

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

### Contributions

```sh
shoal contribute https://github.com/owner/repo/issues/68  # Work on an issue
shoal contribute https://github.com/owner/repo fix-typo --agent claude
```

`shoal contribute <url> [branch]` creates a workspace in a repository you do not
maintain. The URL names the repository, or an issue, PR or branch in it, which
opens as `shoal add <link>` does; `--agent`, `--here`, `--focus` and `--now` work
as for `add`. A repository that is already registered is used as it is.
Otherwise Shoal clones it as a contribution, first checking whether your forge
login can push to it. When it cannot, Shoal forks it into your account, reusing
an existing fork, and the clone gets the fork as its `fork` remote with
`remote.pushDefault` set to it, so [`pr open`](#issue-and-pr-actions) pushes
there and opens the PR from the fork. Forking needs GitHub, Forgejo or Gitea.

The cleanup sweep removes a contribution repository as `repo rm` does once it has
had no workspace, queued agent, chat, pending issue proposal, kept branch or
resource another repository's workspace borrows, no unexpired
[checkpoint](#checkpoints-and-restore) or removal record, and no changed files,
stash, Git operation under way or commits in its checkout that no branch or tag
its remotes advertise has, for
`[cleanup] grace_minutes`, then records a `repository_removed` notification.
From the start of the removal, new workspaces, queued agents and chats in it are
refused.

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
leases, its seed and shared caches, and the saved config, stopping managed commands first. Shoal attempts to
delete workspace branches; a branch whose recorded worktree directory is already
missing is retained because its contents cannot be inspected.
Interactive calls identify the resolved repository, source, and checkout path before asking
`Are you sure? [y/N]`; `-y`/`--yes` skips the prompt and is required for scripts
and `--json`. `repo remove` is an alias.
Linked worktrees outside Shoal must be removed first; prunable stale records do
not block, locked worktrees do. Shoal refuses redirected paths, a checkout whose
Git config sets `core.bare` although it has a working tree, and deletions
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
again. Landed commits count as pushed for `rm` and automatic cleanup.

An agent's scoped `shoal land` needs the user's approval for the branch at its
current commit unless `[land] requires_approval = false`. The first run records a
`land` request in `shoal approvals` and exits 2 with `code: "approval_pending"`;
after approval, running it again lands that commit with every check above. A new
commit replaces the request with one for the new commit, approving a request
after its branch moved is refused as stale, and a denied commit stays refused
with `"approval_denied"`.

With `[land] push`, or `--push` for one run, Shoal pushes the default branch to
its upstream after merging; `--no-push` keeps one landing local. A default branch without an
upstream is refused before merging, and a failed push keeps the landed merge.

```toml
[land]
push = false             # Default: false
requires_approval = true # Default: true
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
copy_ignored = false                  # Skip copying the checkout's ignored files
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

Tracked setup starts with Worktrunk's `wt step copy-ignored`, which copies the
repository checkout's ignored files, such as `.env` or local signing settings,
into the workspace before `setup_cmd`. It runs without a `setup_cmd`, and a
failure fails setup. `copy_ignored = false`, also a global default, turns it off.
Files the workspace already has are kept, including paths cloned from a
[seed](#seed-builds). Without a `.worktreeinclude` in the checkout every
ignored file is copied, build caches included; its gitignore-style patterns limit
the copy, and `[step.copy-ignored] exclude` in `.config/wt.toml` narrows it further.

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
shoal rm fix-login --yes                    # Delete this workspace
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

### Build tool presets

`presets` names the build tools a repository uses, and Shoal sets each one up so
its workspaces stop repeating the same work. The global config's list applies to
every repository that does not name its own, and `presets = []` turns it off:

```toml
presets = ["cargo", "bun"]
```

| Preset | Sets up |
|---|---|
| `cargo` | A [seed](#seed-builds) running `cargo build --all-targets`, keeping `target` |
| `npm` | A seed running `npm ci`, keeping `node_modules` |
| `pnpm` | A seed running `pnpm install --frozen-lockfile`, keeping `node_modules` |
| `yarn` | A seed running `yarn install --frozen-lockfile`, keeping `node_modules` |
| `bun` | A seed running `bun install --frozen-lockfile`, keeping `node_modules` |
| `go` | `GOFLAGS=-trimpath`, and a seed running `go build ./...` |
| `xcode` | One compilation cache for every workspace's `xcodebuild`, with shared Swift package checkouts |
| `zig` | The shared cache `preset.zig` as `ZIG_LOCAL_CACHE_DIR`, and a seed running `zig build` |
| `ccache` | ccache as the compiler launcher of CMake builds, with the workspace as its base directory |

Presets that keep the same seed output cannot be combined. A variable `[env]`
sets takes precedence over a preset's. Go's build cache is shared by every
checkout, but it keys a package by its directory unless paths are trimmed; with
`-trimpath`, binaries, panics and `runtime.Caller` report module paths instead
of file system paths. The seed fills that cache from the default branch.

The `ccache` preset needs ccache installed and CMake 3.17 or later, and applies
when a build directory is first configured. With the workspace as ccache's base
directory and its working directory left out of the hash, a compile another
workspace ran hits, and debug info records that workspace's directory.

The `xcode` preset keeps a [shared cache](#shared-caches) named `preset.xcode` holding
an xcconfig, Xcode's compilation cache and Swift package checkouts. It sets
`XCODE_XCCONFIG_FILE` to the xcconfig, and puts an `xcodebuild` wrapper first on
the PATH of every process Shoal starts in a workspace and of `shoal env`. The
xcconfig turns compilation caching on and maps the workspace and its DerivedData
folder out of cache keys, so a workspace reuses what another compiled. Packages
checked out per workspace would still miss, so the wrapper passes
`-clonedSourcePackagesDirPath` keyed by the content of the `Package.resolved`
of the project it builds, unless the command passes its own. Mapping replaces
the workspace path in `#filePath` and debug info with `/^workspace`. Builds from
the Xcode app, root-level Swift packages (whose package name is the folder
name) and a configured `XCODE_XCCONFIG_FILE` do not use it. Each workspace
still keeps its own DerivedData, since Xcode copies cached results into it.
Removing a workspace deletes the DerivedData folder of every project inside it,
found by the `WorkspacePath` its `info.plist` records, in Xcode's custom
DerivedData location when that is absolute and in
`~/Library/Developer/Xcode/DerivedData` otherwise. It also deletes the package
checkouts that no build used for a day and no `Package.resolved` pins in the
registered checkout, an existing worktree or the default branch's tip.

### Seed builds

A repository can keep a build of its default branch that new workspaces start
from, so a branch rebuilds only what it changed and shares the rest on disk:

```toml
[seed]
cmd = "scripts/seed.sh"  # In the worktree; builds the default branch and must exit 0
paths = ["target"]       # Optional build outputs, relative to the worktree, new workspaces start with
```

[Presets](#build-tool-presets) add their builds and outputs to the seed; `cmd`
replaces the presets' commands and `paths` their outputs.

The daemon keeps a locked, detached worktree at the local default branch's tip in
the repository directory's `.seed`, and runs the build there when that branch or
the seed's settings change. It checks every minute, one repository at a time, and
waits while memory or free disk space is past its `[overload.warning]` threshold;
a build that failed is retried only after such a change or a daemon restart. The
settings come from the repository's saved config and its registered checkout's
file. `seed.cmd` is an executable inside the seed worktree, given as a relative
path and run like `setup_cmd`; preset commands run in the worktree's root. The
build gets the repository's `[env]`, with placeholders
rendered for the seed worktree, but no inherited `SHOAL_*` variables or agent secrets. It has a
two-hour limit and writes each command's output to `.seed/build.log` after a
`$` line naming it; anything a command leaves
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

### Shared caches

Tool caches that lock or address their entries by content can serve every
workspace of a repository at once:

```toml
[caches.go]
scope = "shared"  # Required; the only scope

[env]
GOCACHE = "{cache.go}"
SCCACHE_DIR = "{cache.sccache}"  # Needs its own [caches.sccache]
```

Each `[caches.NAME]` is a directory in the repository directory's
`.caches/shared`, which `{cache.NAME}` names in `[env]`, so every process Shoal
starts in a workspace, and the seed build, uses it. Names are lowercase like port
names. The daemon creates the directory when it starts a process or exports the
environment, and marks `.caches` with the repository, refusing to start one while
an unmarked `.caches` is in the way; registrations without a repository directory leave the field
literal. Build output directories such as Cargo's `target/` do not belong here:
concurrent builds serialize or corrupt them, so use a [seed](#seed-builds).

`shoal cleanup --caches` and [disk space](#overload-protection) cleanup empty a
repository's caches, configured or not, while none of its tracked executions and
no seed build of it is running; disk space cleanup records a `caches_emptied`
notification. Processes started with an exported environment are not tracked.
Removing the repository deletes `.caches`.

### Remove a workspace

Removal deletes the workspace's branch when a remote-tracking branch or the
default branch has every one of its commits, including squash and rebase merges
the default branch's remote-tracking branch knows about. A branch with commits
only it has stays, and so do the default branch and every branch while the
default branch is unknown. Removal never asks about the branch. Shoal deletes a
kept branch later, once a remote or the default branch has all its commits, unless
it moved, is checked out or belongs to a workspace again.
Uncommitted changes, untracked files and commits on a detached HEAD are deleted
only after `Are you sure? [y/N]`, which lists the changes with Git status codes
first, or with `--yes`. Large previews show an omitted-entry count; use
`git status` for the full list. Every removal of an existing worktree first keeps
them, with HEAD, in a [checkpoint](#checkpoints-and-restore).

Removal stops tracked commands and every verified process that carries the
workspace's identity, including those hooks and `shoal env` exports started,
leaving unrelated processes alone. Ownership and removal policy are checked again after removal hooks,
and the branch decision uses that last inspection. Ignored files go; shared caches
stay; Worktrunk hooks are disabled.
Git protects other checkouts. Failed ancestry or exact-ref checks stop removal
with the Git diagnostic, as they do branch selection and refresh; a missing ref
is a negative answer. Shell integration returns to `<root_dir>/<repo>`.

### Kept branches

```sh
shoal branch                         # List kept branches and their unpushed commits
shoal branch rm                      # Pick kept branches to delete; Tab selects several
shoal branch rm fix-login --yes      # Delete one kept branch and its unpushed commits
shoal branch rm --all --repo project # Delete every kept branch of a repository
```

`shoal branch` lists the branches removal kept and Shoal has not deleted yet,
with how many of their commits no remote-tracking ref or the default branch has,
and their last commit.
`shoal branch rm` deletes them with those commits after `Are you sure? [y/N]`, or
with `--yes`; it checks every branch first and deletes none when one has moved
since it was listed, is checked out or is no longer kept, or when its
repository's default branch is unknown. Name a branch kept in several repositories
with `--repo`. The removal's checkpoint still holds the commits until it expires.
Workspace processes cannot list or delete kept branches.

### Checkpoints and restore

```sh
shoal checkpoint --name "before migration"  # keep the current workspace's state
shoal restore topic    # pick one of topic's checkpoints, or open removed topic again
shoal restore topic 3  # return topic to checkpoint 3
shoal restore          # pick a removed workspace to open again
```

A checkpoint is a commit of HEAD plus the uncommitted and untracked files, without
ignored ones, under `refs/shoal/checkpoints/<commit>` in the
repository; taking one leaves the worktree and its index untouched. `shoal
checkpoint [workspace]` takes one, and Shoal takes one when an agent exits or is
stopped, before a restore to a checkpoint, and on every removal of an existing
worktree. An automatic checkpoint that would keep what the newest one keeps
extends that one instead. When the unsaved files exceed `archive_max_mb`, include
an untracked nested repository or a changed submodule, whose files it cannot hold,
the checkpoint keeps HEAD only. Checkpoints expire after `retention_days`; their
numbers are never reused, so a number names one checkpoint for good.

`shoal restore <workspace> <number>` keeps the current state in a new checkpoint,
then moves the workspace's branch to the checkpoint's HEAD and puts its files back
as uncommitted changes, deleting tracked changes and untracked files it lacks.
It refuses while an execution other than the caller's own runs in the workspace,
during a Git operation, on another branch than the checkpoint's, when a
checkpoint could not keep the unsaved files, or when the checkpoint has a path at,
inside or around an ignored file, which it would delete or overwrite and no
checkpoint keeps. Workspace processes may list and
restore their own workspace's checkpoints.

`shoal restore` and `shoal restore <workspace>` open a picker of the removed
workspaces or of an existing workspace's checkpoints and restore the one picked;
without a terminal, or with `--json`, they list them instead. When
`<workspace>` names a removed one, it opens the newest removed workspace with that name or ID on its branch and recorded base, which restores its stack link while the base
workspace exists, recreating the branch at the removed HEAD when it was deleted,
puts its unsaved files back as uncommitted changes, links its issue and its
opened and related items, saves the agents stopped by the removal or before it
and the last session for `shoal resume`, runs setup, links its PRs, and keeps its checkpoints; a link or save that fails is
a warning. It refuses when another
workspace uses the branch, when the branch no longer contains the removed HEAD,
or when it moved past HEAD while unsaved files would have to be applied to it;
the error names the commit to recover by hand. Workspace processes cannot reopen removed workspaces.

Removal records the branch, base, links, stopped agents and last session with its checkpoint, and
keeps them until the checkpoint expires. Links cannot change once removal
starts, so the record holds the final ones. Removal stops agents as `shoal stop`
does, keeping their recovery records in the removal record; interrupted commands
are not saved. When the removal's checkpoint keeps HEAD only, merged cleanup keeps the workspace instead.

```toml
[cleanup.restore]
retention_days = 30
archive_max_mb = 50
```

### Move a workspace to another machine

```sh
shoal export topic -o topic.shoal         # remove topic and write it to a file
shoal import topic.shoal                  # open it in the repository with the same origin
shoal export topic -o - | ssh other shoal import -
shoal export --repo app --all -o moving   # remove every workspace of app, a file each
shoal import moving                       # open every workspace exported to moving
```

`shoal export [workspace] -o <file>` removes the workspace as `shoal rm` does and
writes its removal record and checkpoint to a new file, or to standard output for
`-`. The daemon writes the file, so it is complete even when the terminal closes
with the workspace, as its Herdr tab does. It refuses before removing anything
when the file exists or lies inside the workspace, when the daemon cannot create
a file in its directory within 30 seconds, when a checkpoint cannot keep
the uncommitted files, or when `-` would stream from a Herdr tab that closes with
the workspace, and asks before releasing holds unless given
`--yes`. A workspace already removed is exported from its removal record without
another removal. The checkpoint travels as a Git bundle without the commits of
the default remote's default branch; a checkpoint that branch already has
travels without one. Ignored files, leases, permits, approvals, grants and holds
stay behind; setup rebuilds what the workspace needs.

`shoal export --repo <repository> --all -o <directory>` exports every workspace
of the repository to `<workspace>.shoal` in the directory, creating it when
missing. It checks every workspace as a single export does, that none is in the
middle of another lifecycle operation, and that the terminal does not close with
one of them, before removing any: when one cannot
be exported, it lists each refusal and removes nothing. It asks once before
releasing all of their holds, then exports each workspace and lists the files it
wrote; a failure there leaves the remaining ones to export, and the command
exits 1 naming each failed workspace.

`shoal import <file>` reads an export, or standard input for `-`, and opens it in
the registered repository with the exported repository's origin, or the one
`--repo` names. Given several files, or a directory, whose `.shoal` files it
reads in name order, it imports each one, going on after a failure, lists every
workspace it opened and exits 1 naming each file it could not import or whose
`post_setup_cmd` failed. It fetches the default branch when the repository lacks the
commit the bundle builds on, records the workspace as removed with its
checkpoint, then restores it as `shoal restore` does; when that restore fails,
`shoal restore <workspace>` retries it. On the machine that exported it, `shoal
resume` continues each agent's reported session by ID, which Claude Code and
Codex find from any directory; Shoal leaves the agents' session files alone.
`--keep-sessions` keeps the sessions on any machine, such as one they were
copied to, and `--new-sessions` on none. Every reopen, by import or `shoal
restore`, trusts the workspace's path for the agents that resume there. An agent
without a kept session `shoal resume` starts as `shoal claude` or `shoal codex`
starts a new one, telling it the workspace moved, instead of running
`[agent_resume]`. Workspace processes can neither export nor import.

### Assignment completion

`shoal done [workspace]` records that the assignment is finished and notifies the
user. `[done] cleanup` defaults to true; `--cleanup` requests cleanup when it is
false. The setting follows normal repository and worktree configuration
precedence. Scoped agents may mark only their own workspace or a child workspace done; completion does
not verify or assert a merge. Completion without cleanup keeps the workspace from
idle and PR cleanup, not from [merged cleanup](#merged-workspaces). Removal is
reversible with [`shoal restore`](#checkpoints-and-restore); to review in a workspace,
[hold](#workspace-holds) it. `done` closes the workspace's open
[questions](#questions), so completion and PR cleanup wait only while a hold, a
question asked afterwards or a [PR proposal](#approval-for-agent-issues-and-prs)
remains.

Only `done` completes an assignment; issue closure and merged PRs do not. A
merged workspace nobody uses is removed by [merged cleanup](#merged-workspaces)
whether or not its agent ran `done`.

```toml
[done]
cleanup = true    # Default: true
```

`shoal undone [workspace]` withdraws a recorded completion; `--json` returns
`{"withdrawn": <completion>}`, with null when none was recorded. Scoped agents may mark only their own workspace or a child workspace undone.
Issue associations and PR watches remain registered. It does not keep the workspace:
cleanup proceeds as if `done` had not run.

`post_done_cmd` runs in the daemon after recording completion, before cleanup,
with the worktree as its working directory, no terminal, and a 60-second limit.
It follows normal global and repository configuration precedence and receives the
[hook identity environment](#workspace-setup-and-hooks) plus
`SHOAL_DONE_CHOICE=keep` or `cleanup`. It runs even when cleanup retains the
workspace. Failure records
a `hook_failed` notification without undoing completion or blocking cleanup.
Lifecycle and permit changes are rejected while it runs. The hook holds the
completion/PR gate, so it must not call Shoal commands that change completion or
PR registrations, including in other workspaces. Each `done` runs it again; daemon
restart does not replay it.

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

Issue associations suppress idle cleanup, not merged cleanup.

Cleanup runs in the daemon without an idle delay; tracked agent exits trigger a
sweep after their exit hook instead of waiting for the next poll. It stops tracked
commands using normal branch retention and resource release. Without a PR registration,
files must be clean and all commits pushed or merged; an
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
while the worktree exists, as open questions, pending PR proposals, child workspaces and queued agent launches do; releasing the last hold lets cleanup recheck its usual
conditions. Idle cleanup restarts its
timer after release. Explicit removal and deleted-directory cleanup release holds.
Upgrading turns a `shoal continue` from earlier versions into a hold named
`continue` with the reason “Converted from an earlier Shoal version”, which the
next explicit `done` releases.

Reacquiring a name returns the original hold, including its reason. Acquisition
requires a ready, verified worktree and is excluded while a resource hook runs.
Release remains available while the workspace record exists. Holds belong to the
user and to apps hosting external sessions: scoped callers can list their own
workspace's holds but not acquire or release them. `status`, `inspect`,
and `work --json` show holds on the workspace record. `rm` lists holders before
confirmation and includes them in its JSON result; `--yes` skips confirmation.

### Forges

Shoal reads and changes the issues and PRs of the forge serving a repository's
`origin`. `github.com` is GitHub, `gitlab.com` GitLab and `bitbucket.org`
Bitbucket Cloud. The daemon asks any other host once which API it serves, through
GitHub's `meta` with `gh`, GitLab's `version` with `glab`, and Forgejo's or
Gitea's `version` with the login below, and records the answer; GitHub Enterprise
Server and self-hosted GitLab need the CLI logged in to the host: asking sends
only a login saved for that host, never a token such as `GH_ENTERPRISE_TOKEN` or
`GITLAB_TOKEN` that the CLI would send to any host. `shoal doctor`
asks recorded hosts again. A host that answers as none of them fails until you
set its type, which also stops Shoal asking it:

```toml
[forges."git.example.com"]
type = "gitlab"   # or "github", "forgejo" (also Gitea) or "bitbucket"
```

Lookups use
your login: `gh` on GitHub, `glab` on GitLab, on Forgejo or Gitea the token `fj`
saved for the host, else `tea`'s login through `tea api`, and on Bitbucket the
token `[bitbucket]` configures. Without a login, only public repositories are
readable. Shoal stores no forge credentials. An issue or PR number may be
written `#42`.

Forges differ in what Shoal can do there. `shoal capabilities` lists each
capability of the workspace repository's forge, such as resolving review
threads, rerunning CI jobs, labels or a merge method, and whether the forge has
it, with what to do instead when it does not; `--json` returns `forge` and
`capabilities`, each with `capability`, `supported` and an optional `hint`. A
command that needs a capability the forge lacks fails before sending anything,
naming the forge and the capability.

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
`merge` uses the project's merge method, squashed with `--method squash`. CI
checks are the latest status of each job and external check on the head
commit, and reviews are reviewers' states and approvals; approval rules and
merge trains are not modeled.

Bitbucket repositories live under a workspace and its PR URLs use
`/pull-requests/`. It reports no merge conflicts. Reviewers are Atlassian
account IDs, declining a PR closes it, and `--method rebase` rebases and
fast-forwards. CI checks are the builds reported on the PR, and reviews are
participants' approvals and change requests. Agents' changes use your Bitbucket credentials.

### Issue and PR actions

```sh
shoal pr open --label area/cli      # push, then link the branch's open PR or open one
shoal pr edit --title "Fix login" --add-label bug --draft
shoal pr comment --body-file notes.md
shoal pr comment --body-file notes.md --attach shot.png --attach 'demo.mp4#Login flow'
shoal pr reply 812 --body-file why.md   # in the thread of review comment 812
shoal pr resolve 812 --body-file why.md # reply, then resolve; unresolve reopens
shoal pr close 12                   # reopen works the same way
shoal pr merge --method rebase --delete-branch
shoal issue list --label bug flaky  # search by text; --state, --author and --limit too
shoal pr list --state all --author dev
shoal issue edit --add-label bug    # comment, close and reopen work too
shoal issue open --title "Flaky watch test" --label bug
shoal labels                        # the repository's labels with their descriptions
shoal capabilities                  # what the repository's forge supports
```

`shoal pr open` verifies the workspace's worktree identity and that it is on its
recorded branch, then pushes the branch without forcing to its push remote
(`branch.<name>.pushRemote`, else `remote.pushDefault`, else `origin`), setting
its upstream, as a tracked command with your Git configuration and hooks and
its output on stderr. The daemon then links the open PR whose head is that
branch in the repository the push remote names, or opens one in the `origin`
repository and links it. A push remote on the same forge host other than
`origin` is a fork the PR comes from; GitLab merge requests from a fork are
refused. A new PR's title is
`--title`, else the linked issue's title, else the last commit's subject; its
description is the supplied `--body` or `--body-file`, or `Closes #<number>`
when no body is supplied for a linked issue; its base is `--base`, else the base
workspace's branch, else the default branch. It is a draft with `--draft` or
`[pr] draft = true`, which `--no-draft` overrides. Draft state, repeated
`--label` and repeated `--reviewer` apply only to a new PR. Its JSON adds
`created` (false for a PR it found), `linked`, and the
[convention](#repository-conventions) `warnings` it prints on stderr.

```toml
[pr]
draft = false # Default: false
required_labels = ["area/", "type/"] # Default: []
```

`shoal pr` and `shoal issue` change an item of the workspace's repository: the
number or URL given, otherwise the linked issue or the only linked PR. `edit`
takes `--title`, `--body` or `--body-file`, and repeated `--add-label` and
`--remove-label`; PRs also take `--base`, repeated `--add-reviewer` and
`--remove-reviewer`, and `--draft` or `--no-draft`. Edits apply in that order,
and a failed request stops the rest with the forge's message. Labels are
replaced as a set by name; Forgejo's draft state is the title's `WIP: ` prefix,
and GitLab's its `Draft: ` prefix.
`open`, `edit` and `comment` also take repeated `--attach FILE` or
`--attach 'FILE#ALT TEXT'`: a PNG, JPEG, GIF, WebP or SVG image of at most
10 MiB, or an MP4, MOV or WebM video of at most 100 MiB, checked before anything
is sent. Each file is uploaded through the forge and shown in place of the
body's image references to it, such as `![The error](shot.png)` relative to the
body file's directory or the working directory, and otherwise after the body;
`edit` without a new description adds them after the current one. GitHub and
GitLab keep uploads with the repository. Forgejo and Gitea keep them with the
item or comment, so a new comment or item is first posted naming the files,
then updated to show them; Shoal first refuses a file the instance's attachment
settings would refuse, by type or size. It resends an upload up to three times
while the connection fails before sending it or the forge answers 503, and a
comment's update also after a broken connection or another 5xx; a 429 is the
forge's [rate limit](#linked-items-and-cleanup), which holds back requests until it ends.
GitHub needs write access to the repository and Forgejo
an fj login.
`reply`, `resolve` and `unresolve` take the ID of any comment in a review
thread, as `shoal view` shows it, before the PR's number or URL. `reply` takes
`--body` or `--body-file`; `resolve` posts one as a reply first when given.
Resolving a thread that has no reply asks for confirmation; `--yes` skips it.
`merge` takes `--method merge|rebase|squash`; `--delete-branch` deletes the
PR's branch on the forge when it is in the repository, never a local branch.
`issue list` and `pr list` list the repository's issues or PRs, newest first,
through the forge's own search: words after the command are text it searches for
in titles and descriptions, repeated `--label` keeps items with every label,
`--author` those a login opened, and `--state open|closed|all` defaults to open,
with merged PRs counted as closed. `--limit` caps the list at 1 to 1000 items,
30 by default. Workspace processes search as the agent account, anyone else
with your own logins. Each line shows an item's number, state (`draft` for an
open draft PR), title and labels; JSON lists the items with the fields every
other item command reports. Bitbucket's `--author` matches a nickname.
`issue open` takes `--title`, `--body` or `--body-file`, and repeated `--label`;
the new issue is recorded as opened, and assigned only with `--link`, which fails
while another issue is assigned. A workspace being removed refuses it. Its JSON
adds `created` and `linked`, and `warnings`, on stderr too, with the
[convention](#repository-conventions) warnings and when removal started before
the workspace could record and link the new issue.

#### Approval for agent issues and PRs

A repository whose issues and PRs other people see can require your approval
before an agent opens one. `"direct"`, the default, opens at once; `"propose"`
makes `issue open` or `pr open` in a workspace process record an
[approval request](#resource-approvals) instead. Unscoped callers always open
directly.

```toml
[approvals]
issue_open = "propose" # Default: "direct"
pr_open = "propose" # Default: "direct"
```

A proposed `issue open` records every option it was given, with the workspace,
branch, commit and agent; running it again unchanged returns the same request.
The request belongs to the repository, so it stays when the workspace is removed.
A proposed `pr open` pushes the branch as usual, links the branch's open PR if
there is one, and otherwise records the PR it would open, with its defaults
filled in, at the branch's current commit. Running it again at the same commit
returns that request; at a newer commit it replaces it, and the old one stays
`outdated`. A pending PR request keeps the workspace from automatic cleanup.
Files given with `--attach` are copied into Shoal's state directory, since the
agent's copies may be gone when you decide, and deleted once no pending proposal
uses them, by the next decision or proposal or by the cleanup sweep. Both commands exit 0 with JSON `proposed`, the request, and the agent
continues its work. Only you decide proposals; they are never routed to a
coordinating agent, even in a child workspace.

```sh
shoal approvals                     # proposals show their labels and description
shoal approve 4 --title "Fix login timeout" --body-file edited.md
shoal deny 5 --reason "Not a bug"
shoal deny 6 --duplicate-of 120     # an existing issue's number or URL
shoal deny 7 --duplicate-of-approval 4
```

`approve` opens the item as the workspace's agent account, with `--title`,
`--body` or `--body-file` replacing what was proposed. An issue is recorded as
opened by its workspace and assigned when it was proposed with `--link` and no
issue is assigned yet; after the workspace is removed, it opens through the
repository's checkout with the repository's agent account. A PR is linked
like one `pr open` opened. Approving a PR request whose branch was renamed, or has
another commit locally or where `pr open` pushed it, is refused, the request
becomes `outdated`, and the agent is told to run `pr open` again. When labeling
the PR, requesting reviews or attaching its files fails after it opened, the
request keeps the PR, and approving it again finishes that work. `deny` drops the proposal, with an optional `--reason` for the agent.
`--duplicate-of` denies an issue proposal and adds its title and description as a
comment on that issue; `--duplicate-of-approval` keeps them with another pending
issue proposal, which adds them as comments when it is approved and drops them
when it is denied. When anything fails after the issue opened, such as adding its
labels, its files or a duplicate's comment, the request keeps the issue and the
work left, so approving it again, without edits, finishes that work instead of
opening another issue. A comment that reached the forge is never posted again:
when attaching its files or reading the issue afterwards fails, the decision
finishes and reports the failure in `warnings`. Approving also returns the
[convention](#repository-conventions) warnings that opening prints; required
labels were checked when the item was proposed. A decided proposal is no longer listed, and the decision
reaches the workspace's agent as an [agent message](#agent-messages) naming
the opened item.

#### Repository conventions

`shoal labels` lists the workspace repository's labels as the forge reports them,
with each one's description, and `exclusive` for a label that allows no other
of its scope: Forgejo's exclusive labels and GitLab's `key::value` labels.

`[pr] required_labels` and `[issue] required_labels` name entries that every new
item's labels must each start with, so `area/` takes any area label. `pr open`
and `issue open` refuse a new item missing one before creating it, listing the
repository's labels that would satisfy it; an empty list in a higher layer turns
inherited entries off.

```toml
[issue]
required_labels = ["type/"] # Default: []
```

Once the item exists, `pr open`, `issue open` and `ready` warn, without failing,
about an item missing a required label, or about one without labels in a
repository that has some. `pr open` and `issue open` also warn when a
description they sent lacks a heading of the forge's description template in the
worktree: for PRs `pull_request_template.md` in `.forgejo`, `.gitea`, `.github`,
the root or `docs`, or GitLab's `.gitlab/merge_request_templates/Default.md`; for
issues the Markdown files of the first `ISSUE_TEMPLATE` directory or
`issue_template.md` there, or GitLab's `.gitlab/issue_templates`, where a
description that has every heading of any template passes. Names match without
regard to case, front matter is skipped, and a template without headings asks for
nothing. A warning names the `edit` command that fixes it, and one whose lookup
fails is left out.

The daemon sends each change through the forge's REST API as the workspace's
[agent account](#agent-accounts), except `merge`, which uses your login and is
refused to processes inside a workspace execution. Each change records an
`item_changed` [workspace event](#workspace-events). `--json` returns the item
as the forge then reports it: `kind`, `number`, `url`, `title`, `state` (`open`,
`closed` or `merged`), `labels`, and for a PR, `pr` with `head`, `base` and
`draft`.

```sh
shoal ci rerun                      # rerun every failed job on the linked PR's head
shoal ci cancel 12 --check rust     # cancel one running check of PR 12
```

`shoal ci rerun` reruns failed jobs on the current head of the linked PR or the
one named, and `shoal ci cancel` cancels running ones: every such CI job, leaving
other providers' checks alone, or each repeated `--check` by the name `shoal view`
shows, which must be a CI job in that state. The daemon uses the forge's job API
as the agent account and records an `item_changed` event once a request succeeds;
a failed request stops the rest. GitHub reruns a failed Actions job, or every
failed job of a run when several of its jobs are selected, because a rerun starts
a new attempt of the run, refuses a rerun while another job of the run is still
running, and cancels the job's run; GitLab retries or cancels each job; Forgejo
cancels each check's run. `--json` returns `url`, `action` and the
`checks` acted on.

### Linked items and cleanup

`shoal link <number-or-url> <relation>` associates an issue or PR with the
workspace; `shoal link issue <number-or-url> <relation>` and `shoal link pr
<number-or-url> <relation>` select the kind explicitly, and omitting the item picks
an open one. The relation is required:

| Relation | Effect |
|---|---|
| `assigned` | The work the workspace exists for: the rules below and in [ready for review](#ready-for-review) apply |
| `opened` | Records that the workspace created the item; `issue open` and `pr open` record it themselves |
| `related` | Records an item the work relates to |

Opened and related items have no watch, completion or cleanup effect; `status pr
<number>` finds the workspaces that link an item under any relation. Commands that select items this way give a number without a
kind the kind of the linked item with that number, otherwise the kind the forge
reports; GitLab and Bitbucket number issues and PRs separately, so there such a
number must be linked. Use `--workspace` to select a workspace instead of the current
context. Numbers resolve against its origin and URLs must match it. Links are
idempotent; an assigned issue must be unlinked before assigning a different one.
Linking an issue records its title for workspace lists; when the forge cannot be
read, the link is recorded without one and a warning names the error.
Assigned PRs accumulate without duplicates and must name the recorded workspace
branch. Assigned items suppress idle cleanup.

`shoal view` shows linked items as the forge reports them now: title, state,
author, labels, description, comments, and reviews with their inline comments; a
PR adds its branches and the merge conflicts, CI checks and review state `status`
reports, plus the last 8 KiB of each failed check's log, where the forge
[reads job logs](#forges), and its
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
with `fj` or the web interface, is reported. A PR's conflict is reported once
two polls in a row see it at the same head and base commits, and again when it
persists across new ones.
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
unbounded. A watch survives daemon restarts, waiting up to two minutes for a
daemon that went away to return and know the caller's scope again, or 10 seconds
for one that was not running. Expiration returns an empty array and `timed_out: true`. A newer watch
in the same workspace
supersedes a running one, which returns an empty array and `superseded: true`
without taking updates; updates acknowledged while a newer watch runs stay pending
for it. Scoped callers can watch only in their own workspace and its child
workspaces; a watch also reports the activity its agent receives from
[child workspaces](#child-workspaces). Activity polling continues when PR cleanup is disabled. Lookup failures report errors or `lookup_failed` updates; a source
that keeps failing with the same error is reported again every 10 minutes. A
forge that refuses a request for its rate limit (`429`, or `403` with
`Retry-After` or no requests remaining), or reports no requests remaining, is
not asked again as that account until its `Retry-After` or reset time, at most
an hour away. Polls wait until then instead of reporting lookup failures, and
the daemon records a `forge_limited` notification. Forgejo
comment and review changes are grouped; CI results are the commit statuses of the
PR's head commit, read with fj's login for the host when it has one. The daemon
repeats a REST read as a conditional request with the `ETag` and `Last-Modified`
of the last response it got for that URL as the same account, and takes a
`304 Not Modified` to mean that response still holds; it keeps them in memory
only. Reads through tea and GitHub's `gh pr view` are sent in full.
Failed Forgejo checks include the failing step when the runner records it and
a command to read the log; a log lookup failure stays on that check's update.

Once every linked PR has merged and at least one contains current HEAD, or the
freshly fetched default branch has its changes, as after a rebase that followed
the merge, a workspace marked `done` is cleaned up; PRs are looked up only after `done`. A
confirmed set survives restart without querying again. Cleanup uses normal branch retention and resource release, retaining dirty
or newer work. `inspect` shows lookup and removal
errors in `pr_cleanup`. Invalid registrations retain the workspace.

`shoal unlink <relation>` removes the relation's items; `unlink pr <relation>` or
`unlink issue <relation>` removes one kind, and an explicit number or URL removes
just that item. Removing the last
link does not mark done or undo recorded completion. `[cleanup.pr] enabled = false` pauses PR completion independently of idle
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
ask for attention. New commits make a mark outdated: `work` shows
`ready (outdated)`, and `status`, `inspect` and `work --json` list
`review` marks with `kind`, `url`, `head`, `created_at` and `stale`.
`shoal inbox` lists every current mark across repositories. Unlinking an
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
unlinking leaves the PR as it is. `shoal ready` then adds each marked item's
label [convention warnings](#repository-conventions) to its mark as `warnings`
and prints them on stderr; they are never stored.

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
instead of `waiting`. Other agents can call the command from their own hooks; a JSON event
on stdin with a `session_id`, as Claude Code and Codex hooks pass, names the session
to [resume](#overload-protection). The command reads only that first event, and stops waiting after
two seconds without input. `work` and `status` show
`working`, `needs input` for `waiting`, or `waiting` for `idle`, since an idle
agent waits for the user or for activity that [wakes it](#agent-messages); `status`,
`inspect` and `work --json` give `agent_state` with `state` and the Unix-seconds
`since` it began. A state
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
simulator leases, permits, holds, open [questions](#questions), pending PR proposals and failed checks block cleanup. Pushed means reachable
from locally known remote branches, or merged into the default branch as removal
decides; no fetch.
Sweeps run about every 30 seconds; a failed step does not skip the others, and
timers reset on restart. The daemon needs `lsof` on PATH.

#### Grace period

Every automatic removal except for low disk space waits `grace_minutes` from the
first sweep that would remove the workspace, so its agent keeps running while you
decide to take the work back: idle, merged and PR cleanup, and cleanup after
`shoal done`. A workspace that would not pass removal's checks, such as one with
unsaved work after `done`, does not start it. `shoal status` shows when the grace
period ends; a sweep that no longer removes the workspace, as after `undone`, a
new commit or new activity, ends it, and a later attempt starts a new one. The end
time survives restarts, except that idle cleanup starts over with its timers. `shoal rm`, `shoal cleanup` and disk-space removal do not wait.

```toml
[cleanup]
grace_minutes = 10 # Default: 10; 0 removes at once
```

#### Merged workspaces

A workspace whose branch merged into the default branch is removed once nothing
has used it for `idle_minutes`, whether or not its agent ran `shoal done` or it
has linked issues and PRs. Merged has the meaning [removal](#remove-a-workspace)
gives it, against the default branch's remote-tracking branch, fetched at most
every ten minutes; until a fetch succeeds, the repository's workspaces are kept. Uncommitted and untracked files go into the
[removal's checkpoint](#checkpoints-and-restore); they keep the workspace when the checkpoint cannot keep
them all or the branch has no commits of its own. A merge, rebase or other
Git operation under way, open [questions](#questions), commands, agents saved for
`shoal resume`, directory users, leases, permits, holds and
workspaces stacked on it keep it too; PR cleanup moves stacked workspaces once the base's PR merges. Shoal records the
time of each command start and exit, lease and hold change, and takes the newest
change time of the worktree's files, including ignored ones, and of its HEAD,
index and reflog; the period runs from the latest of those and survives restarts.
Removals record the `merged` event cause.

```toml
[cleanup.merged]
enabled = true
idle_minutes = 240
```

`shoal cleanup` removes idle and merged workspaces now, without their idle
periods and also where those rules are disabled, and reports any it retained; `--dry-run` lists them
instead, with the reason `status` gives for keeping each other workspace. `--caches`
also empties [shared caches](#shared-caches) nothing is using, reporting each
repository's `caches` with the `outcome` `emptied`, `busy` or `failed`. Removals record the `manual` event cause. Workspace processes cannot run it.

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
`review_cleared`, `linked`, `unlinked`, `base_changed`, `agent_state`, `item_changed`, `item_activity` and `chat_changed`; history from earlier versions may also contain `continued`. Review events add a `review` object with the mark's `kind`,
`url` (both null for a workspace mark) and `head`. Link events add a `link`
object with the linked issue or PR's `kind` and canonical `url`. Agent state
events add `agent_state` with the reported `state`, null when its execution ended.
`item_changed` events add an `item` object with the `kind`, `url` and `action`
of a change Shoal made to an issue or PR. `item_activity` events add an
`item_activity` object with the item's `kind` and `url`, and an `update` object
with the observed update's `kind` and `message`. `created` and `base_changed`
events add `base_workspace` with the base's `id`, `name` and `branch`, or null.
`chat_changed` events add `chat`, the ID of the [chat](#chats) that opened the
workspace, or null once it returned to you.
Causes are `manual`, `idle`, `issue`, `pr` (including a base workspace's merged
PRs), `completion`, `missing_directory`, `disk_space`, or `removed` for a base workspace's removal, and null
when inapplicable; `error` describes setup or cleanup failures. Branch changes
are observed during daemon sweeps; detached HEAD has a null branch, and the
recorded workspace branch remains unchanged.

A `type: "chat"` line records a chat's start or end: its `id`, `created_at`,
`kind` (`chat_started` or `chat_ended`) and a `chat` object with its `id`,
`repository_id`, `number`, `agent`, `pid` and Unix-seconds `started_at`.

A `type: "gap"` line gives `since`, `oldest_id`, and `latest_id` when a cursor
falls outside retained history, then replay continues with retained events.
Resync with `shoal work --json` after a gap or on first connection: the journal
starts when the daemon upgrades and does not reconstruct older changes.

### Notifications

```sh
shoal notifications            # Oldest new ones first, then marked read; says how many remain
shoal notifications --all      # Recent ones including read (--limit, default 50)
shoal notifications --follow   # Keep printing, and raise terminal notifications
shoal notifications --keep-unread  # Show without marking read, also with --follow
shoal notifications clear      # Delete all of them, including unread ones
shoal notify "PR #12 is ready to merge" [--workspace W]  # Add one for a workspace
shoal notifications withdraw <number> [--workspace W]   # Take back an unread one notify added
```

The daemon records what happens while you are not looking: a resource or
simulator request that found no capacity (naming the workspaces holding the
pool), a preferred port in use, a tracked agent
exiting (with its code, or a note to run `doctor` when it left processes
behind), [agent queue](#agent-queue) starts and failures, [seed build](#seed-builds) failures, [shared caches](#shared-caches) it emptied, a [sleep inhibitor](#keeping-the-machine-awake) that failed, and workspaces it removed or retained on its own through PR, merge, or
idle cleanup, and automatic cleanup that starts failing. `shoal notify` adds a one-line message (at most 512 bytes, without
control characters) for a workspace; agents use it to ask for attention without
marking the assignment done. It prints the notification's number, or that a
[coordinating agent](#child-workspaces) received the message instead; the
workspace may withdraw its message by that number until it is shown. Each line shows the local time, the workspace, and the message;
`--json` returns records with `kind`, `created_at`, and `read`. On a terminal,
`--follow` also sends each entry as an OSC 9 terminal notification, which iTerm2,
Ghostty, WezTerm, and Kitty show as a desktop notification (tmux needs
`allow-passthrough`); other terminals ignore it. Repeated
identical conflicts, cleanup failures and messages collapse into one entry until read;
every agent exit is listed. `shoal work` and `daemon status` mention pending
ones. Plain `exec` commands and manual `rm` are your own and record nothing.
Scoped commands cannot read or clear notifications but may notify about their own
workspace and withdraw those messages. Read entries older than the newest
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
exits 2, so Claude Code starts the idle agent's next turn with it. A failed
wait, such as one a restarted daemon refuses before the agent's wrapper
reattaches, is tried again after a delay that doubles up to a minute, so the
agent is never left idle without one. Its command
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
`shoal chat` registers the same message hooks, and for Claude Code the wake
hook, without turn-state hooks. Under a chat, outside any workspace scope, they
and `shoal messages` deliver the chat's messages about the workspaces it
[coordinates](#child-workspaces); a Codex chat receives them after its next tool
call or prompt.
An identical message that is still unread is not queued again, and a workspace
or chat holds at most 50 unread messages of 25,600 bytes in total. Scoped commands may read their own workspace's
messages and send messages only to the agents of the [workspaces below their own](#child-workspaces). Removing the workspace deletes its messages.

### Questions

```sh
shoal ask "Which database?" --recommend SQLite [--reason R] [--choice C]... [--wait]
shoal questions [workspace]          # Open questions
shoal questions withdraw <number>    # Close a question the agent no longer needs answered
shoal answer [number] [answer]       # Answer text or a choice number; omitted, pick and type
```

`shoal ask` records a question for the user from a workspace's agents: the
decision, the recommended answer, an optional reason and optional choices, each
one line with the same limits as `notify`. It raises a `question_asked`
notification and prints the question number. `shoal answer` records the answer,
resolving a choice number to that choice, and queues it for the workspace's agents
as a message naming the question, so the [agent messages](#agent-messages) hooks
deliver it and wake an idle agent; it confirms with the question text, since a
stale notification may name a reused number. An answer may span lines, up to 4096 bytes.
Delivering the answer deletes the question. A new question takes the smallest
number no stored question holds, across workspaces; without a number,
`answer` picks among open questions, showing each one's workspace, recommendation
and choices, and refuses the answer if another question took the number meanwhile.
`--wait` blocks without a time limit until the answer arrives and prints it, which
also delivers its message; it survives daemon restarts, waiting up to two minutes
for a daemon that went away to return and know the caller's scope again, or 10
seconds for one that was not running. Asking an identical
question again returns it while it is open or its answer is undelivered, so an
interrupted `--wait` can be rerun; the same question with other details is refused.
A listing returns the newest open questions that fit one response and counts the older
ones it leaves out. Only unscoped callers answer, except that a workspace process
answers, by number, the questions asked in the [workspaces below its own](#child-workspaces).
Scoped callers ask, list and withdraw only their own workspace's questions, or a
child's with `--workspace`. An open question keeps the workspace from idle
cleanup; removing the workspace deletes its questions. `shoal done` closes the
workspace's open questions, and `withdraw` closes one of them: answers to a closed
question are refused, waits on it fail, and `shoal undone` does not reopen it.
Withdrawing deletes the question's `question_asked` notification while it is
unread, and otherwise raises `question_withdrawn`. A closed question keeps its
number until the workspace goes.

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
<name>` goes directly, and `shoal cd -` switches back to the workspace it last
left or entered. Scoped agents cannot navigate outside their worktree. Without
the function, Shoal prints the destination and, in an interactive terminal,
explains how to load the integration;
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
and scope with a 500 ms timeout, never starting a daemon or picker. A workspace
the command would refuse for its state, or for `setup` a running execution, is
described by the reason; one without what the command acts on is left out. Targets sort before flags,
also in fzf-tab. `shoal completions <shell>` prints scripts for
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
the daemon's PATH, whether the daemon can create a file in `root_dir`,
registered checkouts whose Git config sets `core.bare`
although they have a working tree, Git worktrees under registered repository
roots that Shoal does not track, and shell integration in the calling shell.
Each forge host the daemon recorded is asked again which API it serves
(`forge:<host>`): a new answer replaces the recorded one, and a host that
cannot be asked keeps it with a warning; a host whose type is set in
`[forges]` is not asked. They run regardless of the workspace selection and
otherwise only diagnose, even with `--repair`. An unavailable or mismatched
daemon leaves its checks marked as skipped (`dependency:<tool>`, `checkout:*`,
`forge:*` and `worktrees:*` when repository names are unavailable); doctor
never starts or restarts it.
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
before forced termination, even if its leader exits first. When an agent exits,
Shoal also stops its verified processes outside its process group, such as
background shell commands. Detection is cooperative:
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

PR watches, merge acknowledgements, `notify`, `messages`, `ask`, `questions`,
`ready`, `unready` and `internal agent-state` are own-workspace scope exceptions.
Processes carrying a Shoal scope token are confined to their own worktree:
`status`, inspect, execute, `merge`, `diff`, `setup`, and resources. They may read
effective configuration for their own workspace, but cannot change configuration.
They `land` only with [approval](#land-into-the-default-branch) and cannot reach other worktrees, create or remove workspaces, read
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
`{workspace_path}` (also `{path}`), `{repo}`, `{branch}`, reserved `{port.NAME}` and
[`{cache.NAME}`](#shared-caches) fields once. Unknown fields remain literal, inserted values are not expanded, and
`SHOAL_*` names are reserved for Shoal. Names must be ASCII identifiers; values
must be strings without NUL. An empty string sets an empty value; inherited
variables and shell expressions are not expanded. Reserved port exports take
precedence over `[env]`. Desktop handoffs may reuse an existing process and do
not receive these variables.
Only unscoped callers can export or revoke tokens, and export requires a ready,
verified worktree. Add the returned variables to the processes your app starts.
Each exported token survives daemon restarts until `shoal env [workspace] --revoke
<token>` or successful workspace removal; revoking one leaves other tokens valid.
Running processes retain their port snapshot. These processes are untracked, the
token provides no cleanup protection, and removing the workspace stops them.

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
these requests, land requests from a scoped `shoal land`,
[proposed issues and PRs](#approval-for-agent-issues-and-prs), and retained workspace grants; `shoal approvals <workspace>` filters
by workspace. Scoped callers can see only their own requests. An unscoped user
reviews the recorded settings and uses `shoal approve [number]` or
`shoal deny [number]`; without a number, they pick among pending requests, shown
with their workspace, reason and settings, and a request that took the chosen
number meanwhile is refused. A request takes the smallest number no
stored request holds. A keep or land request that no longer applies, because a request
for a newer commit replaced it, its completion was withdrawn or it was found stale,
stays listed as `outdated` until the workspace is removed, so its number is not reused and deciding it is refused. Approval reserves no capacity: retry acquisition after
the decision. Pending requests produce a daemon notification, collapsed until read;
a decision queues an [agent message](#agent-messages) for the requesting workspace
naming the request, its target and the decision.

`shoal approvals withdraw <number> [--workspace W]` lets the requesting workspace
take back a pending request it no longer needs, as releasing a lease does for a
lease request. The request stays listed as `withdrawn` on the same terms as
`outdated`, except a proposed issue, which is deleted as a decided proposal is.
Withdrawal deletes the request's `approval_requested` notification while it is
unread; otherwise its deciding coordinator is told, or the user gets
`approval_withdrawn`. A command that retries the request, such as a queued
`acquire`, requests it again.

A request from a [child workspace](#child-workspaces) with an agent of its own
goes up the tree first. The nearest coordinating agent above it that holds an
approved grant for the same target and settings decides it instead of the user,
by number with `shoal approve <number>` or `shoal deny <number>`. It may also
pass the request up with `shoal escalate <number>`, to the next coordinating
agent with such a grant, or to the user. Each coordinating agent passed over
without a grant receives a message saying so. Without a deciding coordinator,
the user is notified as above. A land request names its own branch and commit,
which no other workspace holds a grant for, so it always reaches the user. The
request records its `decider`, the coordinator's workspace, while one decides
it; the user may still decide it.

An acquisition waiting for a decision returns exit 2 and JSON
`code: "approval_pending"` with the request; denial returns `"approval_denied"`.
`--queue` waits for pending approvals and capacity, stopping on denial. Retrying the same
name preserves the request; changing its settings or reason requires release first.
Release cancels a pending or denied request even without a lease. Changed effective
settings cannot reuse a grant. Requests survive restart and failed removal and are
deleted with the workspace, except proposed issues, which belong to the repository;
only a pending PR proposal prevents idle cleanup.

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
unchanged. Debug builds neither install nor update skills unless
`SHOAL_DEV_USER_FILES=1`.
