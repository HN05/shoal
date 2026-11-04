use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use anyhow::{Context as _, Result};

use super::{commit, git, install_test_script, isolated_test, repository};

#[tokio::test]
async fn git_fixtures_ignore_inherited_git_variables() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let sentinel = repository(root.path(), "sentinel");
    let hooks = root.path().join("hooks");
    fs::create_dir(&hooks)?;
    install_test_script(
        &hooks.join("post-commit"),
        &format!("#!/bin/sh\ntouch '{}'\n", sentinel.join("hooked").display()),
    );
    let before = snapshot(&sentinel)?;
    // Point Git at the sentinel as a hook or wrapper running the tests would.
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        isolated_test(
            root.path(),
            "test_support::tests::git_fixtures_ignore_inherited_git_variables_child",
        )
        .env("GIT_DIR", sentinel.join(".git"))
        .env("GIT_WORK_TREE", &sentinel)
        .env("GIT_INDEX_FILE", sentinel.join(".git/index"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", &hooks)
        .env("GIT_AUTHOR_NAME", "Inherited Name")
        .output(),
    )
    .await
    .context("fixture repository creation stalled")??;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(snapshot(&sentinel)?, before);
    Ok(())
}

#[test]
#[ignore = "isolated environment launched by inherited Git variable regression"]
fn git_fixtures_ignore_inherited_git_variables_child() -> Result<()> {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return Ok(());
    }
    let root = std::env::current_dir()?;
    let fixture = repository(&root, "fixture");
    fs::write(fixture.join("added"), "added\n")?;
    commit(&fixture, "added");
    assert_eq!(
        git(&fixture, &["log", "--format=%an %s"]),
        "Shoal Test added\nShoal Test tracked\n"
    );
    assert_eq!(git(&fixture, &["status", "--porcelain"]), "");
    Ok(())
}

/// Every file below `dir` with its contents.
fn snapshot(dir: &Path) -> Result<BTreeMap<std::path::PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path)?);
            }
        }
    }
    Ok(files)
}
