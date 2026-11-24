use shoal_protocol::integrations::IntegrationKind;

use super::super::{Leaving, copies::Listed};
use super::told;
use crate::model::{ChatPart, LinkedItem, Space, SpacePart, Workdir, Workspace};

const ISSUE: &str = "https://forge.example/team/repo/issues/7";

/// A workspace on branch `fix-login` that links issue 7.
fn workspace() -> Space {
    let mut workspace = Workspace::new_record(
        "repo".into(),
        "login".into(),
        "/work/fix-login".into(),
        crate::model::Head::Branch("fix-login".into()),
        crate::state::WorkspaceState::Ready,
    );
    workspace.links.issues = vec![LinkedItem {
        url: ISSUE.parse().unwrap(),
        title: Some("Fix the login".into()),
    }];
    Space::from(workspace)
}

/// A chat in the checkout at `/repos/repo`, without links.
fn chat() -> Space {
    let mut chat = Space {
        workdir: Workdir::Checkout {
            path: "/repos/repo".into(),
        },
        part: SpacePart::Chat(ChatPart {
            agent: None,
            started_at: crate::time::UnixTime::now(),
            tmp_dir: None,
            completion: None,
        }),
        ..workspace()
    };
    chat.links = Default::default();
    chat
}

fn claude_stopped(session: &str) -> Leaving {
    Leaving {
        agent: "claude".into(),
        session: Some(session.into()),
        execution: "e1".to_owned().into(),
        origin: None,
    }
}

fn copied(agent: &str, session: &str, transcript: &str, others: usize) -> Listed {
    Listed {
        agent: agent.into(),
        session: session.into(),
        integration: IntegrationKind::Claude,
        transcript: Some(transcript.into()),
        others,
        missing: None,
    }
}

#[test]
fn the_new_tool_learns_the_space_and_where_each_session_is_copied() {
    let listed = [
        copied("codex", "c0", "/files/1/c0.jsonl", 0),
        copied("claude", "s1", "/files/2/s1.jsonl", 2),
        Listed {
            missing: Some("its files are not on this machine".into()),
            transcript: None,
            ..copied("claude", "s0", "-", 0)
        },
    ];
    let told = told(
        &workspace(),
        Some(&claude_stopped("s1")),
        &"codex".into(),
        &listed,
    );
    let [space, sessions] = told.split("\n\n").collect::<Vec<_>>()[..] else {
        panic!("two paragraphs: {told}");
    };
    for fact in [
        "workspace",
        "login",
        "claude",
        "/work/fix-login",
        "fix-login",
        ISSUE,
    ] {
        assert!(space.contains(fact), "{fact}: {space}");
    }
    let lines: Vec<_> = sessions.lines().skip(1).collect();
    assert_eq!(lines.len(), 3, "{sessions}");
    assert!(
        lines[0].starts_with("- codex session c0: /files/1/c0.jsonl"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("s1 (the one just stopped"),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].contains("/files/2/s1.jsonl, with 2 more files"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains("s0: not copied, its files are not on this machine"),
        "{}",
        lines[2]
    );
    assert!(sessions.contains("the worktree"), "{sessions}");
}

#[test]
fn a_space_without_sessions_or_an_agent_is_told_only_what_it_is() {
    let told = told(&chat(), None, &"codex".into(), &[]);
    assert!(!told.contains("\n\n"), "{told}");
    assert!(
        told.contains("/repos/repo") && told.contains("chat"),
        "{told}"
    );
    assert!(!told.contains("started"), "{told}");
}
