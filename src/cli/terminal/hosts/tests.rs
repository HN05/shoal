use super::*;

#[test]
fn only_a_start_from_the_callers_place_splits_it_where_config_asks() {
    let resolved = |global: &str| {
        let config: crate::config::Config = toml::from_str(global).unwrap();
        config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap()
    };
    let splits = resolved("[rex]\nplacement = 'split'\n");
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
        let parsed = toml::from_str::<crate::config::Config>(&format!("default_host = '{name}'"));
        let Some(registered) = registration(name) else {
            assert!(parsed.is_err(), "{name}");
            continue;
        };
        let settings = parsed
            .unwrap()
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap();
        assert_eq!(Configured::default_host(&settings).host.name(), name);
        assert_eq!(registered.host.name(), name);
    }
}

#[test]
fn a_host_serving_a_place_of_its_own_opens_places_beside_it_unless_it_cannot() {
    let config: crate::config::Config = toml::from_str("").unwrap();
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
