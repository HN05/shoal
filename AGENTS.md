# Shoal

Read the files of `docs/design/` that cover your change for product decisions, and
`docs/design/overview.md` for the implementation order. Update them when behavior
changes, distinguishing decisions from proposals.

Read this whole file before you start, in as many reads as it takes: the rules
for delivering a PR, under Committing near the end, decide when work is done.

## Implementation

- Shoal is a Rust CLI and local daemon. Keep Superlogical, Macraft, and other
  caller-specific integration logic outside its core.
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
- A change that adds or extends a feature that coordinates or communicates
  implements it for chats too, through the same code path, or its PR says why
  it cannot apply to them. `docs/design/chats.md` says which features chats get.
- A space is a workspace or a chat. Flags, arguments, user-facing text and
  code call what applies to both a space (`--space`), and say workspace only
  for what needs a worktree. A command's flag becomes `--space` in the change
  that lets it act on a chat. Command names stay short: `shoal start` starts
  every agent, in a workspace or a chat, and a command that maintains a space
  is top-level, such as `shoal status` and `shoal rename`.
- Refuse a request only for a reason `docs/design/overview.md` accepts. Support
  any other request instead, or file an issue for it when that lies outside the
  change.
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
- `scripts/crate_dependencies.toml` lists the Shoal crates each crate may
  depend on, and CI's `scripts/check_crate_dependencies.py` refuses any other.
  A new crate or dependency gets an entry that follows the rules below.
- Forge requests live in `crates/shoal-forge`, and the forge data the
  protocol carries, with the logic that reads it without a request or the
  daemon's state, such as repositories, item URLs, check states, item
  snapshots and run IDs, in `crates/shoal-forge-types`, at the module path
  the type has in `shoal-forge`, which re-exports none of them and adds its
  requests on them as each module's extension trait. `shoal-forge` takes the
  account to act as, while scope, approvals, identity choice and workspace
  records stay in the daemon. What one forge does its own way belongs in its
  implementation of `Routes` in `shoal-forge-types`, where reading a URL
  needs it, or else of the trait of the `shoal-forge` module that covers it,
  such as `transport`'s `Backend` or `ci`'s `Runs`, which each forge
  implements in its module under `backends`, never in a match on the forge
  kind; no other module names `backends`, and reading an issue, PR or run
  by its kind goes through `ForgeRepo`'s one operation for it.
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
  carries without reading, lives in `shoal-core`.
- `crates/shoal-schedule` holds the agent queue's order: priorities, the
  rank types that explain an entry's place, the snapshot a policy reads, the
  `Scheduler` trait and the built-in policies. A policy reads only that
  snapshot, which the daemon builds from its state, and never the state
  itself; admission stays in the daemon.
- `crates/shoal-store` holds the daemon's `Store`, its schema, its
  migrations and the row mappers and guards every subsystem shares; a
  subsystem's own record functions stay with it. The store migrates from the
  schema of the release its floor names, and raising the floor deletes the
  migrations at or below it with their tests.
- `crates/shoal-daemon-base` holds what more than one subsystem of the
  daemon uses. What it needs of a subsystem is an interface it declares, a trait whose methods
  return a `Send` `BoxFuture`, which the daemon implements; its code is no
  method of `Manager`.
- A subsystem moved out of the daemon is a crate of its own, such as
  `crates/shoal-leases` for ports, resource permits, simulators, containers
  and secrets, `crates/shoal-reviews` for local reviews, the gate agent
  and the one-shot agent runs they start, `crates/shoal-hub` for the hub
  role and membership of a hub, `crates/shoal-runner` for triggers and
  the CI jobs this machine runs, `crates/shoal-links` for the issues,
  PRs and CI runs spaces link, `crates/shoal-repositories` for the
  registered repositories, their config layers, seeds and caches,
  `crates/shoal-agents` for the agents the daemon runs,
  `crates/shoal-apps` for the settings and the store of apps built on Shoal,
  `crates/shoal-demos` for demos and their viewers,
  `crates/shoal-coordination` for alerts, threads and the changes made for
  them, approval requests, permissions and the scope of scope tokens, and
  `crates/shoal-spaces` for the workspaces and chats themselves: their
  records and tree, worktrees, lifecycle, removal and cleanup, restores,
  conversions, moves and launches.
  Its service, a field of `Manager`, holds what the subsystem
  keeps in memory and offers its operations; what it needs of other
  subsystems is an interface it declares, as the base does, which the daemon
  implements and passes each operation, except what runs in the subsystem's
  own transaction, a synchronous method over it that the service keeps. Its
  tests build the service over a temporary store and stand-ins for those
  interfaces; a test across subsystems stays in `shoal-daemon`. A subsystem
  crate depends on no other, except that `shoal-spaces` calls coordination's
  service directly, which its interface gives with the daemon coordination
  takes.
- The CLI lives in `crates/shoal-cli` and the daemon in `crates/shoal-daemon`,
  neither of which depends on the other, so an edit in one rebuilds neither
  the other nor its tests. Nor does the daemon depend on a crate that calls
  the daemon, even as a dev-dependency: a test that drives a daemon through
  such a crate lives in that crate's tests and starts the daemon through its
  `test-support` feature. The `shoal` package is only `main`,
  which runs the CLI and passes it the daemon for `shoal daemon run`. Code both use, directly
  or through code they call, lives in `crates/shoal-core`, in a top-level
  module named for what it does; it names neither the CLI nor the daemon
  and makes no daemon calls. Its items are `pub` only where another Shoal
  crate uses them, and fixtures the other crates' tests share sit behind its `test-support` feature.
- `crates/shoal-cli` serves only the CLI. Code that the daemon, the execution
  wrapper, the proxy or the workers use lives outside it, in `shoal-workers`
  where it calls the daemon.
- `crates/shoal-workers` holds the execution wrapper and the workers: the
  code that runs in the processes Shoal starts and calls the daemon, which the
  CLI reaches there too. Neither core nor the daemon's own code depends on
  it. Where core logic needs a value the daemon records, its
  caller reads it and passes core the value, never a callback that calls the
  daemon.
- A scope token is a `ScopeToken`, whose `Debug` redacts it, never a bare
  string; code reads its text with `expose` only where it sends it. The daemon
  decides what a caller may do, and which account it acts as, from its
  `Option<&Caller>`, `None` for the user, never from a `bool` saying whether it
  is scoped.
- The daemon checks the permission of each request it receives. The CLI asks
  the daemon before what a command does itself, and never decides a permission
  alone.
- Lifecycle states, resource scopes, approval targets and approval
  specifications are typed and reject unknown values.

## Documentation

Documentation and comments say what the code cannot. Keep them short and
current.

### Where things go

- `docs/design/` explains the big picture: the architecture, how the
  subsystems fit together, and each decision with its reason. It has one file
  per subsystem. Where a crate holds a subsystem, its file covers that crate.
  `docs/design/overview.md` states Shoal's purpose and boundaries, the
  principles of its architecture and the core terms.
- `docs/reference/` says what each command does. It has one file per command
  group.
- The configuration reference describes each option. It lives in
  `crates/shoal-cli/src/cli/commands/configuration/reference.md`, which
  `shoal config reference` prints.
- README.md shows how to use Shoal.
- AGENTS.md holds the rules for working in this repository.
- `docs/ci.md` covers how CI runs, and `docs/profiling.md` the tools that
  measure where time goes.
- `.review/review.md` briefs the review bot. Keep it current when a rule in
  AGENTS.md or `docs/design/` changes.
- The skill, `crates/shoal-cli/src/cli/commands/skill/shoal.md`, says what
  Shoal's commands do. A practice for using them goes in a user rule. A rule
  about anything else, such as how to write code, goes in AGENTS.md.
- Shoal ships rules and mode instructions under `crates/shoal-core/src/config/`,
  and every user's agents receive them. Put this repository's own practices,
  such as how its agents deliver a PR, in AGENTS.md instead.
- State each fact in one place. Other places link to it.
- Open each file with a sentence that says what it covers. There is no index
  file.
- Start a new document only for a subject that none of these covers.

### Comments

- A comment explains a detail of the code beside it that the code cannot show:
  why it takes a non-obvious approach, an invariant, a contract a caller must
  keep, or a workaround and its cause. When there is no such detail, write no
  comment.
- Architecture, how modules relate and product behavior belong in
  `docs/design/`. A comment links to the design file instead of describing
  them.
- A comment adds what the code and its signature do not say. For example,
  write `// Record the claim before simctl runs: a crash must leave the device
  owned.` rather than `// Record the claim.`
- A module's `//!` names its responsibility in one or two sentences. It adds a
  contract only when that contract applies to the whole module.

### Writing

- Write short sentences with one idea each. Put the subject first and use the
  active voice.
- Use plain, common words. Use the same term for the same concept everywhere.
  `docs/design/overview.md` defines the core terms.
- State a rule as what to do. Use "never" only for a hard limit.
- State the rule or principle, not an inventory: no file trees, no lists of
  crates, flags or tests, and no counts. A sentence must stay true after a
  rename or an addition. When something must be listed, such as each crate's
  allowed dependencies, put the list in a file that CI checks.
- Name something only when a reader cannot find it from the rule. Narrow a
  rule that says too much instead of listing its exceptions.
- When behavior changes, edit the sentence that states it. When that sentence
  would then carry a second idea, split it instead of adding a clause.
- Give an example where it makes a rule concrete.
- Give each section one topic. Split a section when it gains another.
- State what Shoal does and what it does not support. Leave out history,
  investigation notes and what was tested, run or verified; the PR says what
  you could not test.
- Judge length by these rules, not by a line count.

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
Simulator tests use an isolated xcrun fixture and never touch the user's own
simulator devices.
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

Code that tests run reads paths at run time, never one the build compiled in,
such as `env!("CARGO_BIN_EXE_shoal")`: CI starts from a seed build made in
another directory.

## Profiling

[docs/profiling.md](docs/profiling.md) covers the tools that time Shoal's
commands and its tests.

## CI

[docs/ci.md](docs/ci.md) covers the runners, the CI image and the workflows.

A PR description follows `.forgejo/pull_request_template.md`. Push the branch
and let CI verify it instead of running the full suite locally first.

### A failed check

A failed check is a defect even when a rerun passes. Read its log, then fix the
cause in its own commit. When it lies outside the change, first rebase onto the
current `main`, which often has the fix, and let CI run again. When it still
fails and `main`'s own run (`shoal ci ls --branch main`) fails the same way, the
break is `main`'s, and gets one issue:

1. Under `shoal acquire resource main-broken --queue`, which a chat goes
   without, take the issue `shoal issue ls --label main-broken` lists, or
   else open one with that label, the failing test and a log excerpt; then
   release the lease.
2. After opening one, list them again: when an older one is open, close yours
   as its duplicate with a comment linking it, so that every agent, on any
   machine, ends on the oldest.
3. Block on the issue you end on with `shoal progress --add-blocker <issue>
   --reason "main is broken"`: Shoal stops you and restores you once it
   closes, to rebase and rerun.

Any other failure outside the change gets an issue of its own with the failing
test and log excerpt. Rerun a failed check only once its fix or issue exists,
never instead of them.

A check that was cancelled rather than failed, whose log names no failing step
or command and ends `context canceled`, is no defect: people and Shoal stop
jobs on purpose, and Shoal reruns those it cut off once their run has finished.
Rerun one that is still cancelled after its run finished, and file nothing for
it. A log that names a command killed with SIGKILL, such as `rustc`, records a
failure.

## Issues

Issues and PRs carry one or more `area/` labels, one `type/` label and one
`complexity/` label. `shoal repo labels` lists what each means. `type/` names
what the change is for, wherever its code lives: work that makes the tests
faster is `type/test`.

An issue states the problem or goal, what was observed of it, such as a
command and its output, and what done looks like. It leaves the approach to
the Plan section the owner settles: a fix written when the issue is filed goes
stale, and whoever takes it on reads the code as it is then. An issue whose
approach is settled is labelled `planned`, and agents implement it without
asking. One that leaves a choice for the owner is labelled `needs-plan` until
`shoal start --mode plan` settles a Plan section in its body. Issues with
neither label, such as tracking and meta issues, stay out of that flow.

Comment on an issue only to add what changes its work, such as a new cause,
failure mode or fix. Post neither another sighting of a failure it already
describes nor a status its watcher already gets from Shoal, such as a PR
opening or CI passing. To agree with an issue or a comment, react to it with
`shoal issue react`, which reaches no watcher.

Close an audit issue when it is fixed, or explain in a comment or with
`audit/wontfix` why it will not be.

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
- The review bot reviews a PR when it opens ready or leaves draft, and
  whenever `review/default`, `review/claude` or `review/codex` is added;
  `review/none` suppresses it. A request label added to a draft is answered
  when the PR leaves draft. Opening a ready PR already requests its review, so
  open it with its classification labels alone.
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
- When your work is a PR, run `shoal done` only after it merges and Shoal
  has reported the `ci.yml` run on `main` for its merge: until then, keep
  handling its reviews, CI results and merge conflicts. You are finished once
  the run passes. When it fails, open or join its `main-broken` issue at
  once, by steps 1 and 2 under A failed check, link it with `shoal link issue <n>
  assigned` and open the PR that fixes `main` with `Closes #<n>`; you are
  then finished only once the issue closes, since a done workspace that
  links it clears every agent's blocker on it. When merges cross and the
  runs of both fail, each merging agent does this, which puts them on one
  issue that the first fix closes; the others then close their fix PRs.

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
