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
fn a_given_permission_applies_as_given_and_only_from_one_who_holds_it() {
    let settings = settings("", "");
    let worker = resolve(&settings, mode("worker")).unwrap();
    let chat = resolve(&settings, mode("chat")).unwrap();
    let wider = PermissionsConfig::assigned(&["merge=direct".to_owned()]).unwrap();
    // A wider value than the mode's applies, even below a narrower parent.
    let given = worker.below(&worker).given(&wider);
    assert_eq!(given.merge.value, Opening::Direct);
    assert_eq!(given.merge.from, PermissionSource::Given);
    assert_eq!(given.push, worker.push);
    let narrower = PermissionsConfig::assigned(&["push=propose".to_owned()]).unwrap();
    assert_eq!(chat.given(&narrower).push.value, Pushes::Propose);
    // Only one that holds a value gives it.
    assert!(wider.ensure_held(&worker).is_err());
    assert!(wider.ensure_held(&chat).is_ok());
    assert!(narrower.ensure_held(&worker).is_ok());
}

#[test]
fn a_dropped_key_gives_the_value_it_falls_back_to() {
    let settings = settings("", "");
    let chat = resolve(&settings, mode("chat")).unwrap();
    let worker = resolve(&settings, mode("worker")).unwrap();
    let drop_merge = PermissionChange::assigned("merge=mode").unwrap();
    // A chat's mode merges directly, which a worker does not hold.
    let refused = drop_merge.ensure_held(&chat, &worker).unwrap_err();
    assert!(format!("{refused:#}").contains("merge=mode would give merge=direct"));
    assert!(drop_merge.ensure_held(&worker, &worker).is_ok());
    assert!(drop_merge.ensure_held(&chat, &chat).is_ok());
    let set_merge = PermissionChange::assigned("merge=direct").unwrap();
    assert!(set_merge.ensure_held(&worker, &worker).is_err());
    // A registration or restore without a recorded key drops it.
    let recorded =
        PermissionsConfig::assigned(&["merge=propose".to_owned(), "push=deny".to_owned()]).unwrap();
    let given = PermissionsConfig::assigned(&["push=deny".to_owned()]).unwrap();
    assert_eq!(PermissionChange::between(&recorded, &given), drop_merge);
    let between = PermissionChange::between(&given, &recorded);
    assert_eq!(
        (between.set.merge, between.dropped),
        (Some(Opening::Propose), Vec::new())
    );
    assert_eq!(
        PermissionChange::between(&recorded, &recorded),
        PermissionChange::default()
    );
}

#[test]
fn a_permission_change_keeps_the_keys_it_does_not_name() {
    let given =
        PermissionsConfig::assigned(&["push=propose".to_owned(), "merge=propose".to_owned()])
            .unwrap();
    let change = |assignments: &[&str]| {
        let changes: Vec<_> = (assignments.iter())
            .map(|assignment| PermissionChange::assigned(assignment).unwrap())
            .collect();
        PermissionChange::combined(&changes)
    };
    let widened = change(&["push=direct"]).applied_to(given);
    assert_eq!(
        (widened.push, widened.merge),
        (Some(Pushes::Direct), Some(Opening::Propose))
    );
    let dropped = change(&["merge=mode"]).applied_to(given);
    assert_eq!((dropped.push, dropped.merge), (Some(Pushes::Propose), None));
    // A later assignment of a key replaces an earlier one, either way.
    assert_eq!(
        change(&["merge=mode", "merge=direct"])
            .applied_to(given)
            .merge,
        Some(Opening::Direct)
    );
    assert_eq!(
        change(&["merge=direct", "merge=mode"])
            .applied_to(given)
            .merge,
        None
    );
    for refused in ["pushes=mode", "push=never"] {
        assert!(PermissionChange::assigned(refused).is_err());
    }
}
