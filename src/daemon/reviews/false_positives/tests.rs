use super::*;

fn id(number: i64) -> FindingId {
    FindingId { review: 7, number }
}

fn listed(number: i64, reason: &str) -> FalsePositive {
    FalsePositive {
        finding: Some(id(number)),
        claim: format!("Claim {number}"),
        path: "src/lib.rs".into(),
        reason: reason.into(),
    }
}

#[test]
fn entries_are_kept_newest_first_until_their_finding_is_resolved_otherwise() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(".shoal/false-positives.toml");
    record(&path, id(1), None).unwrap();
    assert!(!path.exists());
    record(&path, id(1), Some(listed(1, "Checked"))).unwrap();
    record(&path, id(2), Some(listed(2, "Sorted"))).unwrap();
    assert_eq!(
        read(&path).unwrap(),
        [listed(2, "Sorted"), listed(1, "Checked")]
    );
    record(&path, id(1), Some(listed(1, "Bounded"))).unwrap();
    assert_eq!(
        read(&path).unwrap(),
        [listed(1, "Bounded"), listed(2, "Sorted")]
    );
    record(&path, id(2), None).unwrap();
    assert_eq!(read(&path).unwrap(), [listed(1, "Bounded")]);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(HEADER), "{text}");
}

#[test]
fn the_users_entries_and_comments_stay() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("false-positives.toml");
    let written = "# Mine\n\n[[false_positive]]\nclaim = \"Unchecked\"\npath = \"src/a.rs\"\nreason = \"Checked\"\n";
    std::fs::write(&path, written).unwrap();
    record(&path, id(1), Some(listed(1, "Sorted"))).unwrap();
    record(&path, id(1), None).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
    let users = FalsePositive {
        finding: None,
        claim: "Unchecked".into(),
        path: "src/a.rs".into(),
        reason: "Checked".into(),
    };
    assert_eq!(read(&path).unwrap(), [users]);
}

#[test]
fn an_invalid_file_is_refused_and_kept() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("false-positives.toml");
    std::fs::write(&path, "[[false_positive]]\nclaim = \"Unchecked\"\n").unwrap();
    let error = format!("{:#}", read(&path).unwrap_err());
    assert!(error.contains("false-positives.toml"), "{error}");
    record(&path, id(1), Some(listed(1, "Sorted"))).unwrap_err();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[[false_positive]]\nclaim = \"Unchecked\"\n"
    );
}
