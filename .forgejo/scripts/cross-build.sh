#!/usr/bin/env bash
# Run from the source checkout, after acquiring a /ci-target slot.
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd)
targets_file="$script_dir/release-targets.txt"
# Columns: Rust target, archive platform, Homebrew OS and architecture selectors.

target=${1:?expected a Rust release target}
platform=''
while read -r known_target known_platform _selectors; do
  if [ "$known_target" = "$target" ]; then
    platform=$known_platform
    break
  fi
done < "$targets_file"
if [ -z "$platform" ]; then
  echo "Unknown release target: $target" >&2
  exit 1
fi

export CI_ZIG_BINARY=$(command -v zig)
export CARGO_ZIGBUILD_ZIG_PATH="$script_dir/zig-wrapper.py"

case "$target:$(uname -s)" in
  *-apple-darwin:Linux)
    # Expose Zig's bundled macOS headers and libSystem stub in SDK layout so
    # rustc can locate it without xcrun. This SDK supplies no Apple frameworks.
    zig_lib=$("$CI_ZIG_BINARY" env | python3 -c 'import json, pathlib, sys; print(pathlib.Path(json.load(sys.stdin)["lib_dir"]).resolve())')
    sdk="${CARGO_TARGET_DIR:?}/zig-macos-sdk"
    mkdir -p "$sdk/usr"
    ln -sfn "$zig_lib/libc/include/any-darwin-any" "$sdk/usr/include"
    ln -sfn "$zig_lib/libc/darwin" "$sdk/usr/lib"
    ln -sfn "$zig_lib/libc/darwin/SDKSettings.json" "$sdk/SDKSettings.json"
    export SDKROOT="$(cd "$sdk" && pwd)"
    ;;
esac

cargo clean -p shoal --release --target "$target"
cargo zigbuild --locked --release --target "$target"
