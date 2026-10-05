# Reviewing Shoal

Rust CLI plus a local daemon that owns shared state for coding-agent
workspaces, ports, simulator leases and permits. `AGENTS.md` holds the working
rules and `design.md` the product decisions; `docs/reference.md` documents
behavior. When a change touches lifecycle, ownership, resource leases or
removal, check it against the rule those files state before judging it.

## What matters most

- The minimum Rust version follows current stable; keep the manifest and CI
  toolchain aligned and validate with locked dependencies.
- Check function abstraction levels against AGENTS.md. Judge responsibility
  boundaries rather than function length or the number of helpers.
- Safety of user work: removal goes through the one shared path, preserves
  work and leases on failure, never adopts moved or replaced worktrees, and
  never signals a process whose recorded identity was not verified. Unreadable
  inventory entries block ownership proof only while their recorded identity is live;
  recovery polls incomplete proof within the stop budget without weakening it.
  Worktree ownership is proven by the owner marker in the Git admin directory; the recorded
  inode/birth-time identity proves only unmarked records, which gain a marker only
  after it still matches. Only an explicit `doctor --repair --reclaim` may
  re-establish ownership that failed verification, and only at the recorded path,
  repository and branch. Any new
  path that deletes a directory, kills a process or mutates a simulator
  without those checks is a blocker. Removal confirmations list uncommitted changes
  and untracked files with Git status codes, bounding the preview and reporting omitted
  entries. Doctor environment and untracked-worktree
  checks diagnose only, including when repair is requested; dependency checks use
  the shared executable catalog.
- Untracked hooks use global defaults below repository config.
  Pre-setup hooks run untracked in the daemon after ownership and execution
  checks, with a timeout; failure gates readiness even without a setup command.
  Lifecycle and permit changes are excluded while the hook runs. Post-remove
  runs from the repository checkout after ownership is released; failure is a
  warning and notification, never restored ownership. Already-missing worktrees
  skip hooks, and post-remove events are not replayed on restart. Post-done runs
  after completion persists and before cleanup, including issue and PR completion;
  failure notifies without undoing completion or blocking cleanup. It excludes
  lifecycle and permit changes and is not replayed on restart. Agent-exit hooks
  expose tracked agent exits while ready, including disconnects, without changing
  the exit result or marking completion; removal uses its own hooks.
- Doctor reports current issues before falling back to the recorded
  failure and repair guidance; repair remains an explicit choice.
- Scope: workspace commands carry a scope token and get own-worktree access
  only; workspace allocation/removal/recovery and shared repository/service
  administration stay denied, while own-workspace setup, PR registration,
  merge acknowledgements, completion signals and effective-configuration reads are allowed;
  notifications are read by the unscoped user only and never fail the
  operation they record. PR cleanup defaults on,
  completes the workspace only after every watched PR merges and the set contains
  HEAD; completion honors the done default and explicit keep choices before
  stopping tracked agents with clean files and unchanged merged HEAD. PR
  numbers resolve against the workspace's origin and persist as repository-bound URLs.
  Registration variants preserve existing stored and JSON shapes and reject ambiguous
  records and conflicting actions without blocking other workspaces' cleanup;
  manual acknowledgements bind to exactly the recorded HEAD. Completion cleanup
  requires preserved commits without a PR registration, honors existing merge
  checks, and rechecks its recorded HEAD; keeping completion blocks idle and PR
  cleanup. This is cooperative,
  not a security boundary, so judge it as such.
- Simulator plans never authorize mutations by themselves: the executor rechecks
  recorded ownership and live capacity under the simulator gate. Indexed workspace
  lookups use current ownership, falling back to last ownership only when unclaimed;
  malformed ownership must not disappear from filtered results.
- Allocation is atomic and persisted before the external mutation (simctl,
  Worktrunk). Leases survive restart and failed removal and are released only
  with successful removal. Active permits block automatic cleanup. Permit hooks
  run after persistent acquisition and before release, including removal while
  the worktree exists; failure retains leases. Retrying acquisition reruns its
  hook. Permit/lifecycle changes cannot overlap resource hooks in one workspace;
  user scripts own external resource integrations.
- Resource approvals bind effective allocation settings and require an unscoped
  decision. Pending requests reserve nothing; grant lifetime follows configuration
  and approval never bypasses capacity or simulator cleanup rules. Approval lookups
  validate their selected ID or active workspace/target/name record; grant reuse
  validates every workspace/target candidate, including released grants. Listings
  reject invalid selected records; malformed history outside a query does not block it.
- Explicit creation bases resolve locally and are recorded for diff; only a base
  naming the local default branch is refreshed.
- Daemon ref updates disable Git credential and SSH askpass prompts without
  overriding the user's SSH transport; interactive Git retains normal prompting.
  Ancestry and exact-ref failures stop branch selection, refresh, and removal;
  only a documented negative exit status is a negative answer. Merge sources
  fall back to remote discovery only for a missing local ref.
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
  ancestor releases and merged PRs excluding release preparation; deleted branch
  labels require matching repository and exact PR identity. GitHub
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
  repository's remote; lookup failures or already-closed issues create nothing. The
  canonical URL persists before tracked setup or agent launch and cannot be replaced.
  Associated issues suppress idle cleanup and complete the assignment once confirmed
  closed, without replacing an existing completion or bypassing removal checks.
  Lookup failures and changed origin identity never count as closure.
- Single-workspace actions use an explicit target, then scope or current directory,
  then an interactive picker; a deleted current directory provides no workspace
  context. Explicit misses never fall back. Noninteractive and
  JSON calls without a current workspace need a target. Bare `cd` always picks;
  all-workspace operations and checkout-aware `config show` keep their selection.
- Shell integration recovers deleted current directories and pending destinations
  to their nearest surviving ancestor without a daemon, preserving exit status and
  existing prompt hooks. Scoped callers cannot use this recovery.
- Herdr handoffs belong to the CLI after interactive choices, preserving resolved
  choices, literal arguments and state/config selection; tab labels use the
  allocated workspace branch. Preparation failures keep the tab readable; only
  tracked agent completion closes it, including cleanup stops.
  JSON, help, noninteractive and `--here` calls run in place. Shell and untracked
  desktop handoffs retain the tab.
- Agent pickers offer “No agent”: issue creation continues without a launch,
  and review returns without starting a reviewer.
- Claude and Codex launches (including Happy and Codex app handoffs) persist
  workspace trust even when the user config is absent, preserving other settings.
- Machine-wide memory overload protection is opt-out; sustained CPU protection is
  opt-in. Stop one connected tracked
  agent at a time through its wrapper, newest first, with configurable thresholds
  and timing; retain work and leases, notify, and never select disconnected
  executions. Failed readings reset the sustained timer and authorize no stop.
  Automatic recovery requires a configured session restore command, proven child
  termination, verified ready workspace ownership and sustained healthy headroom.
  Keep waiting wrappers tracked, serialize restores and honor manual stop/removal.
  Persist manual recovery without replaying the original prompt; consume the
  selected record only after its replacement process is registered. Unrelated
  executions do not block manual recovery.
  Manual pause targets connected tracked agents, saves recovery for explicit
  resume, preserves work and leases, and leaves ordinary commands running.
  Scoped callers cannot pause agents.
- Agent forge wrappers are opt-in, resolve per tool through configuration layers,
  and change only the tracked agent's PATH, inherited by descendants. Wrappers
  own authentication; Shoal must not read tokens or switch the user's login.
- Git profiles apply only to newly created worktrees, before setup, using
  per-worktree config; other worktrees keep their settings.
- Root help groups built-in commands by task; configured commands are discovered
  through `run`.
- Named commands follow repository/global precedence per name and use the
  tracked execution wrapper with workspace scope. CLI agent argument defaults
  are replaceable; substitutions expand once and forwarded arguments stay literal.
  Repository-only commands require a current or explicit workspace; unknown names
  report command errors with built-in suggestions without opening a picker.
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
  The execution wrapper retains buffered daemon controls across execution transitions.
  Terminal I/O stays in the execution wrapper, which restores the caller's foreground
  group and OS terminal settings after commands and interactive hooks. When `TERM` is
  nonempty and not `dumb`, it also resets emulator input modes for the shell, writing
  best-effort cleanup to the terminal rather than redirected output; stop requests
  give the entire command process group a shared grace period, even after its leader exits. The daemon owns
  state and prepares each execution kind before shared registration through one
  request. CLI styles, enum `Display` formatting and transient progress belong at presentation
  sites; machine output and stored values stay plain. Progress clears before results and stays off for JSON,
  redirected stderr and dumb terminals.
- Client response extraction reports expected and received variants and preserves
  typed daemon error codes and messages without changing the wire format; unfamiliar
  codes survive verbatim for version compatibility.
  Resource protocol methods use domain-then-verb names matching the CLI operations.
  Independent per-workspace overview reads use bounded concurrency, preserve
  workspace order and account for failures without blocking other reads.
- Persistence: SQLite work stays on its dedicated thread, with one connection and
  a bounded queue. Shutdown drains admitted operations before releasing ownership;
  cancellation or failure never replays a closure that may have committed.
  Closed Shoal enums with matching display and wire names share
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
  Packaged skills use the runtime `SHOAL_SKILL_PATH`, then the build-time path,
  then an adjacent `shoal-skill` symlink; the selected path must name an absolute,
  existing file and stays uncanonicalized so stable prefixes follow upgrades.
- Tests run in temporary state directories and repositories without the launching
  environment's Shoal variables or configuration locations, use the isolated
  xcrun fixture, and never install services or touch personal simulators.
  Check timing against AGENTS.md's validation rule: tests must not depend on
  runner speed.
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
scope: `default` uses `REVIEW_AGENT` or Codex; reviewer requests are consumed
at run start, while `none` persists to suppress review.
