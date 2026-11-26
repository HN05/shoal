# Space log

This file says what `shoal log` shows and how integrations read the events
behind it.

Shoal records the events of every space in a journal. A space's log shows the
important ones, and the [event stream](#space-events) gives integrations all
of them.

## Read a log

```sh
shoal log                 # This space, or Shoal's log outside one
shoal log 3               # A space by number, or a removed one by ID
shoal log --all           # Every event Shoal recorded for the space
shoal log --limit 20      # The newest 20 entries (default 50)
shoal log --follow        # Keep printing new entries
shoal log --json          # One JSON line per entry
```

`shoal log [space]` prints a workspace's or chat's entries, oldest first. Each
line gives the entry's local time, a label and its details. A heading names
each date.

`--json` prints each entry as one line, in the shape the
[event stream](#lines) gives it.

## Entries

A space's log shows these events of the space:

- its creation, with its base and parent, and its renames;
- the start of its removal, the removal with its cause, a removal that failed
  or stopped with the reason, and a workspace's restore;
- its agent's starts, stops and exits. A start names its launch's source, such
  as `manual` for `shoal start`'s. An exit gives the exit code;
- its ready marks, its completion, and the withdrawal of each;
- the issues, PRs and CI runs it links and unlinks, and the changes Shoal makes
  to them;
- the closing, merging and reopening of those items on the forge;
- the threads numbered in it, with their posts, forwards and resolutions;
- the approvals it requested and their decisions;
- the alerts it sent, each with its number;
- the blockers added to it, removed or cleared;
- the spaces created and removed below it, and its changes of parent;
- an absorb: into which space and by what merge, or what the space took over;
- a [CI job](ci.md#how-a-job-ends) Shoal ended, with the processes it stopped;
- a merge it watched for which no CI run of the default branch started.

## Every event

`--all` adds every other event of the space, dimmed. These include setup
results, branch and base changes, its agent's turn states and progress
changes, other forge activity such as comments and checks, demos, simulator
leases and attached files.

## Retention

A space keeps its events while it exists. A removed space keeps them for
[`cleanup.restore.retention_days`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
after its removal, and `shoal log` selects it by its ID, as `shoal files`
does. Its removal record, or attached files that have not expired, keep its
events longer. A restored workspace's log continues the removed one's.

A space keeps only its newest 500 agent turn states, progress changes and
forge activity events. This cap leaves out an item's closing, merging and
reopening. The [daemon design](../design/daemon.md#journal-retention) says why each space keeps
its own events.

## Scope

A scoped caller, such as an agent Shoal launched, reads the logs of its own
space and the spaces below it, removed ones included. It reads Shoal's log as
its space's [`notifications` permission](modes.md#permitted-actions) allows.

## Shoal's log

Outside a space, `shoal log` shows Shoal's log. It holds the entries of every
space, those of removed spaces while their log lasts, and Shoal's own events.
Each line names its space after the time, and Shoal's own events name `Shoal`.
`--all`, `--limit`, `--follow` and `--json` work as they do for a space.

Shoal's own events are:

- the daemon's starts, stops and restarts;
- each config reload, with the error when the daemon refused the file;
- each automatic cleanup pass that removed spaces, naming them, or that failed
  in a new way.

Shoal's own events stay for the global `cleanup.restore.retention_days`. The
daemon's diagnostic log is not part of Shoal's log.

## Space events

```sh
shoal internal events --json                       # Replay the journal and exit
shoal internal events --follow --json              # Also stream new events
shoal internal events --follow --json --since 123  # Replay after an event ID
```

`shoal internal events` gives an integration the journal's events. Without
`--since`, it replays every event the journal keeps. Without `--follow`, it
exits after that backlog. Reading events never marks alerts read. Scoped
callers cannot read events.

The journal keeps events as [space logs](#retention) do.

### Lines

Each line has a `type`:

- `event` records an event of a space.
- `file` records a file [attached](files.md) to a space, `kind: "file_added"`,
  or deleted from it, `kind: "file_removed"`, by any route. Attaching a file
  that is already attached records nothing.
- `shoal` records an event of Shoal itself, which belongs to no space.
- `gap` reports events that are [gone](#gaps).

Every line but a gap has an increasing `id` and a Unix-seconds `created_at`.

A space event names its space by `space_id` and `name`. Its `space_kind` is
`workspace` or `chat`, as the space was then. A workspace's events add its
`path` and `branch`. `kind` names the event. `cause` says why Shoal made the
change, such as `merged` for [merged cleanup](removal.md#merged-workspaces),
and `error` says what failed. Each is null where it does not apply. An object
named for the event's subject, such as `thread`, `approval` or `job`, gives
its details.

The [`events`](../../crates/shoal-protocol/src/events.rs) module of
`shoal-protocol` defines each line, event kind and field. A Rust program reads
them with its types, as the [`shoal-client`](apps.md) library does.

Shoal records a `branch_changed` event when a sweep finds a workspace on
another branch. A detached HEAD gives the event a null `branch`, and the
workspace's recorded branch stays as it was.

### Gaps

A `gap` line gives `since`, `oldest_id` and `latest_id`. Shoal sends one when
the cursor falls outside the events the journal keeps, or when retention
deleted events after it. The replay then continues from `oldest_id`. A running
stream reports deletions after its cursor the same way.

The journal records changes, not state. Read `shoal list --json` on first
connection and after a gap.
