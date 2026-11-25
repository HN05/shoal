# Apps

This file covers `shoal-apps`: how apps built on Shoal connect and keep their
data in it.

An app is the user unless it gives a scope token, which only the `shoal` command
reads from its environment; with one, it is that workspace's agent, under the
scope checks and approvals of its `shoal` commands.

Apps keep their data in Shoal without Shoal knowing them: every daemon keeps an
app store of entries per app, bytes with a content type under path-like keys,
shared across the user's machines and devices or one device's own, in its state
directory and reached only through the protocol; Shoal never reads them. Each
change takes a revision from one counter, and a write or deletion names the
revision it replaces and is refused once another change took the entry past it,
so devices never silently overwrite each other; a watch of a key prefix compares
again after each change. Without a hub the local daemon keeps everything, so one
PC needs none, and turning the hub on keeps its entries since it is the same
daemon; with one, a joined machine keeps its own entries and sends requests of
shared ones to the hub, over a stream of its link for an answer that goes on, an
entry's bytes or a watch, while the shared entries it held before it joined stay
local. The hub keeps each device's own entries by its member ID. Limits per
entry and per app come from `[machine.app_store]`; a request fits one frame, so
an entry's bytes follow its write as frames of their own. An app's settings the
user edits are its `[apps.<name>]` table in any config layer, which Shoal checks
only as TOML, layers as it layers its own options and hands to the app.
