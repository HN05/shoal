#!/usr/bin/env sh
# Fails unless the machine's Rust is the workspace's rust-version: Shoal
# runners use the host's toolchain, and a newer clippy would fail a PR on one
# machine that passes on another. Fails too without cargo-nextest, which runs
# the tests: a run installs nothing on the runner's host.
set -eu
rustc --version
cargo clippy --version
if ! cargo nextest --version; then
  echo "::error::cargo-nextest is not installed on this machine: install it here with \`cargo install --locked cargo-nextest\`; Shoal runners install nothing during a run (AGENTS.md, CI)"
  exit 1
fi
want=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)
case "$(rustc --version)" in
  "rustc $want."*) ;;
  *)
    echo "::error::this machine's Rust is not $want, the workspace's rust-version: install Rust $want here, or bump rust-version as AGENTS.md (CI) describes"
    exit 1
    ;;
esac
