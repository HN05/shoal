#!/usr/bin/env sh
# Build the CI image, on the runner host so its Podman can use it directly.
#
#   build.sh                 the version in the Containerfile's ARG RUST
#   build.sh 1.99            another Rust version
#   build.sh 1.99 --push     also push (needs `podman login git.henriknordvik.com` once)
#
# Jobs resolve the tag in the local store first and force_pull is off, so the
# next run uses the new image with no restart and never re-pulls; pushing is
# only for surviving a runner rebuild, and an image pushed from elsewhere needs
# a `podman pull` here. --network host: the runner is an LXC without
# /dev/net/tun, so Podman's pasta cannot give RUN steps a network namespace.
set -eu

cd "$(dirname "$0")"
REGISTRY=git.henriknordvik.com/hn05/ci-shoal
DEFAULT_RUST=$(sed -n 's/^ARG RUST=//p' Containerfile)
RUST=${1:-$DEFAULT_RUST}
PUSH=${2:-}
IMAGE=$REGISTRY:$RUST

podman build --network host --build-arg "RUST=$RUST" -t "$IMAGE" .

if [ "$PUSH" = "--push" ]; then
  podman push "$IMAGE"
fi

echo
echo "built $IMAGE"

# More than one workflow runs in this image; one left on an older tag would
# keep using the previous toolchain with nothing saying so.
ESCAPED_REGISTRY=$(printf '%s' "$REGISTRY" | sed 's/[./]/\\&/g')
STALE=''
for wf in ../workflows/*.yml; do
  tag=$(sed -n "s/.*image: *$ESCAPED_REGISTRY:\([0-9.]*\).*/\1/p" "$wf" | head -1)
  [ -n "$tag" ] || continue # a workflow that does not use this image
  [ "$tag" = "$RUST" ] || STALE="${STALE:+$STALE, }$(basename "$wf") ($tag)"
done
if [ -n "$STALE" ]; then
  echo "still asking for an older tag: $STALE; bump each to $RUST (and ARG RUST in the Containerfile and package.rust-version in Cargo.toml) in a PR, fixing any new clippy lints there."
else
  echo "every workflow using this image already asks for this tag; the next run picks the image up."
fi
echo "Old versions stay in the store until removed: podman rmi $REGISTRY:<old>"
