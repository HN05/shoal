use super::*;

fn pairs(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn line_arguments_split_the_path_at_its_last_colon() -> Result<()> {
    let comments = line_comments(&pairs(&["a:b.rs:7", "Why?"]), Side::Old)?;
    assert_eq!(
        comments,
        [LineComment {
            path: "a:b.rs".into(),
            line: 7,
            side: Side::Old,
            body: "Why?".into(),
        }]
    );
    let error = line_comments(&pairs(&["a.rs", "Why?"]), Side::New).unwrap_err();
    assert_eq!(error.to_string(), "a.rs names no line; give PATH:LINE");
    assert!(line_comments(&pairs(&["a.rs:x", "Why?"]), Side::New).is_err());
    Ok(())
}

#[test]
fn comments_files_take_line_comments_and_refuse_unknown_fields() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("comments.json");
    std::fs::write(
        &path,
        r#"[{"path": "a.rs", "line": 4, "side": "old", "body": "x"}]"#,
    )?;
    assert_eq!(comments_file(&path)?[0].side, Side::Old);
    std::fs::write(
        &path,
        r#"[{"path": "a.rs", "line": 4, "body": "x", "end": 5}]"#,
    )?;
    let error = format!("{:#}", comments_file(&path).unwrap_err());
    assert!(error.contains("unknown field `end`"), "{error}");
    Ok(())
}
