# Demos

This file says what the `demo` commands do.

A demo is a command Shoal keeps running in a workspace so you can try a
change, usually from another machine over SSH, where windows an agent opens
stay out of sight. A repository names its demos in
[`[demos.NAME]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#demos) tables. A demo's `ports` are
[reservations](resources.md#port-reservations) made before it starts, taking another free
port when a configured one is in use, and its `preset` sets up how it reaches
you:

| Preset | Sets up |
|---|---|
| `server` | Reserves a TCP port named after the demo, or the first of `ports`, and passes it as `PORT`. Opening prints `localhost:<port>`, and over SSH the `ssh -L` command that forwards it to your machine. |
| `web` | `server` for a page you open in a browser: opening prints `http://localhost:<port>`, or `https://` with `scheme = "https"`. |
| `simulator` | Leases the workspace's `default` simulator as `shoal acquire sim` does (`--queue` waits for one) and passes its UDID as `SHOAL_SIM_UDID`; the command installs and launches the app and follows its log. Opening brings the device forward in Simulator. Opened over SSH or from another machine, it starts the demo's viewer, which shows the device in a browser and takes touches, rotation and hardware buttons. The demo takes over the lease when an agent acquired it, so it ends with the demo; the user's stays until `shoal release sim` or removal. |

`shoal demo start [NAME] [--workspace W] [--preset P] [--scheme S] [--port NAME]... [--open COMMAND] [-- COMMAND...]` starts
the configured demo `NAME`, or the only one configured. A command after `--`
replaces the configured one, or defines a demo of that name, `demo` by default;
`--preset`, `--scheme` and `--port` replace the configured ones, and `--open` replaces `open`
with a shell command. Starting a running name replaces
it. A workspace can run several demos. The start returns once the demo runs,
printing how to open it, and a workspace process also notifies you.

`shoal demo [NAME] [--workspace W]` opens a demo: a picker of the running demos
of the current workspace, or of every workspace outside one, or the demo `NAME`.
Opening runs the demo's `open` command in the workspace with the demo's
variables, otherwise shows it where it runs, as `shoal attach` shows a session: its
host focuses its place inside that host, and from another terminal attaches to it
or shows it where the host shows its places, such as Rex's Mac app.
A simulator demo's viewer, serve-sim unless its `viewer` names another command,
starts when the demo is first opened over SSH, from another machine or by an
app, bound to loopback on a port reserved under the demo's name as the opener
reserves it, so a port that needs approval refuses the open until it is
approved, and runs
until the demo stops; a demo nobody opens runs none. Over SSH opening prints
its URL and the `ssh -L` command that forwards it. Without serve-sim the demo
keeps running and opening fails, naming the command that installs it.
`shoal demo [NAME] --workspace W` for a `server`, `web` or `simulator` demo another
[machine of the hub](hub.md) runs listens on this machine's loopback, on the
demo's own port number, or its viewer's, when it is free and otherwise on any, prints the local
`localhost` address or URL there, and carries each connection to the demo
through the hub, both ways and unread, so WebSockets work, until Ctrl-C. An
agent runs it in the foreground for a demo in its scope; with `--json` it
prints the demo's `name`, its `machine`, the local `location` and `port`
first. A terminal demo another machine runs opens in your terminal, which it
needs, as [`shoal attach`](agents.md) opens a session there. A demo whose
machine is offline fails at once, naming the machine and when the hub last
saw it.
`shoal demo ls` lists the configured and
running demos, with each running demo's `kind` (`terminal`, `server`, `web`
or `simulator`), a web demo's `url` and a simulator demo's `viewer` in its JSON, `shoal demo screen NAME [--lines N]` prints the end of what it
shows, and `shoal demo send NAME KEYS` types into it, so an agent sees and drives
the screen you are looking at. `shoal demo stop [NAME]` stops one demo, or every
demo of the workspace.

Each demo runs as a tracked command where queued agents start: in a background
place of the host it was started from, such as a Herdr tab, otherwise of
`terminal.default_host`, such as a detached zmx session
named `<workspace>-<name>`. Before each run, a `server` or `web` demo without
`open` prints there where it listens, as opening prints it. A demo needs a terminal host: without one, such as
when zmx is missing from `PATH`, `shoal demo start` fails, saying why, and
starts nothing. A command that exits runs again; an exit
within 10 seconds of a start doubles the wait before the next run, from 1 to 30
seconds. Stopping the demo, `shoal stop`, `shoal done` without `--keep-demos`
and removing the workspace end it, and its session with it; a running demo keeps the workspace from idle cleanup as
other commands do. Failed session closes notify and retry at startup or with
`shoal cleanup`, including while the workspace still exists.
