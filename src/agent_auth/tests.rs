use super::*;

#[test]
fn wrappers_resolve_per_tool() {
    let global: crate::config::Config =
        toml::from_str("[agent_auth]\nfj = '~/bin/fj-agent'\ngh = '/global/gh-agent'\n").unwrap();
    let layers = crate::config::repo::ConfigLayers {
        worktree_file: crate::config::repo::parse("[agent_auth]\nfj = '/repo/fj-agent'\n").unwrap(),
        saved_repository_config: crate::config::repo::parse(
            "[agent_auth]\nfj = '/saved/fj-agent'\n",
        )
        .unwrap(),
    };
    let effective = global.resolve(&layers).unwrap();
    assert_eq!(effective.agent_auth.fj, Some("/saved/fj-agent".into()));
    assert_eq!(effective.agent_auth.gh, Some("/global/gh-agent".into()));
}

#[test]
fn forge_identity_names_the_logins_and_ignores_the_git_profile() {
    let profile = |gh: Option<&str>, git_profile: &str| Config {
        gh: gh.map(Into::into),
        git_profile: Some(git_profile.into()),
        ..Default::default()
    };
    assert_eq!(profile(None, "work").forge_identity().unwrap(), None);
    let agent = profile(Some("/bin/gh-agent"), "work")
        .forge_identity()
        .unwrap();
    assert!(agent.is_some());
    assert_eq!(
        profile(Some("/bin/gh-agent"), "home")
            .forge_identity()
            .unwrap(),
        agent
    );
    assert_ne!(
        profile(Some("/bin/gh-other"), "work")
            .forge_identity()
            .unwrap(),
        agent
    );
}

#[test]
fn wrapper_paths_must_not_depend_on_the_launch_directory_or_path() {
    for path in ["", "fj", "./fj", "~other/fj"] {
        let config = Config {
            fj: Some(path.into()),
            ..Default::default()
        };
        assert!(config.validate().is_err(), "{path}");
    }
    assert!(crate::config::repo::parse("[agent_auth]\ngh = 'gh'").is_err());
    assert!(crate::config::repo::parse("[agent_auth]\nunknown = '/bin/gh'").is_err());
}
