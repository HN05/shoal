# Apps

This file says how apps built on Shoal use it.

Apps other than the CLI, such as Quay, use Shoal's protocol through the
`shoal-client` library and the `shoal-protocol` types it carries, both from
a git tag of a Shoal release, with no features:

```toml
shoal-client = { git = "https://git.henriknordvik.com/HN05/shoal", tag = "<release tag>" }
shoal-protocol = { git = "https://git.henriknordvik.com/HN05/shoal", tag = "<release tag>" }
```

`shoal_client` holds every request the CLI makes, each a function of the
module for its subject, such as `threads::post` or `leases::acquire_port`,
returning its reply's types; `connection` sends them to the daemon a
`Connector` names, and `subscriptions`
follows repositories, spaces with their agents, linked issues, PRs and CI
runs, the sessions each space's agents reported, open threads with a post, pending
approval requests and alerts as they change, subscribing again
after a daemon restart. `operations` follows work
the daemon runs for a request, such as the setup and post-setup hook of a
workspace `StartWorkspace` creates: its steps and its processes' output lines as
they come, then its result or a typed failure such as `setup_failed` or
`hook_failed`. It also holds the logic
the CLI applies on its side, such as in `views` what `shoal view` reads;
`output` holds the types only commands print with `--json`. `shoal_protocol`
holds, at the same module paths, the requests and replies with the types they
carry, and the logic over them that needs no connection, such as where a
space's agent sessions keep their files on this machine.
`transcripts::follow_session` follows an agent's session, a named one or the
space's current one, on whichever machine of the hub the space runs: the
session with the tool whose format its files are in, then the bytes of each of
its files, everything so far and then what the tool appends, each with its
path in the tool's session directory, its offset and the file's length, and
a file the tool wrote anew, shorter, again from offset 0 marked as a reset
(Shoal tells a file written anew only by its getting shorter, never by its
content),
and the current session
again whenever the space goes on in another, of whichever agent. Across a
daemon restart, or while the space's machine or the hub restarts, which
the daemon reports as `machine_unreachable`, the follow follows again from
where it read each file to; one under a scope token ends once the token is
revoked.
Shoal copies the bytes unread, so
the app parses each tool's format itself and sends input through the requests
the protocol has.
`files::read` reads the bytes of a space's [attached file](files.md) by
the space and the file's record ID, which an alert's or a post's file record
gives with its `space`, on whichever machine of the hub keeps its copy, under
the scope that lists it, or that reads a thread it was posted in; it fails at
once while that machine is offline.
`demos::open_stream` opens a stream to a running [demo](demos.md) of a space on
whichever machine of the hub it runs, by the space and the demo's name: it
returns the demo, then its connection carries the demo's bytes both ways until
either side closes it, those of one TCP connection to a `server` or `web`
demo's port, or a `simulator` demo's viewer's, which the first stream starts,
and which an app serves from a local port its web view points at. The listed
demo's `viewer` names the `tool` a client speaks to, serve-sim by default, and
its `port` and `url` once it runs; a phone asks serve-sim for H.264 or a smaller frame,
since full-resolution MJPEG is slow there. `demos::open_terminal` opens a
terminal demo's stream, which carries the frames of an attach to another
machine's session: that machine shows the demo's place on a terminal it makes
at the size and `TERM` the app gives, as its host shows a place to another
machine, and sends what it shows, while the app sends keys and resizes and
renders the bytes in its own terminal emulator. A simulator demo whose viewer
is off, or a demo outside a scoped caller's scope, is refused, and a scoped
caller's stream ends once its token is revoked.
Following an agent, though not following it again on
its own, and messaging it are your [interaction](sleep.md) with it, so an agent you watch and talk to from an app
stays awake as one in your terminal does.
`apps::config` reads the app's own settings, its
[`[apps.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#apps)
table, as every layer resolves it for a space, a repository or the global
config alone; Shoal never reads it.
What an app writes itself, such as its layouts or exports, goes in the app
store, under keys of its own such as `layout/main`: `apps::write`, `read`,
`list` and `delete` keep each entry as bytes with a content type, its
settings as `application/json`, either shared across your machines and
devices or one device's own. Shoal never reads an entry's bytes, and keeps
them in its state directory, reached only through the protocol. Each change
gives an entry a new revision; a write or deletion names the revision it
replaces, none for a new entry, and is refused with `stale_revision`, which
`apps::stale` tells, once another change took the entry past it, so two
devices never overwrite each other unseen. `apps::Watch` follows the entries
under a key prefix: all of them, then each one written or deleted, and all
of them again after a daemon restart. Without a [hub](hub.md), the daemon
keeps every entry itself. Once it is the hub, the same entries are the
hub's; a machine that joined one keeps its own entries and reaches the
shared ones on the hub, through its daemon, which sends the hub the
requests and writes of shared entries and relays its answers, an entry's
bytes on a byte stream of its link. The shared entries a machine held
before it joined stay in its state directory, out of reach while it is a
member. A device keeps its own entries on the hub. A watch of shared
entries whose link to the hub breaks off starts again with all of them.
An entry and each app's entries in all are held to the
[`[machine.app_store]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#app-store)
limits. Without a scope token the caller is the user; with one, it is
that workspace's agent, under the rules of its `shoal` commands, reading the
config of its own repository and of the spaces its scope reaches. The library
speaks only its own release's protocol: a daemon of another release refuses
it with `protocol_mismatch` naming both versions, and the app updates with
Shoal.
