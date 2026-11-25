# Triggers

This file says what the `trigger` commands do and how triggers start agents.

```sh
shoal trigger ls                 # This repository's triggers and the global config's
shoal trigger ls --repo docs     # Another registered repository's
shoal trigger pause triage       # It fires and launches its queued run no more, through config edits and restarts
shoal trigger resume triage      # It counts its times from now
shoal trigger run triage         # Fire it now and print the run's outcome
shoal trigger run labelled --payload issue.json --header X-Forgejo-Event=issues
```

A trigger, [`[triggers.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#triggers),
pairs a source with a launch. A source fires it with an input document, which
the trigger's filter, templates and key read whatever the source; `shoal start`
is the manual source of the same launch. A firing that the filter drops does
nothing and leaves no trace; `shoal trigger run` names the condition that did
not hold. Any other firing is a run: its launch's templates
render, each value other than `task` must read as its flag's value would, and
the launch goes through the operation `shoal start` uses, the
[agent queue](queue.md) and `max_heavy_agents` included, as you, starting a
top-level space with its mode's permissions and the usual cleanup, or with `in`
giving the task to that workspace's agent. The journal records the trigger as
the launch's source, which [`shoal log`](log.md) shows with the agent's
start. A path with no value, a value its flag refuses or a launch that fails
fails the run, recording why.

Only a firing that would start a new space meets `overlap`: while the agent
of the trigger's last started run still runs or is queued, its new space's
launch still waiting in the agent queue included, or while a queued run of
the trigger waits, `skip` records the run as skipped, `queue` records it as
queued and launches it once that agent stops, skipping any further firing
while one waits, and `parallel` launches at once. A run whose rendered `key` matches that of a run the trigger
started within `key_window` is recorded as a duplicate; the keys outlive
restarts until their window ends. A trigger without a key never drops a firing
as a duplicate.

The `timer` source fires at its clock times, or `every` interval after its
last firing, the last firing outliving restarts. Triggers fire one at a time,
within about 30 seconds of their time, or once the launch of an earlier firing,
such as a chat waiting for its place to start it, finishes.
A time a daylight-saving change skips fires at the next minute that exists,
and one it repeats fires once. After the daemon was stopped or the machine
slept, the newest time missed fires once if it is at most `catch_up` old,
and the times before it, or every one beyond `catch_up`, are recorded as one
skipped run. Shoal starts counting a trigger's times when it first reads it,
and a resumed trigger from its resume, so neither catches up on earlier ones.

The `webhook` source fires for each request its [route](#webhooks) accepts.
`shoal trigger run` fires one with a sample request instead: `--payload` names
the file whose text is its body, and `--header NAME=VALUE`, repeatable, adds a
header; its signature is not checked. Each value a webhook's request sent that
a `task` renders goes between delimiters carrying a random ID new for each
firing, and the task ends with a note saying that the delimited text is
request data, not instructions; the other launch keys must read as their
flag's values anyway. `{{ headers }}` and `{{ query }}`, the whole tables,
show `[redacted]` for the value of each name that looks like a credential's,
such as `Authorization` or `api_key`, while a lookup naming one gives its
value.

The daemon keeps each trigger's state by its repository, or the global
config, and its name, so it outlives reloads, restarts and edits: whether it
is paused, its last firing, its newest runs, each with its time, whether its
source or `shoal trigger run` fired it, its outcome of started, queued,
skipped, duplicate or failed with the reason, and the space it started, and its
live keys. `shoal trigger ls` lists the triggers of the repository the command
acts in, or `--repo` names, after the global config's, with their source,
whether they are paused, their next firing and their last run; `--json` prints
the same. `pause`, `resume` and `run` name a trigger, the repository's before
the global config's of the same name. Every space may list triggers; a space's
`pause`, `resume` and `run` follow its `triggers`
[permission](modes.md#permitted-actions), `propose` in the built-in modes.

## Webhooks

With [`[machine.webhooks] listen`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#webhooks)
set, the daemon serves HTTP on that address, with no routes but its webhook
triggers': `POST /webhooks/<name>` for the global config's trigger `name` and
`POST /webhooks/<repository>/<name>` for a registered repository's, by the
name `shoal repo ls` shows. A trigger has its route once the config that
defines it is read, after `shoal config reload` for the global file. Without
`listen`, no port is open. Shoal does not expose the listener to the internet:
put a tunnel or reverse proxy in front of it, such as Tailscale Funnel,
cloudflared or Caddy, and give the forge its public URL.

A request the trigger takes gets `202` before it fires, whether it then starts
a run, its filter drops it or its key repeats one, so a caller cannot probe the
filter. A request to no webhook trigger's route, to a paused one, with another
method or with a wrong signature gets `404`, and records nothing; a body over
5 MiB gets `413`, and a request while 16 others are being read, checked or
wait to fire gets `503`. For a trigger with `secret`, the daemon runs the secret's
command for each request to check its signature and keeps the value nowhere;
a command that takes over 15 seconds fails the request with `500`.
A trigger without `secret` takes any request that reaches the listener,
including one from an agent on this machine, which then starts a space as you
whatever its own `triggers` permission.
