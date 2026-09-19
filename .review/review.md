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
  only; `pull`, `land`, lifecycle, repository and service administration stay denied, except
  own-workspace PR registration/merge acknowledgements; notifications are read
  by the unscoped user only and never fail the operation they record. PR cleanup defaults on,
  stops tracked agents, and requires clean files and unchanged merged HEAD. This is
  cooperative, not a security boundary, so judge it as such.
- Allocation is atomic and persisted before the external mutation (simctl,
  Worktrunk). Leases survive restart and failed removal and are released only
  with successful removal. Active permits block automatic cleanup.
- Existing branches use unsuffixed worktrees; reopen verified owned workspaces,
  reject other checkouts, never adopt the main checkout, and keep the default
  branch on removal unless deletion was explicit. Idle cleanup accepts commits
  retained on the local default branch as well as remote-tracking branches.
- Releases are pinned to the merged version commit; changelogs use published
  ancestor releases and merged PRs excluding release preparation, and GitHub
  copies the Forgejo notes.
- Prompt templates follow config precedence: saved repository values, worktree,
  global config. Setup preserves existing template files; substitutions never
  evaluate or recursively expand inserted text.
- Claude launches persist workspace trust even when its user config is absent,
  preserving other settings.
- Git profiles apply only to newly created worktrees, before setup, using
  per-worktree config; other worktrees keep their settings.
- External tools are invoked with argument arrays, never shell strings.
  Terminal I/O stays in the execution wrapper; the daemon owns state. CLI
  styles belong at presentation sites; machine output and stored values stay plain.
- Persistence: lifecycle enums keep their lowercase SQLite/JSON spelling and
  reject unknown values. Schema and state-file changes need a compatibility
  story for existing daemons. Setup preserves compatible daemons and commands,
  deferring service changes until restart; incompatible daemons restart.
  Packaged config installation replaces global TOML only on explicit request,
  keeps a backup, and uses templates embedded in the binary.
- Tests run in temporary state directories and repositories, use the isolated
  xcrun fixture, and never install services or touch personal simulators.
- Documentation: usage lives in README.md, behavior in docs/reference.md,
  decisions in design.md, contributor rules in AGENTS.md. Flag behavior
  changes without the matching sentence edit, text that restates code or
  history, and sentences that list or count what a rule covers instead of
  stating the rule; an overstated rule is narrowed, not annotated with its
  exceptions. Never ask for a comment on clear code, a reworded correct one,
  or a longer document.

## Delivery

When commit history is available, check that each commit has one coherent
purpose and keeps its tests and documentation together. Flag avoidable bundles
of independent changes and suggest concrete boundaries. The size guidance in
AGENTS.md is a prompt to inspect, not a line-count gate; tightly coupled
changes are a valid reason for a larger commit.

## Do not bother with

- Formatting and import order (rustfmt and clippy run in CI).
- Restating what a commit does, or style preferences not backed by `AGENTS.md`.

Label conventions: `area/` allows several affected areas; `type/` and
`complexity/` each allow one. PR review controls use the exclusive `review/`
scope: `default`, `claude` and `codex` are consumed at run start; `none`
persists to suppress review.
