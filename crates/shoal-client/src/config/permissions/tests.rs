use super::*;
use crate::config::{
    Config, Effective,
    repo::{ConfigLayers, parse},
};

/// The settings of a repository whose local config is `repository`, under
/// the global config `global`.
fn settings(global: &str, repository: &str) -> Effective {
    let layers = ConfigLayers {
        local_config: parse(repository).unwrap(),
        ..Default::default()
    };
    Config::from_toml(global).unwrap().resolve(&layers).unwrap()
}

fn mode(name: &str) -> Option<SpaceMode<'_>> {
    Some(SpaceMode {
        name,
        kind: SpaceKind::Chat,
        repository: true,
    })
}

#[test]
fn a_mode_permission_in_any_layer_wins_over_permissions_in_any_layer() {
    // The global config's mode beats the repository's [permissions].
    let settings = settings(
        "[modes.chat.permissions]\nmerge = 'propose'\n",
        "[permissions]\nmerge = 'direct'\npush = 'propose'\n",
    );
    let chat = resolve(&settings, mode("chat")).unwrap();
    assert_eq!(chat.merge.value, Opening::Propose);
    // A key the mode leaves to [permissions] comes from it.
    assert_eq!(chat.push.value, Pushes::Propose);
}

#[test]
fn permissions_win_over_a_built_in_mode_default() {
    let settings = settings("", "[permissions]\nmerge = 'propose'\n");
    let chat = resolve(&settings, mode("chat")).unwrap();
    assert_eq!(chat.merge.value, Opening::Propose);
    let settings = self::settings("", "");
    let chat = resolve(&settings, mode("chat")).unwrap();
    let worker = resolve(&settings, mode("worker")).unwrap();
    assert!(chat.merge.value > worker.merge.value);
}

#[test]
fn a_mode_of_the_user_and_a_space_without_one_have_the_key_defaults() {
    let settings = settings("[modes.triage]\nkind = 'chat'\n", "");
    let triage = resolve(&settings, mode("triage")).unwrap();
    let worker = resolve(&settings, mode("worker")).unwrap();
    assert_eq!(triage, worker);
    assert_eq!(resolve(&settings, None).unwrap(), worker);
}

#[test]
fn a_setup_mode_has_the_permissions_of_its_base() {
    let settings = settings("[modes.orchestrate.permissions]\npush = 'deny'\n", "");
    let global = SpaceMode {
        name: modes::SETUP,
        kind: SpaceKind::Chat,
        repository: false,
    };
    let orchestrate = SpaceMode {
        name: modes::ORCHESTRATE,
        ..global
    };
    assert_eq!(
        resolve(&settings, Some(global)).unwrap(),
        resolve(&settings, Some(orchestrate)).unwrap()
    );
}

#[test]
fn a_mode_no_config_defines_is_refused() {
    let settings = settings("", "");
    assert!(resolve(&settings, mode("gone")).is_err());
}

#[test]
fn unknown_permission_keys_and_values_are_refused() {
    for config in [
        "[modes.chat.permissions]\ndelete = 'direct'\n",
        "[modes.chat.permissions]\npush = 'always'\n",
        "[permissions]\napi_writes = 'propose'\n",
    ] {
        assert!(parse(config).is_err(), "{config}");
    }
}

#[test]
fn a_space_below_another_takes_the_narrower_permission_from_its_parent() {
    let settings = settings("", "");
    let chat = resolve(&settings, mode("chat")).unwrap();
    let worker = resolve(&settings, mode("worker")).unwrap();
    let below = chat.below(&worker);
    assert_eq!(below.merge.value, worker.merge.value);
    assert_eq!(below.merge.from, PermissionSource::Parent);
    // Where the parent allows as much, the space's own mode decides.
    assert_eq!(below.push, chat.push);
    assert_eq!(below.push.from, PermissionSource::Mode);
    // A parent that allows more widens nothing.
    assert_eq!(worker.below(&chat), worker);
}
