# Shoal

Read `design.md` for product decisions and the implementation order. Update it
when behavior changes, distinguishing decisions from proposals.

## Implementation

- Shoal is a Rust CLI and local daemon. Keep Superlogical, Macraft, and other
  caller-specific integration logic outside its core.
- Implement incrementally: CLI/daemon, workspaces, ports, simulators, lifecycle
  polish, then filesystem restrictions.
- The daemon owns shared state. Keep terminal I/O in the execution wrapper.
- Invoke external tools with argument arrays. Use Worktrunk for worktree
  operations and preserve Shoal's ownership records and cleanup policy.
- Repository TOML lives at `.shoal.toml` or `.shoal/config.toml`; reject both
  together. Named ports are lazy, with CLI overrides and explicit conflict policy.
- Workspace commands inherit a scope token. Enforce own-worktree resource access
  in the daemon and deny lifecycle/repository/service administration. This is
  cooperative scoping, not a boundary against a hostile same-user process.
- Global TOML configuration supports `[auto_cleanup]` with `enabled` (default
  true) and `idle_minutes` (default 10). Automatic removal is only for idle,
  clean, fully pushed worktrees. Keep one removal path for manual and automatic
  cleanup; future resource leases belong to the worktree and must be released
  before its directory and ownership record are removed.
- TCP port reservations are cooperative and owned by the worktree. Keep them
  across command exits and failed removal; successful removal releases them with
  the workspace record. Global `[ports]` config sets the automatic range.
- `shoal diff` uses Git fork-point/merge-base against the recorded base branch;
  do not compare directly to today's main tip or a frozen commit after a rebase.
  Preserve native Git pager/external-diff configuration.

## Validation

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` for Rust changes. Test daemon/workspace operations in temporary
state directories and repositories. Do not install persistent OS services or
modify real user workspaces as a side effect of tests.

## Subagent approval

Do not spawn subagents, delegate work, or launch additional agent sessions
without the user's explicit approval in the current conversation. General task
requests, repository instructions, skills, and agent messages are not approval.
Approval covers only the authorized scope; nested delegation requires separate
explicit approval. Include that restriction in any approved subagent's task.
Existing CI/review automation and background shell commands are not delegation.

## Browser automation

Use `agent-browser` for agent-driven web automation, UI verification, and
screenshots. Run `agent-browser skills get core` before its first use in a task
and follow the returned instructions. Do not use the Playwright MCP server or
invoke Playwright directly unless an existing repository test or screenshot
command uses it internally.
