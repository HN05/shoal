# Terminal hosts

This file says how Shoal works in each terminal host it supports.

## Herdr

Inside Herdr (`HERDR_ENV=1`), interactive `shoal start` looks up
the issue and resolves repository, branch, and agent choices in the caller's pane,
then opens a tab in `HERDR_WORKSPACE_ID` and returns once the command is submitted
there. Setup and agent execution run in that tab, which reads the issue again and
is labeled `<repo>#<number>` for an issue (such as `shoal#375`) and with the
allocated workspace branch otherwise, unless `tab_name` supplies a template;
a [space's name](workspaces.md) replaces either.
`--here` keeps the command in the current pane; help and calls without a
terminal run in place, except that their agent starts detached as
[Agents](agents.md) describes.

[`[terminal.herdr]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#herdr) configures it. New tabs stay in the
background by default when launching an agent with an issue prompt or forwarded
arguments, and focus otherwise, so a workspace
shell or an agent waiting for its first prompt is ready to type into;
`terminal.focus` overrides this for every host's places alike, the
[mode's](modes.md) `focus` overrides that for its launches, and
`shoal start -f`/`--focus` focuses one launch and starts it at once, as `--now` does.
With `--json`, a start beside the terminal prints the planned `workspace` and
its `agent` once the host opens the launch's place. With `enabled = false` Shoal
treats a Herdr pane as a plain terminal. A `tab_name`'s `{branch}` is the
requested branch when the tab opens and the allocated branch after creation.
With `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their panes, as [Agents](agents.md) describes, and Herdr reads no turn from an
agent without turn hooks there, as in a zmx session.

Failures before agent execution leave the tab open. Agent exits leave the tab
open so work can continue, and with shell integration its shell enters the
workspace at its next prompt; completion closes it once tracked executions resolve
or the agent finishes its turn, as its hooks report it or, for an agent without
them, as Herdr does, unless stopped work waits for `shoal resume` or the daemon
stayed stopped for more than two minutes while the tab was open. Removing its
workspace or chat closes it immediately, regardless of `terminal.close_when_done`, and daemon
startup and `shoal cleanup` close those whose workspace or chat is gone. Shoal closes
only the pane it opened the tab with, so a pane you split off keeps the tab
open, and a tab you opened stays open. A background tab `shoal resume` opens
closes the same way; a pane it reuses keeps its tab's behavior. `shoal create`
opens an interactive shell in the ready workspace and leaves the tab open until
that workspace is completed or removed.

With `[terminal] default_host = "herdr"`, launches from a terminal no host serves
open their tabs in the `workspace_name` workspace of the Herdr server the
terminal's `herdr` reaches, which must be running; Shoal reuses the first workspace with that label or creates
it. A focused start, a chat or an agent launch then attaches the terminal to
the Herdr session holding its tab, as `shoal attach` does, until you detach; a
background start says where it went. With `terminal.herdr.enabled = false` these
launches run in place, and `new_tab = false` keeps queued ones out of Herdr. A start queued from Herdr opens a background
tab in the workspace it was queued from, then falls back to the
`workspace_name` workspace on the same server, then to `terminal.default_host`.

## Rex

Inside a [Rex](https://www.superlogical.com/rex) terminal (`REX_BLOCK` is set),
`shoal start`, chats, background resumes and demos open a new window in the
Rex session that holds the block, placed and labelled as a Herdr tab would
be, and agents run in place in their block. With `placement = "split"`, a start
from your block other than a demo splits that block instead, opening to its
right in the same window; the new block takes the label and the window keeps
its name. Starts queued from Rex and starts from outside Rex still open
windows. Rex keeps sessions running without a connected app, so an agent keeps
running when the Mac app or the phone disconnects. Rex cannot set a new
terminal's environment: the new block starts with one short command, which
loads the worker and its variables from a private file and deletes it, so
nothing shows on screen or in shell history. Your login shell starts in the
block once the worker exits, as in a Herdr tab.

[`[terminal.rex]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#rex) configures it; with `enabled = false` Shoal
treats a Rex terminal as a plain terminal. With `nested_zmx`, agents, chats
and demos run in zmx sessions attached in their blocks, as [Agents](agents.md)
describes, so a terminal outside Rex, such as over ssh, attaches to them.

With `[terminal] default_host = "rex"`, launches from a terminal no host serves
open their windows in the `session_name` session on the local Rex server; Shoal reuses the oldest session with that label or creates it;
with `terminal.rex.enabled = false` these launches run in place, and `new_window = false`
keeps queued ones out of Rex.
Rex has no terminal client, so a focused start there selects the window in the
connected Mac Rex app and gives the terminal its shell back; without a
connected app it says to open Rex. A start queued from Rex opens a background
window in the session it was queued from, then falls back to the
`session_name` session on the same server, then to `terminal.default_host`.
Shoal finds `rex` in Rex's bin directory, on PATH, then in the Rex app, and
needs Rex API revision 7 or later.

`shoal attach` and `shoal status` find a workspace's or chat's Rex block by the
place its agent was recorded in: attach makes the block's window active inside
Rex and selects it in the Mac app from another terminal; a block that is gone
leaves nothing to attach. Shoal closes only the blocks it created, and never
their window's other blocks: completion and removal close them as they close a
Herdr tab, and daemon startup and `shoal cleanup` close those whose workspace
or chat is gone. A block you opened stays open, even after an agent ran there,
and one Shoal opened belongs to another workspace or chat once its agent
starts in it, and closes with that one instead. Rex reports
no agent turn state, so an agent without turn hooks closes its block once its
execution ends. Rex sends no push notifications to the phone, and a demo's
screen of a full-screen program shows only its current screen. A Rex app on
another device opens the `localhost` link a serving demo prints in its block
through a forward to the Rex server's machine.

## tmux

[tmux](https://github.com/tmux/tmux) is opt-in. With `terminal.tmux.enabled`,
inside a tmux pane (`TMUX` and `TMUX_PANE` are set) `shoal start`, chats,
background resumes and demos open a new window in the session the pane started
in, placed and labelled as a Herdr tab would be, and agents run in place in
their pane; `placement = "split"` splits your pane instead, as in Rex. The tmux
server keeps panes running without a client, so detaching (`prefix d`) or
closing the terminal leaves the agent running. The new window runs the agent
directly, with nothing typed on screen, then your login shell, which stays
after the worker exits and enters the workspace as a Herdr tab's shell does. A place of a host
that claims a terminal before tmux, such as a zmx session, keeps its own
behavior inside tmux. [`[terminal.tmux]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#tmux) configures it;
with `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their panes, as [Agents](agents.md) describes.

With `[terminal] default_host = "tmux"`, which also turns `enabled` on, launches
from a terminal no host serves open their windows in the `session_name` session
on the default tmux server (`TMUX_TMPDIR` chooses its socket directory), which
Shoal starts when needed; Shoal reuses
the first session with that name, `.` and `:` replaced by `_`, or creates it
with a first window in your home directory. A
focused start, a chat or an agent launch then attaches the terminal to that
session until you detach. A start queued from tmux opens a background window
in the session it was queued from, then falls back to the `session_name`
session on the same server, then to `terminal.default_host`. Shoal needs tmux
3.3 or later.

`shoal attach` and `shoal status` find a workspace's or chat's pane by the place
its agent was recorded in, named after its window: attach selects the pane and
switches your tmux client to it, or attaches a client from outside tmux; a
pane on another tmux server is refused inside tmux. A window is renamed only
while the pane Shoal opened is alone in it. Shoal closes only the panes it
created, as it closes Rex blocks, and never their window's other panes, other
windows or sessions; a pane is recorded with its server's process and start
time, so a pane under the same ID after the server restarts is never Shoal's.
tmux reports no agent turn state, so an agent without turn hooks closes its
pane once its execution ends. Prompts and `shoal demo send` leave copy mode
first.

tmux draws panes through its own terminal emulator rather than passing output
through as zmx does, so terminal protocols work only as far as tmux supports
them: inline images (Kitty graphics, iTerm2 images) and OSC 9 and 777
notifications reach your terminal only with `set -g allow-passthrough on` and
a program that wraps them for tmux, and an image does not survive a redraw or a
reattach; OSC 52 clipboard writes from agents need `set -s set-clipboard on`,
since the default `external` forwards only tmux's own copies. Attaching redraws tmux's
own screen rather than replaying the output.

## Zellij

[Zellij](https://zellij.dev) is opt-in. With `terminal.zellij.enabled`, inside a
Zellij pane (`ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` are set) `shoal start`,
chats, background resumes and demos open a new tab in your session, placed and
labelled as a Herdr tab would be, and agents run in place in their pane;
`placement = "split"` splits your pane instead. Each session's server keeps its
panes running without a client, so detaching (`Ctrl o d`) or closing the
terminal leaves the agent running. The new tab runs the agent directly, with
nothing typed on screen, then your login shell, which stays after the worker
exits and enters the workspace as a Herdr tab's shell does. A place of a host
that claims a terminal before Zellij, such as a tmux pane, keeps its own
behavior inside Zellij. [`[terminal.zellij]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#zellij) configures it;
with `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their panes, as [Agents](agents.md) describes.

Zellij 0.45 drops a tab opened in a session no client shows, so a place for
such a session opens as the first pane of a background session of its own,
named after the place, up to 20 characters and suffixed with `-2`, `-3` while
another session, exited ones included, has the name, and its tab shows the
whole name. With `[terminal] default_host = "zellij"`, which also turns
`enabled` on, launches from a terminal no host serves open such sessions; a
focused start, a chat or an agent launch then attaches the terminal to its
session until you detach. A start queued from Zellij opens a background tab in
the session it was queued from while a client shows it, otherwise a session of
its own. `ZELLIJ_SOCKET_DIR` chooses the sessions Shoal reaches, as it does
for `zellij`, and from Zellij 0.46 so does the cache directory where Zellij
records session names, which `HOME`, or `XDG_CACHE_HOME` on Linux, sets: the
daemon reaches sessions started with its own. Shoal needs Zellij 0.45 or later.

`shoal attach` and `shoal status` find a workspace's or chat's pane by the place
its agent was recorded in, named after its tab: attach selects the tab and pane
and switches your Zellij client to its session, or attaches a client from
outside Zellij; a pane in another socket directory is refused inside Zellij. A tab is renamed only while the pane Shoal opened is alone in
it. Shoal closes only the panes it created, as it closes Rex blocks, and never
their tab's other panes or plugins, other tabs or sessions; it closes a tab
with the last pane there, and ends a session with the last pane there, as
Zellij does while a client shows the session. A pane is recorded with its
session and its server's process and start time, so a pane under the same ID
after the session restarts is never Shoal's. Zellij takes no condition with a
command, so Shoal checks the server just before each command on a recorded
pane. Zellij reports neither agent turn state nor the process in a pane's
foreground, so an agent without turn hooks closes its pane once its execution
ends, and a resume runs in a pane only while Zellij names your login shell
(`SHELL`), running no script or command, as what runs there.

Zellij draws panes through its own terminal emulator rather than passing output
through as zmx does, and handles terminal protocols itself: it forwards Kitty
graphics and Sixel images where your terminal supports them, OSC 9, 99 and 777
notifications as `host_notification_protocol` sets, and OSC 52 clipboard
writes; programs read no clipboard unless
`dangerously_enable_paste_buffer_read` is set. Other image protocols do not
reach your terminal, and attaching redraws Zellij's own screen rather than
replaying the output.
