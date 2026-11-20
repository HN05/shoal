use super::{ensure_fits, fit};
use shoal_protocol::launches::{ItemChoice, Launch};

/// A start on an issue, by number positionally or with `--issue`, picks
/// only among the modes that start in a repository, since a space
/// without one starts on no issue.
#[test]
fn a_start_on_an_issue_number_picks_only_modes_in_a_repository() {
    let positional = Launch {
        target: Some("37".parse().unwrap()),
        ..Launch::default()
    };
    let flagged = Launch {
        issue: Some(ItemChoice::Named("37".parse().unwrap())),
        ..Launch::default()
    };
    for launch in [positional, flagged] {
        assert_eq!(fit(&launch).unwrap().repository, Some(true), "{launch:?}");
    }
    assert_eq!(fit(&Launch::default()).unwrap().repository, None);
}

/// A named mode refuses what its kind of space cannot take, such as a
/// branch for a chat, before a picker asks for a repository.
#[test]
fn a_chat_mode_refuses_a_branch_before_any_picker() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let modes = crate::config::modes::shapes(&settings.modes);
    let launch = Launch {
        branch: Some("fix".into()),
        ..Launch::default()
    };
    let error = ensure_fits(&launch, &"chat".into(), &modes).unwrap_err();
    assert!(error.to_string().starts_with("--branch "), "{error}");
    ensure_fits(&launch, &"worker".into(), &modes).unwrap();
}
