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
  together when no local override exists. `setup_cmd` is tracked and gates
  readiness; `post_setup_cmd` (CLI, after ready) and `pre_remove_cmd` (daemon,
  inside the shared removal path) are untracked user processes with the
  workspace identity and no scope token. Optional local repository config lives
  in daemon state, replaces the entire worktree config, and is deleted with its
  registration. Named ports are lazy, with CLI overrides and explicit conflicts.
- Workspace commands inherit a scope token. Enforce own-worktree resource access
  in the daemon and deny `shoal pull`, `shoal land` and lifecycle/repository/service
  administration; own-workspace PR watches/merge acknowledgements are allowed.
  Scope is cooperative, not a boundary against hostile same-user processes.
- Notifications are daemon records the CLI shows: record them where the daemon
  decides (busy resources, port conflicts, agent-shortcut exits, its own
  removals), never fail the operation for one, collapse repeated polled events
  until read, and deny them to scoped callers.
- Agents may merge any local or remote branch into their own recorded workspace
  branch with `shoal merge`. A local source is first fast-forwarded from its
  upstream by the daemon under the `pull` rules, except sources without an
  upstream or checked out in a managed workspace; `--local` skips the refresh.
  Keep merges in the tracked execution wrapper; fetch remote-only sources
  without updating other branches or relying on FETCH_HEAD.
- Global TOML configuration supports `[auto_cleanup]` with `enabled` (default
  true) and `idle_minutes` (default 10). Automatic removal is only for idle,
  clean worktrees with all commits pushed or on the local default branch, or deleted worktrees. Keep one removal path for manual and automatic
  cleanup, retaining the default branch unless explicitly deleted. PR cleanup is
  on by default: stop tracked agents, verify clean merged HEAD, use that path.
- TCP port reservations are cooperative and owned by the worktree. Keep them
  across command exits and failed removal; successful removal releases them with
  the workspace record. Global `[ports]` config sets the automatic range.
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
  with successful removal. Active permits prevent automatic removal. Do not
  enforce or manage the underlying resource's lifecycle.
- Workspace and execution lifecycle states are typed enums; preserve their
  existing lowercase SQLite/JSON representation and reject unknown values.
- Reconciliation reports by default; repair preserves work and resource leases.
  Startup audits ownership but never clears unknown executions or deletes work.
  Verify native PID birth identity before signaling survivors; unknown ownership
  must not authorize a kill. Explicit acknowledgement cannot bypass visible live
  processes. Verify recorded Git metadata identity before exec/removal; moved or
  replaced worktrees are not adopted automatically. Use the shared removal path
  for deleted-worktree cleanup and retain its branch; never forget a moved one.
- Repositories own `<root_dir>/<name>/` (default `~/shoal`): workspaces inside,
  URL clones as `.checkout`, never moved; refuse roots inside state or checkouts.
- Accept literal Git branch names and derive portable workspace names separately.
  Suffix conflicting branch components with `-2`, `-3`, etc.; keep derived workspace
  names/directories unchanged; serialize allocation per repo. Existing branches get
  unsuffixed worktrees or reopen owned ones; reject other checkouts and name collisions.
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
for Rust changes. Test in temporary state directories and repositories; never install
persistent OS services or modify real user workspaces as a side effect of tests.

## CI

`.forgejo/workflows/ci.yml` runs on pull requests and pushes to `main`. Its
`rust` job (the required check) runs `cargo fmt --check`, clippy, `cargo test`
and the Python release-script tests; a PR whose only `area/` label is
`area/docs` skips them. Jobs use the image built from
`.forgejo/ci-image/Containerfile` (`git.henriknordvik.com/hn05/ci-shoal:<rust
version>`), so a run installs nothing: new CI tooling goes into the
Containerfile, and only the owner rebuilds it with `.forgejo/ci-image/build.sh`
on the runner host, then bumps the `image:` tag in every workflow. Cargo builds
share the runner-mounted `/ci-target`; every step that runs cargo there sources
`.forgejo/scripts/lock-target-dir.sh` first and runs `cargo clean -p shoal`.
`.forgejo/workflows/review.yml` posts an advisory review-bot review when a PR
opens and whenever `review/default`, `review/claude` or `review/codex` is
added; `review/none` suppresses it. `.review/review.md` is its project brief;
keep it current when a rule here or in `design.md` changes. Issues and PRs
carry `area/`, `type/` and `complexity/` labels; `.forgejo/scripts/labels.sh`
creates the scheme. Push the branch and let CI verify instead of running the
full suite locally first.

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
