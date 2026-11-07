//! The `cargo` preset's file in its shared cache directory: the wrapper
//! `RUSTC_WORKSPACE_WRAPPER` names, through which Cargo runs rustc for the
//! workspace's own crates. rustc keeps incremental state only for the
//! working directory it was built in, which differs between the seed and
//! each workspace; the wrapper maps that directory to `.` with
//! `-Zremap-cwd-prefix`, which leaves rustc's options the same in every
//! workspace, and passes the output directory relative to it. Stable
//! rustc takes the flag only for the crates `RUSTC_BOOTSTRAP` names, so
//! the wrapper names each crate it builds, and dependencies and build
//! script probes see a stable compiler.
use std::path::Path;

use anyhow::Result;

use super::write_if_changed;

/// The wrapper's file name inside the cache directory.
pub const WRAPPER: &str = "rustc-workspace-wrapper";

/// Write the wrapper into `directory`, leaving it untouched when current.
pub fn prepare(directory: &Path) -> Result<()> {
    write_if_changed(&directory.join(WRAPPER), WRAPPER_SCRIPT, 0o755)
}

/// Cargo runs it as `<wrapper> <rustc> <arguments>`. rustc reuses
/// generated code only for the same output directory, which Cargo passes as
/// an absolute path. Queries without a crate name, such as `rustc -vV`, get
/// no flag, and neither does any invocation when `RUSTC_BOOTSTRAP=-1`
/// refuses unstable flags or the rustc in use no longer knows the flag.
const WRAPPER_SCRIPT: &str = r#"#!/bin/sh
# Written by Shoal for the cargo preset.
cwd=$(pwd -P)/ crate= option= count=$#
for argument in "$@"; do
  case $option in
    --crate-name) crate=$argument ;;
    --out-dir) argument=${argument#"$cwd"} ;;
  esac
  option=$argument
  set -- "$@" "$argument"
done
shift "$count"
[ -n "$crate" ] || exec "$@"
case ${RUSTC_BOOTSTRAP-} in
  -1) exec "$@" ;;
  1) ;;
  '') RUSTC_BOOTSTRAP=$crate ;;
  *) RUSTC_BOOTSTRAP=$RUSTC_BOOTSTRAP,$crate ;;
esac
RUSTC_BOOTSTRAP=$RUSTC_BOOTSTRAP "$1" --crate-name "$crate" -Zremap-cwd-prefix=. \
  --print=sysroot >/dev/null 2>&1 || exec "$@"
export RUSTC_BOOTSTRAP
exec "$@" -Zremap-cwd-prefix=.
"#;

#[cfg(test)]
mod tests;
