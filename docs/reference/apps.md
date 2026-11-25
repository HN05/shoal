# Apps

This file says how apps built on Shoal use it.

## The library

An app other than the CLI, such as Quay, uses Shoal's protocol through the
`shoal-client` library and the `shoal-protocol` types it carries. It takes both
from the git tag of a Shoal release, with no features:

```toml
shoal-client = { git = "https://git.henriknordvik.com/HN05/shoal", tag = "<release tag>" }
shoal-protocol = { git = "https://git.henriknordvik.com/HN05/shoal", tag = "<release tag>" }
```

The library speaks only its own release's protocol. A daemon of another
release refuses it with `protocol_mismatch`, naming both versions, so the app
updates with Shoal.

`shoal_client` holds every request the CLI makes. Each is a function of the
module for its subject, such as `threads::post` or `leases::acquire_port`, and
returns its reply's types. The library also holds the logic the CLI applies on
its side, such as what `shoal view` reads in `views`. Its `output` module holds
the types that commands only print with [`--json`](output.md).

`shoal_protocol` holds the requests and replies at the same module paths, with
the types they carry. It also holds the logic over them that needs no
connection, such as where a space's agent sessions keep their files on this
machine.

## Connect

`connection` sends each request to the daemon a `Connector` names. Without a
scope token, the caller is the user. With one, the caller is that workspace's
agent, under the rules of its `shoal` commands.

## Follow changes

`subscriptions` follows records as they change: first all of them, then each
change. A subscription names its topics, such as repositories, spaces with
their agents, the issues, PRs and CI runs spaces link, the sessions their
agents reported, open threads, pending approval requests and alerts. It
subscribes again after a daemon restart.

`operations` follows work the daemon runs for a request, such as the setup and
post-setup hook of a workspace that `StartWorkspace` creates. It reports the
work's steps and its processes' output lines as they come. Then it reports the
result, or a typed failure such as `setup_failed` or `hook_failed`.

## Follow an agent's session

`transcripts::follow_session` follows an agent's session, a named one or the
space's current one. It follows it on whichever machine of the hub the space
runs. It reports:

- the session, with the tool whose format its files are in;
- the bytes of each of the session's files, everything so far and then what
  the tool appends, each with its path in the tool's session directory, its
  offset and the file's length;
- the current session again whenever the space goes on in another, of any
  agent.

Shoal copies the bytes without reading them. The app parses each tool's format
itself, and sends input through the protocol's requests.

When the tool writes a file anew, the follow sends it again from offset 0,
marked as a reset. Shoal tells that a file was written anew only by its getting
shorter, never by its content.

A daemon restart does not end a follow, nor does a restart of the space's
machine or the hub, which the daemon reports as `machine_unreachable`. The
follow then follows again from where it read each file to. A follow under a
scope token ends once the token is revoked.

Following an agent's session and messaging it count as your
[interaction](sleep.md) with the agent. An agent you watch and talk to from an
app stays awake as one in your terminal does. A follow that follows again on
its own is no interaction.

## Read attached files

`files::read` reads the bytes of a space's [attached file](files.md), by the
space and the file's record ID. An alert's or a post's file record gives both,
the space as its `space`. The read runs on whichever machine of the hub keeps
the file's copy, and fails at once while that machine is offline. The caller
needs the scope that lists the file, or one that reads a thread the file was
posted in.

## Open demos

`demos::open_stream` opens a stream to a running [demo](demos.md) of a space,
by the space and the demo's name, on whichever machine of the hub runs it. It
returns the demo. Its connection then carries the demo's bytes both ways until
either side closes it:

- for a `server` or `web` demo, those of one TCP connection to the demo's port;
- for a `simulator` demo, those of one TCP connection to its
  [viewer](demos.md#simulator-viewer), which the first stream starts.

The app serves the stream from a local port, which its web view points at. The
listed demo's `viewer` names the `tool` the app speaks to, and gives its `port`
and `url` once it runs. On a phone, ask serve-sim for H.264 or a smaller frame:
full-resolution MJPEG is slow there.

`demos::open_terminal` opens a terminal demo's stream, an attach to the demo's
place on its machine. That machine shows the place on a terminal it makes, at
the size and `TERM` the app gives, and sends what the terminal shows. The app
sends keys and resizes, and renders the bytes in its own terminal emulator.

Shoal refuses a stream to a simulator demo whose viewer is off, and a scoped
caller's stream to a demo outside its scope. A scoped caller's stream ends once
its token is revoked.

## Settings

`apps::config` reads the app's own settings: its
[`[apps.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#apps)
table as every layer resolves it. It resolves it for a space, a repository or
the global config alone. A scoped caller
reads the config of its own repository and of the spaces its scope reaches.

## App store

An app keeps what it writes itself, such as its layouts or exports, in the app
store. `apps::write`, `read`, `list` and `delete` act on its entries. Each
entry sits under a key of the app's own, such as `layout/main`, and holds bytes
with a content type. An app stores its settings as `application/json`.

An entry is either shared across your machines and devices, or one device's
own. Shoal keeps the entries in its state directory, reached only through the
protocol, and never reads their bytes. The
[`[machine.app_store]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#app-store)
limits cap each entry and each app's entries in all.

### Revisions

Each change gives an entry a new revision. A write or deletion names the
revision it replaces, or none for a new entry. Once another change took the
entry past that revision, the daemon refuses the request with
`stale_revision`, which `apps::stale` tells. So two devices never overwrite
each other unseen.

### Watches

`apps::Watch` follows the entries under a key prefix: first all of them, then
each one written or deleted. It reports all of them again after a daemon
restart. A watch of shared entries also starts again with all of them when its
link to the hub breaks off.

### Entries and the hub

Without a [hub](hub.md), the daemon keeps every entry itself. Once it becomes
the hub, the same entries are the hub's.

A machine that joined a hub keeps its own entries, and reaches the shared ones
on the hub through its daemon. That daemon sends the hub the requests and
writes of shared entries and relays its answers, an entry's bytes on a byte
stream of its link. The shared entries a machine held before it joined stay in
its state directory, out of reach while it is a member. A device keeps its own
entries on the hub.
