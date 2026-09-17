# Source at the top of a step that runs cargo: holds CARGO_TARGET_DIR until the
# step's shell exits. Forgejo ignores job-level `concurrency`, so this flock is
# what keeps concurrent jobs' cargo runs apart. The lock file lives inside the
# shared directory, so it is the same inode in every job container.
: "${CARGO_TARGET_DIR:?}"
mkdir -p "$CARGO_TARGET_DIR"
exec 9>"$CARGO_TARGET_DIR/.lock"
if ! flock -n 9; then
  echo "waiting for another job's cargo build in $CARGO_TARGET_DIR"
  flock 9
fi
