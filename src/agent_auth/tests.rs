use super::*;

#[test]
fn wrappers_resolve_per_tool() {
    let global = crate::config::Config::from_toml(
        "[agent.forge_clis]\nfj = '~/bin/fj-agent'\ngh = '/global/gh-agent'\n",
    )
    .unwrap();
    let layers = crate::config::repo::ConfigLayers {
        worktree_file: crate::config::repo::parse("[agent.forge_clis]\nfj = '/repo/fj-agent'\n")
            .unwrap(),
        local_config: crate::config::repo::parse("[agent.forge_clis]\nfj = '/saved/fj-agent'\n")
            .unwrap(),
    };
    let effective = global.resolve(&layers).unwrap();
    assert_eq!(
        effective.agent.forge_clis.fj,
        Some("/saved/fj-agent".into())
    );
    assert_eq!(
        effective.agent.forge_clis.gh,
        Some("/global/gh-agent".into())
    );
}

#[test]
fn forge_identity_names_the_logins() {
    let logins = |gh: Option<&str>| ForgeClis {
        gh: gh.map(Into::into),
        ..Default::default()
    };
    assert_eq!(logins(None).forge_identity().unwrap(), None);
    let agent = logins(Some("/bin/gh-agent")).forge_identity().unwrap();
    assert!(agent.is_some());
    assert_ne!(
        logins(Some("/bin/gh-other")).forge_identity().unwrap(),
        agent
    );
}

#[test]
fn wrapper_paths_must_not_depend_on_the_launch_directory_or_path() {
    for path in ["", "fj", "./fj", "~other/fj"] {
        let config = ForgeClis {
            fj: Some(path.into()),
            ..Default::default()
        };
        assert!(config.validate().is_err(), "{path}");
    }
    assert!(crate::config::repo::parse("[agent.forge_clis]\ngh = 'gh'").is_err());
    assert!(crate::config::repo::parse("[agent.forge_clis]\nunknown = '/bin/gh'").is_err());
}
