# Terminal hosts

This file says how Shoal works in each terminal host it supports besides zmx,
the default host, which [Agents](agents.md) covers.

## Places

Inside a pane or block a host serves, `shoal start`, chats, background resumes
and demos open a new place beside yours, such as a Herdr tab or a tmux window.
The host's `tab_name` or `window_name` sets each place's label, as the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#herdr)
describes.

With the host's `placement = "split"`, a start from your own place, other than
a demo, splits that place instead. The new pane or block opens to its right and
takes the label. Shoal renames a window or tab only while the place it opened
is alone there, so a split leaves your window's name as it was. A queued
start, or a start from outside the host, opens a new tab or window.

Once the worker exits, the place keeps your login shell. With
[shell integration](install.md#shell-integration), the shell enters the
workspace at its next prompt.

`--here` keeps a launch in your current place. A call without a terminal, such
as an agent's tool call, opens no place: its agent starts detached, as
[Agents](agents.md) describes.

[`terminal.focus`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#terminal)
decides whether a new place takes the terminal. `shoal start -f`/`--focus`
focuses one launch and starts it at once, as `--now` does. With `--json`, a
start beside the terminal prints the planned `workspace` and its `agent` once
the host opens the launch's place.

With `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their places, as [Agents](agents.md) describes.

## Launches from other terminals

[`[terminal] default_host`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#terminal)
names the host for launches from a terminal no host serves. Each host's section
says where its places open. A focused start, a chat or an agent launch then
shows its place in your terminal, as `shoal attach` does, until you
detach. Rex shows it in its app instead.

## Queued starts

A start queued from a host's place opens a background place in the group it
was queued from, such as the Herdr workspace or the tmux session. It falls back
to the group the host's config names on the same server, then to
`terminal.default_host`. Zellij keeps no configured group, and its section says
where its queued starts go.

## Closing places

Shoal closes only the places it created. A place you opened stays open, even
after an agent ran there. A place Shoal opened passes to another space once
that space's agent starts in it, and closes with that space.

A close ends only that pane or block, never the panes, blocks, tabs, windows or
sessions beside it. A pane that takes a recorded pane's ID after its tmux or
Zellij server restarts is never Shoal's.

A failure before the agent starts leaves its place open. So does the agent's
exit, so work can continue in the place's shell.

When `done` keeps a workspace, Shoal closes the place it opened for it once
tracked executions resolve or the agent finishes its turn, as
`close_when_done` sets. The place stays open while stopped work waits for
`shoal resume`. It also stays open once the daemon has been stopped for more
than two minutes while the place was open.

An agent's turn comes from its hooks. For an agent without them, Herdr reports
the turn, except under `nested_zmx`, where it sees only zmx in the pane. Under
a host that reports none, such as tmux, such an agent's place closes once its
execution ends.

Removing a space closes its places at once, whatever `close_when_done` says.
Daemon startup and `shoal cleanup` close the places whose space is gone.

A place `shoal resume` opens in the background closes the same way. A resume
in an existing place keeps that place's behavior.

The place `shoal create` opens holds a shell in the ready workspace. It stays
open until the workspace is done or removed.

## Herdr

Shoal serves a Herdr pane when `HERDR_ENV` is `1`.
[`[terminal.herdr]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#herdr)
configures it.

Inside Herdr, interactive `shoal start` looks up the issue and makes the
repository, branch and agent choices in your pane. It then opens a tab in the
Herdr workspace `HERDR_WORKSPACE_ID` names, and returns once it has submitted
the command there. Setup and the agent run in that tab, which reads the issue
again.

Herdr closes a tab with its last pane. Shoal closes only the pane it opened the
tab with, so a pane you split off keeps the tab open.

With `default_host = "herdr"`, launches from other terminals open their tabs
in the `workspace_name` Herdr workspace. That workspace is on the Herdr server
your `herdr` reaches, which must be running. Shoal reuses the first Herdr
workspace with that label, or creates one. Showing a tab attaches the terminal
to the Herdr session that holds it. A background start says where it went.

## Rex

Shoal serves a [Rex](https://www.superlogical.com/rex) block when `REX_BLOCK`
is set.
[`[terminal.rex]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#rex)
configures it.

Places open as windows in the Rex session that holds your block. Rex keeps
sessions running without a connected app, so an agent keeps running when the
Mac app or the phone disconnects.

A new block runs one short command. It loads the worker and its variables from
a private file and deletes the file, so neither shows on screen or in shell
history.

With `default_host = "rex"`, launches from other terminals open their windows
in the `session_name` session on the local Rex server. Shoal reuses the oldest
session with that label, or creates one. A focused start selects its window in
the connected Mac Rex app and gives the terminal its shell back. Without a
connected app, it says to open Rex.

Shoal looks for `rex` in Rex's bin directory, then on `PATH`, then in the Rex
app. It needs Rex API revision 7 or later.

`shoal attach` makes a block's window active in Rex. From another terminal, it
also selects the window in the Mac app. A block that is gone leaves nothing to
attach.

Rex sends no push notifications to the phone. A demo's screen of a full-screen
program shows only its current screen. A Rex app on another device opens the
`localhost` link a serving demo prints through a forward to the Rex server's
machine.

## tmux

[tmux](https://github.com/tmux/tmux) is opt-in.
[`[terminal.tmux]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#tmux)
turns it on and configures it. Shoal then serves a tmux pane when `TMUX` and
`TMUX_PANE` are set. A place of a host that claims the terminal before tmux,
such as a zmx session, keeps its own behavior inside tmux.

Places open as windows in the session your pane started in. A new window runs
the agent directly, with nothing typed on screen. The tmux server keeps panes
running without a client, so detaching (`prefix d`) or closing the terminal
leaves the agent running.

With `default_host = "tmux"`, launches from other terminals open their windows
in the `session_name` session on the default tmux server. `TMUX_TMPDIR`
chooses that server's socket directory. Shoal starts the server when needed.
It reuses the first session of that name, with `.` and `:` replaced by `_`, or
creates one with a first window in your home directory.

Shoal needs tmux 3.3 or later.

`shoal attach`'s picker and `shoal status` list a tmux pane as a session named
after its window. `shoal attach` selects the pane and switches your tmux client
to it. From outside tmux, it attaches a new client. Inside tmux, it refuses a
pane on another tmux server.

Prompts and `shoal demo send` leave copy mode first.

tmux draws panes with its own terminal emulator, so terminal protocols work
only as far as tmux supports them:

- Inline images, such as Kitty graphics and iTerm2 images, and OSC 9 and 777
  notifications reach your terminal only with `set -g allow-passthrough on`
  and a program that wraps them for tmux. An image does not survive a redraw
  or a reattach.
- OSC 52 clipboard writes from agents need `set -s set-clipboard on`. The
  default, `external`, forwards only tmux's own copies.
- Attaching redraws tmux's screen instead of replaying the output.

## Zellij

[Zellij](https://zellij.dev) is opt-in.
[`[terminal.zellij]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#zellij)
turns it on and configures it. Shoal then serves a Zellij pane when
`ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` are set. A place of a host that
claims the terminal before Zellij, such as a tmux pane, keeps its own behavior
inside Zellij.

Places open as tabs in your session. A new tab runs the agent directly, with
nothing typed on screen. Each session's server keeps its panes running without
a client, so detaching (`Ctrl o d`) or closing the terminal leaves the agent
running.

A place for a session no client shows opens as the first pane of a background
session of its own. That session is named after the place, cut to 20
characters. While another session, exited ones included, has the name, Shoal
adds `-2`, `-3` and so on. The session's tab shows the whole name.

With `default_host = "zellij"`, every launch from another terminal opens such a
session. A start queued from Zellij opens a background tab in the session it
was queued from while a client shows that session. Otherwise it opens a session
of its own.

`ZELLIJ_SOCKET_DIR` chooses the sessions Shoal reaches, as it does for
`zellij`. From Zellij 0.46, so does the cache directory where Zellij records
session names, which `HOME`, or `XDG_CACHE_HOME` on Linux, sets. The daemon
reaches the sessions started with its own. Shoal needs Zellij 0.45 or later.

`shoal attach`'s picker and `shoal status` list a Zellij pane as a session
named after its tab. `shoal attach` selects the tab and pane and switches your
Zellij client to its session. From outside Zellij, it attaches a new client.
Inside Zellij, it refuses a pane in another socket directory.

Like its other panes, a close leaves a tab's plugins alone. Closing the last
pane of a tab closes the tab, and closing the last pane of a session ends the
session, as Zellij does while a client shows the session.

Zellij does not report the process in a pane's foreground. A resume runs in an
existing pane only while Zellij names your login shell (`SHELL`) as what runs
there, with no script or command.

Zellij draws panes with its own terminal emulator and handles terminal
protocols itself:

- It forwards Kitty graphics and Sixel images where your terminal supports
  them. Other image protocols do not reach your terminal.
- It forwards OSC 9, 99 and 777 notifications as its
  `host_notification_protocol` sets.
- It forwards OSC 52 clipboard writes. Programs read no clipboard unless
  `dangerously_enable_paste_buffer_read` is set.
- Attaching redraws Zellij's screen instead of replaying the output.
