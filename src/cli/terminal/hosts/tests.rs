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
fn every_default_host_setting_names_a_host() {
    for default in crate::config::repo::DefaultHost::ALL {
        let config: crate::config::Config =
            toml::from_str(&format!("default_host = '{}'", default.as_str())).unwrap();
        let settings = config
            .resolve(&crate::config::repo::ConfigLayers::default())
            .unwrap();
        assert_eq!(
            Configured::default_host(&settings).host.name().as_str(),
            default.as_str()
        );
    }
}
