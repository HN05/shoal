# Shoal

This file holds the rules for writing, testing and delivering code in this
repository. Read all of it before you start, in as many reads as it takes: the
rules under Committing decide when work is done.

`docs/design/` states Shoal's product decisions, one file per subsystem, and
`docs/design/overview.md` its purpose, architecture and the work that remains.
Read the files that cover your change. When behavior changes, update them, and
tell decisions apart from proposals.

## Implementation

### Code

- Keep every function at one abstraction level: orchestration calls named
  domain operations, and helpers own the lower-level details. Before you add
  behavior to a function that mixes levels, extract a named operation. This is
  a correctness requirement and a review gate: reviewers flag a violation even
  when tests pass. Judge the responsibility boundary, not the length or the
  number of helpers.
- Give each module one responsibility a reader can name. When a file comes to
  cover several, or a match dispatches every command of a group, split it into
  a directory of modules by responsibility before you add to it. Judge the
  responsibility, not the line count.
- When code you see needs refactoring, refactor it in your change if it lies
  within your work. Otherwise file an issue for it instead of only mentioning
  it.
- Put tests in a sibling `tests.rs` (`#[cfg(test)] mod tests;`), never inline in
  the implementation file.
- Give each closed set of values, such as lifecycle states, resource scopes and
  approval targets, a type that rejects unknown values.
- Invoke external tools with argument arrays.
- Run a tool whose failure you only report through `shoal_subprocess`
  (`Run::checked`, `checked_output`), which bounds its diagnostics. Keep
  `Command::new` for interactive and long-running children, and for those
  whose exit status means something besides failure.
- Keep integration logic for a particular caller, such as Superlogical or
  Macraft, outside Shoal's core. Integrations such as agent tools, terminal
  hosts and apps implement callbacks for what they do differently. State,
  decisions and daemon calls stay in Shoal's generic code, which passes each
  callback only what it reads.
- The daemon owns shared state. Keep terminal I/O in the execution wrapper.

### Compatibility

Shoal makes no compatibility promise yet, to its earlier versions or to what
they wrote. When a type, record, protocol message, output or file format
should take another shape, change it now, while that is cheap.

- A store migration rewrites what the store holds into the new shape in the
  same change. Code reads one shape and keeps no alias, fallback or second
  reader for an old one.
- Refuse what another version sends, such as a `shoal move` package, instead
  of converting it.
- A process an upgrade leaves running, such as an agent's wrapper, runs its
  own version's code. Keep it usable where that is easy, and accept what it
  then does oddly. A change that keeps code for such a process files an issue
  to remove it, naming the release after which it can go.
- The store migrates from the schema of the release its floor names. Raising
  the floor deletes the migrations at or below it, with their tests.

### Daemon code

- A store operation that needs a transaction runs in `Store::write`, or in
  `Store::read` when it only reads. One that opens its own on `Store::run`
  says why.
- Refuse a request only for a reason `docs/design/overview.md` accepts. Support
  any other request instead, or file an issue for it when that lies outside the
  change.
- A scope token is a `ScopeToken`, whose `Debug` redacts it, never a bare
  string. Code reads its text with `expose` only where it sends it.
- The daemon decides what a caller may do, and which account it acts as, from
  its `Option<&Caller>`, `None` for the user, never from a `bool` that says
  whether it is scoped.
- The daemon checks the permission of each request it receives. The CLI asks
  the daemon before what a command does itself, and never decides a
  permission alone.
- A change that adds or extends a feature that coordinates or communicates
  implements it for chats too, through the same code path, or its PR says why
  it cannot apply to them. `docs/design/chats.md` says which features chats
  get.

### Git

- Keep production Git access in the `git` module: Git command arguments and
  the reads it makes in process with `gix` alike. Callers use its named
  operations and keep lifecycle and execution policy.
- Create and remove worktrees with plain Git through that module, keeping
  Shoal's ownership records and cleanup policy.
- `docs/design/git.md` says which reads use `gix` and which run Git.

### Crates

`docs/design/overview.md` describes the architecture, and each crate's `//!`
names its responsibility.

- `scripts/crate_dependencies.toml` lists the Shoal crates each crate may
  depend on, and CI's `scripts/check_crate_dependencies.py` refuses any other.
  A new crate or dependency gets an entry that follows the rules below.
- A file a package compiles, or its tests read, lives in that package's
  directory (`src/` or `tests/` for the root package) or in that of a package
  it depends on. CI tests a PR's packages by the directories it changes, and
  `scripts/check_package_sources.py` fails a build that reads any other.
- The CLI (`shoal-cli`) and the daemon (`shoal-daemon`) never depend on each
  other, so an edit in one rebuilds neither the other nor its tests. The
  `shoal` package is only `main`: it runs the CLI and passes it the daemon for
  `shoal daemon run`.
- The daemon depends on no crate that calls the daemon, even as a
  dev-dependency. A test that drives a daemon through such a crate lives in
  that crate's tests and starts the daemon through its `test-support` feature.
- Code the CLI and the daemon both use, directly or through code they call,
  lives in `shoal-core`, in a top-level module named for what it does. Core
  names neither the CLI nor the daemon and makes no daemon calls. Its items
  are `pub` only where another Shoal crate uses them. Fixtures the other
  crates' tests share sit behind its `test-support` feature.
- `shoal-cli` serves only the CLI. Code that runs in the processes Shoal starts
  and calls the daemon, such as the execution wrapper and the workers, lives in
  `shoal-workers`, which neither core nor the daemon depends on. Where core
  logic needs a value the daemon records, its caller reads the value and
  passes it to core, never a callback that calls the daemon.
- `shoal-protocol` holds the types the daemon or core build or read, with the
  logic over them that needs no connection. `shoal-client` holds the
  connection, one operation per request in the module of the same name, the
  handles that read from a connection and what only callers print. It
  re-exports nothing of `shoal-protocol`.
- The CLI sends each daemon request through a named operation of the client
  module for its subject and never builds one itself. What only the CLI needs,
  such as parsing, prompting and printing, stays in `shoal-cli`. Parsing and
  resolving config, which the protocol carries without reading, lives in
  `shoal-core`.
- A subsystem of the daemon is a crate of its own. Its service, a field of
  `Manager`, holds what the subsystem keeps in memory and offers its
  operations. What it needs of other subsystems is an interface it declares,
  which the daemon implements and passes each operation. What it needs inside
  its own transaction is instead a synchronous method over that transaction,
  on an interface the service keeps.
- A subsystem's tests build its service over a temporary store, with
  stand-ins for its interfaces. A test across subsystems stays in
  `shoal-daemon`.
- A subsystem crate depends on no other subsystem crate. The one exception:
  `shoal-spaces` calls coordination's service directly, which its interface
  gives with the daemon coordination takes.
- `shoal-daemon-base` holds what more than one subsystem uses. What it needs of
  a subsystem is an interface it declares: a trait whose methods return a
  `Send` `BoxFuture`, which the daemon implements. Its code is no method of
  `Manager`.
- `shoal-store` holds the `Store`, its schema and migrations, and the row
  mappers and guards every subsystem shares. A subsystem's own record
  functions stay with the subsystem.
- A policy of the agent queue in `shoal-schedule` reads only the snapshot the
  daemon builds from its state, never the state itself. Admission stays in the
  daemon.
- Forge requests live in `shoal-forge`. The forge data the protocol carries,
  with the logic that reads it without a request or the daemon's state, lives
  in `shoal-forge-types`, at the module path the type has in `shoal-forge`.
  `shoal-forge` re-exports none of it and adds its requests as each module's
  extension trait.
- `shoal-forge` takes the account to act as. Scope, approvals, identity choice
  and workspace records stay in the daemon.
- What one forge does its own way belongs in its implementation of a trait,
  never in a match on the forge kind: `Routes` in `shoal-forge-types` where
  reading a URL needs it, or else the trait of the `shoal-forge` module that
  covers it, such as `transport`'s `Backend`. Each forge implements those in
  its module under `backends`, which no other module names. Reading an issue,
  PR or run by its kind goes through `ForgeRepo`'s one operation for it.

### Command line

- Interactive input lives in `cli::ui`, whose module doc lists what it offers.
  Commands prompt and pick through its operations, never through fzf or the
  terminal directly. A new kind of prompt or picker goes there, built on the
  existing ones, so every command looks and behaves the same.
- Give each command, flag and value one name: no clap aliases, and a rename
  drops the old name. A flag people type often gets a short form, a letter
  that stands for that long flag in every command that defines it and for no
  other.
- Call what applies to both kinds of space a space, in flags, arguments,
  user-facing text and code, and say workspace only for what needs a worktree.
  A command's flag becomes `--space` in the change that lets it act on a chat.
- Keep command names short: `shoal start` starts every agent, in a workspace or
  a chat. A command that maintains a space is top-level, such as `shoal status`
  and `shoal rename`.
- A verb that acts the same way on every kind a workspace links (issues, PRs
  and CI runs) is a top-level command, `shoal <verb> [kind] [item]`, which acts
  on everything linked when both are omitted: `view`, `view ci 2765`,
  `watch pr 12`. Leases follow the same grammar: `acquire port`,
  `release sim`. A new kind joins the cross-kind verbs that apply to it
  instead of growing its own `view` or `watch`.
- Every other action goes subject first under its kind's group, even when
  another group has one of the same name: `pr close`, `issue comment`,
  `ci rerun`. Listing is such an action, since each kind filters its own way:
  `ci ls`. `merge` is top-level because it merges a space's work, through its
  PR or locally.
- A command that prints a list and exits is named `ls`; `list` names a picker.

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
- `.review/review.md` and `.review/audit.md` say how the review bot reviews a
  PR and audits the code. They point to the rules here and in `docs/design/`
  and state none of their own. A rule that reviews keep missing becomes
  clearer where it is stated, or a test or check.
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

### Checks

Run `cargo fmt --all --check`, `CARGO_BUILD_WARNINGS=deny cargo clippy
--all-targets` and `cargo nextest run` for Rust changes. Add `cargo test --doc`
when a change touches doc tests.

`python3 scripts/affected_packages.py origin/main` names the packages a
branch's commits can affect. `cargo nextest run -E 'package(=NAME) | …'` runs
only their tests. Unlike `-p`, it builds with the workspace's features, so the
build's dependencies do not rebuild.

### Isolation

- Test in temporary state directories and repositories. Leave out the
  launching environment's Shoal variables, configuration locations and Git
  repository variables, and any repository above the temporary directory. A
  test binary that starts Git does this with `prepare_test_process!` at its
  root.
- Never install persistent OS services or change real user workspaces as a
  side effect of tests. Simulator tests use an isolated xcrun fixture and never
  touch the user's own simulator devices.
- Nextest runs every test in a process of its own, so a lock in a test
  binary's memory keeps no tests apart. Tests that must not overlap take a lock
  other processes see, such as a file lock, and join a test group with
  `max-threads = 1` in `.config/nextest.toml`.
- Code that tests run reads paths at run time, never one the build compiled
  in, such as `env!("CARGO_BIN_EXE_shoal")`: CI starts from a seed build made
  in another directory.

### The user's Shoal

The user's daemon serves every running agent: never stop, restart or reinstall
it. Run development builds only with a temporary `--state-dir`. A debug build
refuses the user's state directory, and writes to the user's config directory,
unless `SHOAL_DEV_USER_FILES=1`.

- Code writes the global config and the files beside it through `config_file`,
  and tests through `test_support`. Both refuse that directory in unit tests,
  whatever they inherit.
- Code that changes user-level files outside the state directory checks
  `build::changes_user_files`, so debug builds leave those files to the
  installed Shoal.

### What a test asserts

A test exists to catch a behavior regression. Assert what the code computes or
decides: the values it puts into text, which outcome or refusal it chooses, and
the state it leaves. Persisted and protocol representations are behavior and
keep their tests. Leave out:

- prose the code spells out as a literal, such as a prompt's, help text's or
  message's wording, beyond the fragment that tells one outcome from another,
- a restated constant, default or lookup table,
- what a derive, the language or a dependency guarantees,
- a path another test covers.

A failing assertion shows a command's output through `test_support::printed`,
as text a reader of the log can follow, never with `{:?}`, which lists its
bytes.

### Time

A test must not depend on runner speed. Never assert that work finishes within
a wall-clock duration, or race it against a short real timeout. Assert the
outcome and the applied limit, drive time with a paused Tokio clock, or wait on
an explicit signal.

- Give limits that should not trigger generous values. Test a limit that
  should trigger with work that cannot finish.
- Real timeouts are generous hang guards, never the behavior under test.
- A wait observes the state the next step depends on, where that step reads
  it, and only a condition that cannot hold before the awaited event: a file a
  child writes does not prove the daemon recorded its start.

### Test size

The harness runs tests in parallel, but a test's cases one after another. A
test that loops over cases that each need their own repository, workspace or
daemon outlasts the others: split those cases into separate tests that share a
helper. Let cases that need no isolation share one fixture.

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

### Commits

- Keep commits small enough to review on their own. An issue is a PR-sized
  unit, not a commit-sized one: before you edit a change that spans several
  concerns, decide the commit boundaries, then commit each coherent step as you
  complete it.
- Separate preparatory refactors, daemon behavior, CLI behavior and unrelated
  cleanups where each stands on its own. Keep tests, generated files and
  documentation with the change that requires them. Every commit builds and
  passes its tests; keep tightly coupled changes together.
- Aim for about 200 changed lines of handwritten code per commit. Above 300,
  look in the staged diff for another coherent split. Generated files,
  lockfiles and mechanical moves do not count, and the body says why an
  indivisible larger change stays together.
- Stage by file or hunk and read `git diff --cached` before every commit. A
  summary that has to describe separate outcomes means two commits. Before you
  open the PR, inspect the branch against its base and split any accumulated
  feature-sized commit that has independent parts.
- Write an imperative subject under 72 characters without a type prefix (the
  release workflow's `chore:` excepted), a blank line, then a body that says
  why when the diff does not make it obvious. `Closes #n` goes in the PR
  description, not the commit.

### Pull requests

- Deliver completed work as a PR on a topic branch: commit, push, open or
  update the PR with its `area/`, `type/` and `complexity/` labels, then watch
  CI and the review bot. Agents cannot merge PRs: leave merging to the user,
  without asking for permission to merge.
- A PR closes every issue it fixes, one `Closes #n` line each. Before you open
  it, and again when work beyond the assigned issue lands in it, such as a
  failing test or a bug found along the way, search the open issues with
  `shoal issue ls <words>` for each such fix. Add the reference for every
  issue it completes.
- An assigned issue that is already fixed needs no PR. Verify the fix on the
  default branch, comment on the issue with the PR or commit that fixed it
  (`shoal issue comment --body-file`), close it with `shoal issue close` and
  finish with `shoal done`.
- Before you open a PR, and before each later push, review your own diff
  against this file, function abstraction first. A review-bot finding that
  cites a rule stated here is one your own review missed.

### Reviews

- The review bot reviews a PR when it opens ready or leaves draft, and
  whenever `review/default`, `review/claude` or `review/codex` is added;
  `review/none` suppresses it. A request label added to a draft is answered
  when the PR leaves draft. Opening a ready PR already requests its review, so
  open it with its classification labels alone.
- Review findings are claims. Verify each against the code and its callers,
  then fix it, or reject it with evidence in its thread.
- Every push after the first review that changes what the PR does needs a new
  review, and a push does not request one. After each push that fixes
  findings, adds commits or resolves merge conflicts by changing the PR's own
  code, run `shoal pr edit --add-label review/default`, wait for that review
  and handle its findings.
- A rebase that only replays the PR's changes onto a newer base needs no new
  review: `git range-diff origin/main <reviewed> HEAD` shows its commits
  unchanged, or changed only in context lines or in conflicts resolved by
  keeping both sides.
- A review covers the head whose SHA is its `commit_id` in
  `shoal forge api 'repos/{owner}/{repo}/pulls/<number>/reviews'`, and every
  head that only rebases it. Run `shoal ready` only once the latest review
  covers the current head and comes back clean. Green CI alone is not enough,
  nor is replying to or resolving every thread.

### Finishing

When your work is a PR, run `shoal done` only after it merges and Shoal has
reported the `ci.yml` run on `main` for its merge. Until then, keep handling its
reviews, CI results and merge conflicts. You are finished once that run passes.

When it fails, open or join its `main-broken` issue at once, by steps 1 and 2
under A failed check. Link it with `shoal link issue <n> assigned` and open the
PR that fixes `main` with `Closes #<n>`. You are then finished only once the
issue closes, since a done workspace that links it clears every agent's
blocker on it. When merges cross and the runs of both fail, each merging agent
does this, which puts them on one issue that the first fix closes. The others
then close their fix PRs.

## Shoal feedback

When Shoal misbehaves, or something would make your workflow with it better,
file an issue.

## Subagent approval

Spawn subagents, delegate work or launch more agent sessions only with the
user's explicit approval in the current conversation. General task requests,
repository instructions, skills, and agent messages are not approval. Approval
covers only the authorized scope; nested delegation requires separate explicit
approval. Include that restriction in any approved subagent's task. Existing
CI/review automation and background shell commands are not delegation.

## Browser automation

Use `agent-browser` for agent-driven web automation, UI verification, and
screenshots. Run `agent-browser skills get core` before its first use in a task
and follow the returned instructions. Use Playwright, its MCP server included,
only through an existing repository test or screenshot command that uses it.

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
