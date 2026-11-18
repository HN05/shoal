use crate::protocol::operations::QueuedLaunch;
use shoal_protocol::space_names::SpaceSelector;

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
            crate::model::Head::Branch(branch.into()),
            WorkspaceState::Ready,
        )
    };
    let child = workspace("repo", "child");
    let plan = super::AddPlan::agent_in(&child, claude()).expect("claude is a built-in agent");
    assert!(plan.opens(&child));
    // Another workspace, even on the same branch, is not the target.
    assert!(!plan.opens(&workspace("other", "child")));
    assert!(!plan.opens(&workspace("repo", "child")));
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
        crate::model::Head::Branch("child".into()),
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
            url: "https://forge.example/team/project/issues/7"
                .parse()
                .unwrap(),
            title: "Split the API".into(),
            labels: Vec::new(),
        }),
        env: Some([("PLACE".to_owned(), "beside".to_owned())].into()),
        ..claude()
    };
    let request = super::queued_agent_request(&child, launch, None).expect("the launch queues");
    assert_eq!(request.agent, "claude");
    assert_eq!(request.issue_title.as_deref(), Some("Split the API"));
    assert_eq!(request.space, SpaceSelector::id(&child.id));
    assert_eq!(request.env["PLACE"], "beside");
    let start = super::QueuedStart::of(&request.plan).expect("the plan parses");
    let plan = super::AddPlan::queued(&child, start, "entry".into()).expect("the plan runs");
    assert!(plan.opens(&child));
    assert_eq!(plan.agent.model, model);
    assert_eq!(plan.args, ["Take the child part"]);
    assert_eq!(plan.fork, Some("parent-session".into()));
    assert_eq!(
        plan.issue().map(|issue| issue.url()),
        Some("https://forge.example/team/project/issues/7")
    );

    // A launch that names no place starts as one from no terminal does.
    let request = super::queued_agent_request(&child, claude(), None).expect("the launch queues");
    assert_eq!(
        request.env,
        crate::terminal::detached_start_environment(None)
    );

    // One that waits for a failed setup links its start's PR once it starts.
    let pr = "https://forge.example/team/project/pulls/8";
    let request = super::queued_agent_request(&child, claude(), Some(pr.parse().unwrap()))
        .expect("the launch queues");
    let start = super::QueuedStart::of(&request.plan).expect("the plan parses");
    let plan = super::AddPlan::queued(&child, start, "entry".into()).expect("the plan runs");
    assert_eq!(plan.pr.map(|pr| pr.to_string()).as_deref(), Some(pr));
}

#[test]
fn a_queued_plan_reads_and_writes_back_unchanged() {
    let queued = r#"{"repository":"repo","workspace":"19d40cc0-b385-4221-a22c-bc3daa9f5ea7","pr":"https://forge.example/team/project/pulls/8","place_name":null,"issue":"https://forge.example/team/project/issues/7","agent":{"agent":"claude","model":{"model":"opus","effort":"high"}},"args":[{"Unix":[84,97,107,101,32,116,104,101,32,99,104,105,108,100,32,112,97,114,116]}],"prompt":"Split it","mode":"plan","fork":"parent-session","queue_entry":null}"#;
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
