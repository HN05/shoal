# Reviewing Shoal

This brief says how to review a pull request to Shoal. It states no rule of its
own: `AGENTS.md` holds the rules for writing, testing and documenting code, and
`docs/design/` the product decisions, one file per subsystem. A code audit
follows [audit.md](audit.md) instead.

## Read the rules first

- Read `AGENTS.md` whole before the diff. Its Implementation section governs
  the code, Validation the tests and Documentation the documents and comments.
- Read `docs/design/overview.md` for Shoal's purpose, its architecture and
  the core terms, and for the reasons it accepts for refusing a request.
- For each subsystem the diff touches, read that subsystem's file in
  `docs/design/`. Each file opens with a sentence that says what it covers.
  Check the change against every decision the file states, not only those the
  PR names.
- Read the code around each change and its callers before you judge it.

## What to check

Check the diff in this order, which is also the order of weight.

1. **Function abstraction.** Apply `AGENTS.md`'s rule that every function
   stays at one abstraction level. A violation blocks the PR even when the
   behavior is correct and the tests pass. Judge the responsibility boundary,
   not the length or the number of helpers.
2. **Behavior against its design file.** Find what the change does that its
   design file decides, and check that the code does that. When they differ,
   either the code is wrong or the PR must edit the sentence that states the
   decision; report which. A change in behavior without that edit is a
   finding too.
3. **User work and recorded ownership.** Give most weight to a change that
   could lose or damage the user's work, signal a process whose identity was
   not verified, break the ownership Shoal records, release a lease early or
   corrupt the store. Follow its failure, cancellation and restart paths. A
   new path that deletes a directory, stops a process or changes a simulator
   without the checks its design file requires blocks the PR.
4. **The code rules.** Check the rest of `AGENTS.md`'s Implementation
   section: compatibility, daemon code, Git, crates and the command line.
   Check an entry the diff adds to `scripts/crate_dependencies.toml` against
   the Crates rules, not only against the CI check that reads it.
5. **Scope and permissions.** The daemon decides what a caller may do, and
   `docs/design/permissions.md` says what each caller may do. Scope is
   cooperative, not a security boundary: judge a change by whether it keeps
   that cooperation, not by whether a hostile process could get around it.
6. **Chats.** A change that adds or extends a feature that coordinates or
   communicates needs the chat case, as `AGENTS.md` and
   `docs/design/chats.md` state, or a PR that says why it cannot apply.
7. **Tests.** Check them against `AGENTS.md`'s Validation section: isolation,
   time, test size and what a test asserts. A test that cannot catch a
   behavior regression is a finding.
8. **Documentation.** Check changed documents and comments against
   `AGENTS.md`'s Documentation section, including where each fact belongs.
9. **Delivery.** Check the PR description against `AGENTS.md`'s Pull
   requests and Terminal recordings rules.

These `AGENTS.md` guidelines are advisory, not blocking: one responsibility
per module, tests in a sibling `tests.rs`, and integrations that implement
callbacks for what they do differently.

## How to report

- Anchor each finding to the file and line it concerns.
- Name the rule the code breaks and where it is stated: an `AGENTS.md`
  section or a file in `docs/design/`. A finding no stated rule backs is a
  bug report: give the input or state, and the wrong result it leads to.
- Say whether the finding blocks the PR or is advisory.
- Report one finding per cause. When the same cause recurs in the diff, name
  each place in that one finding.
- Report only what you checked in the code. Say what you could not check
  instead of guessing.
- When a design file or `AGENTS.md` is unclear about a case the diff meets,
  say so in the finding, so the rule can be made clearer.

## Do not bother with

- Formatting and import order, which rustfmt and Clippy check in CI.
- A restatement of what the change does.
- A style preference that no rule in `AGENTS.md` backs.
- The commit history: how the PR splits into commits, their sizes and their
  messages.
- A comment on clear code, a rewording of a correct comment, or a longer
  document.
- An old representation a change drops: Shoal promises earlier versions no
  compatibility, as `AGENTS.md` states.
