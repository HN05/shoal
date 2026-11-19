use super::*;
use crate::agents::agent_off;

#[test]
fn enabled_turns_an_agent_off_and_happys_agents_on() {
    let settings = |text: &str| {
        let config = crate::config::Config::from_toml(text).unwrap();
        config.resolve(&Default::default()).unwrap()
    };
    let off = settings("[agents.claude]\nenabled = false\n");
    assert!(agent_off(&off, &"claude".into()).is_some());
    assert!(agent_off(&off, &"codex".into()).is_none());
    assert!(!names(&off).contains(&"claude".into()));
    // A turned-off agent does not review either.
    assert!(off.check_reviewer(&"claude".into()).is_err());
    assert!(off.check_reviewer(&"codex".into()).is_ok());
    assert!(agent_off(&off, &"happy-codex".into()).is_some());
    let on = settings("[agents.happy]\nenabled = true\n");
    assert!(agent_off(&on, &"happy-codex".into()).is_none());
}

#[test]
fn a_reviewer_needs_only_its_review_command_installed() {
    let config = crate::config::Config::from_toml(
        "[agents.present]\ncommand = ['sh']\nreview = ['{prompt}', '{schema}']\n\
         [agents.absent]\ncommand = ['no-such-reviewer']\nreview = ['{prompt}', '{schema}']\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    ensure_reviewer_available(&"present".into(), &settings).unwrap();
    let refused = ensure_reviewer_available(&"absent".into(), &settings).unwrap_err();
    assert!(
        refused.to_string().contains("no-such-reviewer"),
        "{refused}"
    );
    assert!(ensure_reviewer_available(&"unknown".into(), &settings).is_err());
}

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
