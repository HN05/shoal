# Shoal

Read `docs/design.md` for product decisions and the implementation order. Update it
when behavior changes, distinguishing decisions from proposals.

Read this whole file before you start, in as many reads as it takes: the rules
for delivering a PR, under Committing near the end, decide when work is done.

## Implementation

- Shoal is a Rust CLI and local daemon. Keep Superlogical, Macraft, and other
  caller-specific integration logic outside its core.
- Implement incrementally: CLI/daemon, workspaces, ports, simulators, lifecycle
  polish, then filesystem restrictions.
- The daemon owns shared state. Keep terminal I/O in the execution wrapper.
- A store operation that needs a transaction runs in `Store::write`, or in
  `Store::read` when it only reads; one that opens its own on `Store::run`
  says why.
- Shoal makes no compatibility promise yet, to its earlier versions or to
  what they wrote: when a type, record, protocol message, output or file
  format should take another shape, change it now, while that is cheap. A
  store migration rewrites what the store holds into the new shape in the
  same change, so code reads one shape and keeps no alias, fallback or
  second reader for an old one; what another version sends, such as a
  `shoal move` package, is refused, not converted. A process an upgrade
  leaves running, such as an agent's wrapper, runs its version's code, which
  no migration can rewrite: keep it usable where that is easy, and accept
  what it then does oddly, such as a report the daemon no longer takes. A
  change that keeps code for such a process files an issue to remove it,
  naming the release after which it can go.
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
- When you see a problem with the code's structure or code that should be
  refactored, refactor it in your change when it lies within your work, and
  otherwise file an issue for it instead of only mentioning it.
- Tests live in a sibling `tests.rs` (`#[cfg(test)] mod tests;`), not inline in
  the implementation file.
- A file a package compiles, or its tests read, lives in that package's
  directory (for the root package, `src/` or `tests/`) or in that of a package
  it depends on: CI tests a PR's packages by the directories it changes, and
  `scripts/check_package_sources.py` fails a build that reads any other.
- Integrations such as agent tools, terminal hosts and apps implement callbacks
  for what they do differently; state, decisions and daemon calls stay in
  Shoal's generic code, which passes each callback only what it reads.
- A chat gets every feature that coordinates or communicates, including
  requests to the forge such as starting CI runs, but nothing that runs code on
  this machine, such as leases, `shoal run` and demos, or needs its own
  worktree or branch. A change that adds or extends such a feature implements
  it for chats too, through the same code path, or its PR says why it cannot
  apply to them. A workspace has one editing agent, its own: a chat in its
  worktree reads it as a chat, never as the workspace's agent or child, and
  goes with it.
- A space is a workspace or a chat. Flags, arguments, user-facing text and
  code call what applies to both a space (`--space`), and say workspace only
  for what needs a worktree. A command's flag becomes `--space` in the change
  that lets it act on a chat. Command names stay short: `shoal start` starts
  every agent, in a workspace or a chat, and a command that maintains a space
  is top-level, such as `shoal status` and `shoal rename`.
- Spaces form one tree in `space_parents`: every space has at most one parent,
  a ready workspace or a chat of its own repository, or a chat without a
  repository, which takes children in any, never one below it; any other
  child in another repository is refused. Scope, approvals, coordinator and
  delegated alerts, removal and cleanup take one walk up or down that link
  and one check for an agent, with no branch on kind; alerts to a space go
  through one operation.
- Shoal refuses a request only for a very good reason: it would act outside
  the caller's scope or permissions, lose or damage work, or cannot be done at
  all. A limit of how Shoal is built, such as a code path one kind of caller
  does not take, is no such reason: support the request instead, or file an
  issue for it when that lies outside the change. Nor is a view of where a
  setting or action belongs, or of what the user ought to want: a config file
  takes every option its schema has, except where its value cannot apply.
- Invoke external tools with argument arrays. Worktrees are created and
  removed with plain Git through the `git` module; preserve Shoal's ownership
  records and cleanup policy.
- Keep production Git access in the `git` module, both Git command arguments
  and the reads it makes in process with `gix`; callers use named operations
  and retain lifecycle and execution policy. `gix` reads a repository's
  location, HEAD, refs, remotes and config, opening it afresh for each read on
  the blocking pool, and reads through Git where it would not read the
  repository as Git does. Writes, network access, `status`, `ls-files` and
  `diff`, revision syntax Git interprets, such as a caller's `--base HEAD~2`,
  and the checks behind removal and cleanup decisions run Git; HEAD, a full
  ref name or a full object ID, peeled at most once, means the same to both.
- Interactive input lives in `cli::ui`, whose module doc lists what it offers:
  commands prompt and pick through its operations, never through fzf or the
  terminal directly, and a new kind of prompt or picker goes there, built on
  the existing ones, so every command looks and behaves the same.
- Give each command, flag and value one name: no clap aliases, and a rename
  drops the old name. A short form is not an alias: a flag people type often
  gets a letter, which stands for that long flag in every command that defines
  it and for no other.
- A verb that acts the same way on every kind a workspace links (issues, PRs
  and CI runs) is a top-level command, `shoal <verb> [kind] [item]`, that acts
  on everything linked when both are omitted: `view`, `view ci 2765`,
  `watch pr 12`. Leases follow the same grammar: `acquire port`, `release sim`.
  Every other action goes subject first under its kind's group, even when
  another group has one of the same name: `pr close`, `issue comment`,
  `ci rerun`. Listing is one, since each kind filters its own way: `ci ls`.
  `merge` is top-level because it merges a space's work, through its PR or
  locally, and reads a PR it is given as the cross-kind verbs read items.
  A command that prints a list and exits is named `ls`; `list` names a picker.
  A new kind joins the cross-kind verbs that apply to it instead of growing its
  own `view` or `watch`.
- Forge requests live in `crates/shoal-forge`, and the forge data the
  protocol carries, with the logic that reads it without a request or the
  daemon's state, such as repositories, item URLs, check states, item
  snapshots and run IDs, in `crates/shoal-forge-types`, at the module path
  the type has in `shoal-forge`, which re-exports none of them and adds its
  requests on them as each module's extension trait.
  `shoal-forge-types` depends on no Shoal crate but `shoal-forge-derive`,
  `shoal-refs`, `shoal-states` and `shoal-time`; `shoal-forge` on those,
  `shoal-forge-types` and `shoal-subprocess`. It takes the account to act
  as, while scope, approvals, identity choice and workspace records stay in
  the daemon. What one forge does its own way belongs in its implementation
  of `Routes` in `shoal-forge-types`, where reading a URL needs it, or of
  `Backend` or the area trait over it that covers it, such as `Runs`, in
  that area's module of `shoal-forge`, never in a match on the forge kind,
  and reading an issue, PR or run by its kind goes through `ForgeRepo`'s one
  operation for it.
- Apps use Shoal's protocol through the `shoal-client` crate, with no
  compatibility promise: the protocol changes freely and only the exact
  version match protects a client. `crates/shoal-protocol` holds the types
  the daemon or core build or read, with the logic over them that needs no
  connection, and `shoal-client` the connection, one operation per request in
  the module of the same name, the handles that read from a connection and
  what only callers print, re-exporting none of `shoal-protocol`. The CLI and
  apps share `shoal-client`: the CLI sends each daemon request through a
  named operation of the client module for its subject and never builds one
  itself. What the CLI alone needs, such as parsing, prompting and printing,
  stays in `shoal-cli`, and parsing and resolving config, which the protocol
  carries without reading, lives in `shoal-core`. `shoal-protocol` depends on
  no Shoal crate but `shoal-config-derive`, `shoal-forge-types`, `shoal-ids`,
  `shoal-refs`, `shoal-schedule`, `shoal-states` and `shoal-time`;
  `shoal-client`'s code on none but `shoal-forge-types`, `shoal-ids`,
  `shoal-protocol`, `shoal-schedule` and `shoal-states`.
- `crates/shoal-schedule` holds the agent queue's order: priorities, the
  rank types that explain an entry's place, the snapshot a policy reads, the
  `Scheduler` trait and the built-in policies. A policy reads only that
  snapshot, which the daemon builds from its state, and never the state
  itself; admission stays in the daemon. It depends on no Shoal crate but
  `shoal-ids`, `shoal-time` and `shoal-states`.
- `crates/shoal-store` holds the daemon's `Store`, its schema, its
  migrations and the row mappers and guards every subsystem shares; a
  subsystem's own record functions stay with it. A migration calls no code
  outside the store: one that needs a subsystem's logic takes a copy frozen at
  the shape it migrates. It depends on no Shoal crate but `shoal-core`,
  `shoal-ids`, `shoal-protocol` and `shoal-refs`.
- `crates/shoal-daemon-base` holds what every subsystem of the daemon uses:
  its log, a request's caller, the approval requests subsystems make, the
  actions permissions govern, config state, gates, events, alerts, item
  numbers, file access, the proof of the directories Shoal owns and the
  kept copies of attached files. What it needs of a subsystem is an interface it declares, a trait whose methods
  return a `Send` `BoxFuture`, which the daemon implements; its code is no
  method of `Manager`. It depends on no Shoal crate but `shoal-core`,
  `shoal-forge-types`, `shoal-ids`, `shoal-protocol` and `shoal-store`.
- A subsystem moved out of the daemon is a crate of its own, such as
  `crates/shoal-leases` for ports, resource permits, simulators, containers
  and secrets, `crates/shoal-reviews` for local reviews, the gate agent
  and the one-shot agent runs they start, `crates/shoal-hub` for the hub
  role and membership of a hub, `crates/shoal-runner` for triggers and
  the CI jobs this machine runs, and `crates/shoal-links` for the issues,
  PRs and CI runs spaces link.
  Its service, a field of `Manager`, holds what the subsystem
  keeps in memory and offers its operations; what it needs of other
  subsystems is an interface it declares, as the base does, which the daemon
  implements and passes each operation, except what runs in the subsystem's
  own transaction, a synchronous method over it that the service keeps. Its
  tests build the service over a temporary store and stand-ins for those
  interfaces; a test across subsystems stays in `shoal-daemon`. `shoal-leases` depends on no
  Shoal crate but `shoal-core`, `shoal-daemon-base`, `shoal-ids`,
  `shoal-protocol`, `shoal-store` and `shoal-subprocess`; `shoal-reviews` on
  none but `shoal-core`, `shoal-daemon-base`, `shoal-forge`,
  `shoal-forge-types`, `shoal-ids`, `shoal-protocol`, `shoal-refs` and
  `shoal-store`; `shoal-hub` on none but `shoal-core`, `shoal-daemon-base`,
  `shoal-ids`, `shoal-protocol`, `shoal-schedule` and `shoal-store`;
  `shoal-runner` on none but `shoal-core`, `shoal-daemon-base`,
  `shoal-forge`, `shoal-forge-types`, `shoal-ids`, `shoal-protocol`,
  `shoal-refs` and `shoal-store`; `shoal-links` on none but `shoal-core`,
  `shoal-daemon-base`, `shoal-forge`, `shoal-forge-types`, `shoal-ids`,
  `shoal-protocol`, `shoal-refs` and `shoal-store`.
- `crates/shoal-time` holds the wall-clock timestamp type,
  `crates/shoal-refs` the commit and branch name types, `crates/shoal-ids`
  the record IDs and numbers, and `crates/shoal-states` the macros that
  define typed names, that every crate shares; none depends on a Shoal crate.
- The CLI lives in `crates/shoal-cli` and the daemon in `crates/shoal-daemon`,
  neither of which depends on the other, so an edit in one rebuilds neither
  the other nor its tests. Nor does the daemon depend on a crate that calls
  the daemon, even as a dev-dependency: a test that drives a daemon through
  such a crate lives in that crate's tests and starts the daemon through its
  `test-support` feature. The `shoal` package is only `main`,
  which runs the CLI and passes it the daemon for `shoal daemon run`. Code both use, directly
  or through code they call, lives in `crates/shoal-core`, in a top-level
  module named for what it does; it names neither the CLI nor the daemon,
  makes no daemon calls and depends on no Shoal crate but
  `shoal-config-derive`, `shoal-forge`, `shoal-forge-types`, `shoal-ids`,
  `shoal-protocol`, `shoal-proxy`, `shoal-refs`, `shoal-schedule`,
  `shoal-states` and `shoal-subprocess`. Its
  items are `pub` only where another Shoal crate uses them, and fixtures the
  other crates' tests share sit behind its `test-support` feature.
- `crates/shoal-cli` serves only the CLI. Code that the daemon, the execution
  wrapper, the proxy or the workers use lives outside it, in `shoal-workers`
  where it calls the daemon.
- `crates/shoal-workers` holds the execution wrapper and the workers: the
  code that runs in the processes Shoal starts and calls the daemon, which the
  CLI reaches there too. Its code depends on no Shoal crate but `shoal-client`,
  `shoal-core`, `shoal-forge-types`, `shoal-ids`, `shoal-protocol`,
  `shoal-proxy` and `shoal-refs`; neither core nor the daemon's own code
  depends on it. Where core logic needs a value the daemon records, its
  caller reads it and passes core the value, never a callback that calls the
  daemon.
- Repository TOML lives at `.shoal.toml` or `.shoal/config.toml`; reject both
  together. `setup.command` is tracked and gates
  readiness; hooks are untracked user processes with workspace identity and no
  scope token. `hooks.pre_setup` runs in the daemon before tracked setup and gates
  readiness, excluding lifecycle and permit changes; `hooks.post_setup` runs in
  setup's daemon-started operation after ready, and `hooks.pre_remove` inside the shared daemon removal path.
  `hooks.post_remove` runs from the repository checkout after successful removal;
  failure reports a warning and alert without restoring ownership.
  `hooks.post_done` runs in the daemon after completion persists and before cleanup,
  excluding lifecycle and permit changes; failure notifies without undoing completion
  or blocking cleanup. Explicit completion reruns it; restart does not replay it.
  `hooks.post_agent_exit` exposes a space's tracked-agent exits, a workspace's
  while ready, where `hooks.post_done` runs and under the same daemon hook rules,
  without marking completion; removal uses its own hooks.
  `hooks.post_ready` follows the same rules after ready-for-review marks persist;
  failure notifies without removing them, and each explicit mark reruns it.
  Missing-worktree cleanup skips hooks. Optional local repository config is
  `.shoal/config.toml` in the repository's own directory, read in place, layers
  per option over the worktree config, and is deleted with its registration.
  Settings that describe the machine live under the global `[machine]` table;
  every other option follows the repository schema in both files and resolves
  local config, worktree file, global config, shared config (`shared.toml`
  beside it), then default; the CLI reads the global files at launch and asks the
  daemon for the repository layer. Named ports are lazy, with CLI overrides and
  explicit conflicts.
- Workspace commands and every chat's agent inherit a scope token, a chat's
  lasting as long as the chat; only the user's own terminal is unscoped. CLI
  refusals also follow process ancestry for the same state directory, so a
  cleared token does not unscope. A scope covers its space and the spaces below
  it, recursively, and a chat without a repository's every space, through one
  operation every check takes; targets name one of them, and the agent's hooks report for
  its own. A chat's agent follows the workspace-agent rules below, without what
  needs a worktree; it renames its own chat. A chat in a workspace's worktree
  also reads that workspace, with requests that only read state, and changes
  nothing there. `shoal start` and `shoal create` run inside a space create a
  child of that space, or of a space its scope reaches that `--parent`
  names, in its repository; scoped callers create only
  children, run chats only in worktrees of their tree, never take over a workspace outside their scope, and start chats in
  a background place the agent queue opens, unless `start --here` runs it in the caller's terminal, and the user's terminal creates top-level spaces. Enforce own-worktree resource access
  in the daemon and deny workspace allocation/removal/recovery other than creating a child of their own space, removing a space below it and restoring a removed space below its recorded parent where their scope reaches that parent, and administering Shoal and the other actions a permission key governs only as that key allows. For the spaces their scope reaches but their own, scoped callers may hold them, queue
  agents and the restores of stopped agents, a workspace's or a chat's, whoever stopped them, which the daemon starts detached, a chat's in the place it ran in, unless `start --here` runs an agent in the caller's terminal under its own space's scope, restart and stop their agents, confirming a stop Shoal cannot prove, convert and rename them, cancel and prioritize their queue
  entries, a CI job's through the spaces its run is for and a new space's launch through its parent, send them alerts, open threads with them and post in the threads they hold, stop their executions, and discard their stopped agents; they may also stop the untracked processes and leftovers of their own workspace and those below it; the agent
  of the nearest ancestor with one receives a child's agent-caused and agent-lifecycle events
  instead of the user. Own-workspace setup, rename, base workspace, issue, PR and own-repository CI run links and watches, merge
  acknowledgements, their demos, alerts up to their coordinator or the user, opening threads with their coordinator or the user, posting in and waiting on the threads they take part in, resolving the ones they opened, forwarding the ones they hold, reading and linking the threads a space their scope reaches took part in, reading their own space's alerts, ready-for-review marks, checkpoints and restoring them, assignment completion and withdrawal, own-repository
  sync, listing issues and PRs of any repository, opening and changing those of the own repository (for a chat without a repository, every registered one, under its permissions there), opening issues
  in other repositories as their `issue_other_repositories` permission allows, rerunning
  or cancelling their CI jobs, starting, rerunning or cancelling CI runs of the default branch and of the branches of the workspaces their scope reaches in the repository they act in, and reading any repository's issues, PRs and runs, and GET requests to any repository's forge API, or
  other methods to the own repository's where their `api_writes` permission allows them, are allowed, and approving or requesting changes on a PR
  as their `pr_review` permission allows, as the host's `review_token`.
  Scoped callers may run, view, cancel and answer the reviews of their own workspace and those below it;
  with `[review] require_for_pr`, `require_for_ready` or `require_for_merge` holding the caller, a
  new-PR `pr open`, a ready mark and a merge are refused until a finished review covers the commit
  they act on with every finding resolved.
  Scope is cooperative, not a boundary against hostile same-user processes.
  A scope token is a `ScopeToken`, whose `Debug` redacts it, never a bare
  string; code reads its text with `expose` only where it sends it. The
  daemon decides what a caller may do, and which account it acts as, from its
  `Option<&Caller>`, `None` for the user, never from a `bool` saying whether
  it is scoped.
- An alert is one daemon record of a one-way notice, with its typed kind, its
  sender (the user, a space or Shoal) and its recipient (the user or a
  space); one daemon operation decides whom it reaches, up to the
  coordinator or the user, or down to spaces the sender's scope reaches.
  Record Shoal's own where the daemon decides (busy resources, port
  conflicts, agent exits, its own removals), never fail the operation for
  one, collapse repeated polled events until read, and let scoped callers
  read the user's only as their `notifications` permission allows. Only the
  sender withdraws an alert, while it is unread, and an alert stays separate
  from completion.
- A thread is one daemon record of a conversation between its opener (the
  user, a space or Shoal) and its holder (the user or a space), numbered in
  its opener's space, or the space the user or Shoal opened it with, among
  that space's approval requests. Its posts reach the other side, and the
  spaces that linked it, as alerts the transaction that records them
  queues; the holder forwards it, and the opener resolves it with an
  outcome its kind takes, or the holder one Shoal opened. `done` and
  removal close the threads a space is numbered in or holds, and their
  undo and a restore reopen those closed for that cause.
- The daemon keeps each registered checkout's default branch in sync with its
  remote in the background, and `shoal repo sync` syncs it at once; commands that
  only read the default branch use what the checkout has. Syncing never pushes or
  moves workspace branches: only an explicit `shoal refresh`, or the start of
  an agent the queue admits there, fast-forwards one that holds nothing of its
  own, and agents otherwise update it with plain Git.
- Cleanup is space cleanup: chats follow the workspace rules below, with their
  repository's config, or the global config for a chat without one, meeting every condition on a worktree; a running agent
  is use, a restore that waits to run its agent keeps a chat as a queued
  launch keeps a workspace, and a session Shoal saved keeps it from idle
  cleanup as saved stopped work does. Idle cleanup also counts a
  coordinator's running agent as use of a workspace below it in which no
  agent of its own has started, since its subagents work there, and of a
  chat below it whose agent stopped, whose work is left to it. A chat's
  agent exiting stops the chat, which ends only by removal.
- `[cleanup.auto]` has `enabled` (default true) and `idle_minutes` (default
  10), resolved per workspace on each sweep. `[cleanup.merged]` (default false,
  240 idle minutes, persisted across restarts) removes merged, unused workspaces
  whatever their assignment state, archiving unsaved files left over on a branch
  with commits; only holds, threads they opened that wait, pending PR proposals and push
  requests and a space below them keep them. Automatic removal is only for idle,
  clean worktrees with all commits pushed, merged into the default branch (including
  squash and rebase merges) or with their changes on the branch the workspace builds
  on, or deleted worktrees. Unattended removals except for disk
  space first wait `[cleanup] grace_minutes` (default 10) of continuous eligibility,
  persisted across restarts except where idle timers start over. Keep one removal path for manual and automatic
  cleanup, deleting a branch only when a remote or the default branch has all its
  commits and never the default branch, and deleting a kept branch once they do; every removal of
  an existing worktree, except a disposable workspace's, keeps an expiring checkpoint of HEAD and unsaved files, and
  restoring a checkpoint first keeps the state it replaces in another. Only explicit
  `done` records completion. PR cleanup is on by default: all watched PRs must merge and
  contain HEAD, or the default branch have its changes, before a completed workspace is removed. Cleanup stops tracked
  agents, verifies clean merged HEAD, and uses that path. Withdrawing completion
  preserves associations and watches but never defers cleanup; only holds,
  threads it opened that wait and queued agent launches keep a workspace. Children keep
  their parent only from merged cleanup: every removal of a space first removes the spaces below it
  through the same path, whatever keeps them, and a child that cannot go retains
  the parent. A standing completion with cleanup also removes the workspaces and chats below
  that nothing of their own keeps, whatever their work.
  Completion cleanup without a PR registration requires every commit kept that way,
  retaining dirty or newer work through the same
  removal path; completion without cleanup suppresses idle and PR cleanup.
  Associated issues suppress idle cleanup, not merged cleanup; holds, waiting
  threads and queued agent launches suppress every unattended removal. Holds
  record who placed them; scoped callers release the user's holds, and remove
  the spaces the user holds, only as their `user_holds` permission allows, and
  holding their own space is an approval request.
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
  only unscoped callers approve or deny, except that a request from a space
  with an agent goes to the nearest space above it with an agent, of either
  kind, and a space whose `decide` permission is `direct` decides any request
  of a space below it as the user would, but approves one a permission
  governs only where its own value for it is `direct`. The space a request goes to decides
  it or passes it up, the next such space above and the user at the top. For a
  request a permission governs, a parent space whose own effective
  permission for it is `direct` decides it as the user would, editing a
  proposal or marking it a duplicate; one without may deny it, with a reason
  or as a duplicate, or pass it up. For any other request, a parent space
  without an approved grant for the same target and settings may only deny or
  pass it up, a chat as a workspace. A chat whose agent
  stops, or a deciding space that is removed, passes its undecided requests to
  the user. Resource grants bind effective
  allocation settings, consume no capacity, and last until release or workspace
  removal according to configuration. Never bypass existing allocation rules.
  A scoped merge
  is a request bound to the branch HEAD unless the caller's `merge` permission is
  `"direct"`, and a scoped unmerge one bound to the merge it undoes; approval
  lets the retry proceed under every check.
  `issue_open`/`pr_open = "propose"` make a scoped `issue open` or
  `pr open` an approval request the user may edit, deny or mark as a duplicate;
  approval opens it through the normal open path, a PR only at its recorded
  commit. A chat proposes issues, without a branch or commit, but never PRs.
  Issue proposals belong to the repository and outlive the space that made them.
  `issue_other_repositories` (`"propose"` by default, `"direct"` or `"deny"`)
  does the same for a scoped issue in another repository, which is never linked.
  A `"gate"` value, between `"direct"` and `"propose"`, sends a request whose
  narrowest governing key is `"gate"` to the gate agent first, which allows it
  through the approval path, denies it, or passes it on only for an
  `[gate] escalate` case it quotes, which then takes the `"propose"` route.
- Permissions belong to spaces. A space's are its agent's mode's
  (`[modes.NAME.permissions]` over `[permissions]` over the built-in mode's
  defaults over the key's), each narrowed to its parent's effective value,
  then each key the `--permission` given to its agent sets takes that value as
  given; it is recorded beside each `mode` column, kept by restores and
  restarts, and changed per key by their own `--permission`. A scoped process
  gives only values its own space holds, including the value a key it drops
  with `KEY=mode` falls back to and a permission it sets in config, which the
  daemon checks wherever such a process brings one in; it never removes a
  permission from config or replaces a config file. One daemon
  operation computes them at each check from current config, the recorded
  modes and given values and the tree; a scoped check reads the acting caller's space's
  through it, never a repository's settings, and a chat without a repository
  resolves its mode in the config of the repository the action is in.
  A key that governs an action lets a scoped caller take it with `direct`,
  refuses it with `deny`, and with `propose` makes it an approval request
  bound to what it does, which lets the same action proceed once; the user
  holds every key at `direct`. The daemon checks each request it receives,
  and the CLI asks it before what a command does itself, never deciding a
  permission alone; an approved command's own requests take the actions of
  its key without asking again.
- Lifecycle states, resource scopes, approval targets and approval
  specifications are typed and reject unknown values.
- Problems show where the user looks (`inbox`, `alert list`, `status`, the failing
  command's error) and name the command that fixes them; repair preserves work
  and resource leases. Startup audits ownership and never deletes work, and the cleanup sweep readies a
  workspace the audit failed once its ownership verifies; the daemon clears an unknown
  execution only when a saved agent stop and a process scan prove it ended,
  its processes ran in an earlier boot, or it recorded no child or process group
  and its wrapper and every process of it are gone.
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
- The state directory holds only what Shoal keeps to run and to record its runs:
  only Shoal writes, changes or deletes anything there, and it never gives the
  user or an agent a file there to work with, though it may name its own logs
  there for reading. A file the user, an agent or a reviewer works with lives in
  the directory of the repository it belongs to, or in `root_dir` when it
  belongs to none, in a directory marked as Shoal's.
- A space's name is a free-text label that may repeat; it never selects a space
  and nothing is built from it. Accept literal Git branch names and derive a
  portable worktree directory name separately, suffixed while another workspace
  has it. Suffix conflicting branch components with `-2`, `-3`, etc.; keep the
  name and directory unchanged; serialize allocation per repo. Existing branches,
  including an issue's derived local branch, get unsuffixed worktrees or reopen
  owned ones; other checkouts require explicit adoption.
  Adopt only linked, unlocked worktree roots on local branches, preserving files and
  settings without setup; record identity and readiness atomically under normal cleanup.
  Reject ownership collisions and never use adoption to repair moved worktrees.
- `shoal diff` uses Git fork-point/merge-base against the recorded base branch;
  do not compare directly to today's main tip or a frozen commit after a rebase.
  Preserve native Git pager/external-diff configuration.

## Documentation

Keep documentation short and current. Say each thing once, where a reader looks for
it: usage in README.md, behavior in docs/reference.md, configuration options in
`crates/shoal-cli/src/cli/commands/configuration/reference.md`, which
`shoal config reference` prints, decisions in docs/design.md, contributor rules
here; a new document needs a subject none of those covers. The skill,
`crates/shoal-cli/src/cli/commands/skill/shoal.md`, says only what Shoal's
commands do; a practice goes in a user rule, and a rule about anything else,
such as how to write or structure code, goes here. This repository's own practices, such as how its agents
deliver and follow up a PR, go here and never into the rules and mode
instructions Shoal ships under `crates/shoal-core/src/config/`, which every
user's agents receive. Edit
the sentence that describes the changed behavior instead of appending a paragraph,
and cut text that restates code, tests, history, or another section. State a rule
as its condition, never as the list of things that satisfy it: the condition
survives a rename, while a list or a count goes stale silently, and an overstated
rule is narrowed, not annotated with its exceptions. Name something only when a
reader cannot find it from the rule. No milestone reports, inventories of files,
tests or flags, or investigation notes. Length is judged by these rules, not by a
line count. Documentation states what Shoal does and what it does not support,
never what has or has not been tested, run or verified; say what you could not
test in the PR.

## Validation

Run `cargo fmt --all --check`, `CARGO_BUILD_WARNINGS=deny cargo clippy
--all-targets`, and `cargo nextest run` for Rust changes, with `cargo test --doc` when a change touches
doc tests. `python3 scripts/affected_packages.py origin/main` names the packages
a branch's commits can affect; `cargo nextest run -E 'package(=NAME) | …'` runs
only their tests and, unlike `-p`, builds with the workspace's features, which
keeps the build's dependencies from rebuilding. Nextest runs every test in a process of its own, so a lock in a test
binary's memory keeps no tests apart: tests that must not overlap take a lock
other processes see, such as a file lock, and join a test group with
`max-threads = 1` in `.config/nextest.toml`. Test in temporary state
directories and repositories, without the launching environment's Shoal
variables, configuration locations or Git repository variables, or a
repository above the temporary directory, which a test binary that starts Git
leaves behind with `prepare_test_process!` at its root;
never install persistent OS services or modify real user workspaces as a side
effect of tests.
Run development builds only with a temporary `--state-dir`: the user's daemon
serves every running agent, so never stop, restart or reinstall it, and a debug
build refuses the user's state directory, and writes to the user's config
directory, unless `SHOAL_DEV_USER_FILES=1`. Code writes the global config and
the files beside it through `config_file`, and tests through
`test_support`, which refuse that directory in unit tests whatever
they inherit. Code that
changes user-level files outside the state directory checks
`build::changes_user_files`, so debug builds leave them to the installed Shoal.

A test exists to catch a behavior regression. Assert what the code computes or
decides: the values it puts into text, which outcome or refusal it chooses, the
state it leaves. Never assert prose the code spells out as a literal, such as a
prompt's, help text's or message's wording, beyond the fragment that tells one
outcome from another; never restate a constant, default or lookup table, test
what a derive, the language or a dependency guarantees, or repeat a path
another test covers. Persisted and protocol representations are behavior and
keep their tests.

A failing assertion shows a command's output through `test_support::printed`,
as text a reader of the log can follow, never with `{:?}`, which lists its bytes.

A test must not depend on runner speed. Never assert that work finishes within
a wall-clock duration or race it against a short real timeout. Assert the outcome
and the applied limit, drive time with a paused Tokio clock, or wait on an explicit
signal. Give limits that should not trigger generous values; test limits that
should trigger with work that cannot finish. Real timeouts are generous hang
guards only, never the behavior being asserted. A wait observes the state the
next step depends on, where that step reads it, and only a condition that
cannot hold before the awaited event: a file a child writes does not prove the
daemon recorded its start.

The harness runs tests in parallel but a test's cases one after another, so a
test must not outlast the others by looping over cases that each need their own
repository, workspace or daemon: split those cases into separate tests that
share a helper, and let cases that need no isolation share one fixture.

## Profiling

`python3 scripts/profile_shoal.py [--setup ARGS]... -- <shoal args>` times a
command against a foreground daemon in a temporary home and lists where the time
went; `--help` shows its options. It builds the `profiling` cargo profile,
release code with symbols for samplers such as samply. `SHOAL_PROFILE=1` makes a
Shoal process print a `profile` line to stderr for each daemon call, daemon
request, daemon startup step and subprocess run. It is a contributor tool, so the user documentation
leaves it out.

`python3 scripts/profile_tests.py suite` runs each test binary whole, one after
another, and prints its wall, user and system time; `tests --binary NAME` runs
every test of a binary alone, as nextest does, and lists the costliest. Both
count a test's daemons and commands, but not processes that outlive it, and
`--git` counts their Git processes, production code's apart from the fixtures'.

## CI

`.forgejo/workflows/ci.yml` runs on pull requests and pushes to `main`. Its
required checks are its `rust` job, which runs `cargo fmt --check`, clippy,
`cargo nextest run` with the `ci` profile, `cargo test --doc` and the Python
release-script tests, its `musl` job, which checks the build for the
host's musl target with cargo-zigbuild and runs the identity module's tests
there when a PR changes them, the dependencies or the workflow, and its
`crates` job, which checks that each crate builds alone. A PR's jobs
build, lint and test only when it changes a package or a file every package's
build reads, and run the tests of the packages `scripts/affected_packages.py`
names, those it changes and the packages that depend on them. The release
workflow's PR and version commit skip the checks:
`scripts/release.py` releases only on a `main` commit whose required checks
passed, and runs only formatting and Clippy on the version change. A draft PR runs no checks; taking it out of draft
(removing the `WIP:` title prefix, as `shoal ready` does) starts one run.
Its `rust` job and `flaky.yml`'s job, which build, run with `runs-on: shoal` on
Shoal runners; a job that gains nothing from the seed build below, such as one
that only queries the forge or builds with feature sets the seed lacks, runs
on Docker, as does the `musl` job, which needs the CI image's Zig and musl
targets. A Shoal runner
is any Linux or macOS machine whose Shoal has `runner.enabled` for this
repository, with Rust at `rust-version`, which the jobs check, with clippy, rustfmt and
cargo-nextest, which the jobs also check, and the tools the tests spawn installed on
the host (those `.forgejo/ci-image/Containerfile` installs for them; Shoal
runners need no Zig, cargo-zigbuild or musl targets). The test
fixtures' daemons commit SQLite with fsync under `TMPDIR`, which Shoal puts in
the repository directory; where that disk stalls on fsync, the machine's local
config sets `TMPDIR` in `[env]` to a tmpfs. Each job runs in a workspace at its
commit that starts from the `cargo` preset's seed build of `main`, so it has no
checkout step and rebuilds only what its commit changed; keep settings that
change Cargo's fingerprints out of the workflows. The runner sets `CI`, under
which Cargo leaves incremental compilation off, so those jobs set
`CARGO_INCREMENTAL=1` to build as the seed did.
Code that tests run must not keep a path the seed build compiled in, such as
`env!("CARGO_BIN_EXE_shoal")`; read it at run time.
The other jobs and workflows run on the Docker runner, in the image built from
`.forgejo/ci-image/Containerfile` (`git.henriknordvik.com/hn05/ci-shoal:<rust
version>`), so a run installs nothing: new tooling for them goes into the
Containerfile, and only the owner rebuilds it with `.forgejo/ci-image/build.sh`
on the runner host, then updates `workspace.package.rust-version` in `Cargo.toml`, the
Containerfile's `ARG RUST`, and the `image:` tag in every workflow, and the
Shoal runners' machines install the same Rust. The Docker jobs' cargo
builds share the runner-mounted `/ci-target` through
`.forgejo/scripts/lock-target-dir.sh`, as the Containerfile describes; the
`musl` and `crates` jobs keep their slots' builds warm, touching their
checkouts instead of cleaning.
`.forgejo/workflows/deps.yml` runs weekly (and on `shoal ci run deps.yml
--branch main`) and rewrites one evergreen `tracking` issue, found by the marker comment
in its body, with `cargo audit` and `cargo outdated` findings. It is never a
required check; findings do not fail it, a tool that could not run does.
`.forgejo/workflows/cross-build.yml` runs only on `shoal ci run cross-build.yml
--branch <branch>` and cross-builds every release target there.
`.forgejo/workflows/flaky.yml` runs `cargo nextest run` repeatedly on `main` every
night (and on `shoal ci run flaky.yml --branch main`) and rewrites the evergreen
`Flaky test report` issue with every test that failed; a failure fails the run.
The run log keeps each failed test's output for the issue that fixes it.
It is never a required check.
`.forgejo/workflows/review.yml` posts an advisory review-bot review when a PR
opens or leaves draft and whenever `review/default`, `review/claude` or
`review/codex` is added; `review/none` suppresses it. Pushing commits does not
request one; Committing below says which heads a review covers. A draft PR
gets no review; a request label added while it is a draft is answered when it
leaves draft.
`.review/review.md` is its project brief;
keep it current when a rule here or in `docs/design.md` changes.
Opening a ready PR already requests its review; never add reviewer request labels
with its opening classification labels. Claude and Codex use independent queues,
so one of each can run concurrently. Review and audit workflows follow review-bot's
latest `main` commit.
`.forgejo/workflows/audit.yml` runs a read-only code audit using
`.review/audit.md` over the entire application. It has no schedule and runs only
when dispatched manually, with a selected focus (bugs, maintainability, testing,
performance or docs) and ref.
It opens one issue per new finding, labelled `audit/<focus>`; findings are
advisory and never a required check. Close an audit issue when it is fixed, or
explain in a comment or with `audit/wontfix` why it will not be.
Issues and PRs carry one or more `area/` labels and one `type/` and one
`complexity/` label; `shoal repo labels` lists what each means, and
`.forgejo/scripts/labels.py` creates the scheme. `type/` names what the change
is for, wherever its code lives: work that makes the tests faster is
`type/test`. An issue states the problem or goal and what done looks like.
One whose approach is settled is labelled `planned`, and agents implement it
without asking; one that leaves a choice for
the owner is labelled `needs-plan` until `shoal start --mode plan` settles a Plan section in
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
- A PR closes every issue it fixes, one `Closes #n` line each. Before opening
  it, and again when work beyond the assigned issue lands in it, such as a
  failing test or a bug found along the way, search the open issues with
  `shoal issue ls <words>` for each such fix and add the reference for every
  issue it completes.
- An assigned issue that is already fixed needs no PR: verify the fix on the
  default branch, comment on the issue with the PR or commit that fixed it
  (`shoal issue comment --body-file`), then close it with `shoal issue close`
  and finish with `shoal done`.
- Before opening a PR, and before each later push, review your own diff against
  this file, function abstraction first. A review-bot finding that cites a rule
  stated here is one your own review missed.
- Deliver completed work as a PR on a topic branch: commit, push, open or update the
  PR with its `area/`, `type/` and `complexity/` labels, then watch CI and the
  review bot. Review findings are claims to verify against the code and its
  callers before fixing them or rejecting them with evidence in the thread.
  Agents cannot merge PRs; leave merging to the user instead of asking for
  permission to merge.
- Every push after the first review that changes what the PR does needs a new
  one: a push does not request it. After each push that fixes findings, adds
  commits or resolves merge conflicts by changing the PR's own code, run
  `shoal pr edit --add-label review/default`, wait for that review, and handle
  its findings. A rebase that only replays the PR's changes onto a newer base
  needs none: `git range-diff origin/main <reviewed> HEAD` shows its commits
  unchanged, or changed only in context lines or in conflicts resolved by
  keeping both sides. Run `shoal ready` only once the latest review covers the
  current head and comes back clean; green CI alone is not enough, nor is
  replying to or resolving every thread. A review covers the head whose SHA is
  its `commit_id` in
  `shoal forge api 'repos/{owner}/{repo}/pulls/<number>/reviews'`, and every
  head that only rebases it; check that before `shoal ready`.
- When your work is a PR, run `shoal done` only after it merges: until then,
  keep handling its reviews, CI results and merge conflicts. After it merges,
  watch the `ci.yml` run on `main` for its merge (`shoal ci ls --branch
  main`, `shoal view ci <run>`). When that run fails, open a PR that fixes
  `main`. You are finished once the run passes, or once another PR merges
  into `main` after yours, which makes `main` no longer yours to fix.

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
menu, listing or message, shows examples of the new interface, recorded with a
[VHS](https://github.com/charmbracelet/vhs) tape. Record it with
`python3 scripts/record.py <tape>`, which runs the tape against a development
build in a sandbox beside it, under `target/`, and stops every process the
recording started; `--help` shows how a setup script prepares the sandbox. Keep
the tape and its output under `target/`, and commit them only when a document
uses them.

The tape types the keys a user would and shows what each command prints. Leave
that output whole, without piping it through `grep`, `head` or the like, unless
the rest buries what the recording shows; then keep enough of it to read in
context. `Screenshot` captures a screen, `Output <file>.mp4` records a video,
never a GIF, and `Output <file>.txt` writes each frame as text. Pace a video so
the viewer can read each screen before the next key, about two to three seconds
on a screen and longer on a dense one, without idle stretches. VHS renders
headlessly, so look at the screenshots yourself before sharing them. Attach the
video and screenshots to the PR with `shoal pr comment --attach`, and put text
frames in code blocks.

For a change to try rather than watch, offer `python3 scripts/demo.py` as a
demo (`shoal demo start shoal -- python3 scripts/demo.py`): it runs a
development build against sample workspaces, threads and approvals in a
temporary state directory.
