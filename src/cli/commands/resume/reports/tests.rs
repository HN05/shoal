use super::*;

#[test]
fn the_summary_counts_queued_agents() {
    assert_eq!(summary(1, false), "Queued 1 agent; shoal queue shows them");
    assert_eq!(summary(4, false), "Queued 4 agents; shoal queue shows them");
}

#[test]
fn a_stopped_command_reruns_in_its_workspace_by_number() {
    let workspace = Workspace {
        number: 7.into(),
        ..Workspace::new_record(
            "repo".into(),
            "Fix login".into(),
            "/work/fix-login".into(),
            Some("fix-login".into()),
            crate::state::WorkspaceState::Ready,
        )
    };
    let argv = ["npm".to_owned(), "test".to_owned()];
    assert_eq!(
        rerun(&workspace, &argv),
        "shoal run --workspace 7 -- npm test"
    );
}
