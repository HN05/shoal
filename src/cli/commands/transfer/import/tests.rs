use super::*;

/// An export of a copy of chat `plan` in `directory`, whose sessions are
/// newer than its files when `earlier_files`.
fn chat_copy(directory: &Path, earlier_files: bool) -> PathBuf {
    let manifest = serde_json::json!({
        "remote_url": "https://forge.test/o/r",
        "kind": "chat",
        "name": "plan",
        "agent": "claude",
        "copy": {"earlier_files": earlier_files},
    });
    let file = directory.join("plan.shoal");
    std::fs::write(&file, format!("shoal workspace 5\n{manifest}\n")).unwrap();
    file
}

fn options(sessions: Option<bool>) -> ImportOptions {
    ImportOptions {
        repository: None,
        sessions,
        resume: false,
    }
}

#[test]
fn a_copy_never_continues_the_original_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let file = chat_copy(directory.path(), false);
    let refused = Package::read(&file, &options(Some(true)))
        .err()
        .expect("--keep-sessions is refused for a copy");
    assert!(
        format!("{refused:#}").contains("--keep-sessions"),
        "{refused:#}"
    );
    let package = Package::read(&file, &options(None)).unwrap();
    assert_eq!(package.copy, Some(Copied::default()));
    assert!(Package::read(&file, &options(Some(false))).is_ok());
}
