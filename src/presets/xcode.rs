//! The `xcode` preset's files in its shared cache directory: an xcconfig
//! that gives every workspace's `xcodebuild` one compilation cache, with
//! the workspace and DerivedData paths mapped out of its keys, and an
//! `xcodebuild` wrapper that shares Swift package checkouts between
//! workspaces with the same `Package.resolved`, without which package
//! builds miss the cache.
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

/// The xcconfig `XCODE_XCCONFIG_FILE` names, inside the cache directory.
pub const XCCONFIG: &str = "shoal.xcconfig";
/// The directory whose `xcodebuild` goes first on workspace processes' PATH.
pub const BIN: &str = "bin";
/// The directory of package checkouts, one per set of resolved pins.
const PACKAGES: &str = "SourcePackages";
/// The file that records a project's resolved package pins.
pub const PINS: &str = "Package.resolved";

/// Write the preset's files into `directory`, leaving files whose content
/// is current untouched, since Xcode rebuilds its build description when
/// an xcconfig changes.
pub fn prepare(directory: &Path) -> Result<()> {
    write_if_changed(&directory.join(XCCONFIG), &xcconfig(directory), 0o644)?;
    fs::create_dir_all(directory.join(BIN))?;
    write_if_changed(
        &directory.join(BIN).join("xcodebuild"),
        &wrapper(directory),
        0o755,
    )
}

/// `$(OBJROOT)` is `<DerivedData>/Build/Intermediates.noindex`; no setting
/// names the DerivedData folder itself. A missing workspace path maps a
/// path no build uses rather than every path.
fn xcconfig(directory: &Path) -> String {
    format!(
        "// Written by Shoal for the xcode preset.
COMPILATION_CACHE_ENABLE_CACHING = YES
COMPILATION_CACHE_CAS_PATH = {cas}
SWIFT_ENABLE_PREFIX_MAPPING = YES
CLANG_ENABLE_PREFIX_MAPPING = YES
SWIFT_ENABLE_PROJECT_PREFIX_MAPPING = YES
CLANG_ENABLE_PROJECT_PREFIX_MAPPING = YES
SHOAL_DERIVED_DATA = $(OBJROOT:dir:standardizepath:dir:standardizepath)
SHOAL_PREFIX_MAPPINGS = $(SHOAL_DERIVED_DATA)=/^derived-data $({workspace}:default=/nonexistent)=/^workspace
SWIFT_OTHER_PREFIX_MAPPINGS = $(inherited) $(SHOAL_PREFIX_MAPPINGS)
CLANG_OTHER_PREFIX_MAPPINGS = $(inherited) $(SHOAL_PREFIX_MAPPINGS)
",
        cas = directory.join("CompilationCache.noindex").display(),
        workspace = crate::env::WORKSPACE_PATH,
    )
}

/// The name of the package checkouts for a `Package.resolved` with this
/// content, as the wrapper computes it.
pub fn checkouts_key(pins: &[u8]) -> String {
    Sha256::digest(pins)
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Delete the package checkouts in the cache `directory` whose key is not
/// in `used` and that no build started since `unused_since`, returning the
/// deleted checkouts. A build that resolves packages can rewrite its
/// `Package.resolved` while it runs, so keys alone cannot show a checkout
/// unused.
pub fn prune_checkouts(
    directory: &Path,
    used: &HashSet<String>,
    unused_since: SystemTime,
) -> Result<Vec<PathBuf>> {
    let packages = directory.join(PACKAGES);
    match fs::symlink_metadata(&packages) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        metadata => ensure!(
            metadata
                .with_context(|| format!("read {}", packages.display()))?
                .is_dir(),
            "{} is not a directory, so its entries lie outside the cache",
            packages.display()
        ),
    }
    let entries =
        fs::read_dir(&packages).with_context(|| format!("read {}", packages.display()))?;
    let mut deleted = Vec::new();
    for entry in entries {
        let entry = entry?;
        if used.contains(&*entry.file_name().to_string_lossy())
            || entry.metadata()?.modified()? >= unused_since
        {
            continue;
        }
        let checkouts = entry.path();
        fs::remove_dir_all(&checkouts)
            .with_context(|| format!("delete {}", checkouts.display()))?;
        deleted.push(checkouts);
    }
    Ok(deleted)
}

/// Finds the `Package.resolved` of the project or workspace `xcodebuild`
/// builds, as `xcodebuild` picks it: a named container, or else the
/// directory's project ahead of its workspace. The checkouts are keyed by
/// that file's content, and each build marks the ones it uses.
/// A caller's own `-clonedSourcePackagesDirPath` wins.
fn wrapper(directory: &Path) -> String {
    let packages = directory.join(PACKAGES);
    format!(
        r#"#!/bin/sh
# Written by Shoal for the xcode preset.
container= next=
for argument in "$@"; do
  if [ -n "$next" ]; then container=$argument next=; continue; fi
  case $argument in
    -project|-workspace) next=1 ;;
    -clonedSourcePackagesDirPath) exec /usr/bin/xcodebuild "$@" ;;
  esac
done
if [ -z "$container" ]; then
  for candidate in *.xcodeproj *.xcworkspace; do
    if [ -e "$candidate" ]; then container=$candidate; break; fi
  done
fi
case $container in
  *.xcworkspace) resolved=$container/xcshareddata/swiftpm/Package.resolved ;;
  *.xcodeproj) resolved=$container/project.xcworkspace/xcshareddata/swiftpm/Package.resolved ;;
  *) resolved=Package.resolved ;;
esac
if [ -f "$resolved" ]; then
  checkouts='{packages}'/$(shasum -a 256 < "$resolved" | cut -c1-16)
  mkdir -p "$checkouts" && touch "$checkouts"
  exec /usr/bin/xcodebuild -clonedSourcePackagesDirPath "$checkouts" "$@"
fi
exec /usr/bin/xcodebuild "$@"
"#,
        packages = packages.display().to_string().replace('\'', r"'\''"),
    )
}

fn write_if_changed(path: &Path, content: &str, mode: u32) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|current| current == content) {
        return Ok(());
    }
    fs::write(path, content).with_context(|| format!("write {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set permissions on {}", path.display()))
}

#[cfg(test)]
mod tests;
