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
- Reconciliation reports the recorded failure so setup failures retain their
  retry guidance; repair remains an explicit choice.
- Scope: workspace commands carry a scope token and get own-worktree access
  only; workspace allocation/removal/recovery and shared repository/service
  administration stay denied, while own-workspace setup, PR registration,
  merge acknowledgements and effective-configuration reads are allowed;
  notifications are read by the unscoped user only and never fail the
  operation they record. PR cleanup defaults on,
  stops tracked agents, and requires clean files and unchanged merged HEAD. PR
  numbers resolve against the workspace's origin and persist as repository-bound URLs. This is
  cooperative, not a security boundary, so judge it as such.
- Allocation is atomic and persisted before the external mutation (simctl,
  Worktrunk). Leases survive restart and failed removal and are released only
  with successful removal. Active permits block automatic cleanup.
- Explicit creation bases resolve locally and are recorded for diff; only a base
  naming the local default branch is refreshed.
- Explicit workspace paths override only that creation; reject overlaps with state,
  checkouts, workspaces, and other repository directories; never delete their parents.
- Existing branches use unsuffixed worktrees; reopen verified owned workspaces,
  require explicit adoption for other linked checkouts, never adopt the main checkout,
  and keep the default branch on removal unless deletion was explicit. Adoption records
  identity and readiness atomically without setup, hooks, or Git setting changes;
  it takes normal cleanup ownership and cannot repair moved managed worktrees.
  Idle cleanup accepts commits retained on the local default branch as well as
  remote-tracking branches.
- Releases are pinned to the merged version commit; changelogs use published
  ancestor releases and merged PRs excluding release preparation, and GitHub
  keeps change descriptions with a GitHub comparison link, omitting unmirrored
  Forgejo PR and issue references; reruns synchronize notes without replacing
  complete asset sets.
- Prompt templates follow config precedence: saved repository values, worktree,
  global config. `install` preserves existing template files; substitutions never
  evaluate or recursively expand inserted text.
- Issue numbers use an explicit repository, the current registered checkout or
  managed workspace, or an interactive picker. Issue URLs must match the selected
  repository's remote; lookup failures create nothing.
- Claude and Codex launches (including Happy and Codex app handoffs) persist
  workspace trust even when the user config is absent, preserving other settings.
- Agent forge wrappers are opt-in, resolve per tool through configuration layers,
  and change only the tracked agent's PATH, inherited by descendants. Wrappers
  own authentication; Shoal must not read tokens or switch the user's login.
- Git profiles apply only to newly created worktrees, before setup, using
  per-worktree config; other worktrees keep their settings.
- Named commands follow repository/global precedence per name and use the
  tracked execution wrapper with workspace scope. CLI agent argument defaults
  are replaceable; substitutions expand once and forwarded arguments stay literal.
  Repository-only commands require a current or explicit workspace.
  `{diff_base}` resolves lazily through the shared daemon diff-base lookup.
- External tools are invoked with argument arrays, never shell strings.
  Terminal I/O stays in the execution wrapper; the daemon owns state. CLI
  styles and transient progress belong at presentation sites; machine output and
  stored values stay plain. Progress clears before results and stays off for JSON,
  redirected stderr and dumb terminals.
- Persistence: lifecycle enums keep their lowercase SQLite/JSON spelling and
  reject unknown values. Schema and state-file changes need a compatibility
  story for existing daemons. `install` preserves compatible daemons and commands,
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
