# Shoal

Local workspaces and resource allocation for coding agents. See [design.md](design.md)
for the full design and implementation sequence.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Daemon

```sh
cargo install --path .
shoal setup --dry-run       # Preview the OS service definition
shoal setup                 # Register and start the per-user service
shoal daemon status
shoal daemon stop
shoal daemon start
shoal daemon restart
```

macOS uses a launchd LaunchAgent in `~/Library/LaunchAgents`; it requires a GUI
login session. Linux uses a systemd user service. Minimal containers without a
user service manager can run the daemon in the foreground instead:

```sh
shoal --state-dir /tmp/shoal-dev daemon run
# In another terminal:
shoal --state-dir /tmp/shoal-dev --json daemon status
shoal --state-dir /tmp/shoal-dev daemon stop
```

State defaults to `~/.local/state/shoal`. `--state-dir` or `SHOAL_STATE_DIR` selects
an isolated instance for development. Service commands target the one registered
per-user service; its configured state directory must match. Runtime state and
the Unix socket are private to the user. Status returns exit code 1 when offline.

`setup` preserves the invoked executable's symlink path where possible. Use
`--executable /absolute/path/to/shoal` to explicitly select a stable installation
path. A running daemon must be restarted after upgrading its binary. macOS
daemon diagnostics go to `daemon.log` in the state directory; Linux uses the
user journal (`journalctl --user -u shoal.service`).

Service definition serialization and CLI-to-service control are tested with an
isolated service-manager fixture. Tests do not install persistent user services.
Native Linux service integration has not yet been exercised on a Linux host.

## Current scope

The CLI/daemon foundation is implemented. Workspace management is next, followed
by ports, simulators, lifecycle polish, and filesystem restrictions. No Shoal
filesystem sandbox is applied yet. The public repository configuration format
and schema remain undecided.
