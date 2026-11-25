# Triggers

This file says what the `trigger` commands do and how triggers start agents.

## Commands

```sh
shoal trigger ls                 # This repository's triggers and the global config's
shoal trigger ls --repo docs     # Another registered repository's
shoal trigger pause triage       # It fires and launches its queued run no more, through config edits and restarts
shoal trigger resume triage      # It counts its times from now
shoal trigger run triage         # Fire it now and print the run's outcome
shoal trigger run labelled --payload issue.json --header X-Forgejo-Event=issues
```

`shoal trigger ls` lists the global config's triggers, then those of the
repository the command acts in, or of the one `--repo` names. Each row shows
the trigger's source, whether it is paused, its next firing and its last run.
`--json` prints the same.

`pause`, `resume` and `run` name one trigger. When the repository and the
global config both define the name, they act on the repository's. A paused
trigger does not fire and does not launch its queued run. A resumed trigger
counts its times from the resume, so it never catches up on the times it
missed. `run` fires the trigger at once and prints the run's outcome.

Every space may list triggers. A space's `pause`, `resume` and `run` follow
its `triggers` [permission](modes.md#permitted-actions).

## Runs

A trigger,
[`[triggers.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#triggers),
pairs a source with a launch. A source fires the trigger with an input
document. The trigger's filter, templates and key read that document the same
way for every source. `shoal start` is the manual source of the same launch.

A firing that the filter drops does nothing and leaves no trace.
`shoal trigger run` names the condition that did not hold. Every other firing
is a run.

A run renders its launch's templates. Each value other than `task` must read
as its flag's value would. The launch then goes through the operation that
`shoal start` uses, as you, so the [agent queue](queue.md) and
`max_heavy_agents` apply. It starts a top-level space with its mode's
permissions and the usual cleanup. A launch with `in` instead gives the task to
that workspace's agent. [`shoal log`](log.md) shows the trigger as the source
of the agent's start.

A run fails, recording why, when a template's path has no value, when a value
is one its flag refuses, or when the launch fails. A run whose
[`key`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#triggers)
repeats a recent run's is a duplicate. The trigger's `overlap` decides a run
that would start a space while an earlier one is busy.

## Timers

The `timer` source fires at the times its
[config](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#triggers)
sets. Triggers fire one at a time, within
about 30 seconds of their time. A firing waits until the launch of an earlier
firing finishes, such as a chat that waits for its place to start it.

## Webhook requests

The `webhook` source fires for each request its [route](#webhooks) accepts.
`shoal trigger run` fires it with a sample request instead. `--payload` names
the file whose text is the request's body. `--header NAME=VALUE`, repeatable,
adds a header. Shoal checks no signature on a sample request.

A webhook's request is data, not instructions. Each value from the request that
`task` renders goes between delimiters that carry a random ID, new for each
firing. The task ends with a note that says the delimited text is request data.
The other launch keys must still read as their flags' values.

`{{ headers }}` and `{{ query }}` render the whole table. They show
`[redacted]` for the value of each name that looks like a credential's, such as
`Authorization` or `api_key`. A lookup that names one, such as
`{{ headers.authorization }}`, gives its value.

## Webhooks

With
[`[machine.webhooks] listen`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#webhooks)
set, the daemon serves HTTP on that address. Its only routes are those of its
webhook triggers:

- `POST /webhooks/<name>` for the global config's trigger `name`.
- `POST /webhooks/<repository>/<name>` for a registered repository's trigger,
  by the repository name `shoal repo ls` shows.

A trigger has its route once the daemon reads the config that defines it. For
the global file, that is after `shoal config reload`.

## Webhook responses

The daemon answers `202` to a request the trigger takes, before the trigger
fires. It answers `202` whether the request then starts a run, its filter
drops it or its key repeats a recent one, so a caller cannot probe the filter.

The daemon answers `404` and records nothing for a request to a route that
names no webhook trigger, to a paused trigger, with a method other than `POST`,
or with a wrong signature. It answers `413` to a body over 5 MiB. It answers
`503` while 16 other requests are being read, checked or wait to fire.

For a trigger with
[`secret`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#triggers),
the daemon runs the secret's command for each request to check its signature.
It keeps the value nowhere. A command that takes over 15 seconds fails the
request with `500`.

## Trigger state

The daemon keeps each trigger's state by its name and its repository, or the
global config. The state outlives reloads, restarts and config edits. It holds
whether the trigger is paused, its last firing, its live keys and its newest
runs.

Each run records its time, whether the source or `shoal trigger run` fired it,
and the space it started. Its outcome is `started`, `queued`, `skipped`,
`duplicate` or `failed`, with the reason.
