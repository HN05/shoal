# Alerts

This file says what `shoal send` and the `alert` commands do.

```sh
shoal send "Tests pass"                       # Up: to your coordinator, else the user
shoal send "Stop the dev server" --space S    # Down: to the agent of a space below
shoal send --body-file plan.md --space S      # A file's text as one alert
shoal send "Crash on launch" --attach shot.png  # Attach files; repeat --attach for more
shoal send --all "Network is back"            # Every agent your scope reaches
shoal send "Rebase onto main" --repo [R]      # Every agent of R or the current one
shoal send --in-chat [--space S] [--attach F]  # The agent's turn's final message, once it ends
shoal alert list               # Your alerts: a picker on a terminal, else a list
shoal alert list --unread      # Oldest unread ones first; says how many remain
shoal alert list --space S     # Those for a space's agent
shoal alert list --all         # Every recipient's you may read
shoal alert list --follow      # Keep printing the user's, and raise terminal notifications
shoal alert list --keep-unread # Show without marking read, also with --follow
shoal alert withdraw <number> [--space S]  # Take back an unread one you sent
shoal alert clear              # Delete the user's, including unread ones
```

An alert is a one-way notice from the user, a space or Shoal to the user or a
space's agent, a workspace's or a chat's. Each records its `kind`, its
`sender` (`user`, `space` or `shoal`), its `recipient`, a space by `number`,
`id` and `name` or none for the user, and the space that sent it or that it is
about, as `space`, `space_id` and `number`.

`shoal send` routes an alert by where it runs. From a space, without
`--space`, it goes up to the agent of the nearest space above with an agent,
the [coordinating agent](create.md), here or on another
[joined machine](hub.md), or else to the user; such an alert is one line of at
most 512 bytes without control characters, since it may show as a terminal
alert. From the user's terminal without `--space`, it goes to the
workspace the current directory is in, or one picked. `--space`, `--all` and
`--repo` send it down: a down alert is at most 8 KiB, without control
characters other than newlines and tabs, and a scoped process sends one only
to the spaces below its own. `--attach FILE`, repeated for up to 20, sends
files of at most 100 MiB with it, which Shoal copies as
[attached files](files.md): those of the
recipient for an alert sent down, of the sender for one sent up, which a
coordinating agent receives with the alert. An alert may be its files alone.
The command prints each alert's number, and whether it went to the user or the
coordinating agent, or the spaces it reached, under `now` when its agent runs
connected, `on_start` when its agent receives it once it next runs, or
`without_agent` when the space has no agent and the alert waits for one to
start there, each with `workspaces` and `chats` with `--json`. The sender may
withdraw an alert by its number until it is read; `--space` names the space it
went to, which finds it on the joined machine that has that space. On machines
joined to a [hub](hub.md), alert numbers come from one sequence the hub never
gives twice; an alert numbered while its machine could not reach the hub may
take a new number once it can, which the sending space's agent hears of while
the alert is unread.

`--in-chat`, without text or `--body-file`, makes the final message of an
agent's current turn the alert, so the agent writes it in its conversation,
where the user reads it as written, and it still reaches its recipients. It
takes `--space`, `--all`, `--repo` and `--attach` as a plain alert does, and
the command resolves and checks them at once: it prints whom the alert goes
to, and the mark it replaced, which `--json` gives as the `mark` and
`replaced`, an alert's `to` of `kind` `user`, `coordinator`, `above` for a
coordinator on another joined machine, `space`, or `all` with its
`repository`. When the turn ends, Shoal
sends the final message through the same path as `shoal send "<text>"`, as
one line up or as lines down, each shortened to fit, with copies of the files
as they are then; the alert gets its number
then, which the sender's [`shoal log`](log.md) names. A turn without a
final message sends that the agent wrote in its chat. A prompt the user
submits during the turn drops the alert, and the agent's next delivery says
it was not sent, as it says when a marked post or alert fails to send at the
end of the turn, with why; a prompt in a chat also marks read the user's unread
alerts that chat sent in-chat, since the user is reading it there.

`--all` sends to every agent the sender's scope reaches, and `--repo` to every
agent of the repository it names, or without a value the current checkout's
or workspace's, else a picked one: each space but the sender's own and those
being removed, a chat running or stopped, and a workspace whose agent runs,
working or idle, or is stopped, waits for its restore, is queued to start or
restore, or has a wrapper waiting to reattach, as after a daemon restart. An
agent that runs connected receives it now, and any other once it next runs;
the alert starts none of them, except an idle agent Shoal stopped whose
restore waits for an update, as an alert to its space alone starts it.
Workspaces without any agent get nothing. A space that refuses the alert,
such as one whose unread alerts are full or that is being removed, is listed
with why under `refused` while the others still receive it, and the command
then exits 1.

Shoal sends its own alerts too, naming the space they concern: to the user, a
resource or simulator request that found no capacity (naming the workspaces
holding the pool), a preferred port in use, a tracked agent exiting (with its
code, or a note to run `shoal stop` when it left processes behind), an
agent's failed [turn-state report](agents.md#agent-state), [agent queue](queue.md)
starts and failures, [runner](ci.md#runner) failures, [seed build](setup.md#seed-builds)
failures, [shared caches](setup.md#shared-caches) it emptied, a
[sleep inhibitor](machine.md#sleep-inhibitor) that failed, and workspaces it
removed or retained on its own through PR, merge, or idle cleanup, automatic
cleanup that starts failing, a workspace config file it ignores, a directory
whose file access stalled, and, when the daemon starts or reloads its config,
a missing tool on its `PATH`, a `root_dir` it cannot write, a checkout with
`core.bare` set, or a global rule repeating Shoal's guidance; to a space's
agent, answers, approval decisions, reviews, idle wakes and machine load.
Events about a space with a coordinating agent go to that agent instead of the
user. Arbitrary `run` commands and manual `rm` are your own and record
nothing.

`shoal alert list` shows your own alerts: the user's from the user's
terminal, and its own space's from a space's process. `--space` shows those of
a space your scope reaches, and `--all` those of every recipient you may read.
On a terminal it opens a picker, newest first, where Enter prints an alert
with its files and Ctrl-W withdraws one you sent; `-p`/`--print`, `--json` or
output that is not a terminal lists them, oldest first: the newest `--limit`
(default 50), or with `--unread` the oldest unread ones. Each row shows when
it came in, its number, its sender, its recipient and its text: the local
time for today's, otherwise whole days, weeks, months or years ago, such as
`1 day ago` or `2 weeks ago`, followed by an `Attached file:` line with the
path of each file sent with it, or its name and the machine that keeps it for
one a space on another machine sent; `--json` gives each file's record `id`,
`space`, `name`, `title` and `path`, or `machine` in place of `path`. Showing the alerts of one recipient marks them
read unless `--keep-unread`; `--all` marks none. On a terminal, `--follow` also sends each of the user's
alerts as an OSC 9 terminal notification, which iTerm2, Ghostty, WezTerm, and
Kitty show as a desktop notification (tmux needs `allow-passthrough`); other
terminals ignore it. `shoal status` outside a space mentions the user's unread
alerts.

On machines joined to a [hub](hub.md), every machine shows the user's alerts from
all of them, those recorded elsewhere as the hub last sent them while this
machine cannot reach it. Lists order them by when they were recorded. A row,
the followed stream's line and its terminal notification name the machine
that keeps another machine's alert as `on <machine>` after its sender, and
`--json` gives it as `machine`, its `id` and `name`. Following on several
machines notifies on each for an alert still unread when it reaches it. A
machine the hub revokes, or that leaves, takes its alerts with it. Reading
another machine's alert reads it there too, and `alert clear` deletes the
alerts of every machine this one knew of, each machine deleting its own once
it connects, and none it recorded since. `alert withdraw` finds another
machine's alert by its number, as for a space that moved since it sent it,
and while that machine is offline fails at once, naming it and when the hub
last saw it.

Identical unread alerts collapse into one: conflicts, cleanup failures and
messages until read, while every agent exit is listed. A space holds at most
50 unread alerts, which take at most 40 KiB as its agent receives them, with
their files' names and titles, and none while it is being removed.
Scoped commands read the user's alerts, clear them and follow them only as
their space's [`notifications` permission](modes.md#permitted-actions) allows.
Removing a space gives the user the unread alerts its agent received in the
user's place, about the spaces below it, and deletes its alerts. Read alerts
older than the newest 500 are dropped, and every alert for the user, read or
not, expires after [`[cleanup.restore] retention_days`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
of the repository of the space it is about, or the global setting's for one
about no space or a chat without a repository.

Claude Code and Codex agents Shoal starts, and their automatic or `shoal
restore` restores, register `shoal internal deliver` for the `PostToolUse` and `UserPromptSubmit`
events, so a working agent receives new alerts, the activity on its linked
issues and PRs that the daemon has queued for the next watch, and the
[leftovers](daemon.md#untracked-processes) of its workspace or chat that it has not been
told about, with their PIDs, commands, running times and `shoal stop
--leftovers`, as additional context after its next tool call or prompt. Activity queued while cleanup or
another item change runs comes with a later delivery, and a hook that has no
answer from the daemon within 5 seconds, such as while it waits to restart with
an update, prints nothing and leaves the delivery to the next one. A subagent's
tool call, which the hook input names as a subagent's, delivers nothing, since
its context reaches the agent only through the subagent's report. Delivering through the hook replaces a running watch in
the workspace, which ends with `superseded: true`. Claude Code launches also run
`shoal internal wake` as a background `Stop` and `StopFailure` hook with
`asyncRewake`: after each turn, including one an API error ended, it waits up to a week for the next alert or item activity, prints it and
exits 2, so Claude Code starts the idle agent's next turn with it; while a
[turn retry](agents.md) waits for a rate limit's known reset, it waits for the
retry's message or an alert from the user instead. No other hook
reports that turn starting, so it first reports the agent `working`, unless a
stop of the agent came first, which leaves the update unread for its restore.
The wait
outlasts a daemon restart, including its handover to the installed release
after an upgrade, until the agent's wrapper reattaches; a wait that fails
otherwise is tried again after a delay that doubles up to a minute, so the
agent is never left idle without one; only its first failure is printed. Its command
passes Claude Code's process ID as `--agent "$PPID"`, and it exits with that
process: Claude Code runs hooks outside its process group, so stopping the
agent would otherwise leave the hook behind. The daemon's diagnostics record why
a wake hook ends without starting a turn. Codex cannot
be woken by a hook: when a Codex in a place of a host that types into its places reports its turn
finished and has unread alerts or activity, Shoal types `Shoal: new
messages or activity on this workspace's issues and PRs.` into the session and
submits it, once until Codex starts another turn, and its prompt hook delivers
them. Claude Code's wake hook does not follow a turn
`shoal stop --interrupt` ended, so Shoal wakes a Claude Code in one the same
way while it idles after one, and while it idles with updates after no wait
for them has run for three minutes, as when its wake hook has ended. Text typed but not yet sent in that prompt is sent with it. Codex runs the hooks only after you trust them in Codex's
hook review. The hook reads the event from stdin and prints nothing when there
is nothing new; failures go to stderr and never fail the agent. Other agents
whose hooks accept Claude Code's `hookSpecificOutput.additionalContext` output can
run the same command; the rest rely on `shoal alert list` and `shoal watch`, which
Shoal's guidance mentions.
A chat registers the same alert and [turn-state](agents.md#agent-state) hooks, and
for Claude Code the wake hook. Under a chat, outside any workspace scope, they
and `shoal alert list` deliver the chat's alerts about the workspaces it
[coordinates](create.md) and those Shoal sends it for the user; a Codex
chat receives them after its next tool call or prompt.
The hooks, `shoal watch` and the prompt that wakes Codex hold a workspace's or
chat's unread alerts until none has arrived for [`[alerts] settle_seconds`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#alerts), and at
most six times as long after the first, so a run of them, such as several
approval decisions, reaches the agent in one delivery; `shoal alert list`
shows them at once, and 0 delivers each alert as it arrives.
