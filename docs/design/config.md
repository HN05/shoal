# Configuration

This file covers how Shoal reads, layers, writes and shares its config files.
The [configuration reference][reference] lists the files in their order and
describes every option.

[reference]: ../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers

## One schema for every file

Every config file follows the repository schema. The global and shared files
add `[machine]`, the settings that describe this machine. Nothing is thus
declared twice, and a setting copies from one file to another. Every option
outside `[machine]` may be set in any file.

Each option resolves on its own, from the highest layer that sets it. One
declaration of each option, the schema field with its kind, generates its
built-in default, its merge, its resolved form and its provenance. A reported
layer is therefore the one whose value is in use.

The daemon resolves a workspace's settings once per request. Every use site
reads those settings and never layers files on its own. Before a workspace
exists, the registered checkout's file stands in for the worktree's.

Shoal refuses a setting it no longer reads, naming what replaces it, instead of
loading the file with a warning. No setting thus keeps two names.

Shoal parses the global file once. It deserializes `[machine]` and the
repository schema from that parse strictly, each on its own. `serde(flatten)`
would accept unknown keys and lose their error positions.

## Machine policy

Repository config cannot widen machine policy. Only the global and shared
files set `[machine]`. `[machine]` has no provenance to report, so its tables
merge key by key across those two files.

Global resource definitions stay global. A global pool spans repositories, and
a repository's pool spans that repository's worktrees.

## Global config

`shoal daemon install` writes the global file from its stated defaults when the
file is absent. A key edit keeps unrelated settings and comments. It validates
the result once per command, so keys that depend on each other change
together. Every write keeps the previous file as a backup.

The daemon reads the global files at startup and on reload. A CLI write asks a
running daemon to reload. The daemon replaces its snapshot only with a valid
file, so running agents and leases stay as they are. An operation that combines
global definitions with resolved settings reads both from one snapshot.

The CLI reads the global files when it starts and agent settings on each
command. It asks the daemon for a target's repository layers.

## Repository config

Shoal reads repository config on each request. A change therefore needs no
restart and leaves existing leases alone.

### Local config

The local config is a file in the repository's own directory, beside its
workspaces and `.checkout`:

- It lies outside every Git tree, so nothing has to ignore it.
- Shoal reads it in place on every request, so an edit reaches every workspace
  without a copy or a restart.
- The user edits it with any tool, backs it up and keeps its scripts beside
  it. A relative hook or setup path names a script there, and Shoal makes it
  absolute when the file loads.

Key edits of the local config run in the daemon, one at a time with the
repository's moves and removals. They take the file's lock, keep a backup and
save only a valid result, as global edits do. The file moves with the repository's
directory and goes when the repository's registration does.

### A rejected worktree file

A worktree file the current format rejects counts as absent. A branch from
before a format change keeps its old file, which must not stop cleanup or
anything else the other layers settle. The layers name the rejection:
`shoal config show` warns about it, and the daemon logs it and tells the user
once per error while it runs. A file Shoal cannot read still fails the request.

Lease hooks are the exception, since Shoal keeps a lease until its release hook
has run. They fail to resolve while the file is rejected. A removal resolves
the release hook only for the leases it releases.

## Shared config

The shared config holds what the user's machines share. It sits below the
global file, so a machine's own file overrides any of its keys.

- It takes every global key except the `[machine.hub]` settings a machine
  reaches the hub with, since the machine needs them before it can have the
  file.
- It must be valid alone, since a machine whose own file sets nothing reads it
  alone.
- Its paths and commands resolve on each machine as the global file's would.
  Nothing checks them for portability: what cannot be the same everywhere
  belongs in a machine's own file.
- Scoped processes never write it.

## Sharing across a hub

The machines of a hub, the hub's own included, share items of several kinds,
each under keys of its own: the shared files, repositories and the local
configs of shared repositories. The
[hub reference](../reference/hub.md#shared-config) says when an edit goes and
what the user sees.

### Edits and copies

Each machine records, for every item, the copy it last agreed on with the hub,
sent or received. It sends the hub each item that differs from that copy once
the item loads. A copy the hub wrote therefore never goes back.

The hub keeps whichever edit reached it last, with no revision check and no
conflict state. It numbers its copies, so a late copy never undoes a newer one.
It sends each new copy to every other machine, which writes it through
`config_file` and reloads.

When its link opens, a machine first sends its edits of items it agreed on
before, such as an offline edit. It then takes the hub's copies. An item it
never agreed on, as on joining, gives way to the hub's copy. A copy that
replaces an edit the hub never got leaves that edit as a `.backup` file, with
one alert. A local edit is thus either on the hub or in that backup.

### Shared repositories

A repository is shared under its origin's URL, which the store records. A
machine's items are its shared registrations, by origin. Each holds the URL and
name of the copy it agreed on, so a name or URL of the machine's own is no
edit.

A machine takes a shared copy in one of two ways:

- It shares its registration of that origin, whatever its name.
- It registers a clone of the URL under its own `root_dir`, in the background.

Shoal has no registered-but-uncloned state and no setting for it. The machine
agrees on the copy once the clone is registered. A failed clone alerts, naming
`shoal repo add <url> --shared`. It is tried again when the link next opens, or
on the hub's own machine when its daemon restarts. Trying every round would
alert every time the machine looks.

A copy without a value, from `shoal repo unshare` or a removal on any machine,
leaves every registration in place, no longer shared. Machines clone from the
origin, and the hub carries no Git data.

### Repositories of other machines

Each machine reports every registration it has to the hub, with its ID there,
whenever they change. The hub's view serves the other machines'
registrations: `shoal repo ls` lists them, and `shoal repo rm` finds a
repository registered only elsewhere.

The CLI resolves where `shoal repo rm` acts from that view, as it resolves a
named space. Each daemon answers a removal and its preview for its own
registrations. It forwards one that names another machine, which fails at once
while that machine is offline: a removal the hub held would later delete work
that nobody had confirmed by then.

### Local config of a shared repository

A shared repository's local config is shared by its origin, under the shared
files' rules. Shoal checks each file's text as key edits check it, one file at
a time, so one invalid file holds back no other.

A machine agrees on the copy of every origin, but writes it only into its
shared registrations of that origin:

- A registration that comes to be shared, by `shoal repo share`, by taking a
  shared copy or as a clone, takes the copy the machine agreed on. It does so
  as a joining machine takes the shared files.
- Without an agreed copy, the registration's file goes to the hub only after
  the machine has read the hub's copies, since one may be on its way.
- For an origin it shares no registration of, the machine keeps the copy it
  agreed on. Neither unsharing nor a removal is then an edit, and every
  machine's file stays as it is.

Writes go through the registrations, under the gate and lock that key edits
take. A write happens only while the file still reads what the sync compared.

## Environment

The repository [`[env]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#environment)
table renders a space's identity and reserved ports into variables the user
names. Shoal does not manage the tool that reads them.

`[env]` layers by variable name. Shoal renders it once, when an execution or
hook starts. A chat leaves unset a variable that names a field the chat lacks,
such as `{workspace}`, instead of keeping the placeholder, which would give
every chat the same value.
