use super::*;
use crate::sessions::HostName;

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
        instance: None,
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
fn a_start_falls_back_to_its_group_then_the_default_host() {
    use HostName::*;
    for host in [Herdr, Rex] {
        assert_eq!(
            order(&settings(""), Some(&origin(host))),
            [(host, true), (host, false), (Zmx, false)]
        );
    }
    assert_eq!(
        order(&settings("default_host = 'rex'"), None),
        [(Rex, false)]
    );
    assert_eq!(
        order(&settings("default_host = 'herdr'"), Some(&origin(Rex))),
        [(Rex, true), (Rex, false), (Herdr, false)]
    );
}

#[test]
fn hosts_turned_off_go_straight_to_the_default_host() {
    use HostName::*;
    assert_eq!(
        order(&settings("[rex]\nnew_window = false"), Some(&origin(Rex))),
        [(Zmx, false)]
    );
    assert_eq!(
        order(&settings("[herdr]\nnew_tab = false"), Some(&origin(Herdr))),
        [(Zmx, false)]
    );
    assert_eq!(order(&settings(""), None), [(Zmx, false)]);
}

#[test]
fn a_start_queued_from_a_place_outside_a_group_goes_to_the_default_host() {
    use HostName::*;
    let session = Origin::created(Zmx, "session".into());
    assert_eq!(order(&settings(""), Some(&session)), [(Zmx, false)]);
    assert_eq!(
        order(&settings("default_host = 'herdr'"), Some(&session)),
        [(Herdr, false)]
    );
    let ungrouped = Origin::created(Rex, "block".into());
    assert_eq!(
        order(&settings("default_host = 'herdr'"), Some(&ungrouped)),
        [(Herdr, false)]
    );
}

#[test]
fn a_default_host_config_turns_off_leaves_queued_starts_no_host() {
    use HostName::*;
    let turned_off = [
        ("default_host = 'rex'", "[rex]\nenabled = false"),
        ("default_host = 'rex'", "[rex]\nnew_window = false"),
        ("default_host = 'herdr'", "[herdr]\nenabled = false"),
        ("default_host = 'herdr'", "[herdr]\nnew_tab = false"),
    ];
    for (global, repository) in turned_off {
        let settings = repository_settings(global, repository);
        assert_eq!(order(&settings, None), [], "{repository}");
        // A background start's preflight refuses what no host would start.
        assert!(first_queued_host(&settings, None).is_none(), "{repository}");
    }
    // A start from a grouped place still joins its group.
    let settings = repository_settings("default_host = 'herdr'", "[herdr]\nenabled = false");
    assert_eq!(
        order(&settings, Some(&origin(Rex))),
        [(Rex, true), (Rex, false)]
    );
    for (global, host) in [
        ("", Zmx),
        ("default_host = 'rex'", Rex),
        ("default_host = 'herdr'", Herdr),
    ] {
        let settings = repository_settings(global, "");
        let first = first_queued_host(&settings, None).map(|first| first.host.name());
        assert_eq!(first, Some(host));
    }
}
