# Demos

This file says what the `demo` commands do.

A demo is a command Shoal keeps running in a workspace for the user to try. A
workspace runs several demos, each under its own name. A repository configures
its demos in
[`[demos.NAME]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#demos)
tables, and an agent can start any command as a demo.
[Demos](../design/demos.md) in the design says why Shoal has them.

## Presets

A demo's preset sets up how it reaches the user:

| Preset | Sets up |
|---|---|
| `server` | Reserves a TCP port and passes it as `PORT`. The port takes the demo's name, or is the first of `ports`. Opening prints `localhost:<port>`, and over SSH the `ssh -L` command that forwards it to your machine. |
| `web` | A `server` for a page you open in a browser. Opening prints `http://localhost:<port>`, or `https://` with `scheme = "https"`. |
| `simulator` | Leases the workspace's `default` simulator and passes its UDID as `SHOAL_SIM_UDID`. The command installs and launches the app and follows its log. Opening brings the device forward in Simulator, or starts the demo's [viewer](#simulator-viewer) over SSH and from another machine. |

A demo's `ports` are [port reservations](resources.md#port-reservations) that
Shoal makes before the demo starts. When a configured port is in use, Shoal
reserves another free one.

A `simulator` demo leases its device as `shoal acquire sim` does, and
`--queue` waits for one. It takes over a lease an agent acquired, which then
ends with the demo. A lease the user acquired keeps
[its own rules](resources.md#leases).

## Start a demo

```sh
shoal demo start [NAME] [--workspace W] [--preset P] [--scheme S] [--port NAME]... [--open COMMAND] [--queue] [-- COMMAND...]
```

`shoal demo start` starts the configured demo `NAME`, or the only configured
one. A command after `--` replaces the configured command. With no demo
configured under `NAME`, it defines one, named `demo` when `NAME` is left out.

`--preset`, `--scheme` and `--port` replace the configured values. `--open`
replaces the configured `open` with a shell command.

Starting a demo that runs already replaces it. The start returns once the demo
runs and prints how to open it. A start from a workspace process also notifies
the user.

## Open a demo

```sh
shoal demo [NAME] [--workspace W]
```

`shoal demo` opens the running demo `NAME`. Without a name, it offers a picker
of the running demos: those of the current workspace, or of every workspace
outside one.

Opening a demo on this machine needs a terminal and takes the first of these
that applies:

1. The demo's `open` command runs in the workspace, with the demo's variables.
2. The demo's [preset](#presets) opens it its own way.
3. Shoal shows the demo where it runs, as [`shoal attach`](agents.md#attach-to-an-agent) shows a
   session. Inside the demo's host, the host focuses its place. From another
   terminal, Shoal attaches to the place, or shows it where the host shows its
   places, such as Rex's Mac app.

### Simulator viewer

A simulator demo's viewer shows its device in a browser and takes touches,
rotation and hardware buttons. The viewer is serve-sim, unless the demo's
`viewer` names another command.

The viewer starts when someone first opens the demo over SSH, from another
machine or from an app. It listens on loopback, on a port reserved under the
demo's name, as the opener would reserve it. A port that needs approval refuses
the open until it is approved. The viewer then runs until the demo stops, and
a demo nobody opens runs none.

Over SSH, opening prints the viewer's URL and the `ssh -L` command that
forwards it. Without serve-sim, the demo keeps running and opening fails,
naming the command that installs serve-sim.

### A demo on another machine

`shoal demo [NAME] --workspace W` opens a demo that another
[machine of the hub](hub.md) runs.

For a `server`, `web` or `simulator` demo, Shoal listens on this machine's
loopback and prints the local `localhost` address or URL. It takes the demo's
own port number, or its viewer's, when that is free, and any port otherwise.
It carries each connection to the demo through the hub, both ways and unread,
so WebSockets work. It runs until Ctrl-C.

An agent runs it in the foreground for a demo in its scope. With `--json`, it
first prints the demo's `name`, its `machine`, the local `location` and the
`port`.

A terminal demo opens in your terminal, which it needs, as `shoal attach`
opens a session on another machine.

A demo whose machine is offline fails at once. The error names the machine and
when the hub last saw it.

## List and drive demos

`shoal demo ls` lists the configured and running demos. In JSON, each running
demo has its `kind`: `terminal`, `server`, `web` or `simulator`. A web demo
adds its `url`, and a simulator demo its `viewer`.

`shoal demo screen NAME [--lines N]` prints the end of what a demo shows.
`shoal demo send NAME KEYS` types into it. Together they let an agent see and
drive the screen you are looking at.

## Where a demo runs

Each demo runs as a tracked command, in the place where queued agents start.
That place is a background place of the host the demo was started from, such
as a Herdr tab. Otherwise it is one of `terminal.default_host`, such as a
detached zmx session named `<workspace>-<name>`.

A demo needs a [terminal host](terminals.md). Without one, such as when zmx is
missing from `PATH`, `shoal demo start` fails, says why and starts nothing.

Before each run, a `server` or `web` demo without `open` prints in its place
where it listens, as opening prints it.

When the command exits, Shoal runs it again. An exit within 10 seconds of a
start doubles the wait before the next run, from 1 second up to 30 seconds.

## When a demo ends

`shoal demo stop [NAME]` stops one demo, or every demo of the workspace.
`shoal stop`, [`shoal done`](completion.md) without `--keep-demos` and the
workspace's removal also end its demos. A demo's session ends with it.

A running demo keeps its workspace from idle cleanup, as other commands do.

When Shoal fails to close a demo's session, it notifies you. It retries the
close at daemon startup and on `shoal cleanup`, even while the workspace still
exists.
