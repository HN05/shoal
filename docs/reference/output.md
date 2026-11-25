# Output

This file says how every command prints its results and errors.

## Format

Every command prints text by default and JSON with `--json`. The format
changes only what a command prints: it does the same work either way.

`SHOAL_OUTPUT` sets the default format, `json` or `text`. An unset or empty
value means `text`, and any other value makes every command fail. `--json` and
`--text` override it; a command refuses both together.

JSON output turns off prompts and pickers. It confirms nothing: a command that
would ask still needs `--yes`.

## Agents

The agents Shoal starts, chats and resumed sessions included, get
`SHOAL_OUTPUT=json`. An agent keeps a nonempty value that the command
launching it carries instead, also when it starts later from the queue or in a
place a host opens. Shoal sets no format for the commands and shells it runs:
they inherit their environment's. An agent Shoal does not start, such as one an
app opens, gets JSON only when its own environment sets it.

## Errors

A command prints its errors on stderr, in the selected format. A JSON error is
`{"error": {"code": ..., "message": ...}}`. Programs tell failures apart by
`code`, never by the message, which may change.

A refusal carries the protocol's code for it, such as `not_found`,
`name_taken`, `scope_denied` or `setup_failed`. A failure without a specific
code carries `command_failed`. A code the CLI does not know is the daemon's,
printed as the daemon sent it.

A command that waits on an approval it requested does not fail. It exits 2 and
prints `code: "approval_pending"`, or `"approval_denied"`, on stdout.

A Rust program reads every command's JSON, errors included, with the types of
the [`shoal-client`](apps.md) library's
[`output`](../../crates/shoal-client/src/output.rs) module and the
`shoal-protocol` types they name.

## Color

Text output uses color when it goes to a terminal. A nonempty `NO_COLOR` or
`TERM=dumb` turns color off. JSON and redirected output stay plain. Shoal
checks stdout and stderr separately, so one keeps its color while the other is
redirected.

## Progress

A command that waits shows a spinner after half a second. The spinner names
the pending operation and the seconds elapsed, and clears before the result
prints. Work that prints, such as setup, prints its lines above the spinner,
which names the current step. Progress shows only on a terminal's stderr, and
never with JSON output or `TERM=dumb`.
