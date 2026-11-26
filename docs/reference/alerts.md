# Alerts

This file says what `shoal send` and the `alert` commands do, and how alerts
reach agents.

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

An alert is a one-way notice. The user, a space or Shoal sends it, to the user
or to a space's agent, a workspace's or a chat's. The
[coordination design](../design/coordination.md) says why alerts and messages
to agents are one record.

## Records

Each alert records:

- its `kind`;
- its `sender`: `user`, `space` or `shoal`;
- its `recipient`: a space by `number`, `id` and `name`, or none for the user;
- the space that sent it or that it is about, as `space`, `space_id` and
  `number`.

## Send up

From a space, `shoal send` without `--space` sends the alert up. It goes to
the agent of the nearest space above with an agent, the
[coordinating agent](create.md#coordinators), on this machine or another
[joined machine](hub.md). Without one, it goes to the user.

An alert sent up is one line of at most 512 bytes, without control
characters.

## Send down

`--space`, `--all` and `--repo` send the alert down. From the user's terminal,
`shoal send` without them also sends it down: to the workspace the current
directory is in, or else to one you pick. A scoped process sends alerts down
only to the spaces below its own.

An alert sent down is at most 8 KiB. It may hold newlines and tabs, but no
other control characters.

`--all` sends to every agent the sender's scope reaches. `--repo` sends to
every agent of the repository it names. Without a value, `--repo` takes the
current checkout's or workspace's repository, or else one you pick.

`--all` and `--repo` reach every space but the sender's own and those being
removed:

- a chat, running or stopped;
- a workspace whose agent runs, working or idle;
- a workspace whose agent is stopped, waits for its restore, is queued to
  start or restore, or has a wrapper waiting to reattach, as after a daemon
  restart.

A workspace without any agent gets nothing. An agent that runs connected
receives the alert now, and any other once it next runs. The alert starts none
of them. The exception is an idle agent Shoal stopped whose restore waits for
an update: an alert to its space alone starts it.

A space may refuse the alert, such as one whose unread alerts are full or that
is being removed. The command lists it under `refused` with the reason, the
other spaces still receive the alert, and the command exits 1.

## Attach files

`--attach FILE` sends a file of at most 100 MiB with the alert. Repeat it for
up to 20 files. An alert may be its files alone.

Shoal copies each file as an [attached file](files.md). An alert sent down
attaches it to the recipient's space. An alert sent up attaches it to the
sender's space, and the coordinating agent receives it with the alert.

## Output

The command prints each alert's number and where the alert went: to the user,
to the coordinating agent, or to the spaces it reached. It groups those spaces
by when their agents receive the alert:

- `now`: the agent runs connected;
- `on_start`: the agent receives it once it next runs;
- `without_agent`: the space has no agent, and the alert waits for one to start
  there.

With `--json`, each group lists `workspaces` and `chats`.

## Withdraw an alert

The sender withdraws an alert by its number until it is read:
`shoal alert withdraw <number>`. `--space` names the space the alert went to,
which finds it on the joined machine that has that space. On a terminal,
Ctrl-W in the `alert list` picker withdraws an alert you sent.

On machines joined to a [hub](hub.md), alert numbers come from one sequence,
and the hub never gives a number twice. An alert numbered while its machine
could not reach the hub may take a new number once it can. The sending space's
agent hears of the new number while the alert is unread.

## Send the final message

`--in-chat` makes the final message of the agent's current turn the alert. The
agent writes the alert in its conversation, where the user reads it as written,
and it still reaches its recipients. It takes no text and no `--body-file`.

`--in-chat` takes `--space`, `--all`, `--repo` and `--attach` as a plain alert
does. The command resolves and checks them at once. It prints whom the alert
goes to, as `mark` with `--json`. A turn has one mark, a post's or an alert's,
as [threads](threads.md) says, so the command also prints the mark it
replaced, as `replaced`. An alert's `to` has a `kind`:

- `user`;
- `coordinator`;
- `above`, a coordinator on another joined machine;
- `space`;
- `all`, with its `repository`.

When the turn ends, Shoal sends the final message the way `shoal send "<text>"`
sends text: as one line up, or as lines down, each shortened to fit. It
attaches the files as they are then. The alert gets its number then, which the
sender's [`shoal log`](log.md) names. A turn without a final message sends
that the agent wrote in its chat.

A prompt the user submits during the turn drops the alert. The agent's next
delivery then says the alert was not sent. It says the same, with why, when a
marked post or alert fails to send at the end of the turn. A prompt in a chat
also marks read the user's unread alerts that the chat sent with `--in-chat`.

## Shoal's alerts

Shoal sends its own alerts, each naming the space it concerns. It sends the
user what the user would otherwise miss:

- a resource or simulator request that found no capacity, naming the
  workspaces that hold the pool;
- a preferred port in use;
- a tracked agent's exit, with its code, or a note to run `shoal stop` when it
  left processes behind;
- an agent's failed [turn-state report](agents.md#agent-state);
- [agent queue](queue.md) starts and failures;
- failures of the [runner](ci.md#runner), of a
  [seed build](setup.md#seed-builds) and of a
  [sleep inhibitor](machine.md#sleep-inhibitor);
- [shared caches](setup.md#shared-caches) it emptied;
- workspaces it removed or retained on its own through PR, merge or idle
  cleanup, and automatic cleanup that starts failing;
- a workspace config file it ignores, and a directory whose file access
  stalled;
- when the daemon starts or reloads its config: a missing tool on its `PATH`,
  a `root_dir` it cannot write, a checkout with `core.bare` set, or a global
  rule that repeats Shoal's guidance.

It sends a space's agent answers, approval decisions, reviews, idle wakes and
machine load.

An event about a space with a coordinating agent goes to that agent instead of
the user. Shoal records no alert for `shoal run` commands or for a `shoal rm`
you run.

## List alerts

`shoal alert list` shows your own alerts: the user's from the user's terminal,
and its own space's from a space's process. `--space` shows those of a space
your scope reaches. `--all` shows those of every recipient you may read.

On a terminal, it opens a picker, newest first. Enter prints an alert with its
files. `-p`/`--print`, `--json` or output that is not a terminal lists the
alerts instead, oldest first. The list holds the newest `--limit` alerts,
50 by default, or with `--unread` the oldest unread ones.

Each row shows when the alert came in, its number, its sender, its recipient
and its text. It gives today's alerts the local time, and older ones whole
days, weeks, months or years ago, such as `1 day ago` or `2 weeks ago`. An
`Attached file:` line follows for each file sent with the alert. It gives the
file's path, or for a file a space on another machine sent, its name and the
machine that keeps it. `--json` gives each file's record `id`, `space`,
`name`, `title` and `path`, or `machine` in place of `path`.

Showing one recipient's alerts marks them read unless `--keep-unread`. `--all`
marks none read. `shoal status` outside a space mentions the user's unread
alerts.

## Follow alerts

`--follow` keeps printing the user's alerts as they arrive. On a terminal, it
also sends each one as an OSC 9 terminal notification. iTerm2, Ghostty,
WezTerm and Kitty show it as a desktop notification, and tmux needs
`allow-passthrough`. Other terminals ignore it.

## Alerts on a hub

On machines joined to a [hub](hub.md), every machine shows the user's alerts
from all of them. While this machine cannot reach the hub, it shows another
machine's alerts as the hub last sent them. Lists order alerts by when they
were recorded.

A row, a followed line and its terminal notification name the machine that
keeps another machine's alert, as `on <machine>` after its sender. `--json`
gives it as `machine`, with its `id` and `name`. Following on several machines
notifies on each machine for an alert still unread when it reaches that
machine.

Reading another machine's alert reads it there too. `alert clear` deletes the
alerts of every machine this one knew of. Each machine deletes its own once it
connects, and keeps those it recorded since. A machine the hub revokes, or that
leaves, takes its alerts with it.

`alert withdraw` finds another machine's alert by its number, such as for a
space that moved since it sent the alert. While that machine is offline, the
withdrawal fails at once, naming the machine and when the hub last saw it.

## Limits and retention

Identical unread alerts collapse into one: conflicts, cleanup failures and
messages, until read. Every agent exit is listed.

A space holds at most 50 unread alerts. They take at most 40 KiB as its agent
receives them, with their files' names and titles. A space being removed takes
no alerts. Removing a space gives the user the unread alerts its agent received
in the user's place, about the spaces below it, and deletes its alerts.

Shoal drops read alerts older than the newest 500. Every alert for the user,
read or not, expires after
[`[cleanup.restore] retention_days`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup).
The setting comes from the repository of the space the alert is about. For an
alert about no space, or about a chat without a repository, it comes from the
global config.

## Scope

Scoped commands read, clear and follow the user's alerts only as their space's
[`notifications` permission](modes.md#permitted-actions) allows.

## Delivery hooks

Claude Code and Codex agents that Shoal starts register
`shoal internal deliver` for the `PostToolUse` and `UserPromptSubmit` events.
So do their restores, automatic or by `shoal restore`. After the agent's next
tool call or prompt, the hook gives it as additional context:

- its new alerts;
- the activity on its linked issues and PRs that the daemon has queued for the
  next watch;
- the [leftovers](daemon.md#untracked-processes) of its workspace or chat that
  it has not been told about, with their PIDs, commands and running times and
  `shoal stop --leftovers`.

A delivery through the hook replaces a running watch in the workspace, which
ends with `superseded: true`. A subagent's tool call, which the hook input
names as a subagent's, delivers nothing.

Activity queued while cleanup or another item change runs comes with a later
delivery. When the hook has no answer from the daemon within 5 seconds, such as
while the daemon waits to restart with an update, it prints nothing and leaves
the delivery to the next one.

The hook reads the event from stdin and prints nothing when there is nothing
new. Its failures go to stderr and never fail the agent. Codex runs the hooks
only after you trust them in its hook review.

Other agents whose hooks accept Claude Code's
`hookSpecificOutput.additionalContext` output can run the same command. The
rest rely on `shoal alert list` and `shoal watch`, which Shoal's guidance
mentions.

## Wake an idle agent

Claude Code launches also run `shoal internal wake` as a background `Stop` and
`StopFailure` hook with `asyncRewake`. After each turn, including one an API
error ended, it waits up to a week for the next alert or item activity. It
then prints the update and exits 2, and Claude Code starts the idle agent's
next turn with it.

While a [turn retry](agents.md) waits for a rate limit's known reset, the hook
waits for the retry's message or an alert from the user instead.

No other hook reports the woken turn starting, so the wake hook first reports
the agent `working`. A stop of the agent that came first leaves the update
unread for its restore instead.

The wait outlasts a daemon restart, including its handover to the installed
release after an upgrade, until the agent's wrapper reattaches. The hook tries
a wait that fails otherwise again, after a delay that doubles up to a minute.
It prints only the first failure. The daemon's diagnostics record why a wake
hook ends without starting a turn.

The hook's command passes Claude Code's process ID as `--agent "$PPID"`, and
the hook exits with that process.

## Wake prompts

A hook cannot wake Codex. When a Codex in a place of a host that types into its
places reports its turn finished with unread alerts or activity, Shoal types
this prompt into the session and submits it:

```text
Shoal: new alerts or activity on this workspace's issues and PRs.
```

It does so once until Codex starts another turn, and Codex's prompt hook
delivers the updates.

Claude Code's wake hook does not follow a turn `shoal stop --interrupt` ended.
Shoal wakes such a Claude Code the same way while it idles after one. It also
does while Claude Code idles with updates after no wait for them has run for
three minutes, as when its wake hook has ended.

Shoal appends the wake prompt to any text typed but not yet sent, and submits
both.

## Chats

A chat registers the same alert and [turn-state](agents.md#agent-state) hooks,
and for Claude Code the wake hook. A chat's agent runs outside any workspace
scope. Its hooks and `shoal alert list` deliver the chat's alerts about the
workspaces it [coordinates](create.md#coordinators), and those Shoal sends it for the user.
A Codex chat receives them after its next tool call or prompt.

## Settle alerts

The hooks, `shoal watch` and the wake prompt hold a workspace's or chat's
unread alerts until none has arrived for
[`[alerts] settle_seconds`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#alerts).
They hold them at most six times as long after the first. A run of alerts, such
as several approval decisions, then reaches the agent in one delivery.
`shoal alert list` shows them at once. A `settle_seconds` of 0 delivers each
alert as it arrives.
