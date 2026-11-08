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
fn a_mode_no_toml_layer_defines_has_permissions_and_the_key_defaults() {
    // Such as one only a Markdown file beside the global config names.
    let settings = settings("", "[permissions]\npush = 'propose'\n");
    let undefined = resolve(&settings, mode("markdown-only")).unwrap();
    assert_eq!(undefined, resolve(&settings, None).unwrap());
    assert_eq!(undefined.push.value, Pushes::Propose);
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

#[test]
fn only_direct_permissions_are_held_for_a_decision() {
    let settings = settings("", "");
    let chat = resolve(&settings, mode("chat")).unwrap();
    let worker = resolve(&settings, mode("worker")).unwrap();
    use PermissionKey::{ApiWrites, Merge, Push};
    assert!(chat.withheld(&[Merge, Push]).is_none());
    assert!(worker.withheld(&[Push]).is_none());
    assert!(worker.withheld(&[Merge, Push]).is_some());
    assert!(worker.withheld(&[ApiWrites]).is_some());
}

#[test]
fn a_narrowing_takes_known_keys_and_values_and_round_trips() {
    let assignments = ["push=deny".to_owned(), "merge = propose".to_owned()];
    let narrowing = PermissionsConfig::assigned(&assignments).unwrap();
    assert_eq!(narrowing.push, Some(Pushes::Deny));
    assert_eq!(narrowing.merge, Some(Opening::Propose));
    assert_eq!(
        PermissionsConfig::assigned(&narrowing.assignments()).unwrap(),
        narrowing
    );
    // A later assignment of a key replaces an earlier one.
    let again = ["push=deny".to_owned(), "push=propose".to_owned()];
    let replaced = PermissionsConfig::assigned(&again).unwrap();
    assert_eq!(replaced.push, Some(Pushes::Propose));
    for refused in ["push", "pushes=deny", "push=never", "issue_open=deny"] {
        assert!(PermissionsConfig::assigned(&[refused.to_owned()]).is_err());
    }
}

#[test]
fn a_narrowing_never_widens_the_mode() {
    let settings = settings("", "");
    let worker = resolve(&settings, mode("worker")).unwrap();
    let wider = PermissionsConfig::assigned(&["merge=direct".to_owned()]).unwrap();
    assert!(wider.ensure_within(&worker).is_err());
    let narrower = PermissionsConfig::assigned(&["push=propose".to_owned()]).unwrap();
    assert!(narrower.ensure_within(&worker).is_ok());
    let narrowed = worker.narrowed(&narrower);
    assert_eq!(narrowed.push.value, Pushes::Propose);
    assert_eq!(narrowed.push.from, PermissionSource::Narrowing);
    assert_eq!(narrowed.merge, worker.merge);
}
