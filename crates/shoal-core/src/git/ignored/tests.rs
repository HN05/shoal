use std::fs;

use super::*;

/// A main checkout with `ignored` in its `.gitignore` and a linked worktree
/// on `topic`, which nothing has been copied into yet.
struct Fixture {
    _root: tempfile::TempDir,
    checkout: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    async fn new(ignored: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let checkout = crate::test_support::repository(root.path(), "repo");
        fs::write(checkout.join(".gitignore"), ignored).unwrap();
        crate::test_support::commit(&checkout, ".gitignore");
        let workspace = root.path().join("topic");
        super::super::worktree::add(&checkout, &workspace, &"topic".into(), Some("main"))
            .await
            .unwrap();
        Self {
            _root: root,
            checkout,
            workspace,
        }
    }

    /// Write `contents` to `path` in the checkout, creating its directory.
    fn write(&self, path: &str, contents: &str) {
        let path = self.checkout.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    async fn copy(&self, exclude: &[&str]) -> Result<u64> {
        let exclude: Vec<String> = exclude.iter().map(|&pattern| pattern.into()).collect();
        copy(&self.workspace, &exclude).await
    }

    fn copied(&self, path: &str) -> bool {
        self.workspace.join(path).symlink_metadata().is_ok()
    }
}

#[tokio::test]
async fn ignored_files_are_counted_and_copied_once() {
    let fixture = Fixture::new(".env\ntarget/\n").await;
    fixture.write(".env", "KEY=1\n");
    fixture.write("target/debug/a", "a");
    fixture.write("target/debug/b", "b");
    fixture.write("untracked", "not ignored");
    assert_eq!(fixture.copy(&[]).await.unwrap(), 3);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join(".env")).unwrap(),
        "KEY=1\n"
    );
    assert!(fixture.copied("target/debug/b"));
    assert!(!fixture.copied("untracked"));
    assert_eq!(fixture.copy(&[]).await.unwrap(), 0);
}

/// Files the workspace has, such as a seed build's, stay, and a directory it
/// has gets only the files it lacks.
#[tokio::test]
async fn files_the_workspace_has_are_kept() {
    let fixture = Fixture::new(".env\ntarget/\n").await;
    fixture.write(".env", "KEY=checkout\n");
    fixture.write("target/debug/built", "checkout");
    fixture.write("target/debug/only-in-checkout", "checkout");
    fs::write(fixture.workspace.join(".env"), "KEY=workspace\n").unwrap();
    fs::create_dir_all(fixture.workspace.join("target/debug")).unwrap();
    fs::write(fixture.workspace.join("target/debug/built"), "seed").unwrap();
    assert_eq!(fixture.copy(&[]).await.unwrap(), 1);
    assert_eq!(
        fs::read_to_string(fixture.workspace.join(".env")).unwrap(),
        "KEY=workspace\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace.join("target/debug/built")).unwrap(),
        "seed"
    );
    assert!(fixture.copied("target/debug/only-in-checkout"));
}

#[tokio::test]
async fn worktreeinclude_selects_what_is_copied() {
    let fixture = Fixture::new(".env\n*.log\ncache/\n").await;
    fixture.write(".worktreeinclude", ".env\n*.log\n");
    fixture.write(".env", "KEY=1\n");
    fixture.write("logs/run.log", "log");
    fixture.write("cache/blob", "blob");
    assert_eq!(fixture.copy(&[]).await.unwrap(), 2);
    assert!(fixture.copied(".env"));
    assert!(fixture.copied("logs/run.log"));
    assert!(!fixture.copied("cache"));
}

/// Git lists an ignored file on its own beside a tracked file in its
/// directory, so a directory pattern reaches it through its parent.
async fn ignored_file_in_tracked_directory() -> Fixture {
    let fixture = Fixture::new("config/local.yml\n.env\n").await;
    fixture.write("config/app.yml", "tracked");
    crate::test_support::commit(&fixture.checkout, "config/app.yml");
    fixture.write("config/local.yml", "secret");
    fixture.write(".env", "KEY=1\n");
    fixture
}

#[tokio::test]
async fn an_include_directory_pattern_covers_the_files_in_it() {
    let fixture = ignored_file_in_tracked_directory().await;
    fixture.write(".worktreeinclude", "config/\n");
    assert_eq!(fixture.copy(&[]).await.unwrap(), 1);
    assert!(fixture.copied("config/local.yml"));
    assert!(!fixture.copied(".env"));
}

#[tokio::test]
async fn an_exclude_directory_pattern_covers_the_files_in_it() {
    let fixture = ignored_file_in_tracked_directory().await;
    assert_eq!(fixture.copy(&["config/"]).await.unwrap(), 1);
    assert!(!fixture.copied("config/local.yml"));
    assert!(fixture.copied(".env"));
}

#[tokio::test]
async fn invalid_patterns_fail_naming_where_they_are() {
    let fixture = Fixture::new(".env\n").await;
    fixture.write(".env", "KEY=1\n");
    let error = fixture.copy(&["{unclosed"]).await.unwrap_err();
    assert!(format!("{error:#}").contains("\"{unclosed\""), "{error:#}");
    fixture.write(".worktreeinclude", "{unclosed\n");
    let error = fixture.copy(&[]).await.unwrap_err();
    assert!(format!("{error:#}").contains(INCLUDE_FILE), "{error:#}");
    assert!(!fixture.copied(".env"));
}

#[tokio::test]
async fn other_tools_state_is_never_copied() {
    let fixture = Fixture::new(".jj/\n.hg/\n.env\nsub/.svn/\n").await;
    fixture.write(".jj/repo/store", "jj");
    fixture.write(".hg/store", "hg");
    // A tracked file beside it makes Git list the nested directory itself.
    fixture.write("sub/tracked", "tracked");
    crate::test_support::commit(&fixture.checkout, "sub/tracked");
    fixture.write("sub/.svn/entries", "svn");
    fixture.write(".env", "KEY=1\n");
    assert_eq!(fixture.copy(&[]).await.unwrap(), 1);
    assert!(!fixture.copied(".jj"));
    assert!(!fixture.copied(".hg"));
    assert!(!fixture.copied("sub/.svn"));
}

#[tokio::test]
async fn directories_holding_another_worktree_are_not_copied() {
    let fixture = Fixture::new("nested/\n.env\n").await;
    fixture.write(".env", "KEY=1\n");
    let nested = fixture.checkout.join("nested").join("other");
    fs::create_dir_all(nested.parent().unwrap()).unwrap();
    super::super::worktree::add(&fixture.checkout, &nested, &"other".into(), Some("main"))
        .await
        .unwrap();
    assert_eq!(fixture.copy(&[]).await.unwrap(), 1);
    assert!(!fixture.copied("nested"));
    // An exclude that splits the worktree into parts copies none of them,
    // its `.git` file included.
    assert_eq!(fixture.copy(&["nested/other/tracked"]).await.unwrap(), 0);
    assert!(!fixture.copied("nested"));
}

#[tokio::test]
async fn the_main_checkout_copies_nothing_into_itself() {
    let fixture = Fixture::new(".env\n").await;
    fixture.write(".env", "KEY=1\n");
    assert_eq!(copy(&fixture.checkout, &[]).await.unwrap(), 0);
}

/// Git lists paths as bytes, which Linux file names need not make UTF-8;
/// macOS refuses such names.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn names_that_are_not_utf8_are_copied() {
    use std::os::unix::ffi::OsStrExt as _;
    let fixture = Fixture::new("data/\n*.bin\n").await;
    let name = std::ffi::OsStr::from_bytes(b"report-\xff.txt");
    fs::create_dir(fixture.checkout.join("data")).unwrap();
    fs::write(fixture.checkout.join("data").join(name), "x").unwrap();
    let file = std::ffi::OsStr::from_bytes(b"\xfe.bin");
    fs::write(fixture.checkout.join(file), "y").unwrap();
    assert_eq!(fixture.copy(&[]).await.unwrap(), 2);
    assert!(fixture.workspace.join("data").join(name).exists());
    assert!(fixture.workspace.join(file).exists());
}

#[tokio::test]
async fn links_modes_and_times_are_kept() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new("bin/\nlink\n").await;
    fixture.write("bin/tool", "#!/bin/sh\n");
    let tool = fixture.checkout.join("bin/tool");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let modified = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::open(&tool)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    std::os::unix::fs::symlink("bin/tool", fixture.checkout.join("link")).unwrap();
    assert_eq!(fixture.copy(&[]).await.unwrap(), 2);
    let copied = fs::metadata(fixture.workspace.join("bin/tool")).unwrap();
    assert_eq!(copied.permissions().mode() & 0o777, 0o755);
    assert_eq!(copied.modified().unwrap(), modified);
    assert_eq!(
        fs::read_link(fixture.workspace.join("link")).unwrap(),
        Path::new("bin/tool")
    );
}

/// A symbolic link in the workspace to a directory elsewhere is kept as
/// the workspace's own entry, and nothing is written through it or through
/// a link on the way to a file.
#[tokio::test]
async fn nothing_is_written_outside_the_workspace() {
    let fixture = Fixture::new("target/\nconfig/local.yml\n").await;
    fixture.write("target/debug/output", "built");
    fixture.write("config/app.yml", "tracked");
    crate::test_support::commit(&fixture.checkout, "config/app.yml");
    fixture.write("config/local.yml", "secret");
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.workspace.join("target")).unwrap();
    assert_eq!(fixture.copy(&["config/"]).await.unwrap(), 0);
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    std::os::unix::fs::symlink(outside.path(), fixture.workspace.join("config")).unwrap();
    let error = fixture.copy(&[]).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("outside the workspace"),
        "{error:#}"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

/// Git lists an ignored directory with nothing tracked in it as one entry,
/// whose insides the patterns still decide.
async fn collapsed_cache() -> Fixture {
    let fixture = Fixture::new("cache/\n").await;
    fixture.write("cache/public/a", "a");
    fixture.write("cache/private/b", "b");
    fixture.write("cache/top", "top");
    fixture
}

#[tokio::test]
async fn an_exclude_reaches_inside_a_directory_git_lists_whole() {
    let fixture = collapsed_cache().await;
    assert_eq!(fixture.copy(&["cache/private/"]).await.unwrap(), 2);
    assert!(fixture.copied("cache/public/a"));
    assert!(fixture.copied("cache/top"));
    assert!(!fixture.copied("cache/private"));
}

#[tokio::test]
async fn an_include_reaches_inside_a_directory_git_lists_whole() {
    let fixture = collapsed_cache().await;
    fixture.write(".worktreeinclude", "cache/public/\n");
    assert_eq!(fixture.copy(&[]).await.unwrap(), 1);
    assert!(fixture.copied("cache/public/a"));
    assert!(!fixture.copied("cache/top"));
    assert!(!fixture.copied("cache/private"));
}

#[tokio::test]
async fn other_tools_state_inside_a_walked_directory_is_not_copied() {
    let fixture = collapsed_cache().await;
    fixture.write("cache/.jj/store", "jj");
    assert_eq!(fixture.copy(&["*.log"]).await.unwrap(), 3);
    assert!(!fixture.copied("cache/.jj"));
}
