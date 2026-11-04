use crate::protocol::operations::QueuedLaunch;

fn claude() -> QueuedLaunch {
    QueuedLaunch {
        agent: "claude".into(),
        ..Default::default()
    }
}

#[test]
fn a_launch_in_a_workspace_opens_only_that_workspace() {
    use crate::{model::Workspace, state::WorkspaceState};
    let workspace = |repository: &str, branch: &str| {
        Workspace::new_record(
            repository.into(),
            branch.into(),
            format!("/work/{branch}").into(),
            Some(branch.into()),
            WorkspaceState::Ready,
        )
    };
    let child = workspace("repo", "child");
    let plan = super::AddPlan::agent_in(&child, claude()).expect("claude is a built-in agent");
    assert!(plan.opens(&child));
    // The same branch in another repository, or another branch, is not the target.
    assert!(!plan.opens(&workspace("other", "child")));
    assert!(!plan.opens(&workspace("repo", "sibling")));
    let mut creating =
        super::AddPlan::agent_in(&child, claude()).expect("claude is a built-in agent");
    creating.creation.branch = Some("new".into());
    assert!(!creating.opens(&child));
}

#[test]
fn a_queued_launch_keeps_its_model_arguments_fork_issue_and_place() {
    use crate::{
        agent::ModelChoice, model::Workspace, protocol::operations::LaunchIssue,
        state::WorkspaceState,
    };
    let child = Workspace::new_record(
        "repo".into(),
        "child".into(),
        "/work/child".into(),
        Some("child".into()),
        WorkspaceState::Ready,
    );
    let model = ModelChoice {
        model: Some("opus".into()),
        effort: Some("high".into()),
    };
    let launch = QueuedLaunch {
        model: model.clone(),
        args: vec!["Take the child part".into()],
        fork: Some("parent-session".into()),
        issue: Some(LaunchIssue {
            url: "https://forge.example/team/project/issues/7".into(),
            title: "Split the API".into(),
        }),
        env: Some([("PLACE".to_owned(), "beside".to_owned())].into()),
        ..claude()
    };
    let request = super::queued_agent_request(&child, launch).expect("the launch queues");
    assert_eq!(request.agent, "claude");
    assert_eq!(request.issue_title.as_deref(), Some("Split the API"));
    assert_eq!(request.workspace.as_deref(), Some(child.id.as_str()));
    assert_eq!(request.env["PLACE"], "beside");
    let plan: super::AddPlan = serde_json::from_str(&request.plan).expect("the plan parses");
    assert!(plan.opens(&child));
    assert_eq!(plan.agent.model, model);
    assert_eq!(plan.args, ["Take the child part"]);
    assert_eq!(plan.fork.as_deref(), Some("parent-session"));
    assert_eq!(
        plan.issue_url(),
        Some("https://forge.example/team/project/issues/7")
    );

    // A launch that names no place starts as one from no terminal does.
    let request = super::queued_agent_request(&child, claude()).expect("the launch queues");
    assert_eq!(
        request.env,
        crate::cli::terminal::detached_start_environment(None)
    );
}
