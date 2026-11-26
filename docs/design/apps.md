# Apps

This file covers `shoal-apps`: how apps built on Shoal connect to it and keep
their data in it. The [apps reference](../reference/apps.md) says what an app
can request.

## Callers

An app is the user unless it gives a scope token. With one, it is the agent of
the token's workspace, under the scope checks and approvals of that agent's
`shoal` commands. Only the `shoal` command reads a scope token from its
environment. An app passes its token itself, so it acts as an agent only when
it chooses to.

## Settings

The user edits an app's settings as its `[apps.<name>]` table, in any config
layer. Shoal checks only that the table is TOML. It layers the tables as it
layers its own options and hands the result to the app. An app thus gains a
setting without a change to Shoal. The
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#apps)
says how the layers merge.

## The app store

Apps keep their data in Shoal without Shoal knowing them. Every daemon keeps an
app store: entries per app, each holding bytes with a content type under a
path-like key. An entry is shared across the user's machines and devices, or
one device's own. The daemon keeps the entries in its state directory, reached
only through the protocol, and never reads their bytes. The
[`[machine.app_store]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#app-store)
limits cap each entry and each app's entries.

A request fits in one frame, but an entry need not. An entry's bytes therefore
follow its write as frames of their own.

### Revisions

Every change takes its revision from one counter. A write or deletion names the
revision it replaces. The daemon refuses it once another change took the entry
past that revision. Devices thus never overwrite each other unseen.

### Watches

A watch follows the entries under a key prefix. It compares them again after
each change of the entries its daemon keeps.

### Entries and the hub

Without a [hub](hub.md), the local daemon keeps every entry, so a single
machine needs no hub. Turning the hub on keeps the entries, since the hub is
the same daemon.

A machine that joins a hub keeps its own entries. It sends requests of shared
entries to the hub over its link, on a stream of the link when the answer goes
on, as an entry's bytes or a watch do. The shared entries it held before it
joined stay in its state directory, out of reach while it is a member. The hub
keeps each device's own entries under the device's member ID.
