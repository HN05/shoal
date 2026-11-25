# Output

This file says how every command prints its results and errors.

Commands print human-readable output unless `--json` or `SHOAL_OUTPUT=json`
selects JSON, which also turns off prompts and pickers without confirming
anything: a command that would ask still needs `--yes`. Nothing else changes: a
command does the same work with either format. `--text` overrides `SHOAL_OUTPUT`, and an
unset or empty `SHOAL_OUTPUT` means `text`; any other value fails. Errors use the
selected format on stderr. The agents Shoal starts, including chats and resumed
sessions, get `SHOAL_OUTPUT=json` unless the command that launches them sets a
nonempty value, which they keep, also when they start later from the queue or in
a place a host opens; the commands and shells Shoal runs only inherit one. An agent Shoal does not start, such as one an
app launch opens, gets JSON by default only when its own environment sets it.

A JSON error is `{"error": {"code": ..., "message": ...}}`. Programs tell
failures apart by `code`, never by the message, which may change. A refusal
carries the protocol's error code for it, such as `not_found`, `name_taken`,
`scope_denied` or `setup_failed`; `command_failed` is a failure without a
specific code, and any other code is the daemon's, as it sent it. A command
that waits on an approval it requested is not an error: it exits 2 with
`code: "approval_pending"` or `"approval_denied"` on stdout. A Rust program
reads every command's JSON, the error included, with the types of the
[`shoal-client`](apps.md) library's
[`output`](../../crates/shoal-client/src/output.rs) module and the
`shoal-protocol` types they name.

Human-readable output uses color when its destination is a terminal. Set a
nonempty `NO_COLOR` or `TERM=dumb` to disable it. Redirected output and JSON
stay plain; stdout and stderr are detected independently.
Progress feedback shows the pending operation with a spinner and elapsed seconds
after half a second, cleared before the result. Work that prints, such as setup,
shows its lines above the spinner, which names the current step.
Progress uses terminal stderr only and is suppressed with JSON output or `TERM=dumb`.
