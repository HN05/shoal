# CI

This file covers the machines and images Shoal's CI runs on and the workflows
in `.forgejo/workflows/`. AGENTS.md says what to do when a check fails.

## Required checks

`ci.yml` runs on pull requests and pushes to `main`. Its required checks are
three jobs:

- `rust` runs `cargo fmt --check`, Clippy, `cargo nextest run` with the `ci`
  profile, `cargo test --doc` and the Python release-script tests.
- `musl` checks the build for the host's musl target with cargo-zigbuild. It
  runs the identity module's tests there when a PR changes them, the
  dependencies or the workflow.
- `crates` checks that each crate builds alone.

A PR's jobs build, lint and test only when it changes a package or a file
every package's build reads. They run the tests of the packages
`scripts/affected_packages.py` names: those the PR changes and the packages
that depend on them.

A draft PR runs no checks. Taking it out of draft starts one run: `shoal ready`
does this by removing the `WIP:` title prefix.

The release workflow's PR and version commit skip the checks.
`scripts/release.py` releases only a `main` commit whose required checks
passed, and runs only formatting and Clippy on the version change.
[releases.md](releases.md) says how to make a release.

## Shoal runners

The `rust` job and `flaky.yml`'s job run with `runs-on: shoal` on Shoal
runners, because they gain from the seed build below. A Shoal runner is any
Linux or macOS machine whose Shoal has `runner.enabled` for this repository.
It needs:

- Rust at `rust-version`, with Clippy, rustfmt and cargo-nextest. The jobs
  check each of these.
- The tools the tests spawn, installed on the host. They are the tools
  `.forgejo/ci-image/Containerfile` installs for the tests. Shoal runners need
  no Zig, cargo-zigbuild or musl targets.

The test fixtures' daemons commit SQLite with fsync under `TMPDIR`, which Shoal
puts in the repository directory. Where that disk stalls on fsync, the
machine's local config sets `TMPDIR` in `[env]` to a tmpfs.

Each job runs in a workspace at its commit that starts from the `cargo`
preset's seed build of `main`. It has no checkout step and rebuilds only what
its commit changed. Keep settings that change Cargo's fingerprints out of the
workflows. The runner sets `CI`, under which Cargo turns incremental
compilation off, so those jobs set `CARGO_INCREMENTAL=1` to build as the seed
did.

## Docker runner

Jobs that gain nothing from the seed build run on the Docker runner. Examples
are jobs that only query the forge, or build with feature sets the seed lacks.
The `musl` job runs there too, since it needs the image's Zig and musl targets.

The Docker jobs run in the image built from `.forgejo/ci-image/Containerfile`,
`git.henriknordvik.com/hn05/ci-shoal:<rust version>`, so a run installs
nothing. New tooling for them goes into the Containerfile. Only the owner
rebuilds the image, with `.forgejo/ci-image/build.sh` on the runner host. The
same change then updates:

- `workspace.package.rust-version` in `Cargo.toml`,
- the Containerfile's `ARG RUST`,
- the `image:` tag in every workflow.

The Shoal runners' machines install the same Rust.

The Docker jobs' Cargo builds share the runner-mounted `/ci-target` through
`.forgejo/scripts/lock-target-dir.sh`, as the Containerfile describes. They
also share the runner's Cargo registry and Git caches, and the image keeps
Cargo's lock on them in the shared registry, so jobs take turns to unpack a
crate. The `musl` and `crates` jobs keep their slots' builds warm by touching
their checkouts instead of cleaning them.

## Other workflows

None of these is a required check.

- `deps.yml` runs weekly, and on `shoal ci run deps.yml --branch main`. It
  rewrites one evergreen `tracking` issue, found by the marker comment in its
  body, with `cargo audit` and `cargo outdated` findings. Findings do not fail
  it; a tool that could not run does.
- `cross-build.yml` runs only on `shoal ci run cross-build.yml --branch
  <branch>`, and cross-builds every release target there.
- `flaky.yml` runs `cargo nextest run` repeatedly on `main` every night, and on
  `shoal ci run flaky.yml --branch main`. It rewrites the evergreen `Flaky test
  report` issue with every test that failed, and a failure fails the run. The
  run log keeps each failed test's output for the issue that fixes it.
- `review.yml` posts an advisory review-bot review, with `.review/review.md`
  as its project brief. AGENTS.md says when a PR gets one. Claude and Codex
  use independent queues, so one review of each can run at once.
- `audit.yml` runs a read-only code audit of the whole application with
  `.review/audit.md`. It has no schedule. Someone dispatches it by hand with a
  focus (bugs, maintainability, testing, performance or docs) and a ref. It
  opens one issue per new finding, labelled `audit/<focus>`.

The review and audit workflows follow review-bot's latest `main` commit.

`.forgejo/scripts/labels.py` creates the repository's label scheme, which
`shoal repo labels` lists.
