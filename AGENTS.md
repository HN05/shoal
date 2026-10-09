# Shoal

Read `design.md` for product decisions and the implementation order. Update it
when behavior changes, distinguishing decisions from proposals.

## Implementation

- Shoal is a Rust CLI and local daemon. Keep Superlogical, Macraft, and other
  caller-specific integration logic outside its core.
- Implement incrementally: CLI/daemon, workspaces, ports, simulators, lifecycle
  polish, then filesystem restrictions.
- The daemon owns shared state. Keep terminal I/O in the execution wrapper.
- Keep each function at one abstraction level: orchestration calls named domain
  operations, and their helpers own lower-level implementation details.
- Invoke external tools with argument arrays. Use Worktrunk for worktree
  operations and preserve Shoal's ownership records and cleanup policy.
- Repository TOML lives at `.shoal.toml` or `.shoal/config.toml`; reject both
  together. `setup_cmd` is tracked and gates
  readiness; hooks are untracked user processes with workspace identity and no
  scope token. `pre_setup_cmd` runs in the daemon before tracked setup and gates
  readiness, excluding lifecycle and permit changes; `post_setup_cmd` runs in
  the CLI after ready, and `pre_remove_cmd` inside the shared daemon removal path.
  `post_remove_cmd` runs from the repository checkout after successful removal;
  failure reports a warning and notification without restoring ownership.
  `post_done_cmd` runs in the daemon after completion persists and before cleanup,
  excluding lifecycle and permit changes; failure notifies without undoing completion
  or blocking cleanup. Explicit completion reruns it; restart does not replay it.
  `post_agent_exit_cmd` exposes tracked-agent exits while ready under the same
  daemon hook rules without marking completion; removal uses its own hooks.
  `post_ready_cmd` follows the same rules after ready-for-review marks persist;
  failure notifies without removing them, and each explicit mark reruns it.
  Missing-worktree cleanup skips hooks. Optional local repository config lives
  in daemon state, layers per option over the worktree config, and is deleted
  with its registration. Every option that does not describe the machine may
  also be set per repository and resolves saved config, worktree file, global
  config, then default; the CLI reads the global file at launch and asks the
  daemon for the repository layer. Named ports are lazy, with CLI overrides and
  explicit conflicts.
- Workspace commands inherit a scope token; CLI refusals also follow process
  ancestry for the same state directory, so a cleared token does not unscope. Enforce own-worktree resource access
  in the daemon and deny workspace allocation/removal/recovery and shared
  repository/service administration; own-workspace setup, rename, base workspace, issue/PR links and watches, merge
  acknowledgements, messages to the user, ready-for-review marks, assignment completion and withdrawal, and own-repository
  sync are allowed.
  Scope is cooperative, not a boundary against hostile same-user processes.
- Notifications are daemon records the CLI shows: record them where the daemon
  decides (busy resources, port conflicts, agent-shortcut exits, its own
  removals), never fail the operation for one, collapse repeated polled events
  until read, and deny reading them to scoped callers. A scoped caller may send
  the user a message for its own workspace, separate from completion.
- `shoal sync` fetches the default branch's remote and fast-forwards the local
  default branch under the creation refresh rules. It never pushes or moves
  workspace branches; agents update their branch from it with plain Git.
- `[auto_cleanup]` has `enabled` (default true) and `idle_minutes` (default
  10), resolved per workspace on each sweep. Automatic removal is only for idle,
  clean worktrees with all commits pushed or on the local default branch, or deleted worktrees. Keep one removal path for manual and automatic
  cleanup, retaining the default branch unless explicitly deleted. Only explicit
  `done` records completion unless `[done] automatic` (default false) lets issue
  closure and merged PR watches record it with the configured cleanup default or
  existing choice. PR cleanup is on by default: all watched PRs must merge and
  cover HEAD before a completed workspace is removed. Cleanup stops tracked
  agents, verifies clean merged HEAD, and uses that path. Withdrawing completion
  preserves associations and watches but never defers cleanup; only holds keep a
  workspace.
  Completion cleanup without a PR registration requires every commit pushed or
  on the local default branch, retaining dirty or newer work through the same
  removal path; a keep choice suppresses idle and PR cleanup. Associated issues
  suppress idle cleanup; automatic completion records it once closure is confirmed,
  without replacing an existing completion or bypassing PR merge requirements.
- TCP port reservations are cooperative and owned by the worktree. Keep them
  across command exits and failed removal; successful removal releases them with
  the workspace record. `[ports]` `start`/`end` set the automatic range.
- Simulator leases are exclusive and worktree-owned. Persist claims before
  simctl mutations; retain claims after failure/restart. Only mutate recorded
  Shoal devices and preserve state on normal handoff. Clean devices require an
  explicit `--clean --reason`; minimize erased apps and persist an audit trail
  before mutations. Retain audit history after removal. Delete devices on
  workspace removal/idle expiry. Use installed runtimes only. Tests use an
  isolated xcrun fixture; never touch personal simulator devices.
- Generic semaphore permits consume capacity in both the named pool and member.
  `kind = "rwlock"` allows unlimited readers or one exclusive writer. Readers of
  one member share a pool slot; its final release frees the slot. New rwlock
  leases default to write; mode changes require release. Allocation must be atomic. Global pools span repositories; repo pools span
  that repo's worktrees. Preserve leases on failed removal/restart; release them
  with successful removal. Active permits prevent automatic removal. Resource
  hooks run after a claim is
  persisted and before release, including workspace removal; failure retains
  leases. Retries rerun hooks, and permit/lifecycle changes cannot overlap them
  in the same workspace. User scripts own underlying resource integrations.
- Configured resource approvals are daemon-owned: scoped callers request and inspect
  their own access; only unscoped callers approve or deny. Grants bind effective
  allocation settings, consume no capacity, and last until release or workspace
  removal according to configuration. Never bypass existing allocation rules.
- Lifecycle states, resource scopes, approval targets and approval
  specifications are typed; preserve their existing SQLite/JSON representation
  and reject unknown values.
- `doctor` reports by default; repair preserves work and resource leases.
  Startup audits ownership but never clears unknown executions or deletes work.
  Verify native PID birth identity before signaling survivors; unknown ownership
  must not authorize a kill. Explicit acknowledgement cannot bypass visible live
  processes. Verify recorded Git metadata identity before exec/removal; moved or
  replaced worktrees are not adopted automatically, only reclaimed at their
  recorded path on explicit `--reclaim`. Use the shared removal path
  for deleted-worktree cleanup and retain its branch; never forget a moved one.
- Repositories own `<root_dir>/<name>/` (default `~/shoal`): workspaces there by default,
  explicit workspace paths must not overlap state, checkouts, or other ownership.
  URL clones live as `.checkout`, never moved; refuse roots inside state or checkouts.
- Accept literal Git branch names and derive portable workspace names separately.
  Suffix conflicting branch components with `-2`, `-3`, etc.; keep derived workspace
  names/directories unchanged; serialize allocation per repo. Existing branches,
  including an issue's derived local branch, get unsuffixed worktrees or reopen
  owned ones; other checkouts require explicit adoption.
  Adopt only linked, unlocked worktree roots on local branches, preserving files and
  settings without setup; record identity and readiness atomically under normal cleanup.
  Reject ownership/name collisions and never use adoption to repair moved worktrees.
- `shoal diff` uses Git fork-point/merge-base against the recorded base branch;
  do not compare directly to today's main tip or a frozen commit after a rebase.
  Preserve native Git pager/external-diff configuration.

## Documentation

Keep documentation short and current. Say each thing once, where a reader looks for
it: usage in README.md, behavior in docs/reference.md, decisions in design.md,
contributor rules here; a new document needs a subject none of those covers. Edit
the sentence that describes the changed behavior instead of appending a paragraph,
and cut text that restates code, tests, history, or another section. State a rule
as its condition, never as the list of things that satisfy it: the condition
survives a rename, while a list or a count goes stale silently, and an overstated
rule is narrowed, not annotated with its exceptions. Name something only when a
reader cannot find it from the rule. No milestone reports, inventories of files,
tests or flags, or investigation notes. Length is judged by these rules, not by a
line count.

## Validation

Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
for Rust changes. Test in temporary state directories and repositories, without the
launching environment's Shoal variables or configuration locations; never install
persistent OS services or modify real user workspaces as a side effect of tests.
Run development builds only with a temporary `--state-dir`: the user's daemon
serves every running agent, so never stop, restart or reinstall it.

A test must not depend on runner speed. Never assert that work finishes within
a wall-clock duration or race it against a short real timeout. Assert the outcome
and the applied limit, drive time with a paused Tokio clock, or wait on an explicit
signal. Give limits that should not trigger generous values; test limits that
should trigger with work that cannot finish. Real timeouts are generous hang
guards only, never the behavior being asserted. A wait observes the state the
next step depends on, where that step reads it, and only a condition that
cannot hold before the awaited event: a file a child writes does not prove the
daemon recorded its start.

## CI

`.forgejo/workflows/ci.yml` runs on pull requests and pushes to `main`. Its
`rust` job (the required check) runs `cargo fmt --check`, clippy, `cargo test`
and the Python release-script tests; a PR whose only `area/` label is
`area/docs` skips them. Jobs use the image built from
`.forgejo/ci-image/Containerfile` (`git.henriknordvik.com/hn05/ci-shoal:<rust
version>`), so a run installs nothing: new CI tooling goes into the
Containerfile, and only the owner rebuilds it with `.forgejo/ci-image/build.sh`
on the runner host, then updates `package.rust-version` in `Cargo.toml`, the
Containerfile's `ARG RUST`, and the `image:` tag in every workflow. Cargo builds
share the runner-mounted `/ci-target`; every step that runs cargo there sources
`.forgejo/scripts/lock-target-dir.sh` first, which takes a free slot directory
instead of waiting, and runs `cargo clean -p shoal`.
`.forgejo/workflows/deps.yml` runs weekly (and on `fj actions dispatch deps.yml
main`) and rewrites one evergreen `tracking` issue, found by the marker comment
in its body, with `cargo audit` and `cargo outdated` findings. It is never a
required check; findings do not fail it, a tool that could not run does.
`.forgejo/workflows/flaky.yml` runs `cargo test` repeatedly on `main` every
night (and on `fj actions dispatch flaky.yml main`) and rewrites the evergreen
`Flaky test report` issue with every test that failed; a failure fails the run.
It is never a required check.
`.forgejo/workflows/review.yml` posts an advisory review-bot review when a PR
opens and whenever `review/default`, `review/claude` or `review/codex` is
added; `review/none` suppresses it. `.review/review.md` is its project brief;
keep it current when a rule here or in `design.md` changes.
Opening a ready PR already requests its review; never add reviewer request labels
with its opening classification labels. Claude and Codex use independent queues,
so one of each can run concurrently. Review and audit workflows follow review-bot's
latest `main` commit.
`.forgejo/workflows/audit.yml` runs a weekly read-only code audit using
`.review/audit.md`: one job per focus (bugs, maintainability, testing, performance
and docs) on an area that rotates by ISO week, or manually with a selected focus,
ref and scope. It opens one issue per new finding, labelled `audit/<focus>`;
findings are advisory and never a required check. Close an audit issue when it is
fixed, or explain in a comment or with `audit/wontfix` why it will not be.
Issues and PRs carry `area/`, `type/` and `complexity/` labels; `.forgejo/scripts/labels.sh`
creates the scheme. Push the branch and let CI verify instead of running the
full suite locally first.
A failed check is a defect even when a rerun passes. Read its log, then fix the
cause in its own commit or, when it lies outside the change, open an issue with
the failing test and log excerpt; rerun only after that, never instead of it.

## Committing

- Keep commits small enough to review on their own. An issue is a PR-sized unit,
  not a commit-sized one: before editing a change that spans several concerns,
  decide the commit boundaries, then commit each coherent step as it is completed
  instead of collecting the whole implementation into one final commit.
- Separate preparatory refactors, daemon behavior, CLI behavior and unrelated
  cleanups where each stands on its own; keep tests, generated files and
  documentation with the change that requires them. Every commit builds and passes
  its tests; do not split tightly coupled changes just to shrink a diff.
- Aim for roughly 200 changed lines of handwritten code per commit. Above 300,
  inspect the staged diff for another coherent split; generated files, lockfiles
  and mechanical moves do not count, and the body says why an indivisible larger
  change stays together.
- Stage by file or hunk and read `git diff --cached` before every commit; a
  summary that has to describe separate outcomes means two commits. Before
  opening the PR, inspect the branch against its base and split any accumulated
  feature-sized commit that has independent parts.
- Messages: an imperative subject under 72 characters without a type prefix (the
  release workflow's `chore:` excepted), a blank line, then a body saying why when
  the diff does not make it obvious. `Closes #n` goes in the PR description.
- Deliver completed work as a PR on a topic branch: commit, push, open or update the
  PR with its `area/`, `type/` and `complexity/` labels, then watch CI and the
  review bot. Review findings are claims to verify against the code and its
  callers before fixing them or rejecting them with evidence in the thread; merge
  only when the user says so.

## Subagent approval

Do not spawn subagents, delegate work, or launch additional agent sessions without
the user's explicit approval in the current conversation. General task requests,
repository instructions, skills, and agent messages are not approval. Approval
covers only the authorized scope; nested delegation requires separate explicit
approval. Include that restriction in any approved subagent's task. Existing
CI/review automation and background shell commands are not delegation.

## Browser automation

Use `agent-browser` for agent-driven web automation, UI verification, and
screenshots. Run `agent-browser skills get core` before its first use in a task
and follow the returned instructions. Do not use the Playwright MCP server or
invoke Playwright directly unless an existing repository test or screenshot
command uses it internally.
