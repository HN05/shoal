use crate::protocol::operations::QueuedLaunch;
use shoal_protocol::space_names::SpaceSelector;

fn claude() -> QueuedLaunch {
    QueuedLaunch::new("claude".into())
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
    assert_eq!(start.launch.model, model);
    assert_eq!(start.launch.args, ["Take the child part"]);
    assert_eq!(start.launch.fork, Some("parent-session".into()));
    assert_eq!(
        start
            .launch
            .issue
            .map(|issue| issue.url.to_string())
            .as_deref(),
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
    assert_eq!(start.pr.map(|pr| pr.to_string()).as_deref(), Some(pr));
}

#[test]
fn a_place_plan_reads_and_writes_back_unchanged() {
    let planned = r#"{"space":"19d40cc0-b385-4221-a22c-bc3daa9f5ea7","queue_entry":"entry","handoff":{"preparation":"operation"}}"#;
    let plan: super::PlacePlan = serde_json::from_str(planned).expect("the place plan parses");
    assert_eq!(plan.queue_entry, Some("entry".into()));
    let preparation = plan
        .handoff
        .as_ref()
        .and_then(|handoff| handoff.preparation.as_deref());
    assert_eq!(preparation, Some("operation"));
    assert_eq!(serde_json::to_string(&plan).unwrap(), planned);
}
