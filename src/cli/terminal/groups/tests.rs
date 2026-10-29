use super::*;

fn settings(global: &str) -> Effective {
    let config: crate::config::Config = toml::from_str(global).unwrap();
    config
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap()
}

/// Settings with `repository` as the repository's own config file.
fn repository_settings(global: &str, repository: &str) -> Effective {
    let config: crate::config::Config = toml::from_str(global).unwrap();
    config
        .resolve(&crate::config::repo::ConfigLayers {
            worktree_file: crate::config::repo::parse(repository).unwrap(),
            ..Default::default()
        })
        .unwrap()
}

fn origin(host: HostName) -> Origin {
    Origin {
        host,
        group: Some("group".into()),
        place: Some("place".into()),
        data: Default::default(),
        owned: false,
    }
}

/// Each fallback's host, and whether it joins the origin itself.
fn order(settings: &Effective, origin: Option<&Origin>) -> Vec<(HostName, bool)> {
    fallbacks(settings, origin)
        .into_iter()
        .map(|fallback| {
            let joins_origin = matches!(fallback.group, Group::Origin(_));
            (fallback.host.host.name(), joins_origin)
        })
        .collect()
}

#[test]
fn a_rex_start_falls_back_to_its_session_then_the_default_host_then_zmx() {
    use HostName::*;
    let rex = origin(Rex);
    assert_eq!(
        order(&settings(""), Some(&rex)),
        [(Rex, true), (Rex, false), (Zmx, false), (Zmx, false)]
    );
    assert_eq!(
        order(&settings("default_host = 'rex'"), None),
        [(Rex, false), (Zmx, false)]
    );
}

#[test]
fn hosts_without_groups_or_turned_off_go_straight_to_the_default_host() {
    use HostName::*;
    assert_eq!(
        order(&settings(""), Some(&origin(Herdr))),
        [(Herdr, true), (Zmx, false), (Zmx, false)]
    );
    assert_eq!(
        order(&settings("[rex]\nnew_window = false"), Some(&origin(Rex))),
        [(Zmx, false), (Zmx, false)]
    );
    assert_eq!(order(&settings(""), None), [(Zmx, false), (Zmx, false)]);
}

#[test]
fn a_default_host_config_turns_off_leaves_queued_starts_to_zmx() {
    use HostName::*;
    for repository in ["[rex]\nenabled = false", "[rex]\nnew_window = false"] {
        let settings = repository_settings("default_host = 'rex'", repository);
        let hosts = order(&settings, None);
        assert!(
            hosts.iter().all(|(host, _)| *host == Zmx),
            "{repository}: {hosts:?}"
        );
    }
    let disabled = repository_settings("default_host = 'rex'", "[rex]\nenabled = false");
    assert_eq!(Configured::default_host(&disabled).host.name(), Zmx);
    // A background start's preflight checks the host queued starts use.
    for repository in ["[rex]\nenabled = false", "[rex]\nnew_window = false"] {
        let settings = repository_settings("default_host = 'rex'", repository);
        assert_eq!(first_queued_host(&settings, None).host.name(), Zmx);
    }
    let rex = repository_settings("default_host = 'rex'", "");
    assert_eq!(first_queued_host(&rex, None).host.name(), Rex);
}
