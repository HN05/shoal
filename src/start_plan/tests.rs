use crate::client::space_names::SpaceSelector;
use crate::protocol::operations::QueuedLaunch;

fn claude() -> QueuedLaunch {
    QueuedLaunch::new("claude".into())
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
    assert_eq!(request.workspace, Some(SpaceSelector::id(&child.id)));
    assert_eq!(request.env["PLACE"], "beside");
    let plan: super::AddPlan = serde_json::from_str(&request.plan).expect("the plan parses");
    assert!(plan.opens(&child));
    assert_eq!(plan.agent.model, model);
    assert_eq!(plan.args, ["Take the child part"]);
    assert_eq!(plan.fork, Some("parent-session".into()));
    assert_eq!(
        plan.issue_url(),
        Some("https://forge.example/team/project/issues/7")
    );

    // A launch that names no place starts as one from no terminal does.
    let request = super::queued_agent_request(&child, claude()).expect("the launch queues");
    assert_eq!(
        request.env,
        crate::terminal::detached_start_environment(None)
    );
}

#[test]
fn a_plan_queued_by_an_earlier_version_reads_and_writes_back_unchanged() {
    let queued = r#"{"repository":"repo","creation":{"path":null,"branch":null,"existing":"child","base":null,"git_profile":null,"pr":null,"options":{"skip_setup":false,"disposable":false}},"place_name":null,"issue":"https://forge.example/team/project/issues/7","agent":{"agent":"claude","model":{"model":"opus","effort":"high"}},"args":[{"Unix":[84,97,107,101,32,116,104,101,32,99,104,105,108,100,32,112,97,114,116]}],"prompt":"Split it","mode":"plan","fork":"parent-session","queue_entry":null,"chat":null,"parent":null,"workspace":"19d40cc0-b385-4221-a22c-bc3daa9f5ea7"}"#;
    let plan: super::AddPlan = serde_json::from_str(queued).expect("the queued plan parses");
    assert_eq!(
        plan.agent_name().as_ref().map(|agent| agent.name()),
        Some("claude")
    );
    assert_eq!(plan.args, ["Take the child part"]);
    assert_eq!(plan.mode.as_ref().map(|mode| mode.as_str()), Some("plan"));
    assert_eq!(serde_json::to_string(&plan).unwrap(), queued);
}

#[test]
fn plans_queued_before_every_launch_ran_tracked_still_decode() {
    let decode = |json: &str| serde_json::from_str::<super::ResolvedAddAgent>(json).unwrap();
    for json in [
        r#"{"agent":"codex","codex_mode":"app"}"#,
        r#"{"agent":"codex","tracked":true}"#,
    ] {
        assert_eq!(decode(json).agent, Some("codex".parse().unwrap()));
    }
}

#[test]
fn a_finished_dependency_s_branch_is_the_base_of_new_branches_but_not_of_copies() {
    let mut new = super::Creation::default();
    new.build_on(Some("dependency".into()));
    assert_eq!(new.base.as_deref(), Some("dependency"));
    let mut copy = super::Creation {
        copy: Some(super::CopyCreation {
            source: crate::protocol::CopySource {
                workspace: "topic".into(),
                checkpoint: None,
            },
            name: "topic-copy".into(),
        }),
        ..Default::default()
    };
    copy.build_on(Some("dependency".into()));
    assert_eq!(copy.base, None);
}
