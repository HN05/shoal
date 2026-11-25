# Command line

This file covers how the CLI groups commands, selects targets, prompts, prints,
completes and shows problems.

Internal CLI workers are built through one typed argument builder that
explicitly passes the resolved state directory and output mode. Workspace JSON
records expose canonical linked-item URLs under `links`, with an `issues` list
of assigned issues in the order they were assigned, a list of watched PRs, and
`opened` and `related` lists, each item of these lists with its kind, URL and
known title; acknowledgement-only PR cleanup records have no URL to expose.
Linking the same item again records a newly supplied title, and the watches' and
background poll's reads of an item record the title they show. Their `running`
flag says whether a tracked execution is running; unverified executions after a
daemon restart do not count. Their `done` flag says whether a completion is
recorded.

Root help groups built-in commands by task; space maintenance commands such as
`status`, `log` and `rename` are top-level, repository administration under `repo`, moving workspaces to another
machine and repositories to another root under `move`, forge inspection and API access under
`forge`, CI runs and the checks on a PR's head under `ci`, and
shell integration under `shell`. `run` lists configured commands, runs one by
name, or executes an arbitrary command after `--`; `--workspace` selects the
workspace in either form. `done --undo` withdraws completion and `ready --undo`
withdraws review marks. Threads use `thread open`, `forward`, `resolve` and `list`, and
posts in them top-level `send` led by the thread's number; alerts use `send` alone, and `alert list`, `withdraw` and `clear`. Alert rows, in `alert list` and the picker, show
the time for today's and otherwise how long ago they came in whole days, weeks,
months or years, such as `1 day ago` or `2 weeks ago`.
Approval snapshots carry their stable request key through the CLI so reused
numbers cannot decide a replacement. Implicit thread replies and approval decisions
require stdin, stdout and stderr on a terminal, so redirected details cannot
hide the decision being made.

Single-workspace actions select an explicit target, otherwise the caller's scoped
workspace or the workspace containing the current directory, then an interactive
picker; a deleted current directory provides no workspace context. Explicit misses
and other refused arguments fail without fallback, before any prompt for the
omitted ones; `start` first sends the daemon its launch, with what the pickers
choose still unset, which the daemon settles through its own start's steps up to
where they would create or fetch anything, so each input its start refuses is
refused before a picker.
Noninteractive and JSON calls without a current workspace require a target.
An action that can apply to several workspaces takes several targets, and its
picker selects several: Tab selects each entry and Ctrl-A all of them. It checks
every target before acting on any, asks one confirmation that names what each
loses, and goes on after one fails, exiting 1.
Scope remains daemon-enforced. Bare `cd` always picks; all-workspace operations retain their scope.
A `-` target selects from a per-command history of space IDs that the CLI keeps
in the state directory, shared across terminals so `attach -` works from a new pane;
it only saves typing a name, so it never fails a command or authorizes anything, and
scoped callers neither read nor write it. `attach` and every launch that shows a
workspace's or a chat's agent session here, such as `start` or `claude`, record
the space in `attach`'s history once the session is shown and still runs: when the terminal
detaches, or right away for a host that shows it without attaching the terminal. A
focused `start` that opens a place records the workspace once the place hosts it,
since the host may show the place before the workspace exists, and records a chat,
which it recorded before opening the place, once it shows it. So `attach -` returns to the
session last detached from, not one another terminal still shows or one that ended.
Every command records and selects workspaces and chats through the same history,
passing over removed spaces, and `attach` over chats that no longer run, which
have no session left; a command that acts only on workspaces keeps a chat it refuses
out of its history, so its `-` still returns to a workspace.
Bringing back an agent and bringing back a removed space are two commands, each
with one subject: `resume` brings back a space's agent, workspace or chat alike,
from whatever state it is in, and `restore` reopens removed spaces and returns a
workspace to a checkpoint. A removed space's agents resume in the same run that
reopens it, through `resume`'s operation, so it comes back in one command;
`restore` refuses a space that is not removed, naming `resume`. `resume` uses
the current workspace only when its agent can resume; its picker offers the
spaces whose agent can resume, with what each would resume, and `restore`'s
the removed spaces, each listing them without a terminal; completion also names
the workspaces with nothing to resume. Its agent picker names agents by name,
stop time and reason. Shell
navigation refreshes the Zsh line-editor prompt after changing directories,
while Bash uses its prompt command hooks.
An omitted required argument that names an existing record opens a picker on a
terminal; noninteractive and JSON calls require it.
An agent whose executables are not on PATH
is refused before any work starts and left out of pickers, the workspace menu
and completion; a start's picker offers only agents, since every start starts
one, and the review picker offers “No agent” to run no review. `config show` reports effective repository values with their winning layers and uses a registered checkout
before a workspace exists, then the picker; `--repo` names a repository's checkout directly. `status` shows any space,
a workspace or a chat, running or stopped, through one path: links, watched PR state,
completion, sessions and what awaits the user for both, and for a workspace also
lifecycle, fork-point changes, active work and leases; the daemon looks PRs up on each
request and keeps a failed lookup on its PR. Human output uses `Display` for enum values and a shared
semantic palette at the CLI presentation layer; machine output and stored values
stay unstyled. The output format resolves once after parsing, from `--json` or
`--text`, then `SHOAL_OUTPUT`, then text, so results, errors and prompts agree.
The format changes only the output, turning prompts and pickers off with it;
where and how a command does its work follows whether it has a terminal.
Internal commands name their format, and hooks run without the setting.
Agent launches set `SHOAL_OUTPUT=json` unless the launcher carries a setting,
rather than guessing from the workspace or terminal: people use workspaces
too, agents sometimes have terminals, and piped human output keeps its meaning. Progress during silent waits belongs to the CLI and shows transient elapsed-time feedback on terminal stderr,
suppressed for JSON and dumb terminals. Rust chooses paths, including
`<root_dir>/<repo>` after removal; the Bash/Zsh wrapper changes directory
without evaluating repository code. Completion leaves navigation to prompt recovery:
the shell changes directory only after cleanup removes the current worktree. If
cleanup removes the current directory or a pending navigation destination, shell
integration recovers to its nearest surviving ancestor after the command or at the
next prompt, preserving the command status.
Recovery needs no daemon and is disabled for scoped callers.
Service installation prints the shell initialization hint only while neither
startup file contains an active initialization command.
Confirmations show the action and ask `[y/N]`, cancel on Enter, `n`, EOF or
Ctrl-C (also after a tracked execution), and are bypassed only by
explicit flags such as `-y`/`--yes`. Removal's confirmation names the unsaved
work it deletes and lists uncommitted changes and untracked files with Git
status codes, reporting omitted entries when the preview reaches its size limit;
the daemon refuses unsaved work the confirmation did not cover. A failure whose fix is one
command asks interactive callers `[y/N]` whether to run it and continue;
declining, and noninteractive or JSON calls, fail naming that command. Interactive
navigation without the shell wrapper reports how to load it without changing
redirected or JSON output.

Bare `shoal` is the one place to see what needs the user and the work going
on: a plain fzf picker whose inbox view lists threads waiting for the user, pending approvals,
agents that need input, failed workspaces and ready marks, and whose
spaces view lists workspaces, followed by a view of recent alerts. It opens
on the inbox when that has rows, searching for the current workspace's items
when it has some; Tab reloads the next view in place, and each view binds only the keys that act on
its rows, with a key meaning the same wherever it is bound and its header listing
them by how often each is used. Acting on a row
runs the command that handles its kind. `inbox` and `list` open the same picker
on their view, and print for scripts with `--print`, filters, JSON or without a
terminal. Items that only report status stay out of the inbox, including idle
agents, which may be waiting for PR activity rather than the user.

Completion queries the installed binary per Tab and uses read-only daemon calls
with a 500 ms timeout for live targets, honoring state directory and scope and
never starting a daemon or picker. Targets sort before flags, including in fzf-tab.
Each request kind declares, with no default, the one check it makes of the space
it names, such as a ready workspace or one where an agent runs, and the daemon
judges the space by it before the handler runs, accepting it, refusing it with
the check's reason, or finding it without what the request acts on, which its
handler answers.
A picker names the request its command sends for the space it picks, and
the daemon of each machine judges that machine's spaces by that request's check,
including facts only it has, such as where its terminal hosts keep agent
sessions; the picker leaves out a space without what the request acts on and
lists one the request refuses last, with the refusal's reason. A command that
acts on this machine's spaces in its own process asks this machine's daemon
alone. Every command declares that request, or that it sends none a space's
offer depends on, in one match over the command tree with no default arm,
which its pickers and completion read alike; completion asks this machine's
daemon for the request of the line typed so far, and offers every space while
the line does not parse yet.

There is no diagnostic command: each problem shows where the user looks and
names the command that resolves it. A failing command's error says what to run,
`inbox` lists failed workspaces and untracked worktrees (skipping a repository
whose Git gate is busy), and the daemon checks its PATH with the shared
executable catalog, `root_dir`, checkouts, the global config and recorded forge
hosts when it starts or reloads its config, recording an alert for each problem.
Repair is a request the commands that need it send: `adopt` reclaims, `setup
--skip` accepts a failed setup, and both restore verified worktrees and clear
executions proven stopped while preserving work and leases; `stop` stops
survivors and asks before forgetting an execution it cannot prove ended. Every
other command reports an unreachable or mismatched daemon itself, and `shoal cd`
says when shell integration is not loaded.
