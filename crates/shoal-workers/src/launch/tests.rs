use super::*;

/// A chat in the checkout of a registered repository at `/repos/shoal`.
fn chat_in_checkout() -> (crate::model::Chat, crate::model::Repository) {
    let repository = crate::model::Repository {
        id: "repo".into(),
        path: "/repos/shoal".into(),
        source: "/repos/shoal".into(),
        last_used: 0,
        name: Some("shoal".into()),
        workspaces_dir: "/tmp/shoal/repo".into(),
        contribution: false,
        shared_url: None,
    };
    let chat = crate::model::Chat {
        narrowing: Default::default(),
        mode: Some(modes::ModeName::CHAT),
        number: 1.into(),
        id: "chat-id".into(),
        repository_id: Some("repo".into()),
        name: "talk".into(),
        running: true,
        links: Default::default(),
        review: Vec::new(),
        holds: Vec::new(),
        agent_state: None,
        progress: None,
        sleep: None,
        wake_at: None,
        asleep: None,
        restart: None,
        parent: None,
        children: Vec::new(),
        workdir: crate::model::Workdir::Checkout {
            path: "/repos/shoal".into(),
        },
        part: crate::model::ChatPart {
            agent: Some("claude".into()),
            started_at: crate::time::UnixTime::from_seconds(0),
            tmp_dir: None,
            completion: None,
        },
    };
    (chat, repository)
}

/// A chat's agent that takes its rules only in its first message gets
/// them there ahead of what it is told, such as what its converted space
/// is now, while one that takes them apart gets what it is told alone.
#[test]
fn a_chats_agent_gets_its_rules_in_its_first_message_unless_it_takes_them_apart() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let (chat, repository) = chat_in_checkout();
    let space = HomeRecords::Chat {
        chat,
        repository: Some(repository),
        workspace: None,
    };
    let home = space.home().unwrap();
    let opening = Opening::New {
        mode: modes::ModeName::CHAT,
        subject: Some(templates::Subject::told(
            "You now work in a workspace.".into(),
        )),
        note: None,
    };
    let brief = || opening.brief(&settings, &"claude".into(), home).unwrap();

    let joined = delivered(brief(), home, false);
    assert_eq!(joined.first_message, None);
    assert!(joined.system.contains("/repos/shoal"), "{}", joined.system);
    assert!(
        joined.system.ends_with("You now work in a workspace."),
        "{}",
        joined.system
    );

    let apart = delivered(brief(), home, true);
    let told = apart.first_message.unwrap();
    assert!(told.contains("You now work in a workspace."), "{told}");
    assert!(!told.contains("/repos/shoal"), "{told}");
}

/// A handoff's note follows the mode's instructions, which follow what the
/// new session is told.
#[test]
fn a_new_sessions_note_ends_its_first_message() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let (chat, repository) = chat_in_checkout();
    let space = HomeRecords::Chat {
        chat,
        repository: Some(repository),
        workspace: None,
    };
    let home = space.home().unwrap();
    let opening = Opening::New {
        mode: modes::ModeName::DEFAULT,
        subject: Some(templates::Subject::told("Codex takes over.".into())),
        note: Some("Focus on the failing test".into()),
    };
    let brief = opening.brief(&settings, &"codex".into(), home).unwrap();
    let first = brief.first_message.unwrap();
    assert!(first.starts_with("Codex takes over.\n\n"), "{first}");
    assert!(first.ends_with("\n\nFocus on the failing test"), "{first}");
    // The mode's instructions come between them.
    assert_ne!(first, "Codex takes over.\n\nFocus on the failing test");
}
