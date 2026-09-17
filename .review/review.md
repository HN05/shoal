# Reviewing Shoal

Rust CLI plus a local daemon that owns shared state for coding-agent
workspaces, ports, simulator leases and permits. `AGENTS.md` holds the working
rules and `design.md` the product decisions; `docs/reference.md` documents
behavior. When a change touches lifecycle, ownership, resource leases or
removal, check it against the rule those files state before judging it.

## What matters most

- Safety of user work: removal goes through the one shared path, preserves
  work and leases on failure, never adopts moved or replaced worktrees, and
  never signals a process whose recorded identity was not verified. Any new
  path that deletes a directory, kills a process or mutates a simulator
  without those checks is a blocker.
- Scope: workspace commands carry a scope token and get own-worktree access
  only; lifecycle, repository and service administration stay denied. This is
  cooperative, not a security boundary, so judge it as such.
- Allocation is atomic and persisted before the external mutation (simctl,
  Worktrunk). Leases survive restart and failed removal and are released only
  with successful removal. Active permits block automatic cleanup.
- Existing branches use unsuffixed worktrees; reopen verified owned workspaces,
  reject other checkouts, and never adopt the main checkout.
- External tools are invoked with argument arrays, never shell strings.
  Terminal I/O stays in the execution wrapper; the daemon owns state.
- Persistence: lifecycle enums keep their lowercase SQLite/JSON spelling and
  reject unknown values. Schema and state-file changes need a compatibility
  story for existing daemons.
- Tests run in temporary state directories and repositories, use the isolated
  xcrun fixture, and never install services or touch personal simulators.
- Documentation: `tests/docs.rs` budgets every Markdown file. Usage lives in
  README.md, behavior in docs/reference.md, decisions in design.md,
  contributor rules in AGENTS.md. Flag behavior changes without the matching
  sentence edit, and text that restates code or history. Never ask for a
  comment on clear code or a reworded correct one.

## Do not bother with

- Formatting and import order (rustfmt and clippy run in CI).
- Restating what a commit does, or style preferences not backed by `AGENTS.md`.

Label conventions: `area/` allows several affected areas; `type/` and
`complexity/` each allow one. PR review controls use the exclusive `review/`
scope: `default`, `claude` and `codex` are consumed at run start; `none`
persists to suppress review.
