use super::{ItemInput, new_space_flag};
use crate::{
    cli::commands::workspaces::AddOptions,
    client::space_names::SpaceSelector,
    protocol::CopySource,
    start_plan::{CopyCreation, Creation},
};

#[test]
fn a_start_into_a_workspaces_branch_takes_only_a_forks_parent() {
    let parent = || Some(SpaceSelector::parse("3").unwrap());
    let options = |fork: bool| AddOptions {
        parent: parent(),
        fork: fork.then(|| "session".into()),
        ..AddOptions::default()
    };
    let flag = |creation: &Creation, options: &AddOptions| new_space_flag(creation, None, options);
    // A fork's parent names the workspace whose agent it forks.
    assert_eq!(flag(&Creation::default(), &options(true)), None);
    assert_eq!(
        flag(&Creation::default(), &options(false)),
        Some("--parent")
    );
    // A copy's fork forks the copied workspace's agent, so its parent is the
    // new workspace's.
    let copy = Creation {
        copy: Some(CopyCreation {
            source: CopySource {
                workspace: SpaceSelector::parse("4").unwrap(),
                checkpoint: None,
            },
            name: "copy".into(),
        }),
        ..Creation::default()
    };
    assert_eq!(flag(&copy, &options(true)), Some("--parent"));
    assert_eq!(
        new_space_flag(
            &Creation::default(),
            Some(&ItemInput::Number(7)),
            &options(true)
        ),
        Some("--issue")
    );
}
