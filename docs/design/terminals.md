# Terminal hosts

This file covers the terminal hosts that run agents, chats and demos, the places
they open and the status Shoal reports to them.
[Terminal hosts](../reference/terminals.md) and
[Where an agent runs](../reference/agents.md#where-an-agent-runs) say what each
host does for the user; this file gives the design and its reasons.

## The terminal layer

### Commands say only what they launch

A command states what it launches, never where. One terminal layer chooses the
placement and the host that carries it out. So focus, splits and closing behave
alike under every host, and a new host plugs in without touching commands.

Generic code never names a host. Each host implements the callbacks for what it
does differently. Shoal's generic code keeps the state, the decisions and the
daemon calls, and a host never calls the daemon.

### What a host does

A host opens a place for a start, shows a place, closes one and reports on it.
Generic code decides the rest:

- whether a start stays in this terminal,
- whether an agent runs in place,
- which host takes a queued start,
- that a focused place opened from a terminal the host does not serve shows
  there, as `shoal attach` and opening a demo show one.

Showing is the host's own. Herdr attaches the terminal to the session that holds
the tab. Rex has no terminal client, so it selects the window in its app. zmx
attaches the session. Across machines, a host callback decides how a place
shows, as [Attaching to another machine's session](hub.md#attaching-to-another-machines-session)
says.

### Which host serves a terminal

A host declares the environment variables that name its places. Hosts can nest,
and inherited variables cannot tell which one is innermost. So hosts claim a
terminal in a fixed order, the opt-in multiplexers last. A place of a host that
claims a terminal before tmux, such as a zmx session, keeps its own behavior
inside tmux.

A zmx session drops the other hosts' variables. Shoal inside it thus never takes
the caller's place for its own. A [nested session](#nested-zmx-sessions) is the
exception: it keeps the variables of the place it runs in.

A host's `enabled = false` turns off every call to it, so its places behave as
plain terminals. `new_tab` and `new_window` govern only whether starts open new
places.

### The default host

`terminal.default_host` serves a terminal no host serves. It is zmx unless set
to another host. That host's places join the group its config names, such as
`terminal.herdr.workspace_name`. Generic code reads the setting and renders the
name. A host only finds or creates a group by its label.

The daemon requires only the host `terminal.default_host` names. A user who sets
another one thus need not install zmx.

### Supported versions

A launch refuses a host older than Shoal supports, such as a zmx older than
1.0.1. Since 1.0.0, a zmx reaches only servers of its own version, and 1.0.0's
macOS release could not reach its own sessions. Shoal does not restart agents
left in sessions an older zmx started. It reports those sessions as
unreachable.

While Rex is in alpha, Shoal supports only its latest API revision and refuses
an older one.

## Places

### One origin record

Shoal remembers every place as one host-neutral origin record. It holds the
host, its group and place, host-defined data, and whether Shoal created the
place.

The place is the one ID the host names it by, for its watcher as well. A Herdr
place is the pane, whose tab Herdr finds to close, rename or focus. Generic code
passes the record to the host that reads it and never interprets the data. For
Rex, the data says how to reach the server and its CLI.

Servers number places alike. So a record names its place by host, by the server
the host reads from its data, and by ID.

### Where records live

- A queued entry carries the origin of the terminal that queued it.
- A recovery record carries the stopped agent's origin.
- A demo carries its launch's origin.
- The daemon records the place each execution and chat runs in, and the places
  Shoal created for each workspace. These records outlive their sessions.

`attach`, `status` and attach completion ask the host about these places, for
every host.

### IDs a host reuses

A host may reuse a place's ID once its server restarts. A record then names the
server's instance beside the ID, and a recorded place is Shoal's only on that
instance:

- tmux numbers panes afresh when its server restarts. A record names the
  server's socket and its instance, the server's process and start time.
- Zellij runs a server per session and numbers panes per server. A place's ID
  names its session and pane. The record names the socket directory, and the
  instance is the server's process and birth. Zellij takes no condition with a
  command, so the host checks the instance just before each command on a
  recorded pane.
- zmx reuses session names. A record names its session's process and creation
  time, so `attach` passes over a later session under the name. That session's
  own record replaces it.

A Rex block keeps its ID when it moves to another session, and Rex names only
the block to the programs in it. So a block's record names no session. Shoal
reads the session from the block when it opens a window beside it, and a start
queued from any block joins that session.

## Placement

### Focus

By default, an agent that starts with input, such as an issue prompt or
forwarded arguments, stays in the background. A place that waits for the user's
input takes the terminal. A focus setting overrides this choice, the mode's over
the terminal's, and `--focus` overrides both. So each mode decides whether its
spaces take the terminal. The built-in chat modes, whose agents the user talks
with, take it. `repo setup` and `config setup` always focus the agent, since the
user works with it straight away.

A launch the user waits for starts past `max_heavy_agents`, as `--now` does.
That is a `--focus` start, or an interactive call's agent that waits for input
and that the setting does not send to the background. `--add-blocker` still
makes either wait for its work. The setting alone never skips the limits, so it
cannot switch them off.

A host that opens places focuses one or opens it in the background. A host that
keeps agents in sessions the terminal attaches to, such as zmx, attaches the
caller or starts a detached session.

A restore takes no new input. So it restores agents where they stopped, or in
the background. Only `--focus` restores one in the caller's terminal.

### Run in place

A launch in a place of an enabled host runs in place, for two reasons. A zmx
session cannot show another session to its terminal. A host such as Herdr reads
agent state from the place's process.

`start --here` keeps the launch in place. A script uses it to wait on the
agent's exit code. A scoped caller, such as a master session's terminal, uses it
to run an agent there. `restore --focus` keeps it for callers outside Shoal.

A scoped caller's `--here` claims its own launch's entry, as the user's does.
Only the user's terminal starts agents beyond the limits. So without a free
slot, a scoped `--here` leaves the agent queued and returns rather than hold
the terminal.

### Callers without a terminal

A caller without a terminal, such as an orchestrating agent's tool call, starts
the agents of `start` and `restore --now` detached through the daemon, as a
scoped caller does. Run in place, the agent would hold the call until it exits
and die with an interrupt of the caller.

An agent Happy runs leaves the call at once anyway. Such a caller therefore
runs it in its own process once the workspace is prepared, as a start here does.

### The agent belongs to its own space

An agent's execution belongs to its own space, with that space's scope. Its
wrapper, and what it started, never count among the processes of the execution
that started it. The wrapper leaves its starter's process group, which the
starter's wrapper signals as it stops, and takes the terminal along when that
group held it. So the caller's stops leave the agent running.

### Splits

Generic code decides a split. Only a start from the caller's own place passes
the split to the host. Queued starts, and starts without a place of their own,
open new windows or tabs.

A block or pane beside others carries its own label. Shoal renames a window or
tab only while the place it opened is alone there. So a split never renames the
user's window.

### Queued starts

A queued start falls back in order:

1. the origin's group,
2. the host's configured group on the same server,
3. `terminal.default_host`.

A start queued from a zmx session joins no group, so it goes to
`terminal.default_host`. With that host turned off, a terminal no host serves
runs work in place. A queued start or a demo without another fallback then
fails.

Zellij keeps no configured group. Generic code then hands the host no origin,
and the host decides between the origin's session and a new one. Only the host
can tell whether a client shows a session.

## Launching in a new place

### The launch plan

The CLI looks up the issue and makes the interactive workspace and agent choices
before it hands a start to a new place, such as a Herdr tab. The launch plan
travels in the new place's environment, as every place receives it. It names the
starting launch, whose issue the place reads from the queue, rather than carry
the issue's unbounded body. It keeps the state and config selection and the
literal arguments without hook scripts or state files. The plan also carries the
resolved label template, so the worker keeps the caller's naming choice.

The worker drops the plan before it starts children. A retained place's shell
keeps it, inert. A non-default state directory goes only to the worker.

### The launch step

Some hosts, such as Rex and tmux, cannot set a new place's environment. A new
place of theirs runs a launch step instead. The step loads the worker and its
variables from a private file and deletes the file, which keeps them off screen
and out of shell history. The user's login shell runs after it, so the place
keeps a shell, as a Herdr tab does.

tmux starts a pane with a command, so a new tmux window runs the launch step
directly, with nothing typed. The step also drops other hosts' place variables
that the tmux server's environment may carry. A tmux demo, whose worker needs
its pane, replaces the window's shell instead. Only a resume in an existing tmux
pane types the launch step at its prompt.

### Typing into a new shell

Herdr cannot start a tab with a command, and a Rex demo needs its block before
its command. So Shoal types those into the new shell. A line typed before the
prompt shows twice: the terminal echoes it, and the line editor draws it again.
Shoal therefore waits until the terminal no longer takes input a line at a time,
or long enough for a shell without a line editor.

### The shell a place keeps

A place that keeps a shell, such as a Herdr tab, stays open after its tracked
agent exits. The shell enters the workspace through its own exported navigation
file, which it follows once at a prompt after the worker writes it.

## Labels

### One layer renders labels

A place is labelled with the space's name by default, which a template's
`{name}` also gives. An optional template such as `terminal.herdr.tab_name`
follows the normal configuration precedence. It substitutes the repository,
branch and issue fields once, and updates the branch after allocation. Shoal
renders the label in its generic terminal layer, and a host applies only the
label it is given. So the naming rules, and their daemon and forge lookups,
live in one place.

### Renaming keeps the user's labels

A rename relabels the places the space's agents run in and the places Shoal
created for it, wherever the command runs. Each goes through the host that
opened it, on the server its record names, as a close reaches it.

A place takes a new label only while it still shows the label Shoal gave it.
Any other label is the user's: a chat's old name, or the label a workspace's
previous title rendered. A place labelled before its workspace's template, issue
or branch changed keeps that stale label rather than risk overwriting the
user's.

`shoal rename --request` asks the agent to choose a name with a fixed alert, as
[Rename a space](../reference/workspaces.md#rename-a-space) says. It is refused
without a running agent, since the alert would stay unread until removal.

## Ownership

### Shoal closes only what it created

Shoal closes only the places it created. A place the user opened stays theirs,
even after an agent ran in it. A record names its owner, a space of either kind.
Every removal of a space closes its places through one operation, so no kind of
space keeps a place it cannot close.

An agent of another workspace or a chat that starts in a recorded place takes it
over. A nested command takes nothing. A place Shoal opens for a chat is the
chat's from its start, which the chat learns from the place's environment.

### Labels prove ownership of zmx sessions

zmx reuses session names. So a recorded name does not prove that a later session
under it is Shoal's. A label goes with the session, so labels carry ownership,
and a close from a record checks the label.

- A session is labelled with its owner's ID and the canonical identity of its
  state directory. Daemons that share zmx thus tell their sessions apart.
- Creation sets `shoal.created=1`, which relabelling never adds.
- A detached session that Shoal starts labels itself once its workspace exists.
- Relabelling replaces obsolete ownership before any agent or chat launches in
  place. So removal keeps a session another owner reused.
- A session from before the labels has no known origin, and Shoal leaves it
  alone.

The daemon still records the sessions Shoal creates, as it does other hosts'
places. So `attach` finds a session whose launch failed and left its shell.

A session zmx lists as unreachable, such as one a zmx of another version
started, shows neither labels nor instance. Every lookup of it, and every sweep
while one exists, fails naming it instead of taking it for another owner's or
for gone.

zmx has no conditional kill. So ownership checks, relabelling and closes hold a
lock beside zmx's socket directory, which every state directory using that
server shares. Finding sessions needs neither the labels nor the lock: they
exist only for cleanup.

### Records prove ownership of other places

Rex blocks, Herdr panes and tmux and Zellij panes carry no ownership marks. The
daemon's records of the places Shoal created prove ownership instead. Neither
Rex nor Herdr reuses IDs on one server, Herdr not even across its restarts, and
tmux and Zellij records name their server's instance. So a recorded place that
still exists is Shoal's. Each state directory knows only its own records.

Rex's own `own_block` arbitrates terminal size between clients and is not
ownership.

### Removal and sweeps

The daemon closes a space's places when it removes the space, whatever
`terminal.close_when_done` says. Startup and cleanup sweeps close the places
whose space record is gone. A failed close stays pending and is retried, without
restoring ownership. Every host's places go through the same removal hook,
orphan sweep and pending-close retries.

A failed detached handoff closes its zmx session only while the session's
process and creation time still match.

## Closing places

### When completion closes a place

Completion that keeps the workspace closes a place Shoal opened for it: for an
addition, an attached agent or a background resume. It closes the place only
once tracked executions have resolved or the agent's turn finished, since
interactive agents stay running at their prompt.

It never closes a place while stopped work waits for `shoal resume`. The place
then keeps the stop's cause, and `shoal resume --all` can restore the agent
there.

Removal closes a place at once. Completion closes it at the turn's end instead,
so completion cleanup leaves the agent its grace period.

Top-level `terminal.close_when_done` applies to every host, and a host's own
`close_when_done` overrides it for its places, as `terminal.focus` is shared.
Preparation failures and shell or untracked desktop handoffs keep their places
until that lifecycle change.

### Places a restore leaves

A restored agent can start in another place than the saved agents it restores
ran in, as a restore does when that place is not back at its prompt. Once it
runs, it closes the places Shoal created that those agents left. It keeps a
place where an execution is recorded or another saved agent waits. A place left
otherwise serves nothing, and `attach` offers only the sessions that show the
agent. The wrapper names the records it restores from, since a workspace may
save several agents.

### Where the turn comes from

An agent's turn comes from its hooks. The host reports it only for an agent
without hooks, as Herdr does with `idle` or `done`. zmx reads no turn, so its
sessions of such agents close with their execution.

### The place watcher

A watcher closes a place once its rule holds. The rule, the watcher and its
marker are generic. A host only closes a place and reports whether it is open,
what its agent is doing, and whether closing it ends another place. Closing a
Herdr pane's tab, for one, ends the tab's other panes.

Generic code tells the Shoal it starts in a new place whether to watch it. A
restore in the pane its agent stopped in leaves that place's watcher as it was.

A watcher asks the daemon whose its place is, until the daemon answers:

- A place the daemon records, for its workspace or another owner, or whose host
  labels it, is the daemon's to close.
- The watcher closes the place at removal only when the daemon knows nothing of
  it, such as a tab opened before Shoal recorded its places.
- It never closes before an answer, which could close what removal already
  did.

Only a watcher that closes its place through a host that closes more than the
place keeps the marker. A batch removal or a streaming export from a place that
would close with it then refuses to start, since the close would end it.

Place watchers close recorded places through the daemon. The daemon reads the
owner and closes under one lock. For a host whose places it cannot close, it
forgets the record and leaves the close to the watcher.

A watcher ends once its host reports the place missing, whatever the daemon
reports. It leaves the place open once no daemon has owned its state directory
for two minutes, which is longer than a restart takes.

A watcher refused for a protocol mismatch restarts from the installation path
Shoal was started through, once another binary is installed there. It never
restarts from a resolved versioned binary. So places opened before an upgrade
still close.

### A close ends one place

A close ends only that block or pane. It leaves the user's other blocks in its
window and panes in its tab. Herdr closes a tab with its last pane.

Zellij closes a tab or session with its last pane only while a client shows the
session. So a close ends either along with the last pane there, as an attached
client would see it, and leaves no empty tab or session. A session's last pane
closes by ending the session, never its tab, since a `zellij action` whose
server ends before answering never returns.

## zmx, the default host

Terminal launches run in zmx sessions, so agents outlive the terminal. Detaching
or closing the terminal leaves the agent running, and `attach` reattaches.

zmx passes output through unchanged while attached, and restores the screen from
its own terminal state on attach. Terminal protocols such as images thus keep
working. Multiplexers that re-render output through their own emulator, such as
tmux, break them or support them only in part. So zmx stays the default host.

The tracked wrapper runs inside the session's pty. Tracking, scope, stop and
recovery are thus unchanged, and the daemon still never touches terminal I/O.

zmx reports no exit status, so a session launch exits 0. The daemon still
records the agent's exit.

When zmx returns control to the caller, the CLI resets the emulator's input
modes on the caller's terminal, as [The terminal](execution.md#the-terminal)
says. An addition whose agent ran in the session leaves the caller's shell where
it was. The workspace is the session's, and detaching returns to the shell the
user left.

## Nested zmx sessions

`nested_zmx` runs what the user attaches to in a host's place in a zmx session
attached there: agents, chats and demos. So any terminal reaches it, such as one
over ssh. A Rex block, which has no terminal client, becomes reachable from one.

The option is off by default. A host that reads an agent's turn from its pane
sees only zmx there.

Generic code decides it from the resolved value and never names a host. The
host that serves a place nests the sessions zmx runs there. The option does
nothing on a terminal no host serves, whose launches use zmx already, or inside
a zmx session. Setup output, launch steps and the shells a place keeps stay in
the place, since nothing attaches to them.

The nested launch keeps the place's variables and sets `SHOAL_NESTED_SESSION`.
Shoal inside thus takes the place for its own: it opens starts beside it,
restores there and closes it as before. The record of where an execution or
demo runs nests the session in the place's origin.

Attach shows the place itself where its host serves the terminal, or to a viewer
whose app for the host shows it. From any other terminal, it attaches to the
session.

The session carries the labels of any zmx session Shoal creates. So removal and
the sweeps close it with its owner. A demo's close ends the session with its
place, whose closing ends only the zmx client.

## Rex

Rex is a host as Herdr is. Inside a Rex block, Shoal opens a window in the
caller's session, or splits the caller's block with
`terminal.rex.placement = "split"`. Rex keeps sessions running without a
connected client, so agents run in place in their block.

## tmux and Zellij

### Opt-in multiplexers

tmux and Zellij are opt-in hosts for users who already work in them. Agents then
run in their panes without a zmx session nested inside, unless `nested_zmx` asks
for one.

Each draws panes through its own emulator, so terminal protocols work only as
far as it supports them. tmux passes images and notifications to the outer
terminal only through its passthrough, which is off by default. Zellij handles
images, notifications and clipboard writes itself. An attach redraws either's
screen. Shoal documents these limits rather than work around them, and keeps
zmx the default.

`terminal.tmux.enabled` lets tmux serve its panes. Unset, it follows whether
`terminal.default_host` names tmux. Choosing the default host thus opts in
everywhere, and tmux users who set neither keep zmx. Zellij is enabled the same
way and claims a terminal after tmux.

### Groups and places

A tmux or Zellij session is a group and a pane a place, as a Rex session and
block are. Shoal opens a window or tab in the caller's session, or splits the
caller's pane, and runs agents in place. Outside tmux, it opens windows in a
configured session of the default tmux server and attaches the terminal to show
one.

A tmux server Shoal starts gets none of Shoal's own variables. A queued start
carries the caller's `TMUX` and socket directory, empty when unset. A daemon
started inside tmux thus never sends a start to its own server. A start queued
from a pane reaches its origin's server through the socket its origin records.

Zellij 0.45 drops tabs opened in a session no client shows, which would lose a
background start. So a place for a session without a client, or for a terminal
outside Zellij, opens as the first pane of a session of its own. The pane
records nothing of how its session began, since the place records itself from
its own environment.

### Zellij under load

Zellij can answer wrongly under load, so the host checks again before it acts:

- Zellij can list none of the clients that show a session. The host asks a few
  times, briefly, before it takes a session as shown by none.
- Zellij counts a session only when its server answers a probe in time, so it
  can report a busy session as missing. A running session can also list no pane
  before it lays out its first tab, or while it is busy. The host asks again
  while the session's server process still runs, the recorded instance for a
  recorded pane. It takes a session as ended only once that process does not.
- Zellij can let a command return before its answer and hand that answer to the
  next command. The host takes a tab or pane a command printed only when the
  session listed no such one before. It asks a pane or client listing or a
  screen dump again when it holds another command's answer or none.

## Terminal status

Shoal reports each space's status to the terminal its agent runs in, through the
Program Status Protocol (OSC 7501). Any terminal that reads it, such as Rex,
thus shows which agents work and which wait on the user, without a plugin.
[Terminal status](../reference/agents.md#terminal-status) lists each report.

### A record of Shoal's own

Shoal writes the record `id=shoal`, never the root record, so it never
overwrites an agent's own report. A report replaces its record whole, so every
report carries `app=shoal` and the space's name as its title.

Each agent integration declares whether its agent reports its own turns there,
as Claude Code does. For one that does not, Shoal also reports the turn its
hooks report.

A thread or approval request that waits on the user blocks the space. One a
coordinating agent holds does not, since `blocked` means the user must act.
Completion and ready marks show as `done` only between the agent's turns, so a
new turn replaces them.

### The wrapper writes reports

The daemon decides each report from the space's records. It sends the report
over the wrapper's existing connection whenever it changes, and the wrapper
writes it to the terminal it holds. The daemon never opens a terminal, since
pty device names are reused and terminal I/O stays in the wrapper. Hooks write
nothing there.

The wrapper writes each report in one write. The terminal never splits a write,
so the report stays whole beside the agent's own output. It can still fall
between two writes of one escape sequence the agent draws. That garbles the
frame until the agent next redraws, a cost Shoal accepts for status a terminal
shows outside the agent's screen.

The wrapper reports the agent's exit itself, also while the daemon is away.

### Only terminals that answer

Reports go only to a terminal that answers the protocol's query. Before the
wrapper hands the terminal to the agent, it sends the query followed by a
primary device attributes request, which every terminal answers. A device
attributes reply that arrives first thus means the terminal does not read the
protocol. A generous time limit guards only against a terminal that answers
neither.

tmux, zmx and Zellij answer only the device attributes request. So Shoal sends
nothing into a host that would drop the report. No host declares support, and
there is no option.

Each launch queries again, and a detached one sends nothing. Input typed during
the query, before the agent starts, may be lost.
