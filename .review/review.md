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
  without those checks is a blocker. Doctor environment and untracked-worktree
  checks diagnose only, including when repair is requested; dependency checks use
  the shared executable catalog.
- Pre-setup hooks run untracked in the daemon after ownership and execution
  checks, with a timeout; failure gates readiness even without a setup command.
  Lifecycle and permit changes are excluded while the hook runs. Post-remove
  runs from the repository checkout after ownership is released; failure is a
  warning and notification, never restored ownership. Already-missing worktrees
  skip hooks, and post-remove events are not replayed on restart.
- Doctor reports current issues before falling back to the recorded
  failure and repair guidance; repair remains an explicit choice.
- Scope: workspace commands carry a scope token and get own-worktree access
  only; workspace allocation/removal/recovery and shared repository/service
  administration stay denied, while own-workspace setup, PR registration,
  merge acknowledgements and effective-configuration reads are allowed;
  notifications are read by the unscoped user only and never fail the
  operation they record. PR cleanup defaults on,
  stops tracked agents, and requires clean files and unchanged merged HEAD. PR
  numbers resolve against the workspace's origin and persist as repository-bound URLs.
  Registration variants preserve existing stored and JSON shapes and reject ambiguous
  records and conflicting actions without blocking other workspaces' cleanup;
  manual acknowledgements bind to exactly the recorded HEAD. This is cooperative,
  not a security boundary, so judge it as such.
- Allocation is atomic and persisted before the external mutation (simctl,
  Worktrunk). Leases survive restart and failed removal and are released only
  with successful removal. Active permits block automatic cleanup. Permit hooks
  run after persistent acquisition and before release, including removal while
  the worktree exists; failure retains leases. Retrying acquisition reruns its
  hook. Permit/lifecycle changes cannot overlap resource hooks in one workspace;
  user scripts own external resource integrations.
- Resource approvals bind effective allocation settings and require an unscoped
  decision. Pending requests reserve nothing; grant lifetime follows configuration
  and approval never bypasses capacity or simulator cleanup rules.
- Explicit creation bases resolve locally and are recorded for diff; only a base
  naming the local default branch is refreshed.
- Daemon ref updates disable Git credential and SSH askpass prompts without
  overriding the user's SSH transport; interactive Git retains normal prompting.
- Explicit workspace paths override only that creation; reject overlaps with state,
  checkouts, workspaces, and other repository directories; never delete their parents.
  Resolve existing symlinks and lexical `..` before checking and creating repository
  roots or explicit workspace paths, including when trailing components are missing.
- Worktrunk compatibility is shared across creation and existing-branch selection:
  reserved names get a leaf suffix on creation and an error on opening; suffixes
  never change derived workspace names or directories. Adapter outcomes preserve
  unfamiliar strings without treating them as confirmed branch deletion.
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
  Forgejo PR and issue references. Runtime dependency links target the tagged
  README on each host; reruns synchronize notes without replacing
  complete asset sets. Homebrew releases install checksummed binaries after
  both release hosts publish assets; only `--HEAD` builds from source with Rust.
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
  Custom agent names select named commands and run as tracked agents. Their
  `{prompt}` combines general instructions and issue context, prepended to
  forwarded arguments when no prompt placeholder is configured.
  Plain command invocations expand `{prompt}` to an empty string.
- AI skill directories are machine-only configuration. Skill installation accepts
  configured tool names, needs no daemon, and remains denied to scoped processes.
- External tools are invoked with argument arrays, never shell strings. Captured
  subprocesses share optional deadlines and bounded diagnostics, terminate on
  cancellation, and drain output while sending input. Internal CLI workers use
  one typed builder with explicit state directory and output mode.
  Terminal I/O stays in the execution wrapper; the daemon owns state and prepares
  each execution kind before shared registration through one request. CLI
  styles, enum `Display` formatting and transient progress belong at presentation
  sites; machine output and stored values stay plain. Progress clears before results and stays off for JSON,
  redirected stderr and dumb terminals.
- Client response extraction reports expected and received variants and preserves
  typed daemon error codes and messages without changing the wire format; unfamiliar
  codes survive verbatim for version compatibility.
  Resource protocol methods use domain-then-verb names matching the CLI operations.
- Persistence: closed Shoal enums with matching display and wire names share
  explicit spellings through one macro and reject unknown values. Opening persistence
  only migrates schema; daemon startup
  quarantines interrupted operations atomically after migration and before ownership
  auditing, requests, or cleanup. Failure aborts startup without undoing migration.
  The separate native simulator state enum preserves
  unfamiliar strings and native spellings; only confirmed shutdown frees running
  capacity. Schema and state-file changes need a compatibility
  story for existing daemons. `install` preserves compatible daemons and commands,
  deferring service changes until restart; incompatible daemons restart.
  Config key edits preserve unrelated settings and comments and validate before saving;
  repository edits serialize read/modify/write in the daemon and leave worktree files alone.
  Packaged config installation replaces global TOML only on explicit request,
  keeps a backup, and uses templates embedded in the binary.
  Packaged skills use the runtime `SHOAL_SKILL_PATH` before the build-time path;
  the path must name an absolute, existing file.
- Tests run in temporary state directories and repositories without the launching
  environment's Shoal variables or configuration locations, use the isolated
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
