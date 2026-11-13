use std::fs;

use super::*;

#[test]
fn agents_get_each_configured_directory_as_they_resolve_it() {
    let settings = |text: &str| {
        let config = crate::config::Config::from_toml(text).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let home = tempfile::tempdir().unwrap();
    let defaults = settings("");
    let tmp = fs::canonicalize("/tmp").unwrap();
    assert_eq!(
        agent_dir_args(&defaults, &"claude".into(), home.path()),
        [OsString::from(format!("--add-dir={}", tmp.display()))]
    );
    assert_eq!(
        agent_dir_args(&defaults, &"codex".into(), home.path()),
        [OsString::from("--add-dir"), tmp.clone().into()]
    );
    // A tool that takes no other working directory gets none.
    assert!(agent_dir_args(&defaults, &"pi".into(), home.path()).is_empty());
    // `~/` names a path under the home directory; one reached again through
    // a symlink counts once.
    let configured = settings("agent.dirs = ['/tmp', '~/cache', '~/link']\n");
    let cache = home.path().join("cache");
    fs::create_dir(&cache).unwrap();
    std::os::unix::fs::symlink(&cache, home.path().join("link")).unwrap();
    assert_eq!(
        agent_dirs(&configured, home.path()),
        BTreeSet::from([tmp.clone(), fs::canonicalize(&cache).unwrap()])
    );
    assert!(agent_dirs(&settings("agent.dirs = []\n"), home.path()).is_empty());
    // A workspace's or chat's own directory follows the configured ones.
    assert_eq!(
        dir_args(
            &defaults,
            &"codex".into(),
            home.path(),
            &[PathBuf::from("/root/repo")]
        ),
        [
            OsString::from("--add-dir"),
            tmp.clone().into(),
            "--add-dir".into(),
            "/root/repo".into()
        ]
    );
}

#[test]
fn a_restored_agent_whose_mode_is_gone_gets_the_default_rules() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let workspace: crate::model::Workspace = serde_json::from_value(serde_json::json!({
        "id": "id", "repository_id": "repo", "name": "topic", "branch": "topic",
        "path": "/work/topic", "state": "ready", "created_at": 0, "updated_at": 0,
    }))
    .unwrap();
    let told = |mode: &modes::ModeName| {
        restored_instruction_args(&settings, &"claude".into(), &workspace, mode)
    };
    let kept = told(&modes::ModeName::DEFAULT);
    assert!(!kept.is_empty());
    assert_eq!(told(&"removed".into()), kept);
}

fn chat(
    mode: impl Into<crate::config::modes::ModeName>,
    repository_id: Option<&str>,
    tmp_dir: Option<std::path::PathBuf>,
) -> Chat {
    Chat {
        narrowing: Default::default(),
        mode: mode.into(),
        number: 1.into(),
        id: "chat-id".into(),
        repository_id: repository_id.map(Into::into),
        name: "talk".into(),
        agent: "codex".into(),
        workspace: None,
        running: true,
        started_at: crate::time::UnixTime::from_seconds(0),
        origin: None,
        tmp_dir,
        root_dir: None,
        session: None,
        links: Default::default(),
        review: Vec::new(),
        completion: None,
        agent_state: None,
        progress: None,
        sleep: None,
        parent: None,
        children: Vec::new(),
        converted: false,
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
        chat_dir_args(&defaults, &"codex".into(), home.path(), &chat),
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
    let setup = chat(crate::config::modes::ModeName::SETUP, None, None);
    let args = chat_dir_args(&defaults, &"codex".into(), home.path(), &setup);
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
    let other = chat(crate::config::modes::ModeName::ORCHESTRATE, None, None);
    let args = chat_dir_args(&defaults, &"codex".into(), home.path(), &other);
    assert_eq!(args, [OsString::from("--add-dir"), tmp]);
}
