# Configuration

This file covers how Shoal reads, layers, writes and shares its config files.

The generic repository `[env]` table is available to tracked executions and
hooks: it renders workspace identity and reserved ports into user-selected
variables without managing the integrated tool.

Global TOML is `[machine]`, the settings that describe this machine, plus the
repository schema for this machine's defaults, so nothing is declared twice and
a setting copies between the files. Each part is deserialized strictly on its
own from one parse: `serde(flatten)` would accept unknown keys and lose error
positions. It is seeded by `install` with the stated defaults when absent;
explicit key edits preserve unrelated settings and comments and validate once
per command before saving, so dependent keys change together, while reset or
named template installation replaces the file. Global writes keep the previous
file as a backup; the daemon reads it at startup and on reload, which global CLI
writes request of a running daemon, replacing its snapshot only with a valid
file so agents and leases are untouched; an operation that combines global
definitions with resolved settings reads both from one snapshot. The CLI reads
agent settings per command. Repository TOML comes from the worktree
(`.shoal.toml` or `.shoal/config.toml`, both together is an error) with a local
config layered over it per option, a named table replacing the one below it
whole. The local config is a file, `.shoal/config.toml` in the repository's own
directory beside its workspaces and `.checkout`: outside every Git tree, so
nothing has to ignore it, and read in place on every request, so an edit reaches
every workspace without copying or a restart. As a file, the user can edit it
with any tool, back it up and keep its scripts beside it: its relative hook and
setup paths name scripts there, made absolute when it loads. Key edits run in
the daemon, serialized with moves and removal, hold the file's lock, keep a
backup and save only a valid result, as global edits do; unset restores
lower-layer values. It moves with the repository's directory and is deleted with
the registration. Every option that does not describe the machine may be set at
either level and resolves per option: local config, worktree file, global
config, shared config, then the built-in default. The shared config,
`shared.toml` beside the global file with its mode and rule files in `shared/`,
holds what the user's machines share, so a machine's own file overrides any of
its keys. It takes every global key but the `[machine.hub]` settings a machine
reaches the hub with before it can have the file, and must be valid alone, since
a machine whose own file sets nothing reads it alone. `[machine]` has no
provenance to report, so its tables merge key by key across the two files. Its
paths and commands resolve on each machine as the global file's would; nothing
checks them for portability, and what cannot be the same everywhere belongs in a
machine's own file. Scoped processes never write it. A hub's machines, its own
included, share it and its mode and rule files: each machine looks at them every
ten seconds and on each reload and sends the hub each file that differs from the
copy it last agreed on with the hub, sent or received, once the files load, so a
write from the hub is never sent back. The hub keeps whichever edit reached it
last, with no revision check or conflict state, numbering its copies so a late
copy never undoes a newer one, and sends it to every other machine, which writes
it through `config_file` and reloads. On each link a machine sends its edits of
files it agreed on before, which an offline edit is, then takes the hub's
copies, so a file it never agreed on, as on joining, gives way to the hub's. A
copy that replaces an edit the hub never got leaves it as `<file>.backup` with
one alert, so a local edit is either on the hub or in that backup. The same
sync, over items of any kind under keys of their own, shares repositories too: a
registration is shared under its origin's URL, which the store records, and each
machine's items are its shared registrations by origin, holding the URL and name
the copy it agreed on has, so another name or URL of its own is no edit. A
member takes a shared copy by sharing its registration of that origin, whatever
its name, or by registering a URL clone under its own `root_dir` in the
background, with no registered-but-uncloned state and no setting for it,
agreeing on the copy once the clone is registered, so a failed clone, which
alerts naming `repo add <url> --shared`, is tried again when the link next
opens, or on the hub's own machine when its daemon restarts, rather than every
round, which would alert every ten seconds. A copy without a value, from `repo
unshare` or a removal anywhere, leaves every registration, no longer shared.
Machines clone from the origin; the hub carries no Git data. Each machine's
report to the hub also carries every registration it has, with its ID there,
whenever they change, and the hub view serves the other machines', so `repo ls`
lists them and `repo rm` finds a repository registered only elsewhere, without
spaces. `repo rm` resolves where it acts from that view in the CLI, as a named
space resolves; each daemon answers a removal and its preview for its own
registrations and forwards one that names another machine there, which fails at
once while that machine is offline, since a removal the hub held would delete
work nobody confirmed then. A shared repository's local config syncs the same
way, keyed by its origin, under the shared files' rules: its text is checked as
key edits check it, file by file, so one invalid file holds back no other, and a
replaced edit stays as `config.toml.backup` with one alert. A machine agrees on
the copy of every origin but writes it only into its shared registrations of it,
so a registration that comes to be shared, by `repo share`, by taking a shared
copy or as a clone, takes the copy it agreed on as a joining machine takes the
shared files; without one, its file goes only after the machine has read the
hub's copies, since one may be on its way. An origin it shares no registration
of holds the copy it agreed on, so neither unsharing nor removal is an edit and
every machine's file stays as it is. Writes go through the registrations, under
the gate and lock key edits take, and only while the file still reads what the
sync compared. A worktree file the current format rejects counts as absent,
since a branch from before a format change keeps its old file and must not stop
cleanup or anything else the other layers settle. The layers name the rejection,
which `config show` warns about, and the daemon tells the log and the user once
per error while it runs. A file that cannot be read still fails the request.
Lease hooks are the exception, since a lease is kept until its release hook ran:
they fail to resolve while the file is rejected, and removal resolves the
release hook only for leases it releases. One declaration of each option, the
field of the repository schema with its kind, generates its built-in default,
merge, resolved form and provenance, so a reported layer is the one whose value
is in use; the daemon resolves a workspace's settings once per request and every
use site reads them rather than layering on its own.

Before a workspace exists, the registered checkout's file stands in for the
worktree's. Repository config is read per request, so changes need no restart
and leave existing leases alone. Repository config cannot expand machine policy:
the `[machine]` table holds `root_dir`, simulator limits and profiles, Git
profile definitions, and global resource definitions stay global; global pools
span repositories, repository pools span that repository's worktrees. The
repository `[env]` table layers by variable name and is rendered once when a
space's execution or hook starts. It accepts workspace, path, repository,
branch, reserved-port and cache fields; a chat leaves unset a variable naming a
field only a workspace has, such as `{workspace}`, or for a chat without a
repository `{repo}` or a cache, rather than keep the placeholder, which would
give every chat the same value. Values are literal strings after substitution,
and `SHOAL_*` names remain owned by Shoal.
