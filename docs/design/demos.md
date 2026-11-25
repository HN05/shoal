# Demos

This file covers `shoal-demos`: demos and how what they show reaches the user.

Demos let the user try a change where the agent's windows are out of sight,
usually over SSH. A demo is a named command the repository configures or the
agent gives, and a workspace runs several. Its wrapper is an ordinary tracked
command that records the demo with its execution, so stopping, removal and
recovery use that execution. Its verified end closes the demo session, including
a disconnected wrapper stopped by the daemon, only while the host still verifies
its launch ownership. Pending closes persist with execution deletion and retry
at startup or cleanup, even while the workspace remains. The wrapper
restarts the command, backing off after quick exits so a failing command keeps
its error on screen. The CLI places the wrapper where queued agents start,
through the terminal hosts, since a demo is something the user opens and reads
and the agent reads and types into; a start no host takes fails rather than
run a demo without a terminal. Shoal knows how a demo reaches
the user only through presets, which reserve ports or lease a simulator and say
how to open it; the repository's `open` command replaces that on the demo's own
machine. A serving demo prints where it listens in its own place before each
run, so a terminal host that forwards a `localhost` link from another device,
such as a Rex app, opens it there, and opening it from a plain SSH session
names the `ssh -L` forward. Starting, reading and stopping
a demo are own-workspace actions without approval: the agent can already run
the command, and a demo only keeps it running and tells the user.
Shoal carries a demo's bytes to a device or another joined machine, rendering
nothing and parsing no demo's protocol. One request opens a stream to a demo:
it names a space and a demo, never a raw port or device, so a client reaches
only what a demo exposes, and the hub routes it to the space's machine as any
other request, failing at once while that machine is offline, naming it and
when the hub last saw it. The user and devices open any demo and an agent the
demos in its scope, without approval, as they read and type into one. The
demo's kind decides what the stream carries. A `server` or `web` demo's stream
is one TCP connection, which the owning machine opens to the demo's reserved
port on loopback, `127.0.0.1` else `::1`, and relays both ways under the
stream's flow control, each direction with credit of its own; Shoal never
reads HTTP, so WebSockets and hot reload work. A UI listens on a local port
and points its web view there, while the listing keeps the demo's own URL.
`shoal demo` on a serving demo another machine runs does the same from the
command line, on the demo's own port number when it is free, until Ctrl-C;
an agent may run it in the foreground for a demo in its scope. A `simulator`
demo's stream carries the port of a viewer of its device, as `web`. The
viewer is an external tool, serve-sim unless the demo's `viewer` names another
command: Shoal starts it and relays its bytes, never parsing its protocol, so
only the UI that speaks it changes with it. The daemon starts it on the
demo's first open from elsewhere, bound to loopback on a port reserved under
the demo's name, with the demo's execution's marker, so stopping, removal and
recovery stop it with the demo's other processes; it outlives the command's
runs and the daemon's restarts, which know it by the process recorded with the
demo, and a demo nobody opens runs none. serve-sim's HTTP mode suits the
hub's TCP streams, where WebRTC needs UDP. A missing viewer leaves the demo
running and fails the open, naming its install command; Shoal installs
nothing. Agents drive the device with serve-sim's or AXe's own commands, which
Shoal does not wrap. A terminal demo's stream is an attach to another
machine's session pointed at the demo's place: the owning machine runs a worker
on a terminal it makes at the viewer's size, which shows the place through the
host callback that shows a session there, and carries the same frames, its
screen one way and keys and resizes the other, so the host's own detach key
detaches and a device renders the bytes in its own terminal emulator. An agent on
another machine building and running on this one stays out: it needs leases
across machines.
The journal records a demo's start at its command's first run, when a start
reports it, and its stop when its record goes with the execution, so clients
showing demos follow them without polling; restarts between runs are not
changes. The listing carries each demo's kind and a web demo's URL, which
the daemon derives as opening does, so clients neither infer the kind from the
preset nor rebuild the URL.
