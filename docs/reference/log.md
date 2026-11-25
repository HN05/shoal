# Space log

This file says what `shoal log` shows and which events a space records.

```sh
shoal log                 # This space, or Shoal's log outside one
shoal log 3               # A space by number, or a removed one by ID
shoal log --all           # Every event Shoal recorded for the space
shoal log --limit 20      # The newest 20 entries (default 50)
shoal log --follow        # Keep printing new entries
shoal log --json          # One JSON line per entry
```

`shoal log [space]` shows the important events in a workspace's or
chat's life, oldest first, each with its local time, a label and its
details, under a heading for each date:

- the space's creation, with its base and parent, its renames, the start
  of its removal and the removal with the cause, a removal that failed or
  stopped before it finished with the reason, and a workspace's restore;
- its agent's starts, with what made the launch a start carries out,
  `manual` for `shoal start`'s, its stops and exits, with the exit code;
- its ready marks and their withdrawal, and `done` and its withdrawal;
- the issues, PRs and CI runs it links and unlinks, the changes Shoal made to
  them, and their closing, merging and reopening on the forge;
- the threads numbered in it, their posts, forwards and resolutions, the approvals it requested and
  their decisions, and the alerts it sent, each with its number;
- the spaces created and removed below it, its parent changes, and an absorb,
  into which space and by what merge, or what it took over;
- a CI job Shoal ended after its run finished, with the processes it stopped.

`--all` adds every other event of the space, dimmed: setup results, branch
and base changes, its agent's turn states and progress changes, other
forge activity such as comments and checks, demos, simulator leases and
attached files. With `--json` each line is the entry as
[`shoal internal events`](#space-events) prints it.

A removed space's log stays for `cleanup.restore.retention_days` after its
removal and is selected by the space's ID, as `shoal files` selects it; a
restored workspace's log goes on from the removed one's. A space keeps its
newest 500 agent turn states, progress changes and forge activity other than
an item's closing, merging and reopening, so a busy space cannot push out another's
log. A workspace process reads the logs of its own space and the spaces
below it, removed ones included.

Outside a space, `shoal log` shows Shoal's log: the entries of every space,
removed ones while their log lasts, each line naming its space after the
time, with Shoal's own events, named `Shoal`: the daemon's starts, stops and
restarts, each config reload, with the error when the file was refused, and
each automatic cleanup pass that removed spaces, naming them, or failed in a
new way. `--all`, `--limit`, `--follow` and `--json` work there too. Shoal's
own events stay for the global `cleanup.restore.retention_days`. The daemon's
diagnostic log is not part of it. Workspace processes read Shoal's log as
their space's [`notifications` permission](modes.md#permitted-actions) allows.

## Space events

```sh
shoal internal events --follow --json             # stream lifecycle changes
shoal internal events --follow --json --since 123  # replay after an event ID
```

The daemon keeps a space's events while the space exists and, once it is
removed, for `cleanup.restore.retention_days` or while its removal record
lasts, independently of alerts; of its agent's turn states, its
progress changes and its forge activity other than an item's closing, merging and reopening, it keeps
the newest 500 per space. Reading events never marks alerts read. Without `--since`, events replay
all retained history; without `--follow`, the command exits after that backlog.
Scoped callers cannot read events.

Each JSON line has `type: "event"`, an increasing `id`, Unix-seconds `created_at`,
the UUID of the space it records as `space_id`, `space_kind` (`workspace` or
`chat`, as the space was then), `repository_id` (null for a chat without a
repository), `name`, a workspace's `path` and `branch` (null for a detached
HEAD), which a chat's events leave out, `kind`, `cause`, and `error`. A
`converted` event names the kind the space became, with that kind's fields,
or with `handoff` the agent tool that took the space's work over: `from`
and `to` each give an `agent`, the `session` it reported and its
`integration`, journaled once the new tool reports its session. Kinds are `created`, `ready`, `setup_failed`,
`completed`, `undone`, `removed`, `retained`, `branch_changed`, `review_ready`,
`review_cleared`, `linked`, `unlinked`, `base_changed`, `agent_state`, `item_changed`, `item_activity`, `parent_changed`, `demo_started`, `demo_stopped`, `simulator_acquired`, `simulator_released`, `renamed`, `restored`, `agent_started`, `agent_stopped`, `agent_exited`, `thread_opened`, `thread_posted`, `thread_forwarded`, `thread_resolved`, `approval_requested`, `approval_decided`, `gate_decided`, `alert_sent`, `progress_changed`, `child_created`, `child_removed` and `job_ended`. Review events add a `review` object with the mark's `kind` and
`url`, both null for a workspace mark. Link events add a `link`
object with the linked issue or PR's `kind` and canonical `url`. Agent state
events add `agent_state` with the reported `state`, null when its execution ended.
`item_changed` events add an `item` object with the `kind`, `url` and `action`
of a change Shoal made to an issue or PR. `item_activity` events add an
`item_activity` object with the item's `kind` and `url`, and an `update` object
with the observed update's `kind` and `message`. `created` and `base_changed`
events add `base_workspace` with the base's `id`, `name` and `branch`, or null,
and `created` events add `base_ref`, the ref the workspace started from, both
null for a chat.
`created` and `parent_changed` events add `parent`, the space above the
workspace with its `id`, `name`, `kind` and a workspace's `branch`, or null once
a [chat](chats.md) that opened it returned it to you.
`renamed` events record a space's new `name` and a workspace's `branch`, as
`shoal rename` or a branch rename gives them. `restored` events record a workspace
`shoal restore` reopened, which takes the removed workspace's events
under its own ID. `agent_started`, `agent_stopped` (Shoal or a stop request
stopped it) and `agent_exited` (it exited on its own) events add `agent`
with its `name`, `execution_id` and `exit_code`, null for a start or stop
and when the wrapper reported none, and for a stop or exit `oom_kills`, how
many of its processes the kernel's OOM killer killed, when any; a start that
carries out a launch adds its `source`, `{"kind": "manual"}` for `shoal
start`'s. Thread events add `thread` with its `number` and `name`, a post's
`sender` and `text`, a forward's `to`, and a resolution's `outcome`, none
when the thread was reopened.
`approval_requested` and `approval_decided` events add `approval` with its
`number`, `target`, `subject` and `status`. `gate_decided` events add `gate`
with its `number`, `subject`, `verdict` (`allow`, `deny`, `escalate` or
`failed`), `reason`, and the `rule` an escalation quoted, the `agent` and its
`session`. `alert_sent` events add `alert`
with its `message`. `progress_changed` events add `progress` as
`shoal progress --json` shows it, with empty `steps` and a null `status` once
both are cleared. `child_created` and `child_removed` events, in the log of
the space above, add `child` with its `id`, `name`, `kind` and a workspace's
`branch`.
`job_ended` events, when Shoal ended a [CI job](ci.md#runner), stopped what one's
steps left running, or the kernel's OOM killer killed processes of one, add
`job` with the job's `name`, its `run`'s URL, the `reason`, and the processes
Shoal `stopped`, each with its `pid` and `command`, the first 20
of them, and `omitted`, how many more, when there were more.
`demo_started` events, when a [demo](demos.md)'s command first runs, and
`demo_stopped` events, when a demo that ran ends, add `demo` with its `name`,
`preset` and, for a simulator demo, the device's `udid`.
`simulator_acquired` events, once a [simulator](resources.md#simulators-macos) lease's device
has booted, and `simulator_released` events, when that lease is released or
its device deleted, add `simulator` with the lease's `name` and the device's
`udid`, `device` and `runtime`.
Causes are `manual`, `idle`, `issue`, `pr` (including a base workspace's merged
PRs), `completion`, `missing_directory`, `disk_space`, `merged` for
[merged cleanup](removal.md#merged-workspaces), `removed` for a base workspace's removal,
`parent` for the removal of the space above, `repository` for its
repository's removal, `parent_completion` for an
ancestor's `done` with cleanup, `start_failed` for a chat that could not
start, or `blocker` for a new workspace that builds on the done work its agent
waited for, and null when inapplicable; `error` describes
setup or cleanup failures. Branch changes are observed during daemon sweeps;
detached HEAD has a null branch, and the recorded workspace branch remains
unchanged.

A `type: "file"` line records a file [attached](files.md) to a workspace
or chat by any route (`kind: "file_added"`) or deleted by any route, including
`shoal files rm`, workspace removal, the retention sweep and disk space cleanup
(`kind: "file_removed"`): its `id`, `created_at`,
`repository_id` (null for a chat without a repository),
and `file`, the record as
`shoal files --json` lists it, with its own `id`. Attaching a file that is
already attached adds nothing.

A `type: "shoal"` line records an event of Shoal itself, which belongs to no
space: its `id`, `created_at` and `kind`: `daemon_started` with the daemon's
`version`; `daemon_stopped`, with `error` when the daemon failed;
`daemon_restarted` when it stopped for the next daemon to take over running
work, as `shoal daemon restart` and an update do; `config_reloaded`, with
`error` when the daemon kept its settings; and `cleanup_ran` with `removed`,
the names of the spaces automatic cleanup removed since the last such line,
or `error` when a pass failed in a new way.

A `type: "gap"` line gives `since`, `oldest_id`, and `latest_id` when a cursor
falls outside retained history or retention deleted events after it, then
replay continues with the retained events from `oldest_id`; a stream reports
deletions after its cursor while it runs the same way.
Resync with `shoal list --json` after a gap or on first connection: the journal
starts when the daemon upgrades and does not reconstruct older changes.
