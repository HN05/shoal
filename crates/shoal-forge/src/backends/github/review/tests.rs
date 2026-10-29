use super::*;
use serde_json::json;

#[test]
fn changed_files_keep_their_former_path_and_lack_lines_without_a_patch() -> Result<()> {
    let renamed = json!({
        "filename": "new.rs",
        "previous_filename": "old.rs",
        "patch": "@@ -1,2 +1,2 @@\n-a\n+b\n c",
    });
    let file = changed_file(&renamed)?;
    assert_eq!(
        (file.old_path.as_str(), file.path.as_str()),
        ("old.rs", "new.rs")
    );
    assert_eq!(file.lines.len(), 3);
    let large = changed_file(&json!({"filename": "big.lock"}))?;
    assert_eq!(large.old_path, "big.lock");
    assert!(large.lines.is_empty());
    assert!(changed_file(&json!({"patch": ""})).is_err());
    Ok(())
}
