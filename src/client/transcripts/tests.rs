use std::{collections::BTreeMap, fs, path::PathBuf};

use super::*;
use crate::client::transcripts::{claude::Claude, codex::Codex};

fn write(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "x").unwrap();
}

fn paths(files: &[&str]) -> Vec<PathBuf> {
    files.iter().map(PathBuf::from).collect()
}

/// An environment that inherits none of the user's own variables, such as
/// a `CODEX_HOME` that would point the lookup at their sessions.
fn isolated(home: &Path) -> AgentEnvironment<'_> {
    AgentEnvironment {
        inherited: |_| None,
        ..AgentEnvironment::inherited(home)
    }
}

/// [`super::locate`] in environments that inherit none of the user's
/// variables.
fn locate(home: &Path, sessions: &SpaceSessions) -> Vec<Transcript> {
    let environments = Environments {
        inherited: |_| None,
        ..Environments::new(home, &sessions.environments)
    };
    locate_in(&environments, sessions)
}

#[test]
fn claude_sessions_are_the_transcript_and_its_directory_in_any_project() {
    let home = tempfile::tempdir().unwrap();
    let environment = isolated(home.path());
    let directory = Claude.session_dir(&environment).unwrap();
    assert_eq!(directory, home.path().join(".claude/projects"));
    assert_eq!(
        Claude.session_files(&directory, "s1").unwrap(),
        SessionFiles::default()
    );
    write(&directory.join("-a/s1.jsonl"));
    write(&directory.join("-a/s1/subagents/one.jsonl"));
    write(&directory.join("-a/s2.jsonl"));
    write(&directory.join("-b/s1/tool-results/big.txt"));
    assert_eq!(
        Claude.session_files(&directory, "s1").unwrap(),
        SessionFiles {
            transcript: Some("-a/s1.jsonl".into()),
            files: paths(&[
                "-a/s1.jsonl",
                "-a/s1/subagents/one.jsonl",
                "-b/s1/tool-results/big.txt"
            ]),
        }
    );

    let configured = BTreeMap::from([(
        crate::client::env::CLAUDE_CONFIG_DIR.to_owned(),
        home.path().join("claude").into_os_string(),
    )]);
    let environment = AgentEnvironment {
        configured: &configured,
        ..isolated(home.path())
    };
    assert_eq!(
        Claude.session_dir(&environment).unwrap(),
        home.path().join("claude/projects")
    );
}

#[test]
fn codex_sessions_are_the_rollout_named_after_them() {
    let home = tempfile::tempdir().unwrap();
    let environment = isolated(home.path());
    let directory = Codex.session_dir(&environment).unwrap();
    assert_eq!(directory, home.path().join(".codex/sessions"));
    write(&directory.join("2026/10/30/rollout-2026-10-30T01-02-03-s1.jsonl"));
    write(&directory.join("2026/10/30/rollout-2026-10-30T01-02-03-xs1.jsonl"));
    write(&directory.join("2026/10/29/rollout-2026-10-29T01-02-03-s2.jsonl"));
    let rollout = PathBuf::from("2026/10/30/rollout-2026-10-30T01-02-03-s1.jsonl");
    assert_eq!(
        Codex.session_files(&directory, "s1").unwrap(),
        SessionFiles {
            transcript: Some(rollout.clone()),
            files: vec![rollout],
        }
    );
}

#[test]
fn an_inherited_tool_directory_counts_unless_configured() {
    let home = tempfile::tempdir().unwrap();
    let mut environment = isolated(home.path());
    environment.inherited =
        |name| (name == crate::client::env::CODEX_HOME).then(|| "/codex".into());
    assert_eq!(
        Codex.session_dir(&environment).unwrap(),
        Path::new("/codex/sessions")
    );
    let configured = BTreeMap::from([(
        crate::client::env::CODEX_HOME.to_owned(),
        home.path().join("codex").into_os_string(),
    )]);
    environment.configured = &configured;
    assert_eq!(
        Codex.session_dir(&environment).unwrap(),
        home.path().join("codex/sessions")
    );
}

#[test]
fn a_session_is_found_in_the_first_environment_that_has_it() {
    let home = tempfile::tempdir().unwrap();
    let elsewhere = home.path().join("elsewhere");
    let environments = Environments {
        inherited: |_| None,
        ..Environments::new(
            home.path(),
            &[BTreeMap::from([(
                crate::client::env::CODEX_HOME.to_owned(),
                elsewhere.display().to_string(),
            )])],
        )
    };
    let environments = environments.each();
    let found = |id| find(&Codex, &environments, id).unwrap();
    assert_eq!(found("s1"), None);
    write(&elsewhere.join("sessions/2026/10/30/rollout-1-s1.jsonl"));
    let found = found("s1").unwrap();
    assert_eq!(found.directory, elsewhere.join("sessions"));
    assert_eq!(
        found.files.transcript,
        Some("2026/10/30/rollout-1-s1.jsonl".into())
    );
}

#[test]
fn session_ids_name_one_file() {
    assert!(is_file_name("0f0e-11"));
    for id in ["", "..", "a/b", "/abs", "."] {
        assert!(!is_file_name(id), "{id}");
    }
}

fn reported(agent: &str, session: &str, execution: Option<&str>) -> ReportedSession {
    ReportedSession {
        agent: agent.into(),
        session: session.into(),
        execution: execution.map(Into::into),
        current: execution.is_none(),
    }
}

#[test]
fn each_session_comes_with_the_files_this_machine_has() {
    let home = tempfile::tempdir().unwrap();
    let claude = home.path().join(".claude/projects/-work");
    write(&claude.join("s1.jsonl"));
    write(&claude.join("s1/subagents/one.jsonl"));
    let rollout = home
        .path()
        .join(".codex/sessions/2026/10/30/rollout-2026-10-30T01-02-03-s2.jsonl");
    write(&rollout);
    let sessions = SpaceSessions {
        sessions: vec![
            reported("claude", "s1", Some("e1")),
            reported("codex", "s2", Some("e2")),
            // Moved here from another machine, or deleted.
            reported("claude", "gone", Some("e3")),
            // An agent whose tool Shoal does not know.
            reported("custom", "s4", Some("e4")),
            // A chat's session, which no execution runs.
            reported("claude", "s1", None),
        ],
        integrations: BTreeMap::from([
            ("claude".into(), IntegrationKind::Claude),
            ("codex".into(), IntegrationKind::Codex),
            ("custom".into(), IntegrationKind::None),
        ]),
        environments: Vec::new(),
    };
    let transcripts = locate(home.path(), &sessions);
    let found: Vec<_> = transcripts
        .iter()
        .map(|transcript| (transcript.path.clone(), transcript.files.clone()))
        .collect();
    let transcript = claude.join("s1.jsonl");
    let claude_files = vec![transcript.clone(), claude.join("s1/subagents/one.jsonl")];
    assert_eq!(
        found,
        [
            (Some(transcript.clone()), claude_files.clone()),
            (Some(rollout.clone()), vec![rollout]),
            (None, Vec::new()),
            (None, Vec::new()),
            (Some(transcript), claude_files),
        ]
    );
    let reported: Vec<_> = transcripts
        .into_iter()
        .map(|transcript| {
            assert_eq!(transcript.error, None);
            transcript.session
        })
        .collect();
    assert_eq!(reported, sessions.sessions);
}

#[test]
fn a_session_whose_directory_cannot_be_named_says_why() {
    let home = tempfile::tempdir().unwrap();
    let sessions = SpaceSessions {
        sessions: vec![reported("claude", "s1", Some("e1"))],
        integrations: BTreeMap::from([("claude".into(), IntegrationKind::Claude)]),
        environments: vec![BTreeMap::from([(
            crate::client::env::CLAUDE_CONFIG_DIR.to_owned(),
            "relative".to_owned(),
        )])],
    };
    let [transcript] = locate(home.path(), &sessions).try_into().unwrap();
    let error = transcript.error.unwrap();
    assert!(error.contains("absolute"), "{error}");
    assert!(transcript.files.is_empty());
}
