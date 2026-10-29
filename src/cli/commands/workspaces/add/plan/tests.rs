#[test]
fn a_launch_in_a_workspace_opens_only_that_workspace() {
    use crate::{model::Workspace, state::WorkspaceState};
    let workspace = |repository: &str, branch: &str| {
        Workspace::new_record(
            repository.into(),
            branch.into(),
            format!("/work/{branch}").into(),
            branch.into(),
            WorkspaceState::Ready,
        )
    };
    let child = workspace("repo", "child");
    let plan = super::AddPlan::agent_in(&child, "claude".parse().unwrap(), None, Vec::new(), None);
    assert!(plan.opens(&child));
    // The same branch in another repository, or another branch, is not the target.
    assert!(!plan.opens(&workspace("other", "child")));
    assert!(!plan.opens(&workspace("repo", "sibling")));
    let mut creating =
        super::AddPlan::agent_in(&child, "claude".parse().unwrap(), None, Vec::new(), None);
    creating.creation.branch = Some("new".into());
    assert!(!creating.opens(&child));
}
