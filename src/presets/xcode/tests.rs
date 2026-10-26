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
