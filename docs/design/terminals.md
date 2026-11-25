# Terminal hosts

This file covers the terminal hosts that run agents, chats and demos, the places
they open and the status Shoal reports to them.

`terminal.herdr.enabled = false` turns off every Herdr call, so a Herdr pane behaves as a
plain terminal; `new_tab` only governs opening new tabs.
The CLI owns Herdr tab handoffs after issue lookup and interactive workspace and
agent choices and labels the tab with the space's name by default, which a
template's `{name}` also gives. The user may rename the space or ask the
agent to choose a name, a fixed alert refused without a running agent,
which would leave it unread until removal. A rename relabels the places the
space's agents run in and those Shoal created for it, wherever the command runs: each through the host
that opened it, on the server its record names, as closing reaches it. An optional `terminal.herdr.tab_name` template
follows normal configuration precedence, substitutes repository, branch and issue
fields once, and updates the branch after allocation. The resolved template travels
with the launch plan so the worker preserves the caller's naming choice. Shoal
renders the label in its generic terminal layer; a host only applies the label it
is given, so naming rules and their daemon and forge lookups live in one place.
The plan travels in the tab's environment, naming the starting launch, whose
issue the place reads from the queue, rather than carrying its unbounded body,
and preserving
state/config selection and literal arguments without hook scripts or state files.
By default, agents that start with an issue prompt or forwarded arguments stay in
the background, and tabs that wait for the user's input receive focus; an
explicit focus setting, the mode's over the terminal's, or `--focus`
overrides this choice, so each mode can decide whether its spaces take the
terminal, and the built-in chat modes, whose agents the user talks with,
are set to; `repo
setup` and `config setup` always focus the agent the user works with straight
away. A launch the user waits for starts past `max_heavy_agents`, as `--now` does:
one `--focus` focuses, or an interactive call's agent waiting for input that
the setting does not send to the background, unless `--add-blocker` makes it wait for work; the setting alone does not, so it
cannot switch the limit off. Commands say only what they
launch; one terminal layer chooses the placement and the backend that carries
it out, so focus behaves alike everywhere and another backend plugs in with
its own primitives without touching commands: a host that opens places, such
as Herdr, focuses one or opens it in the background, and one that keeps agents
in sessions the terminal attaches to, such as zmx, attaches the caller or starts
a detached session running the plan, which it receives in its environment as
every place does. A restore takes no new input, so it restores agents where they stopped
or in the background; only `--focus` restores one in the caller's terminal.
The worker drops the plan before starting children; a retained tab's shell keeps
it, inert, while a non-default state directory is passed only to the worker.
A place that keeps a shell, such as a Herdr tab, remains open after tracked
agent exits, and its shell enters the workspace through its own exported
navigation file, followed once at a prompt after the worker writes it.
Completion that keeps the workspace closes a place Shoal opened for it, for an
addition, an attached agent or a background resume, only after tracked executions
have resolved or the agent's turn finished, since interactive agents stay running
at their prompt, and never while stopped work waits for resume, so the place keeps
the stop's cause and `shoal resume --all` can restore the agent there; removal
closes it immediately, so completion cleanup leaves the agent its grace period.
A restored agent that starts in another place than the saved agents it
restores ran in, as a restore whose place is not back at its prompt does,
closes the places Shoal created that they left once it runs, unless an
execution is recorded there or another saved agent waits there: such a place
serves nothing, and `attach` offers only the sessions that show the agent.
The wrapper names the records it restores from, since a workspace may save
several agents.
The turn comes from the agent's hooks, and from the host only for an agent
without them, such as Herdr's `idle` or `done`; zmx reads none, so its sessions
of such agents close with their execution. The rule, the watcher and its marker
are generic: a host only closes a place and reports whether it is open, what
its agent is doing and whether closing it ends another place, as closing a Herdr
pane's tab ends its other panes, where a batch removal or a streaming export
then refuses to start; generic code tells the Shoal it starts in a new place
whether to watch it, while a restore in the pane its agent stopped in leaves that
tab's watcher as it was. Top-level `terminal.close_when_done` applies to every host, and
`terminal.herdr.close_when_done` overrides it for tabs, as `terminal.focus` is shared.
Preparation failures and shell or untracked desktop handoffs retain them until
that lifecycle change. A watcher ends once its host reports the place missing,
whatever the daemon reports, and leaves the place open once no daemon has owned
its state directory for two minutes, longer than a restart takes. It asks the
daemon whose its place is until the daemon answers; a place the daemon records, for its workspace or another
owner, or whose host labels it, is the daemon's, and the watcher closes it at
removal only when the daemon knows nothing of it, such as a tab opened before
Shoal recorded its places, never before an answer, which could close what
removal already did. Only such a watcher, whose host closes more than the
place, keeps the marker. One refused for a protocol mismatch restarts from the installation path Shoal was started through, never a
resolved versioned binary, once another binary is installed there, so tabs
opened before an upgrade still close.

Shoal closes only the zmx sessions it created; a session the user opened stays
theirs even after an agent ran in it. The daemon records the sessions Shoal
creates as it does other hosts' places, so `attach` still finds one whose launch
failed and left its shell, but labels carry ownership: zmx reuses session names,
so a recorded name does not prove a later session under it Shoal's, while a
label goes with the session, and a close from the record checks it. A record
names its session's process and creation time, so `attach` passes over a later
session under the name, whose own record replaces it. Creation
sets `shoal.created=1`, which relabelling never adds. The daemon closes a
space's sessions when it removes the space, a workspace or a chat, whatever
`terminal.close_when_done` says, and startup and cleanup sweeps close those whose space record is gone,
retrying failures without restoring ownership. Sessions from before the label
have no known origin and are left alone. A session zmx lists as unreachable,
such as one a zmx of another version started, shows neither labels nor
instance, so every lookup of it, and every sweep while one exists, fails
naming it instead of taking it for another owner's or for gone. Relabelling replaces obsolete ownership
before any in-place agent or chat can launch, so removal preserves a session
reused by another owner. zmx has no conditional kill, so ownership checks,
relabelling and closes hold a lock beside zmx's socket directory, shared by
every state directory using that server. Finding sessions needs neither: the
labels and the lock exist only for cleanup.

Rex is a terminal host as Herdr is: inside a Rex block, Shoal opens a window in
the caller's session, or with `terminal.rex.placement = "split"` splits the caller's
block, and runs agents in place in their block, since Rex keeps sessions
running without a connected client. Generic code decides a split: only a start
from the caller's own place passes it to the host, so queued starts and starts
without one open windows. A block beside others carries its own label, and
Shoal renames a window only while the block it opened is alone there, so a
split never renames the user's window. A terminal no host
serves goes to `terminal.default_host`, zmx unless set to another host, whose places
then join the group the host's config names, such as `terminal.herdr.workspace_name`;
generic code reads the setting and renders the name, and a
host only finds or creates a group by label. A host opens a place for a
start and shows a place; generic code decides the rest: whether a start stays
in this terminal, whether an agent runs in place, which host takes a queued
start, and that a focused place opened from a terminal the host does not
serve is shown there, as `shoal attach` and opening a demo show one. Showing is
the host's own: Herdr attaches the terminal to the session holding the tab,
Rex, without a terminal client, selects the window in its app, and zmx
attaches the session. Shoal remembers
every place as one host-neutral origin record: the host, its group and place,
host-defined data, and whether Shoal created the place. The place is the one ID
a host names it by, for its watcher as well: a Herdr place is the pane, whose
tab Herdr finds to close, rename or focus. Generic code passes the
record to the host that reads it and never interprets the data, which for Rex
is how to reach the server and its CLI. A Rex block keeps its ID when it
moves to another session, and Rex names only the block to the programs in
it, so a block's record names no session: Shoal reads the session from the
block when it opens a window beside it, and a start queued from any block
joins that session. Shoal supports only the latest Rex while Rex is in
alpha, refusing an older API revision. Queued entries carry the queuing
terminal's origin, recovery records the stopped agent's, demos their launch's,
and the daemon the place each execution and chat runs in, which `attach`, `status`
and attach completion ask the host about for every host, together with the
places Shoal created for each workspace, whose records outlive their sessions;
the host never calls the daemon. A host
declares the variables naming its places, and a zmx session drops the
other hosts', so Shoal inside it never takes the caller's place for its own,
except a session nested in the place it runs in, which keeps that place's. A queued start
falls back from its origin's group to its host's configured group on the same
server, then to `terminal.default_host`; with that host turned off, a terminal no host
serves runs work in place, and a queued start or a demo without another
fallback fails. Rex cannot set a new terminal's
environment, so a new block runs a launch step that loads the worker and its
variables from a private file and deletes it, keeping them off screen and out
of shell history, then the user's login shell, which stays as in a Herdr tab.
Herdr cannot start a tab with a command, and a Rex demo needs its block before
its command, so Shoal types those into the new shell. Typed before the prompt,
a line shows twice, echoed by the terminal and drawn again by the line editor,
so Shoal waits until the terminal no longer takes input a line at a time, or
long enough for a shell without a line editor. Rex blocks and Herdr panes carry
no ownership marks, so the daemon's records of the places Shoal created are
the proof of ownership, closed through the same removal hook, orphan sweep
and pending-close retries as zmx sessions; neither host reuses IDs on one
server, Herdr not even across its restarts, so a recorded place that
still exists is Shoal's, and each state directory knows only its own. Servers
number places alike, so a record names its place by host, the server the host
reads from its data, and ID. A record names its owner, a space of either
kind, and every removal of a space closes its places through one operation,
so no kind of space keeps a place it cannot close. An agent of
another workspace or a chat that starts in a recorded place takes it over, as
relabelling does for a zmx session, while a nested command takes nothing; a
place Shoal opens for a chat is the chat's from its start, which the chat
learns from the place's environment; place watchers close recorded places
through the daemon, which reads the owner and closes under one lock, or forgets
the record and leaves the close to the watcher for a host it cannot close
places of. A close ends only that block or pane, leaving the user's other
blocks in its window and panes in its tab; Herdr closes a tab with its last
pane. Rex's own
`own_block` arbitrates terminal size between clients and is not ownership.

Shoal reports each space's status to the terminal its agent runs in through
the Program Status Protocol (OSC 7501), so any terminal that reads it, such as
Rex, shows which agents work and which wait on the user without a plugin. It
writes the record `id=shoal`, never the root record, so it never overwrites
an agent's own report, and every report carries `app=shoal` and the space's
name as its title, since a report replaces its record whole. Each agent
integration declares whether its agent reports its own turns there, as Claude
Code does; for one that does not, Shoal also reports the turn its hooks
report. The daemon decides each report from the space's records and sends it
over the wrapper's existing connection whenever it changes, and the wrapper
writes it in one write to the terminal it holds: the daemon never opens a
terminal, since pty device names are reused and terminal I/O stays in the
wrapper, and hooks write nothing there. One write keeps a report whole
beside the agent's own output, which the terminal never splits within a
write; it can still fall between two writes of one escape sequence the agent
draws, which garbles that frame until the agent next redraws, a cost Shoal
accepts for status a terminal shows outside the agent's screen. The wrapper
reports the agent's exit itself, also while the daemon is away. Reports go only to a terminal that
answers the protocol's query: before it hands the terminal to the agent, the
wrapper sends the query followed by a primary device attributes request,
which every terminal answers, so a device attributes reply arriving first
means the terminal does not read the protocol, and a generous time limit
guards only against a terminal that answers neither. tmux, zmx and Zellij
answer only that request, so Shoal sends nothing into a host that would drop
it; no host declares support and there is no option. Each launch queries
again, a detached one sends nothing, and input typed during the query, before
the agent starts, may be lost. A thread or approval request that waits on the
user blocks the space, and one a coordinating agent holds does not, since
`blocked` means the user must act; completion and ready marks show as `done`
between the agent's turns, so a new turn replaces them.

tmux is an opt-in host for users who already work in it, so agents run in their
tmux panes without a zmx session nested inside one unless `nested_zmx` asks
for it. tmux draws panes through its
own emulator, so images and notifications reach the outer terminal only through
its passthrough, off by default, and an attach redraws tmux's screen; Shoal
documents these limits rather than working around them. `terminal.tmux.enabled`
lets tmux serve its panes and, unset, follows whether `terminal.default_host`
names tmux, so choosing the default host opts in everywhere and tmux users who
set neither keep zmx. Inherited variables cannot tell which of nested hosts is
innermost, so hosts claim a terminal in a fixed order, the opt-in multiplexers
last: a place of a host that claims a terminal before tmux, such as a zmx
session, keeps its own behavior inside tmux. A tmux session is a
group and a pane a place, as a Rex session and block are: Shoal opens a window
in the session the caller's pane started in, or splits that pane, runs agents in
place, and outside tmux opens windows in a configured session of the default
server and attaches the terminal to show one. tmux starts a pane with a
command, so a new window runs the launch step, which reads the command and its
variables from a private file and drops other hosts' place variables the
server's environment may carry, then the user's login shell; nothing is typed
on screen. A demo, whose worker needs its pane, replaces the window's shell
with it. Only a resume in an existing pane types the launch step at its prompt.
A server Shoal starts gets none of Shoal's own variables. A queued start carries
the caller's `TMUX` and socket directory, empty when unset, so a daemon started
inside tmux never sends it to the daemon's own server, and a start queued from a
pane reaches its origin's server through the socket its origin records.
tmux numbers panes afresh when its server restarts, so a place records the
server's socket and instance, its process and start time, beside the pane ID,
and a recorded pane is Shoal's only on that instance. Panes carry no ownership
marks, so the daemon's records prove ownership as for Rex blocks, and a close
kills only that pane, leaving the user's other panes, windows and sessions.

Zellij is an opt-in host on the same terms as tmux, enabled the same way and
claiming a terminal after it: a Zellij session is a group and a pane a place, a
new place opens as a tab in the caller's session or splits the caller's pane,
and agents run in place. Zellij also redraws panes through its own emulator,
but handles images, notifications and clipboard writes itself, as its
documentation describes, so Shoal documents its limits as for tmux and keeps zmx
the default. Zellij 0.45 drops tabs opened in a session no client shows, which
a background start would otherwise lose, so a place for a session without a
client, or for a terminal outside Zellij, opens as the first pane of a session
of its own. Zellij keeps no configured group: generic code then hands the
host no origin, and the host decides between the origin's session and a new
one, since only it can tell whether a client shows a session. Under load
Zellij can list none of the clients that show a session, so the host asks a
few times, briefly, before it takes a session as shown by none. Zellij runs a
server per session and numbers panes per server, so a place's ID names its
session and pane, it records the socket directory, and its instance is the
server's process and birth; Zellij takes no condition with a command, so the
host checks the instance just before each command on a recorded pane. Zellij
counts a session only when its server answers a probe in time, so it can
report a busy session as missing, and a running session can list no pane
before it lays out its first tab or while it is busy: the host asks again while
the session's server process still runs, the recorded instance for a recorded
pane, and takes a session as ended only once it does not. Under load Zellij
can also let a command return before its answer and hand that answer to the
next command, so the host takes a tab or pane a command printed only when the
session listed no such one before, and asks a pane or client listing or
screen dump again when it holds another command's answer or none.
Zellij closes a tab or session with its last pane only while a client shows the
session, so a close ends either along with the last pane there, as an attached
client would see it, leaving no empty tab or session. A session's last pane
closes by ending the session, never its tab, since a `zellij action` whose
server ends before answering never returns; the pane records nothing
of how its session began, since the place records itself from its own
environment.

Terminal agent launches run in zmx sessions so agents outlive the terminal:
detaching or closing it leaves the agent running, and `attach` reattaches. zmx
passes output through unchanged while attached and restores the screen from its
own terminal state on attach, so terminal protocols such as images keep working,
which multiplexers that re-render output through their own emulator, like tmux,
break or support only in part; zmx therefore stays the default host. The daemon requires whichever host
`terminal.default_host` names, so a user who sets another one need not install
zmx. A launch refuses a zmx older than 1.0.1, as it does an old tmux or Zellij:
since 1.0.0 a zmx reaches only servers of its own version, and 1.0.0's macOS
release could not reach its own sessions. Shoal does not restart agents left in
sessions an older zmx started; it reports those sessions as unreachable. The tracked wrapper
runs inside the session's pty, so tracking, scope, stop and recovery are
unchanged and the daemon still never touches terminal I/O. A failed detached
handoff closes its session only while its process and creation time still match.
When zmx returns control to the caller, the CLI resets emulator input modes on the caller's
terminal, and an addition whose agent ran in the session leaves the caller's
shell where it was: the workspace is the session's, and detaching returns to
the shell the user left. Sessions are named
after their workspace and labelled with its ID and canonical state-directory identity,
so daemons sharing zmx can distinguish their sessions. A detached session that Shoal
starts labels itself once its workspace exists. A start queued from a zmx
session joins no group, so it goes to `terminal.default_host`. Launches in a place of an enabled
host run in place: a zmx session cannot show another session to its terminal,
and a host such as Herdr reads agent state from the place's process. A caller without a terminal, such as an
orchestrating agent's tool call, starts the agents of `start` and `restore --now`
detached through the daemon, as a scoped caller does: run in place, the agent
would hold the call until it exits and die with an interrupt of the caller.
An agent Happy runs leaves the call at once anyway, so such a caller runs it
in its own process once the workspace is prepared, as a start here does.
`start --here` keeps the in-place launch, for a script that waits on the agent's
exit code and for a scoped caller, such as a master session's terminal, that runs
an agent there; `restore --focus` keeps it for callers outside Shoal. A scoped
caller's `--here` claims its own launch's entry as the user's does; with no
slot free it leaves the agent queued and returns rather than holding the
terminal, since only the user's terminal starts agents beyond the limits. Its
agent's execution is its own space's, with that space's scope: an agent's wrapper,
and what it started, never count among the processes of another execution that
started it, and the wrapper leaves its starter's process group, which the
starter's wrapper signals as it stops, taking the terminal along when that group
held it, so the caller's stops leave the agent running. zmx reports no exit status, so a session launch
exits 0; the daemon still records the agent's exit.

`[terminal] nested_zmx`, which each host's table overrides as it does
`close_when_done`, runs what the user attaches to in a host's place, agents,
chats and demos, in a zmx session attached in that place, so any terminal,
such as one over ssh, reaches it as well, and a Rex block, which has no
terminal client, becomes reachable from one. It is off by default, since a host
that reads an agent's turn from its pane sees only zmx there. Generic code
decides it from the resolved value and never names a host: the host serving
a place nests the sessions zmx runs there, where the option does nothing on a
terminal no host serves, whose launches use zmx already, or in a zmx
session. Setup output, launch steps and the shells a place keeps stay in the
place, since nothing attaches to them. The nested launch keeps the place's
variables and sets `SHOAL_NESTED_SESSION`, so Shoal inside takes the place
for its own, opening starts beside it, restoring there, and closing it as
before, while the record of where an execution or demo runs nests the session
in the place's origin. Attach shows the place itself where its host serves
the terminal, or to a viewer whose app for the host shows it, and attaches to
the session from any other terminal. The session carries the labels of any
zmx session Shoal creates, so removal and the sweeps close it with its owner,
and a demo's close ends it with its place, whose closing ends only the zmx
client.
