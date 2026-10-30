# shoal-client

A typed, stable API to Shoal's daemon for apps that are not Shoal's CLI, such
as Quay. The daemon serves it on its socket beside the internal protocol the
CLI speaks, which changes with every release; this API changes only as its
compatibility rule allows.

Depend on it through a git tag of a Shoal release:

```toml
shoal-client = { git = "https://git.henriknordvik.com/HN05/shoal", tag = "<release tag>" }
```

## Example

```rust
use shoal_client::{AcquireHold, Connector, Topic, Update};

let client = Connector::default_socket()?.name("quay").connect().await?;
let hold = client
    .call(AcquireHold {
        workspace: "api".into(),
        name: "quay".into(),
        reason: Some("open in Quay".into()),
    })
    .await?;

let mut subscription = client.subscribe(vec![Topic::Workspaces, Topic::Questions]).await?;
loop {
    match subscription.next().await? {
        Update::Snapshot { records, resynced } => { /* replace what is shown */ }
        Update::Changed(record) => { /* add or update it */ }
        Update::Removed(key) => { /* drop it */ }
    }
}
```

## Connecting

`Connector::new(socket)` connects to the daemon listening on `socket`;
`Connector::default_socket()` finds it as Shoal does: `daemon.sock` in
`$SHOAL_STATE_DIR`, or in `~/.local/state/shoal`. Without a scope token the
caller is the user. With `.scope(token)`, it is the agent of the workspace the
token belongs to, under the same scope rules as that agent's `shoal` commands:
it reaches only its own workspace and those split from it, cannot take holds
or read notifications, and decides only the approval requests passed to it.

A `Client` sends one call at a time and reconnects on the next call after the
daemon closed its connection. A call cut off by a restart fails with
`Error::Closed`; whether it took effect shows in the daemon's state.

## Protocol

Frames are newline-delimited JSON of at most 1 MiB. The client's first frame
selects the API and names the version it speaks:

```json
{"client_api": {"version": "1.0.0", "scope": "<token>", "client": "quay"}}
```

The daemon answers with what it serves, or a refusal such as
`unsupported_version`:

```json
{"welcome": {"version": "1.0.0", "supported_majors": [1], "features": ["workspace.get", "topic:workspaces"]}}
```

`features` names each method and subscription topic the daemon offers. Each
request then names its method and parameters, and its responses carry its ID:

```json
{"id": 1, "method": "workspace.get", "params": {"workspace": "api"}}
{"id": 1, "result": {"id": "…", "name": "api", "branch": "api", "state": "ready"}}
```

A result too large for one frame arrives as `page` frames holding the first
items of its list, then a `result` holding the rest. `schema/client-api.json`
is the JSON Schema of every frame, method, result and record.

## Compatibility

The API's version is this crate's. Adding fields, methods, record kinds,
events, topics or error codes keeps the major version: clients and the daemon
ignore fields they do not know, a new method gets a new name, and an enum value
a peer does not know decodes as `Unknown`. Removing or renaming any of them
raises the major version, and the daemon serves every major version it still
supports. A test fails when a change removes or renames a name in the checked-in
schema without a major version bump; `SHOAL_UPDATE_SCHEMA=1 cargo test -p
shoal-client schema` regenerates it.

## Errors

Every refusal has a code and a message; act on the code.

| Code | Meaning |
| --- | --- |
| `invalid_request` | The request is malformed, or names a method the daemon does not serve |
| `unsupported_version` | The daemon does not serve the client's major version |
| `not_found` | The workspace, repository, link, question or approval does not exist |
| `name_taken` | A workspace, an unfinished rename or a running chat has the name or branch |
| `scope_denied` | The caller's scope does not allow the request |
| `approval_pending` | The request waits for the user's approval, which `approval` holds |
| `approval_denied` | The user denied the approval `approval` holds |
| `busy` | A resource or the daemon's request capacity is taken |
| `not_ready` | The workspace is not ready, or its worktree is not verified |
| `unsaved_work` | Removal would delete unsaved work the call did not allow |
| `operation_failed` | Anything else; the message says what |

## Subscriptions

`subscribe` takes the topics to cover: `repositories`, `workspaces`, `links`,
`questions`, `approvals`, `messages` and, for the user only, `notifications`.
The daemon sends the snapshot of those records in one or more parts, then each
record that changed, with its new state, and each one removed, by its key,
until the client hangs up. A record's `type` names its kind; its key is that
kind with its ID. A daemon restart closes the connection: the crate's
`Subscription` reconnects and reports the new snapshot with `resynced` set,
which replaces everything known before.
