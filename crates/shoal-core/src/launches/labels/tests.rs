use super::*;
use shoal_protocol::launches::ItemChoice;

#[test]
fn an_items_labels_name_its_mode() {
    let modes = [("plan", "needs-plan"), ("worker", "planned")];
    let mode = |labels: &[&str]| {
        let labels: Vec<String> = labels.iter().map(|label| (*label).into()).collect();
        labelled_mode("issue 7".into(), modes, &labels)
    };
    assert_eq!(
        mode(&["type/bug", "planned"]),
        LabelledMode::Called(ModeName::DEFAULT)
    );
    assert_eq!(mode(&["needs-plan"]), LabelledMode::Called(ModeName::PLAN));
    assert_eq!(mode(&["type/bug"]), LabelledMode::Unlabelled);
    let LabelledMode::Several(several) = mode(&["needs-plan", "planned"]) else {
        panic!("the labels of two modes call for one");
    };
    assert_eq!(several.item, "issue 7");
    assert_eq!(
        several.modes,
        [
            (ModeName::PLAN, "needs-plan".to_owned()),
            (ModeName::DEFAULT, "planned".to_owned())
        ]
    );
}

/// A start on an issue, by number positionally or with `--issue`, takes
/// only a mode that starts in a repository, since a space without one
/// starts on no issue.
#[test]
fn a_start_on_an_issue_number_takes_only_modes_in_a_repository() {
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

/// Only the labels of modes that start the space the other arguments fit
/// count: a PR starts in a workspace, so a chat mode's label is no call.
#[test]
fn only_modes_that_fit_the_arguments_count_their_labels() {
    let shape = |kind, label: &str| ModeShape {
        kind,
        repository: true,
        label: Some(label.into()),
    };
    let modes = ModeShapes::from([
        ("plan".into(), shape(SpaceKind::Chat, "needs-plan")),
        ("worker".into(), shape(SpaceKind::Workspace, "planned")),
    ]);
    let on_pr = Launch {
        target: Some("https://forge.example/o/r/pulls/8".parse().unwrap()),
        ..Launch::default()
    };
    assert_eq!(labelled(&on_pr, &modes).unwrap(), [("worker", "planned")]);
    let on_issue = Launch {
        target: Some("https://forge.example/o/r/issues/7".parse().unwrap()),
        ..Launch::default()
    };
    assert_eq!(labelled(&on_issue, &modes).unwrap().len(), 2);
}
