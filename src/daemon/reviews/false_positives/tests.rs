use super::*;
use crate::reviews::ReviewId;

fn id(number: i64) -> FindingId {
    FindingId {
        review: ReviewId(7),
        number,
    }
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
    assert!(read(&path).is_err());
    record(&path, id(1), Some(listed(1, "Sorted"))).unwrap_err();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[[false_positive]]\nclaim = \"Unchecked\"\n"
    );
}

#[test]
fn entries_in_an_inline_array_are_edited_in_place() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("false-positives.toml");
    for written in [
        "false_positive = []\n",
        "false_positive = [{ claim = \"Unchecked\", path = \"src/a.rs\", reason = \"Checked\" }]\n",
    ] {
        std::fs::write(&path, written).unwrap();
        let before = read(&path).unwrap();
        record(&path, id(1), Some(listed(1, "Sorted"))).unwrap();
        assert_eq!(read(&path).unwrap()[0], listed(1, "Sorted"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("false_positive = [")
        );
        record(&path, id(2), None).unwrap();
        record(&path, id(1), None).unwrap();
        assert_eq!(read(&path).unwrap(), before);
    }
}
