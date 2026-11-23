use super::*;

#[test]
fn only_a_start_from_the_callers_place_splits_it_where_config_asks() {
    let resolved = |global: &str| {
        let config = crate::config::Config::from_toml(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let splits = resolved("[terminal.rex]\nplacement = 'split'\n");
    let windows = resolved("");
    let block = Origin {
        group: Some("session:1".into()),
        ..Origin::created(HostName::Rex, "block:1".into())
    };
    let session = Origin {
        place: None,
        ..block.clone()
    };
    let rex = |settings| Configured::named(settings, HostName::Rex);
    assert_eq!(rex(&splits).beside(Some(&block)).split, Some("block:1"));
    assert_eq!(rex(&splits).beside(Some(&session)).split, None);
    assert_eq!(rex(&splits).beside(None).split, None);
    assert_eq!(rex(&windows).beside(Some(&block)).split, None);
    let herdr = Configured::named(&splits, HostName::Herdr);
    assert_eq!(herdr.beside(Some(&block)).split, None);
}

#[test]
fn every_host_but_the_plain_terminal_can_be_the_default() {
    for &name in HostName::ALL {
        let parsed = crate::config::Config::from_toml(&format!("terminal.default_host = '{name}'"));
        let Some(registered) = registration(name) else {
            assert!(parsed.is_err(), "{name}");
            continue;
        };
        let settings = parsed
            .unwrap()
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap();
        let host = Configured::default_host(&settings).map(|host| host.host.name());
        assert_eq!(host, Some(name));
        assert_eq!(registered.host.name(), name);
    }
}

#[test]
fn a_default_host_config_turns_off_is_none() {
    let config = crate::config::Config::from_toml(
        "terminal.default_host = 'herdr'\n[terminal.herdr]\nenabled = false",
    )
    .unwrap();
    let settings = config
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap();
    assert!(Configured::default_host(&settings).is_none());
}

#[test]
fn a_host_serving_a_place_of_its_own_opens_places_beside_it_unless_it_cannot() {
    let config = crate::config::Config::from_toml("").unwrap();
    let settings = config
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap();
    let serving = |name| {
        Configured::named(&settings, name)
            .serving()
            .places()
            .is_some()
    };
    assert!(serving(HostName::Herdr));
    assert!(serving(HostName::Rex));
    // Starts in a zmx session run in place.
    assert!(!serving(HostName::Zmx));
    assert!(
        Configured::named(&settings, HostName::Zmx)
            .places()
            .is_some()
    );
}

#[test]
fn a_place_keeps_none_of_its_openers_scope() {
    for host in hosts() {
        let dropped = foreign_place_variables(host.name());
        assert!(
            dropped.contains(&crate::env::SCOPE_TOKEN),
            "{:?}",
            host.name()
        );
    }
}

#[test]
fn a_hosts_nested_zmx_overrides_the_top_level_one() {
    let resolved = |global: &str| {
        let config = crate::config::Config::from_toml(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let nests = |settings, name| {
        Configured::named(settings, name)
            .nesting()
            .map(|host| host.name())
    };
    let off = resolved("");
    assert_eq!(nests(&off, HostName::Herdr), None);
    let on = resolved("[terminal]\nnested_zmx = true\n[terminal.rex]\nnested_zmx = false\n");
    for name in [HostName::Herdr, HostName::Tmux, HostName::Zellij] {
        assert_eq!(nests(&on, name), Some(HostName::Zmx), "{name}");
    }
    assert_eq!(nests(&on, HostName::Rex), None);
    // zmx and the plain terminal run agents in sessions of their own already.
    assert_eq!(nests(&on, HostName::Zmx), None);
    assert_eq!(nests(&on, HostName::Terminal), None);
    let tmux_only = resolved("[terminal.tmux]\nnested_zmx = true\n");
    assert_eq!(nests(&tmux_only, HostName::Tmux), Some(HostName::Zmx));
    assert_eq!(nests(&tmux_only, HostName::Herdr), None);
}

#[test]
fn only_a_place_its_host_serves_nests_agent_sessions() {
    let resolved = |global: &str| {
        let config = crate::config::Config::from_toml(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let session_hosts = |settings, name| -> Vec<HostName> {
        Configured::named(settings, name)
            .session_hosts()
            .map(Host::name)
            .collect()
    };
    let nested = resolved("[terminal]\nnested_zmx = true\n[terminal.tmux]\nenabled = true\n");
    let in_place = resolved("[terminal.tmux]\nenabled = true\n");
    // SAFETY: nextest runs each test in a process of its own, whose other
    // threads leave the environment alone.
    unsafe {
        forget_inherited_places();
        std::env::set_var("TMUX", "/tmp/tmux-1000/default,4242,0");
        std::env::set_var("TMUX_PANE", "%3");
    }
    assert_eq!(
        session_hosts(&nested, HostName::Tmux),
        [HostName::Tmux, HostName::Zmx]
    );
    assert_eq!(session_hosts(&in_place, HostName::Tmux), [HostName::Tmux]);
    // A default host opens places of its own for a terminal it does not
    // serve, which the option leaves alone.
    assert_eq!(session_hosts(&nested, HostName::Herdr), [HostName::Herdr]);
}

#[test]
fn a_nested_session_keeps_only_the_variables_of_the_place_it_nests_in() {
    let dropped = nested_session_variables(HostName::Zmx, HostName::Tmux);
    assert!(!dropped.contains(&"TMUX_PANE"));
    assert!(dropped.contains(&"HERDR_PANE_ID"));
    assert!(dropped.contains(&crate::env::SCOPE_TOKEN));
    assert!(dropped.contains(&crate::env::NESTED_SESSION));
}
