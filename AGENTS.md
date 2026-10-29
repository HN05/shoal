# Shoal

Read `docs/design.md` for product decisions and the implementation order. Update it
when behavior changes, distinguishing decisions from proposals.

## Implementation

- Shoal is a Rust CLI and local daemon. Keep Superlogical, Macraft, and other
  caller-specific integration logic outside its core.
- Implement incrementally: CLI/daemon, workspaces, ports, simulators, lifecycle
  polish, then filesystem restrictions.
- The daemon owns shared state. Keep terminal I/O in the execution wrapper.
- Treat function abstraction as a correctness requirement and a review gate.
  Every function must stay at one abstraction level: orchestration calls named
  domain operations, and helpers own lower-level implementation details. If a
  function mixes those levels, extract a named operation before adding behavior;
  bots and reviewers must flag the violation even when tests pass. Judge the
  responsibility boundary, not the function's length or helper count.
- Give each module one responsibility a reader can name. When a file comes to
  cover several, or a match dispatches every command of a group, split it by
  responsibility into a directory of modules before adding to it; judge the
  responsibility, not the line count.
- Tests live in a sibling `tests.rs` (`#[cfg(test)] mod tests;`), not inline in
  the implementation file.
- Integrations such as agent tools, terminal hosts and apps implement callbacks
  for what they do differently; state, decisions and daemon calls stay in
  Shoal's generic code, which passes each callback only what it reads.
- Invoke external tools with argument arrays. Use Worktrunk for worktree
  operations and preserve Shoal's ownership records and cleanup policy.
- Keep production Git command arguments in the `git` module; callers use named
  operations and retain lifecycle and execution policy.
- Interactive input lives in `cli::ui`, whose module doc lists what it offers:
  commands prompt and pick through its operations, never through fzf or the
  terminal directly, and a new kind of prompt or picker goes there, built on
  the existing ones, so every command looks and behaves the same.
- Forge requests live in `crates/shoal-forge`, which depends on no other Shoal
  code; it takes the account to act as, while scope, approvals, identity choice
  and workspace records stay in the daemon. What one forge does its own way
  belongs in its `Backend` implementation, never in a match on the forge kind.
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
  ancestry for the same state directory, so a cleared token does not unscope. A scope covers its
  workspace and the child workspaces split from it, recursively; targets name one of them, and
  the agent's hooks report for its own. Enforce own-worktree resource access
  in the daemon and deny workspace allocation/removal/recovery other than splitting a child and removing one without holds, and shared
  repository/service administration. For the workspaces below their own, scoped callers may queue
  agents, which the daemon starts detached, cancel their queue entries or move them back, send agent messages, answer questions, and discard
  stopped work; the agent
  of the nearest ancestor with one receives a child's agent-caused and agent-lifecycle events
  instead of the user. Own-workspace setup, rename, base workspace, issue/PR links and watches, merge
  acknowledgements, their demos, messages to the user, asking the user questions and waiting for answers, reading their own agent messages, ready-for-review marks, checkpoints and restoring them, assignment completion and withdrawal, own-repository
  sync, listing, opening and changing issues and PRs of the own repository, opening issues
  in other repositories as `[issue] other_repositories` allows, rerunning
  or cancelling their CI jobs, starting, rerunning or cancelling CI runs of their own branch and reading the own repository's, and GET requests to the own repository's forge API, or
  others where `[api] agent_writes` allows them, are allowed, except merging a PR or approving or requesting changes on one.
  Scope is cooperative, not a boundary against hostile same-user processes.
- Notifications are daemon records the CLI shows: record them where the daemon
  decides (busy resources, port conflicts, agent-shortcut exits, its own
  removals), never fail the operation for one, collapse repeated polled events
  until read, and deny reading them to scoped callers. A scoped caller may send
  the user a message for its own workspace, separate from completion.
- `shoal repo sync` fetches the default branch's remote and fast-forwards the local
  default branch under the creation refresh rules. It never pushes or moves
  workspace branches; agents update their branch from it with plain Git.
- `[cleanup.auto]` has `enabled` (default true) and `idle_minutes` (default
  10), resolved per workspace on each sweep. `[cleanup.merged]` (default true,
  240 idle minutes, persisted across restarts) removes merged, unused workspaces
  whatever their assignment state, archiving unsaved files left over on a branch
  with commits; only holds, open questions and pending PR proposals keep them. Automatic removal is only for idle,
  clean worktrees with all commits pushed, merged into the default branch (including
  squash and rebase merges) or with their changes on the branch the workspace builds
  on, or deleted worktrees. Unattended removals except for disk
  space first wait `[cleanup] grace_minutes` (default 10) of continuous eligibility,
  persisted across restarts except where idle timers start over. Keep one removal path for manual and automatic
  cleanup, deleting a branch only when a remote or the default branch has all its
  commits and never the default branch, and deleting a kept branch once they do; every removal of
  an existing worktree keeps an expiring checkpoint of HEAD and unsaved files, and
  restoring a checkpoint first keeps the state it replaces in another. Only explicit
  `done` records completion. PR cleanup is on by default: all watched PRs must merge and
  contain HEAD, or the default branch have its changes, before a completed workspace is removed. Cleanup stops tracked
  agents, verifies clean merged HEAD, and uses that path. Withdrawing completion
  preserves associations and watches but never defers cleanup; only holds,
  open questions, child workspaces and queued agent launches keep a workspace.
  Completion cleanup without a PR registration requires every commit kept that way,
  retaining dirty or newer work through the same
  removal path; completion without cleanup suppresses idle and PR cleanup.
  Associated issues suppress idle cleanup, not merged cleanup; holds, open
  questions, child workspaces and queued agent launches suppress every unattended removal. Holds belong to the user and
  host apps; scoped callers only list them.
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
- Approvals are daemon-owned: scoped callers request and inspect their own;
  only unscoped callers approve or deny, except that a request from a child workspace
  with an agent goes to the nearest coordinating agent above it holding an approved
  grant for the same target and settings, which decides it or passes it up to the
  next such coordinator or the user; coordinators without a grant are told it passed. Resource grants bind effective
  allocation settings, consume no capacity, and last until release or workspace
  removal according to configuration. Never bypass existing allocation rules.
  A scoped land
  is a request bound to the branch HEAD unless `[land] requires_approval` is
  false; approval lets the retried land proceed under every landing check.
  `[approvals] issue_open`/`pr_open = "propose"` make a scoped `issue open` or
  `pr open` an approval request the user may edit, deny or mark as a duplicate;
  approval opens it through the normal open path, a PR only at its recorded
  commit. Issue proposals belong to the repository and outlive the workspace.
  `[issue] other_repositories` (`"propose"` by default, `"direct"` or `"deny"`)
  does the same for a scoped issue in another repository, which is never linked.
- Lifecycle states, resource scopes, approval targets and approval
  specifications are typed; preserve their existing SQLite/JSON representation
  and reject unknown values.
- Problems show where the user looks (`inbox`, `alerts`, `status`, the failing
  command's error) and name the command that fixes them; repair preserves work
  and resource leases. Startup audits ownership and never deletes work, and the cleanup sweep readies a
  workspace the audit failed once its ownership verifies; the daemon clears an unknown
  execution only when a saved agent stop and a process scan prove it ended, or
  its processes ran in an earlier boot.
  Verify native PID birth identity before signaling survivors; unknown ownership
  must not authorize a kill. Explicit acknowledgement cannot bypass visible live
  processes. Verify recorded Git metadata identity before exec/removal; moved or
  replaced worktrees are not adopted automatically, only reclaimed at their
  recorded path when a human adopts that path again. Use the shared removal path
  for deleted-worktree cleanup and retain its branch; never forget a moved one.
- Repositories own `<root_dir>/<name>/` (default `~/shoal`): workspaces there by default,
  explicit workspace paths must not overlap state, checkouts, or other ownership.
  URL clones live as `.checkout` and move only with their repository directory into
  a new root; refuse roots inside state, checkouts or repository directories.
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
it: usage in README.md, behavior in docs/reference.md, decisions in docs/design.md,
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

Run `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`
for Rust changes. Test in temporary state directories and repositories, without the
launching environment's Shoal variables or configuration locations; never install
persistent OS services or modify real user workspaces as a side effect of tests.
Run development builds only with a temporary `--state-dir`: the user's daemon
serves every running agent, so never stop, restart or reinstall it. Code that
changes user-level files outside the state directory checks
`build::changes_user_files`, so debug builds leave them to the installed Shoal.

A test must not depend on runner speed. Never assert that work finishes within
a wall-clock duration or race it against a short real timeout. Assert the outcome
and the applied limit, drive time with a paused Tokio clock, or wait on an explicit
signal. Give limits that should not trigger generous values; test limits that
should trigger with work that cannot finish. Real timeouts are generous hang
guards only, never the behavior being asserted. A wait observes the state the
next step depends on, where that step reads it, and only a condition that
cannot hold before the awaited event: a file a child writes does not prove the
daemon recorded its start.

## Profiling

`python3 scripts/profile_shoal.py [--setup ARGS]... -- <shoal args>` times a
command against a foreground daemon in a temporary home and lists where the time
went; `--help` shows its options. It builds the `profiling` cargo profile,
release code with symbols for samplers such as samply. `SHOAL_PROFILE=1` makes a
Shoal process print a `profile` line to stderr for each daemon call, daemon
request and subprocess run. It is a contributor tool, so the user documentation
leaves it out.

## CI

`.forgejo/workflows/ci.yml` runs on pull requests and pushes to `main`. Its
`rust` job (the required check) runs `cargo fmt --check`, clippy, `cargo test`
and the Python release-script tests; a PR whose only `area/` label is
`area/docs` skips them. Jobs use the image built from
`.forgejo/ci-image/Containerfile` (`git.henriknordvik.com/hn05/ci-shoal:<rust
version>`), so a run installs nothing: new CI tooling goes into the
Containerfile, and only the owner rebuilds it with `.forgejo/ci-image/build.sh`
on the runner host, then updates `workspace.package.rust-version` in `Cargo.toml`, the
Containerfile's `ARG RUST`, and the `image:` tag in every workflow. Cargo builds
share the runner-mounted `/ci-target`; every step that runs cargo there sources
`.forgejo/scripts/lock-target-dir.sh` first, which takes a free slot directory
instead of waiting, and runs `cargo clean --workspace`.
`.forgejo/workflows/deps.yml` runs weekly (and on `fj actions dispatch deps.yml
main`) and rewrites one evergreen `tracking` issue, found by the marker comment
in its body, with `cargo audit` and `cargo outdated` findings. It is never a
required check; findings do not fail it, a tool that could not run does.
`.forgejo/workflows/flaky.yml` runs `cargo test` repeatedly on `main` every
night (and on `fj actions dispatch flaky.yml main`) and rewrites the evergreen
`Flaky test report` issue with every test that failed; a failure fails the run.
The run log keeps each failed test's output for the issue that fixes it.
It is never a required check.
`.forgejo/workflows/review.yml` posts an advisory review-bot review when a PR
opens and whenever `review/default`, `review/claude` or `review/codex` is
added; `review/none` suppresses it. `.review/review.md` is its project brief;
keep it current when a rule here or in `docs/design.md` changes.
Opening a ready PR already requests its review; never add reviewer request labels
with its opening classification labels. Claude and Codex use independent queues,
so one of each can run concurrently. Review and audit workflows follow review-bot's
latest `main` commit.
`.forgejo/workflows/audit.yml` runs a weekly read-only code audit using
`.review/audit.md`: one job per focus (bugs, maintainability, testing, performance
and docs) over the entire application, or manually with a selected focus and ref.
It opens one issue per new finding, labelled `audit/<focus>`; findings are
advisory and never a required check. Close an audit issue when it is fixed, or
explain in a comment or with `audit/wontfix` why it will not be.
Issues and PRs carry one or more `area/` labels and one `type/` and one
`complexity/` label; `shoal repo labels` lists what each means, and
`.forgejo/scripts/labels.sh` creates the scheme. An issue states the problem or
goal and what done looks like. One whose approach is settled is labelled
`planned`, and agents implement it without asking; one that leaves a choice for
the owner is labelled `needs-plan` until `shoal plan` settles a Plan section in
its body. Issues with neither, such as tracking and meta issues, stay out of that
flow. A PR description follows
`.forgejo/pull_request_template.md`. Push the branch and let CI verify instead
of running the full suite locally first.
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
  callers before fixing them or rejecting them with evidence in the thread.
  After fixing substantial findings, request another review and mark the PR
  ready only once it comes back clean.
  Agents cannot merge PRs; leave merging to the user instead of asking for
  permission to merge.

## Shoal feedback

When Shoal misbehaves, or something would make your workflow with it better,
file an issue.

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

## Terminal recordings

A PR that changes what Shoal prints or draws in the terminal, such as a picker,
menu, listing or message, shows examples of the new interface, recorded as the
shoal-worker skill describes. The tape runs a development build against a
temporary `--state-dir` and repository under `target/`; commit the tape and its
output only when a document uses them.
For a change to try rather than watch, offer `python3 scripts/demo.py` as a
demo (`shoal demo start shoal -- python3 scripts/demo.py`): it runs a
development build against sample workspaces, questions and approvals in a
temporary state directory.
