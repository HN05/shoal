//! The `xcode` preset's files in its shared cache directory: an xcconfig
//! that gives every workspace's `xcodebuild` one compilation cache, with
//! the workspace and DerivedData paths mapped out of its keys, and an
//! `xcodebuild` wrapper that shares Swift package checkouts between
//! workspaces with the same `Package.resolved`, without which package
//! builds miss the cache.
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

use anyhow::{Context, Result};

/// The xcconfig `XCODE_XCCONFIG_FILE` names, inside the cache directory.
pub const XCCONFIG: &str = "shoal.xcconfig";
/// The directory whose `xcodebuild` goes first on workspace processes' PATH.
pub const BIN: &str = "bin";

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

/// Finds the `Package.resolved` of the project or workspace `xcodebuild`
/// builds, as `xcodebuild` picks it, and keys the checkouts by its content.
/// A caller's own `-clonedSourcePackagesDirPath` wins.
fn wrapper(directory: &Path) -> String {
    let packages = directory.join("SourcePackages");
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
  for candidate in *.xcworkspace *.xcodeproj; do
    if [ -e "$candidate" ]; then container=$candidate; break; fi
  done
fi
case $container in
  *.xcworkspace) resolved=$container/xcshareddata/swiftpm/Package.resolved ;;
  *.xcodeproj) resolved=$container/project.xcworkspace/xcshareddata/swiftpm/Package.resolved ;;
  *) resolved=Package.resolved ;;
esac
if [ -f "$resolved" ]; then
  pins=$(shasum -a 256 < "$resolved" | cut -c1-16)
  exec /usr/bin/xcodebuild -clonedSourcePackagesDirPath '{packages}'/"$pins" "$@"
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
        assert!(
            checkouts.starts_with(&format!("{}/SourcePackages/", cache.display())),
            "{checkouts}"
        );
        assert_eq!(lines.collect::<Vec<_>>(), ["-scheme", "App", "build"]);
        assert_eq!(
            run(&["-project", "App.xcodeproj", "build"]).lines().nth(1),
            Some(checkouts)
        );
        assert_eq!(
            run(&["-clonedSourcePackagesDirPath", "/mine", "build"]),
            "-clonedSourcePackagesDirPath\n/mine\nbuild\n"
        );
        assert_eq!(
            run(&["-project", "Other.xcodeproj", "build"]),
            "-project\nOther.xcodeproj\nbuild\n"
        );
    }
}
