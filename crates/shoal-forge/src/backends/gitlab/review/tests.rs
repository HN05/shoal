use super::*;
use serde_json::json;

#[test]
fn the_diff_is_the_version_at_the_head_with_the_commits_it_compares() -> Result<()> {
    let versions = json!([
        {"id": 9, "head_commit_sha": "c2"},
        {"id": 8, "head_commit_sha": "c1"},
    ]);
    assert_eq!(head_version(&versions, "c1")?, 8);
    assert!(head_version(&versions, "c3").is_err());
    let version = json!({
        "base_commit_sha": "b",
        "start_commit_sha": "s",
        "head_commit_sha": "c2",
        "diffs": [
            {"old_path": "a.rs", "new_path": "a.rs", "diff": "@@ -1 +1,2 @@\n a\n+b\n"},
            {"old_path": "gone.rs", "new_path": "gone.rs", "deleted_file": true, "diff": "@@ -1 +0,0 @@\n-x\n"},
        ],
    });
    let diff = version_diff(&version)?;
    assert_eq!(
        diff.bases,
        Some(Bases {
            base: "b".into(),
            start: "s".into()
        })
    );
    assert_eq!(diff.files.len(), 2);
    assert_eq!(diff.files[0].lines.len(), 2);
    assert_eq!(diff.files[1].path, "gone.rs");
    Ok(())
}
