#!/usr/bin/env bash
# Homebrew supplies its versioned prefix and stable opt prefix. Keep packaging
# with the source so --HEAD builds with its own install logic.
set -euo pipefail

if [[ $# -lt 2 || $1 != /* || $2 != /* ]]; then
  echo "usage: install-homebrew.sh <absolute-prefix> <absolute-opt-prefix> [cargo-install-options...]" >&2
  exit 2
fi

install_prefix=$1
stable_prefix=$2
shift 2
cd "$(dirname "$0")/.."

export SHOAL_BUILD_SKILLS_DIR="$stable_prefix/share/shoal/skills"
cargo install --locked --path . --root "$install_prefix" --no-track "$@"
for skill in skills/*/SKILL.md; do
  install -d "$install_prefix/share/shoal/$(dirname "$skill")"
  install -m 644 "$skill" "$install_prefix/share/shoal/$skill"
done
