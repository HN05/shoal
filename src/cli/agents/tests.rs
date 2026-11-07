use std::fs;

use super::*;
use crate::{config::named_commands, test_support::install_test_script};

#[test]
fn agents_are_installed_only_when_every_program_they_start_is_on_path() {
    let bin = tempfile::tempdir().unwrap();
    for program in ["claude", "probe"] {
        let path = bin.path().join(program);
        install_test_script(&path, "#!/bin/sh\n");
    }
    let mut commands = named_commands::defaults();
    for (name, program) in [
        ("probe", "probe".to_owned()),
        ("missing", "missing".to_owned()),
        ("absolute", bin.path().join("probe").display().to_string()),
        ("relative", "./scripts/agent".to_owned()),
    ] {
        commands.insert(name.into(), vec![program]);
    }
    let search_path = bin.path().as_os_str();
    let installed = |name: &str| installed(name, &commands, search_path);

    assert!(installed("claude"));
    assert!(!installed("codex"));
    // Happy needs both its own CLI and the agent it runs.
    #[cfg(feature = "happy")]
    {
        assert!(!installed("happy-claude"));
        install_test_script(&bin.path().join("happy"), "#!/bin/sh\n");
        assert!(installed("happy-claude"));
        assert!(!installed("happy-codex"));
    }

    assert!(installed("probe"));
    assert!(installed("absolute"));
    assert!(installed("relative"));
    assert!(!installed("missing"));
    assert!(!installed("undefined"));
}

fn chat(mode: &str, repository_id: Option<&str>, tmp_dir: Option<std::path::PathBuf>) -> Chat {
    Chat {
        mode: mode.into(),
        number: 1,
        id: "chat-id".into(),
        repository_id: repository_id.map(Into::into),
        name: "talk".into(),
        agent: "codex".into(),
        workspace: None,
        pid: 1,
        started_at: 0,
        origin: None,
        tmp_dir,
        root_dir: None,
        session: None,
        restore: None,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
        agent_state: None,
        parent: None,
        children: Vec::new(),
    }
}

#[test]
fn a_chat_agent_gets_its_temporary_directory_after_the_configured_ones() {
    let defaults = crate::config::Config::from_toml("")
        .unwrap()
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let tmp = fs::canonicalize("/tmp").unwrap();
    let cache = home.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let chat = chat("chat", Some("repo"), Some(cache.join("chat-id")));
    assert_eq!(
        chat_dir_args(&defaults, "codex", home.path(), &chat),
        [
            OsString::from("--add-dir"),
            tmp.into(),
            "--add-dir".into(),
            cache.join("chat-id").into()
        ]
    );
}

#[test]
fn a_setup_chat_without_a_repository_may_write_the_global_config() {
    let defaults = crate::config::Config::from_toml("")
        .unwrap()
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let tmp = OsString::from(fs::canonicalize("/tmp").unwrap());
    let setup = chat(crate::config::modes::SETUP, None, None);
    let args = chat_dir_args(&defaults, "codex", home.path(), &setup);
    let file = crate::config::Config::path_for_home(home.path());
    let config_dir = fs::canonicalize(file.parent().unwrap()).unwrap();
    assert_eq!(
        args,
        [
            "--add-dir".into(),
            tmp.clone(),
            "--add-dir".into(),
            config_dir.into()
        ]
    );
    let other = chat(crate::config::modes::ORCHESTRATE, None, None);
    let args = chat_dir_args(&defaults, "codex", home.path(), &other);
    assert_eq!(args, [OsString::from("--add-dir"), tmp]);
}

#[test]
fn enabled_turns_an_agent_off_and_happys_agents_on() {
    let settings = |text: &str| {
        let config = crate::config::Config::from_toml(text).unwrap();
        config.resolve(&Default::default()).unwrap()
    };
    let off = settings("[agents.claude]\nenabled = false\n");
    assert!(agent_off(&off, "claude").is_some());
    assert!(agent_off(&off, "codex").is_none());
    assert!(!names(&off).contains(&"claude".to_owned()));
    // A turned-off agent does not review either.
    assert!(off.check_reviewer("claude").is_err());
    assert!(off.check_reviewer("codex").is_ok());
    #[cfg(feature = "happy")]
    {
        assert!(agent_off(&off, "happy-codex").is_some());
        let on = settings("[agents.happy]\nenabled = true\n");
        assert!(agent_off(&on, "happy-codex").is_none());
    }
}

#[test]
fn a_reviewer_needs_only_its_review_command_installed() {
    let config = crate::config::Config::from_toml(
        "[agents.present]\ncommand = ['sh']\nreview = ['{prompt}', '{schema}']\n\
         [agents.absent]\ncommand = ['no-such-reviewer']\nreview = ['{prompt}', '{schema}']\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    ensure_reviewer_available("present", &settings).unwrap();
    let refused = ensure_reviewer_available("absent", &settings).unwrap_err();
    assert!(
        refused.to_string().contains("no-such-reviewer"),
        "{refused}"
    );
    assert!(ensure_reviewer_available("unknown", &settings).is_err());
}
