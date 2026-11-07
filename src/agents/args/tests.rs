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
        agent_dir_args(&defaults, "claude", home.path()),
        [OsString::from(format!("--add-dir={}", tmp.display()))]
    );
    assert_eq!(
        agent_dir_args(&defaults, "codex", home.path()),
        [OsString::from("--add-dir"), tmp.clone().into()]
    );
    // A tool that takes no other working directory gets none.
    assert!(agent_dir_args(&defaults, "pi", home.path()).is_empty());
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
            "codex",
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
    let told = |mode| restored_instruction_args(&settings, "claude", &workspace, mode);
    let kept = told(modes::DEFAULT);
    assert!(!kept.is_empty());
    assert_eq!(told("removed"), kept);
}
