use super::*;
use crate::sessions::HostName;
use std::os::unix::fs::PermissionsExt as _;
use tempfile::TempDir;

/// Whether another process could take the lock at `path` now.
fn free(path: &Path) -> Result<bool> {
    let file = OpenOptions::new().write(true).open(path)?;
    Ok(fs2::FileExt::try_lock_exclusive(&file).is_ok())
}

#[tokio::test]
async fn a_group_is_looked_for_once_at_a_time_per_host_and_label() -> Result<()> {
    let state = TempDir::new()?;
    let repo = lock_path(state.path(), "herdr", "repo");
    let held = lock(repo.clone()).await?;
    assert!(!free(&repo)?);

    // Other labels and other hosts' groups wait for none of it.
    let _others = (
        lock(lock_path(state.path(), "herdr", "other")).await?,
        lock(lock_path(state.path(), "rex", "repo")).await?,
    );
    drop(held);
    assert!(free(&repo)?);
    Ok(())
}

/// A `herdr` whose slow `workspace list` lets a second start list before
/// the first creates, unless something keeps them apart.
const FAKE_HERDR: &str = r#"#!/bin/sh
dir=$(dirname "$0")
case "$1 $2" in
"workspace list")
    sleep 0.5
    printf '{"result":{"workspaces":%s}}' "$(cat "$dir/workspaces" 2>/dev/null || echo '[]')" ;;
"workspace create")
    echo created >> "$dir/creations"
    echo '[{"workspace_id":"w1","label":"repo"}]' > "$dir/workspaces"
    echo '{"result":{"workspace":{"workspace_id":"w1","label":"repo"}}}' ;;
esac
"#;

#[tokio::test]
async fn starts_looking_for_one_group_together_create_it_once() -> Result<()> {
    let root = TempDir::new()?;
    let herdr = root.path().join("herdr");
    std::fs::write(&herdr, FAKE_HERDR)?;
    std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755))?;
    let near = Origin {
        data: [(
            "HERDR_BIN_PATH".into(),
            herdr.to_string_lossy().into_owned(),
        )]
        .into(),
        ..Origin::created(HostName::Herdr, "w0:p1".into())
    };
    let settings = crate::config::Config::from_toml("")?
        .resolve(&crate::config::repo::ConfigLayers::default())?;
    let host = Configured::named(&settings, HostName::Herdr);
    let state = root.path().join("state");

    let look = || labelled_group(&state, &host, Some(&near), "repo", root.path());
    let (first, second) = tokio::join!(look(), look());

    let groups = [first?, second?].map(|origin| origin.and_then(|origin| origin.group));
    assert_eq!(groups, [Some("w1".into()), Some("w1".into())]);
    let creations = std::fs::read_to_string(root.path().join("creations"))?;
    assert_eq!(creations.lines().count(), 1);
    Ok(())
}
