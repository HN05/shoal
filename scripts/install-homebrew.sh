#!/usr/bin/env bash
# Homebrew supplies its versioned prefix and stable opt prefix. Keep packaging
# with the source so releases and main each build with their own install logic.
set -euo pipefail

if [[ $# -lt 2 || $1 != /* || $2 != /* ]]; then
  echo "usage: install-homebrew.sh <absolute-prefix> <absolute-opt-prefix> [cargo-install-options...]" >&2
  exit 2
fi

install_prefix=$1
stable_prefix=$2
shift 2
cd "$(dirname "$0")/.."

export SHOAL_SKILL_PATH="$stable_prefix/share/shoal/skill/SKILL.md"
cargo install --locked --path . --root "$install_prefix" --no-track "$@"
install -d "$install_prefix/share/shoal/skill"
install -m 644 SKILL.md "$install_prefix/share/shoal/skill/SKILL.md"
