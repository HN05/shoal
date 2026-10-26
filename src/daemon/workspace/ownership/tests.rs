use super::*;
use crate::state::WorkspaceState;

fn workspace() -> Workspace {
    Workspace::new_record(
        "repository".into(),
        "workspace".into(),
        PathBuf::from("/workspace"),
        "branch".into(),
        WorkspaceState::Ready,
    )
}

#[test]
fn ownership_uses_the_same_marker_path_and_identity_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let git_dir = temp.path().join("admin");
    fs::create_dir(&git_dir)?;
    let different = temp.path().join("different");
    fs::create_dir(&different)?;
    let mut other = workspace();
    other.git_dir = Some(temp.path().join("admin/../admin"));
    other.git_dir_id = Some("old-identity".into());

    assert!(owns_git_dir(&other, &git_dir, Some("new-identity"), None)?);
    assert!(owns_git_dir(
        &other,
        &different,
        Some("old-identity"),
        None
    )?);
    assert!(owns_git_dir(
        &other,
        Path::new("/missing"),
        None,
        Some(&other.id)
    )?);
    assert!(!owns_git_dir(
        &other,
        &different,
        Some("new-identity"),
        None
    )?);
    Ok(())
}

#[test]
fn legacy_identity_matches_the_current_birth_identity() -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir()?;
    let git_dir = temp.path().join("admin");
    let other_dir = temp.path().join("other");
    fs::create_dir(&git_dir)?;
    fs::create_dir(&other_dir)?;
    let metadata = fs::metadata(&git_dir)?;
    let mut other = workspace();
    other.git_dir = Some(other_dir);
    other.git_dir_id = Some(format!("{}:{}", metadata.dev(), metadata.ino()));

    assert!(owns_git_dir(
        &other,
        &git_dir,
        Some(&directory_identity(&git_dir)?),
        None
    )?);
    Ok(())
}
