use super::*;
use crate::execution::recovery::SavedCommand;

#[test]
fn a_recovery_record_cannot_be_claimed_twice() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("recovery.json");
    std::fs::write(&record, "{}").unwrap();
    let first = claim(&record).unwrap();
    assert!(claim(&record).is_err());
    std::fs::remove_file(&record).unwrap();
    drop(first);
    assert!(claim(&record).is_err());
}

#[test]
fn a_claim_follows_a_record_replaced_while_it_was_opened() {
    let root = tempfile::tempdir().unwrap();
    let record = root.path().join("recovery.json");
    std::fs::write(&record, "{}").unwrap();
    // Another process opened the record just before it was replaced.
    let stale = std::fs::File::open(&record).unwrap();
    let replacement = root.path().join("replacement.json");
    std::fs::write(&replacement, "{}").unwrap();
    std::fs::rename(&replacement, &record).unwrap();
    assert!(lock_current(stale, &record).unwrap().is_none());
    // A claim locks the replacement, so a second one is refused.
    let claimed = claim(&record).unwrap();
    assert!(claim(&record).is_err());
    drop(claimed);
}

#[test]
fn discard_leaves_command_records_another_resume_is_reporting() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("execution.command.json");
    std::fs::write(&path, r#"{"argv":["true"]}"#).unwrap();
    let saved = Saved {
        agents: vec![],
        sessions: vec![],
        commands: vec![SavedCommand {
            id: "execution".into(),
            path: path.clone(),
            argv: vec!["true".into()],
        }],
    };
    let reporting = claim(&path).unwrap();
    // Model a concurrent fork retaining the open file description until exec.
    let _inherited = reporting.0.try_clone().unwrap();
    assert!(discard_saved(&saved, |_| false).is_err());
    assert!(path.exists());
    drop(reporting);
    assert_eq!(discard_saved(&saved, |_| false).unwrap(), ["execution"]);
    assert!(!path.exists());
}
