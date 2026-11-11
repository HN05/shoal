//! The `cargo` preset's files in its shared cache directory: the rustc
//! wrapper `RUSTC_WORKSPACE_WRAPPER` names, which keeps the workspace
//! crates' incremental state usable in every workspace, and the one
//! `RUSTC_WRAPPER` names, which does the same for `cargo clippy`.
//! docs/design.md says why they take these flags.
use std::path::Path;

use anyhow::Result;

use super::write_if_changed;

/// The wrapper's file name inside the cache directory.
pub(crate) const WRAPPER: &str = "rustc-workspace-wrapper";

/// The clippy wrapper's file name inside the cache directory.
pub(crate) const CLIPPY_WRAPPER: &str = "rustc-clippy-wrapper";

/// Write the wrappers into `directory`, leaving them untouched when current.
pub(crate) fn prepare(directory: &Path) -> Result<()> {
    write_if_changed(&directory.join(WRAPPER), WRAPPER_SCRIPT, 0o755)?;
    write_if_changed(
        &directory.join(CLIPPY_WRAPPER),
        CLIPPY_WRAPPER_SCRIPT,
        0o755,
    )
}

/// Cargo runs it as `<wrapper> <rustc> <arguments>`, and also sends its
/// target queries through it, so invocations with `--print` or without a
/// crate name pass through unchanged. A rustc that takes the flag as it is,
/// such as a nightly one, gets only the flag. Otherwise the wrapper adds
/// the crate to `RUSTC_BOOTSTRAP` with `-Zallow-features=`, so it can use
/// no unstable feature it could not use on stable, and passes the
/// invocation through unchanged when `RUSTC_BOOTSTRAP=-1` refuses unstable
/// flags or the rustc in use no longer knows the flag.
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
"$1" --crate-name "$crate" -Zremap-cwd-prefix=. --print=sysroot >/dev/null 2>&1 &&
  exec "$@" -Zremap-cwd-prefix=.
case ,${RUSTC_BOOTSTRAP-}, in
  ,-1,) exec "$@" ;;
  ,,) bootstrap=$crate ;;
  *) bootstrap=$RUSTC_BOOTSTRAP,$crate ;;
esac
RUSTC_BOOTSTRAP=$bootstrap "$1" --crate-name "$crate" -Zremap-cwd-prefix=. -Zallow-features= \
  --print=sysroot >/dev/null 2>&1 || exec "$@"
export RUSTC_BOOTSTRAP="$bootstrap"
exec "$@" -Zremap-cwd-prefix=. -Zallow-features=
"#;

/// `cargo clippy` names `clippy-driver` as the workspace wrapper itself, and
/// runs `$RUSTC_WRAPPER clippy-driver <rustc> <arguments>` for the workspace
/// crates and its target queries, and `$RUSTC_WRAPPER <rustc> <arguments>`
/// for dependencies. So this wrapper hands only `clippy-driver` runs to the
/// workspace wrapper beside it, whose probe runs `clippy-driver` as it would
/// rustc, and passes every other run through unchanged, including the
/// workspace wrapper's own runs in ordinary builds.
const CLIPPY_WRAPPER_SCRIPT: &str = r#"#!/bin/sh
# Written by Shoal for the cargo preset.
case ${1##*/} in
  clippy-driver) exec "${0%/*}/rustc-workspace-wrapper" "$@" ;;
esac
exec "$@"
"#;

#[cfg(test)]
mod tests;
