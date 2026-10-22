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
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn the_wrapper_shares_checkouts_by_the_resolved_pins() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        prepare(&cache).unwrap();
        let xcconfig = fs::read_to_string(cache.join(XCCONFIG)).unwrap();
        assert!(xcconfig.contains(&format!(
            "COMPILATION_CACHE_CAS_PATH = {}/CompilationCache.noindex",
            cache.display()
        )));
        let modified = fs::metadata(cache.join(XCCONFIG))
            .unwrap()
            .modified()
            .unwrap();
        prepare(&cache).unwrap();
        assert_eq!(
            fs::metadata(cache.join(XCCONFIG))
                .unwrap()
                .modified()
                .unwrap(),
            modified,
            "unchanged files keep their time"
        );

        // A stand-in for /usr/bin/xcodebuild prints the arguments it gets.
        let script = fs::read_to_string(cache.join(BIN).join("xcodebuild"))
            .unwrap()
            .replace("/usr/bin/xcodebuild", "printf '%s\\n'");
        let project = root.path().join("worktree");
        let resolved = project.join("App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm");
        fs::create_dir_all(&resolved).unwrap();
        fs::write(resolved.join("Package.resolved"), "{}").unwrap();
        let run = |args: &[&str]| {
            let output = Command::new("sh")
                .arg("-c")
                .arg(&script)
                .arg("xcodebuild")
                .args(args)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        let built = run(&["-scheme", "App", "build"]);
        let mut lines = built.lines();
        assert_eq!(lines.next(), Some("-clonedSourcePackagesDirPath"));
        let checkouts = lines.next().unwrap();
        let key = checkouts_key(b"{}");
        assert_eq!(
            checkouts,
            cache.join(PACKAGES).join(&key).display().to_string()
        );
        assert!(
            Path::new(checkouts).is_dir(),
            "the build marks its checkouts"
        );
        assert_eq!(lines.collect::<Vec<_>>(), ["-scheme", "App", "build"]);
        assert_eq!(
            run(&["-project", "App.xcodeproj", "build"]).lines().nth(1),
            Some(checkouts)
        );
        // Beside a workspace, xcodebuild still builds the project by default.
        let workspace = project.join("App.xcworkspace/xcshareddata/swiftpm");
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("Package.resolved"), "{\"pins\": []}").unwrap();
        assert_eq!(run(&["build"]).lines().nth(1), Some(checkouts));
        let pinned = run(&["-workspace", "App.xcworkspace", "build"]);
        assert_ne!(pinned.lines().nth(1), Some(checkouts));
        assert_eq!(
            run(&["-clonedSourcePackagesDirPath", "/mine", "build"]),
            "-clonedSourcePackagesDirPath\n/mine\nbuild\n"
        );
        assert_eq!(
            run(&["-project", "Other.xcodeproj", "build"]),
            "-project\nOther.xcodeproj\nbuild\n"
        );
    }

    #[test]
    fn pruning_keeps_used_and_recent_checkouts() {
        let cache = tempfile::tempdir().unwrap();
        let packages = cache.path().join(PACKAGES);
        for key in ["used", "unused", "recent"] {
            fs::create_dir_all(packages.join(key).join("checkouts")).unwrap();
        }
        let since = SystemTime::now() + std::time::Duration::from_secs(60);
        let recent = fs::File::open(packages.join("recent")).unwrap();
        recent
            .set_modified(since + std::time::Duration::from_secs(1))
            .unwrap();
        let used = HashSet::from(["used".to_owned()]);
        let deleted = prune_checkouts(cache.path(), &used, since).unwrap();
        assert_eq!(deleted, [packages.join("unused")]);
        assert!(packages.join("used").exists());
        assert!(packages.join("recent").exists());
        let missing = cache.path().join("missing");
        assert!(prune_checkouts(&missing, &used, since).unwrap().is_empty());

        // A symlink would lead the deletions outside the cache.
        let linked = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("unused")).unwrap();
        std::os::unix::fs::symlink(outside.path(), linked.path().join(PACKAGES)).unwrap();
        assert!(prune_checkouts(linked.path(), &used, since).is_err());
        assert!(outside.path().join("unused").exists());
    }
}
