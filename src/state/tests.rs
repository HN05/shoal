use super::*;
use clap::ValueEnum;

fn assert_spellings<T>(values: &[(T, &str)])
where
    T: Copy
        + std::fmt::Debug
        + std::fmt::Display
        + PartialEq
        + serde::Serialize
        + serde::de::DeserializeOwned
        + rusqlite::types::ToSql
        + rusqlite::types::FromSql,
{
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for &(value, wire) in values {
        assert_eq!(value.to_string(), wire);
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(serde_json::from_value::<T>(wire.into()).unwrap(), value);
        assert_eq!(
            db.query_row("SELECT ?1", [value], |row| row.get::<_, String>(0))
                .unwrap(),
            wire
        );
        assert_eq!(
            db.query_row("SELECT ?1", [wire], |row| row.get::<_, T>(0))
                .unwrap(),
            value
        );
    }
    assert!(serde_json::from_str::<T>("\"not_a_value\"").is_err());
    assert!(
        db.query_row("SELECT 'not_a_value'", [], |row| row.get::<_, T>(0))
            .is_err()
    );
}

#[test]
fn migrated_enums_preserve_spellings_and_defaults() {
    use crate::{
        agent::CodexMode,
        config::repo::ConflictPolicy,
        daemon::{
            approvals::{DecisionStatus, Lifetime},
            recovery::DirectoryState,
            resources::{LockMode, ResourceKind},
            workspace::ExecutionKind,
        },
    };
    assert_spellings(&[
        (Lifetime::Lease, "lease"),
        (Lifetime::Workspace, "workspace"),
    ]);
    assert_spellings(&[
        (DecisionStatus::Pending, "pending"),
        (DecisionStatus::Approved, "approved"),
        (DecisionStatus::Denied, "denied"),
        (DecisionStatus::Outdated, "outdated"),
        (DecisionStatus::Withdrawn, "withdrawn"),
    ]);
    assert_spellings(&[
        (ResourceKind::Semaphore, "semaphore"),
        (ResourceKind::Rwlock, "rwlock"),
        (ResourceKind::Repo, "repo"),
    ]);
    assert_spellings(&[
        (LockMode::Permit, "permit"),
        (LockMode::Read, "read"),
        (LockMode::Write, "write"),
    ]);
    assert_spellings(&[
        (ConflictPolicy::Auto, "auto"),
        (ConflictPolicy::Suggest, "suggest"),
    ]);
    assert_spellings(&[
        (DirectoryState::Valid, "valid"),
        (DirectoryState::Missing, "missing"),
        (DirectoryState::Moved, "moved"),
        (DirectoryState::Unverified, "unverified"),
    ]);
    assert_spellings(&[(CodexMode::Cli, "cli"), (CodexMode::App, "app")]);
    assert_spellings(&[
        (ExecutionKind::Command, "command"),
        (ExecutionKind::Land, "land"),
        (ExecutionKind::Setup, "setup"),
    ]);
    #[cfg(feature = "happy")]
    assert_spellings(&[
        (crate::happy::Agent::Claude, "claude"),
        (crate::happy::Agent::Codex, "codex"),
    ]);
    assert_eq!(Lifetime::default(), Lifetime::Lease);
    assert_eq!(ResourceKind::default(), ResourceKind::Semaphore);
    assert_eq!(ConflictPolicy::default(), ConflictPolicy::Suggest);
    assert_eq!(CodexMode::default(), CodexMode::Cli);
}

states!(
    /// Test spellings that clap cannot infer from the Rust names.
    #[derive(Default)]
    Example: ValueEnum {
        First => "first_value",
        #[default]
        Second => "other",
    }
);

#[test]
fn optional_default_and_clap_values_share_wire_spellings() {
    assert_eq!(Example::default(), Example::Second);
    assert_eq!(
        Example::value_variants(),
        &[Example::First, Example::Second]
    );
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for (value, wire) in [(Example::First, "first_value"), (Example::Second, "other")] {
        assert_eq!(value.to_string(), wire);
        assert_eq!(serde_json::to_value(value).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Example>(wire.into()).unwrap(),
            value
        );
        assert_eq!(value.to_possible_value().unwrap().get_name(), wire);
        assert_eq!(Example::from_str(wire, false).unwrap(), value);
        assert_eq!(
            db.query_row("SELECT ?1", [value], |row| row.get::<_, String>(0))
                .unwrap(),
            wire
        );
        assert_eq!(
            db.query_row("SELECT ?1", [wire], |row| row.get::<_, Example>(0))
                .unwrap(),
            value
        );
    }
    for invalid in ["First", "first-value", "second", "unknown"] {
        assert!(Example::from_str(invalid, false).is_err());
        assert!(serde_json::from_value::<Example>(invalid.into()).is_err());
        assert!(
            db.query_row("SELECT ?1", [invalid], |row| row.get::<_, Example>(0))
                .is_err()
        );
    }
}

#[test]
fn persisted_and_wire_states_are_compatible_and_reject_unknown_values() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for (wire, state) in [
        ("preparing", WorkspaceState::Preparing),
        ("ready", WorkspaceState::Ready),
        ("stopping", WorkspaceState::Stopping),
        ("removing", WorkspaceState::Removing),
        ("reconciling", WorkspaceState::Reconciling),
        ("restoring", WorkspaceState::Restoring),
        ("updating", WorkspaceState::Updating),
        ("failed", WorkspaceState::Failed),
    ] {
        assert_eq!(
            serde_json::from_value::<WorkspaceState>(serde_json::json!(wire)).unwrap(),
            state
        );
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
        let saved: String = db
            .query_row("SELECT ?1", [state], |row| row.get(0))
            .unwrap();
        assert_eq!(saved, wire);
        assert_eq!(
            db.query_row("SELECT ?1", [wire], |row| row.get::<_, WorkspaceState>(0))
                .unwrap(),
            state
        );
    }
    for (wire, state) in [
        ("running", ExecutionState::Running),
        ("unknown", ExecutionState::Unknown),
    ] {
        assert_eq!(
            serde_json::from_value::<ExecutionState>(serde_json::json!(wire)).unwrap(),
            state
        );
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
        assert_eq!(
            db.query_row("SELECT ?1", [state], |row| row.get::<_, String>(0))
                .unwrap(),
            wire
        );
        assert_eq!(
            db.query_row("SELECT ?1", [wire], |row| row.get::<_, ExecutionState>(0))
                .unwrap(),
            state
        );
    }
    assert!(
        db.query_row("SELECT 'running'", [], |row| row
            .get::<_, WorkspaceState>(0))
            .is_err()
    );
    assert!(
        db.query_row("SELECT 'ready'", [], |row| row.get::<_, ExecutionState>(0))
            .is_err()
    );
    assert!(serde_json::from_str::<WorkspaceState>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<ExecutionState>("\"finished\"").is_err());
}
