use super::*;
use std::os::unix::fs::symlink;

#[test]
fn entries_match_only_what_they_installed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("SKILL.md");
    let copy = Entry::copy(b"skill");
    let link = Entry::Link(root.path().join("packaged.md"));
    assert!(!copy.matches(&path).unwrap());
    fs::write(&path, "skill").unwrap();
    assert!(copy.matches(&path).unwrap());
    assert!(!link.matches(&path).unwrap());
    assert!(!Entry::Kept.matches(&path).unwrap());
    fs::write(&path, "edited").unwrap();
    assert!(!copy.matches(&path).unwrap());
    fs::remove_file(&path).unwrap();
    fs::write(root.path().join("same.md"), "skill").unwrap();
    // A link to identical contents is not the copy Shoal wrote.
    symlink(root.path().join("same.md"), &path).unwrap();
    assert!(!copy.matches(&path).unwrap());
    assert!(!link.matches(&path).unwrap());
    fs::remove_file(&path).unwrap();
    symlink(root.path().join("packaged.md"), &path).unwrap();
    assert!(link.matches(&path).unwrap());
}

#[test]
fn record_round_trips_and_rejects_unknown_contents() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(Record::load(root.path()).unwrap(), None);
    let record = Record {
        skills: BTreeMap::from([
            ("copy".to_owned(), Entry::copy(b"skill")),
            ("link".to_owned(), Entry::Link("/skills/SKILL.md".into())),
            ("kept".to_owned(), Entry::Kept),
        ]),
    };
    record.save(root.path()).unwrap();
    assert_eq!(Record::load(root.path()).unwrap(), Some(record));
    for invalid in [
        r#"{"skills":{"x":"other"}}"#,
        r#"{"skills":{"../outside":"kept"}}"#,
        r#"{"skills":{"nested/skill":"kept"}}"#,
    ] {
        fs::write(root.path().join(FILE), invalid).unwrap();
        assert!(Record::load(root.path()).is_err(), "{invalid}");
    }
}
