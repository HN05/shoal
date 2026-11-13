use crate::config::{
    Config,
    repo::{ConfigLayer, ConfigLayers, parse},
    resolve::Stack,
};

fn layers(worktree: &str, local: &str) -> ConfigLayers {
    ConfigLayers {
        worktree_file: parse(worktree).unwrap(),
        local_config: parse(local).unwrap(),
        ..Default::default()
    }
}

fn ports(global: &str, layers: &ConfigLayers) -> anyhow::Result<(u16, u16)> {
    let settings = Config::from_toml(global)?.resolve(layers)?;
    Ok((settings.ports.start, settings.ports.end))
}

#[test]
fn a_key_takes_its_targets_value_after_every_layer() {
    let global = "[ports]\nend = { same_as = \"start\" }\n";
    let none = ConfigLayers::default();
    assert_eq!(ports(global, &none).unwrap(), (49152, 49152));
    let start = layers("[ports]\nstart = 50000\n", "");
    assert_eq!(ports(global, &start).unwrap(), (50000, 50000));
    // A higher layer's own value replaces the reference, and a higher
    // reference a lower value.
    let end = layers("[ports]\nstart = 50000\n", "[ports]\nend = 60000\n");
    assert_eq!(ports(global, &end).unwrap(), (50000, 60000));
    let reference = layers(
        "[ports]\nstart = 50000\nend = { same_as = \"start\" }\n",
        "",
    );
    assert_eq!(
        ports("[ports]\nend = 60000\n", &reference).unwrap(),
        (50000, 50000)
    );
}

#[test]
fn a_chain_of_references_ends_at_a_value() {
    // Named ports sit in [ports] beside its options.
    let global = "[ports]\nweb = { same_as = \"api\" }\napi = { same_as = \"db\" }\n\
                  db = { port = 50000 }\n";
    let settings = Config::from_toml(global)
        .unwrap()
        .resolve(&ConfigLayers::default())
        .unwrap();
    assert_eq!(settings.ports.definitions["web"].port, Some(50000));
}

#[test]
fn cycles_missing_keys_and_other_types_name_both_keys() {
    for (text, keys) in [
        (
            "[ports]\nstart = { same_as = \"end\" }\nend = { same_as = \"start\" }\n",
            ["ports.start", "ports.end"],
        ),
        (
            "[ports]\nend = { same_as = \"end\" }\n",
            ["ports.end", "ports.end"],
        ),
        (
            "[ports]\nend = { same_as = \"ned\" }\n",
            ["ports.end", "ports.ned"],
        ),
        (
            "[ports]\nend = { same_as = \"on_conflict\" }\n",
            ["ports.end", "ports.on_conflict"],
        ),
    ] {
        let error = format!("{:#}", ports(text, &ConfigLayers::default()).unwrap_err());
        for key in keys {
            assert!(error.contains(key), "{text}: {error}");
        }
    }
}

#[test]
fn a_layer_keeps_its_references_through_the_protocol_and_reports_its_own_layer() {
    let sent = layers(
        "[ports]\nend = { same_as = \"start\" }\nstart = 50000\n",
        "",
    );
    let received: ConfigLayers =
        serde_json::from_value(serde_json::to_value(&sent).unwrap()).unwrap();
    let global = Config::default();
    let report = Stack::new(&global, &received).unwrap().report().unwrap();
    let end = report
        .iter()
        .find(|entry| entry.key == "ports.end")
        .unwrap();
    assert_eq!(
        (end.value.as_u64(), end.layer),
        (Some(50000), ConfigLayer::WorktreeFile)
    );
}

#[test]
fn the_root_of_a_file_cannot_set_same_as() {
    assert!(parse("same_as = { same_as = \"git_profile\" }\n").is_err());
    assert!(Config::from_toml("same_as = []\n").is_err());
}
