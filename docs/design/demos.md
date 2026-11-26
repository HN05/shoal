# Demos

This file covers `shoal-demos`: demos and how what they show reaches the user.
The [demos reference](../reference/demos.md) says what the `demo` commands do.

Demos let the user try a change where the agent's windows are out of sight,
usually over SSH. A demo is a named command that the repository configures or
an agent gives. A workspace runs several demos.

## The wrapper

A demo runs in a wrapper, an ordinary tracked command that records the demo
with its execution. Stopping, removal and recovery therefore handle a demo
through that execution, as they handle any other.

The wrapper runs the command again when it exits. It waits longer after each
quick exit, so a failing command keeps its error on screen.

## Where a demo runs

The user opens and reads a demo, and the agent reads and types into it. A demo
therefore needs a terminal. The CLI places the wrapper where queued agents
start, through the [terminal hosts](terminals.md). A start that no host takes
fails, since a demo without a terminal is of no use.

A demo's session closes once the demo's end is verified. That includes a
disconnected wrapper the daemon stops. Shoal closes it only while the host
still verifies that the demo's launch owns it, so a close never ends a session
that another launch took over.

The request to close commits with the deletion of the demo's execution. A
failed close thus stays recorded, and Shoal retries it, even while the
workspace remains.

## How a demo reaches the user

Shoal knows how a demo reaches the user only through its preset. A preset
reserves ports or leases a simulator, and says how to open the demo. The
repository's `open` command replaces the preset's way on the demo's own
machine.

A serving demo prints where it listens in its own place before each run. A
terminal host that forwards a `localhost` link from another device, such as a
Rex app, then opens it there. Opening it from a plain SSH session names the
`ssh -L` forward.

An agent runs its own workspace's demos without approval, as
[its own space](permissions.md#its-own-space) allows. The agent can already
run the command, and a demo only keeps it running and tells the user.

## Streams to a demo

Shoal carries a demo's bytes to a device or another joined machine. It renders
nothing and parses no demo's protocol.

One request opens a stream to a demo. The request names a space and a demo,
never a raw port or device, so a client reaches only what a demo exposes. The
[hub](hub.md) routes it to the space's machine as any other request. The user
and devices open any demo, and an agent the demos in its scope, without
approval, as they read and type into one.

The demo's kind decides what the stream carries.

### Server and web demos

A `server` or `web` demo's stream is one TCP connection. The demo's machine
opens it to the demo's reserved port on loopback, `127.0.0.1` else `::1`, and
relays it both ways. Shoal never reads HTTP, so WebSockets and hot reload
work.

An app listens on a local port for the stream and points its web view there.
The demo listing still shows the demo's own URL. `shoal demo` does the same from the command line for a
demo another machine runs.

### Simulator demos

A `simulator` demo's stream carries the port of a viewer of its device, as a
`web` demo's does. The viewer is an external tool, serve-sim unless the demo
names another. Shoal starts it and relays its bytes without parsing its
protocol, so a change to the protocol changes only the UI that speaks it.
serve-sim's HTTP mode suits the hub's TCP streams, where WebRTC needs UDP.

The daemon starts the viewer on the demo's first open from elsewhere, so a
demo nobody opens runs none. The viewer carries the marker of the demo's
execution, so stopping, removal and recovery stop it with the demo's other
processes. It outlives the command's runs and the daemon's restarts, which find
it by the process recorded with the demo.

Shoal installs no viewer. A missing one leaves the demo running and fails the
open. Agents drive the device with serve-sim's or AXe's own commands, which
Shoal does not wrap.

### Terminal demos

A terminal demo's stream is an attach to another machine's session, pointed at
the demo's place. The demo's machine runs a worker on a terminal it makes at
the viewer's size. The worker shows the place through the host callback that
shows a session there. It carries the same frames as any attach: the screen
one way, keys and resizes the other. The host's own detach key thus detaches,
and a device renders the bytes in its own terminal emulator.

### Agents on another machine

Shoal does not support an agent on one machine building and running a demo on
another. That needs leases across machines.

## Following demos

The [journal](daemon.md) records a demo's start at its command's first run, and
its stop when its record goes with the execution. Clients that show demos thus
follow them without polling. Restarts between runs are no changes.

The listing carries each demo's kind and a web demo's URL. The daemon derives
the URL as opening does, so clients neither infer the kind from the preset nor
rebuild the URL.
