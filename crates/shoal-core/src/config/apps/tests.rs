use crate::config::{
    Config,
    repo::{self, ConfigLayer, ConfigLayers},
    resolve::Stack,
};

/// The global config's `[apps.quay]` under a checkout's file and a local
/// config that each set part of it.
fn layered() -> (Config, ConfigLayers) {
    let global = Config::from_toml(
        "[apps.quay]\nboard = 'main'\ncolumns = ['todo']\n[apps.quay.colors]\ntodo = 'grey'\ndone = 'green'\n",
    )
    .unwrap();
    let layers = ConfigLayers {
        worktree_file: repo::parse("[apps.quay.colors]\ndone = 'blue'\n[apps.other]\nx = 1\n")
            .unwrap(),
        local_config: repo::parse("[apps.quay]\ncolumns = ['todo', 'done']\ncolors = 'none'\n")
            .unwrap(),
        ..Default::default()
    };
    (global, layers)
}

#[test]
fn an_app_table_merges_key_by_key_and_a_value_replaces_what_is_below() {
    let (global, layers) = layered();
    let settings = global.resolve(&layers).unwrap();
    let quay = settings.apps["quay"].clone();
    let expected: toml::Table =
        toml::from_str("board = 'main'\ncolumns = ['todo', 'done']\ncolors = 'none'\n").unwrap();
    assert_eq!(quay, expected);
    assert_eq!(settings.apps["other"]["x"].as_integer(), Some(1));
    // Without the local config's value, the tables below merge.
    let layers = ConfigLayers {
        local_config: Default::default(),
        ..layers
    };
    let settings = global.resolve(&layers).unwrap();
    let colors = settings.apps["quay"]["colors"].as_table().unwrap();
    assert_eq!(colors["todo"].as_str(), Some("grey"));
    assert_eq!(colors["done"].as_str(), Some("blue"));
}

#[test]
fn each_value_of_an_app_table_reports_the_layer_it_comes_from() {
    let (global, layers) = layered();
    let layers = ConfigLayers {
        local_config: repo::parse("[apps.quay]\ncolumns = ['done']\n").unwrap(),
        ..layers
    };
    let report = Stack::new(&global, &layers).unwrap().report().unwrap();
    let layer = |key: &str| {
        (report.iter())
            .find(|entry| entry.key == key)
            .map(|entry| entry.layer)
    };
    assert_eq!(layer("apps.quay.board"), Some(ConfigLayer::GlobalConfig));
    assert_eq!(layer("apps.quay.columns"), Some(ConfigLayer::LocalConfig));
    assert_eq!(
        layer("apps.quay.colors.todo"),
        Some(ConfigLayer::GlobalConfig)
    );
    assert_eq!(
        layer("apps.quay.colors.done"),
        Some(ConfigLayer::WorktreeFile)
    );
}

#[test]
fn an_app_entry_that_is_not_a_table_is_refused() {
    assert!(repo::parse("[apps]\nquay = 1\n").is_err());
}
