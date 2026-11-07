//! The `cargo` preset's file in its shared cache directory: the rustc
//! wrapper `RUSTC_WORKSPACE_WRAPPER` names, which keeps the workspace
//! crates' incremental state usable in every workspace. docs/design.md says
//! why it takes these flags.
use std::path::Path;

use anyhow::Result;

use super::write_if_changed;

/// The wrapper's file name inside the cache directory.
pub const WRAPPER: &str = "rustc-workspace-wrapper";

/// Write the wrapper into `directory`, leaving it untouched when current.
pub fn prepare(directory: &Path) -> Result<()> {
    write_if_changed(&directory.join(WRAPPER), WRAPPER_SCRIPT, 0o755)
}

/// Cargo runs it as `<wrapper> <rustc> <arguments>`, and also sends its
/// target queries through it, so invocations with `--print` or without a
/// crate name pass through unchanged. So does every invocation when
/// `RUSTC_BOOTSTRAP=-1` refuses unstable flags or the rustc in use no
/// longer knows the flag. A crate the wrapper adds to `RUSTC_BOOTSTRAP`
/// gets `-Zallow-features=`, so it can use no unstable feature it could
/// not use on stable.
const WRAPPER_SCRIPT: &str = r#"#!/bin/sh
# Written by Shoal for the cargo preset.
cwd=$(pwd -P)/ crate= option= query= count=$#
for argument in "$@"; do
  case $option in
    --crate-name) crate=$argument ;;
    --out-dir) argument=${argument#"$cwd"} ;;
  esac
  case $argument in --print|--print=*) query=1 ;; esac
  option=$argument
  set -- "$@" "$argument"
done
shift "$count"
[ -n "$crate" ] && [ -z "$query" ] || exec "$@"
case ,${RUSTC_BOOTSTRAP-}, in
  ,-1,) exec "$@" ;;
  ,1,|*,"$crate",*) features= ;;
  ,,) RUSTC_BOOTSTRAP=$crate features=-Zallow-features= ;;
  *) RUSTC_BOOTSTRAP=$RUSTC_BOOTSTRAP,$crate features=-Zallow-features= ;;
esac
RUSTC_BOOTSTRAP=$RUSTC_BOOTSTRAP "$1" --crate-name "$crate" -Zremap-cwd-prefix=. $features \
  --print=sysroot >/dev/null 2>&1 || exec "$@"
export RUSTC_BOOTSTRAP
exec "$@" -Zremap-cwd-prefix=. $features
"#;

#[cfg(test)]
mod tests;
