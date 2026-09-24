# Source at the top of a step that runs cargo: points CARGO_TARGET_DIR at the
# first free numbered slot under it and holds that slot until the step's shell
# exits. Forgejo ignores job-level `concurrency`, so this flock is what keeps
# concurrent jobs' cargo runs apart. A busy slot moves on to the next instead of
# waiting, because a waiting job would occupy a runner slot (#203); the slot
# count therefore never exceeds the jobs the runner runs at once. Lock files
# live inside the shared directory, so they are the same inodes in every job
# container, whichever repository it builds.
: "${CARGO_TARGET_DIR:?}"
lock_target_base=$CARGO_TARGET_DIR
lock_target_slot=0
while :; do
  CARGO_TARGET_DIR=$lock_target_base/slot-$lock_target_slot
  mkdir -p "$CARGO_TARGET_DIR"
  exec 9>"$CARGO_TARGET_DIR/.lock"
  if flock -n 9; then
    break
  fi
  lock_target_slot=$((lock_target_slot + 1))
done
export CARGO_TARGET_DIR
echo "building in $CARGO_TARGET_DIR"
