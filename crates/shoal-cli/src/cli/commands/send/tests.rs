use super::*;

#[test]
fn attachments_are_checked_before_sending() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    let given = |path: &std::path::Path, title: &str| format!("{}{title}", path.display());
    assert_eq!(
        attachments(&[given(&file, ""), given(&file, "#Crash log ")]).unwrap(),
        [
            NewFile {
                path: file.clone(),
                title: None,
            },
            NewFile {
                path: file.clone(),
                title: Some("Crash log".into()),
            },
        ]
    );
    for refused in [
        given(dir.path(), ""),
        given(&dir.path().join("missing.png"), ""),
        given(&file, "#Crash\u{7}log"),
    ] {
        assert!(
            attachments(std::slice::from_ref(&refused)).is_err(),
            "{refused}"
        );
    }
}

#[test]
fn files_alone_may_be_the_message() {
    assert_eq!(message_text(None, None, true).unwrap(), "");
    assert_eq!(
        message_text(Some("Look".into()), None, true).unwrap(),
        "Look"
    );
    assert!(message_text(None, None, false).is_err());
}
