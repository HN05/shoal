use super::*;

fn settings(global: &str) -> Effective {
    let config: crate::config::Config = toml::from_str(global).unwrap();
    config
        .resolve(&crate::config::repo::ConfigLayers::default())
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
fn hosts_without_groups_or_turned_off_go_straight_to_the_default_host() {
    use HostName::*;
    assert_eq!(
        order(&settings(""), Some(&origin(Herdr))),
        [(Herdr, true), (Zmx, false), (Zmx, false)]
    );
    assert_eq!(order(&settings(""), None), [(Zmx, false), (Zmx, false)]);
}
