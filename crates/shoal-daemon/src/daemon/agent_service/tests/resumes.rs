use std::time::Duration;

use super::queue::{add, create, register};
use crate::paths::StateFiles as _;
use crate::{
    config::permissions::PermissionChange,
    daemon::{executions::ExecutionKind, manager::Manager, test_support::manager},
    execution::recovery::{self, LastSession, LastSessionExt, RecordExt as _},
    model::{AgentResume, QueueEntry, ResumeOutcome, Workspace},
    sessions::{HostName, Origin},
    state::{AgentState, QueueKind, QueueState, WorkspaceState},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::space_names::SpaceSelector;

/// A workspace `name` of a new repository.
async fn workspace(manager: &Manager, root: &tempfile::TempDir, name: &str) -> Workspace {
    let repo = register(manager, root, &format!("{name}-repo")).await;
    create(manager, &repo, name).await
}

/// Resume the agent of `workspace` as `resume` says, for the user.
async fn resume(
    manager: &Manager,
    workspace: &Workspace,
    resume: AgentResume,
) -> anyhow::Result<ResumeOutcome> {
    let space = SpaceSelector::id(&workspace.id);
    (manager.agents.resume_space(manager, None, &space, resume)).await
}

/// The entries `outcome` queued, refusing any other outcome.
fn queued(outcome: ResumeOutcome) -> Vec<QueueEntry> {
    match outcome {
        ResumeOutcome::Queued { entries, .. } => entries,
        other => panic!("not queued: {other:?}"),
    }
}

/// Save the record of a claude agent stopped as `execution` in `workspace`.
fn stop(manager: &Manager, workspace: &Workspace, execution: &str) {
    let path = recovery::record_path(&manager.paths, &workspace.id, &execution.into());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record = r#"{"agent":"claude","mode":"worker","stage":"stopped"}"#;
    std::fs::write(path, record).unwrap();
}

/// Save a stopped command of `workspace` that runs `argv`.
fn stop_command(manager: &Manager, workspace: &Workspace, argv: &[&str]) {
    let state = manager.paths.space_state(&workspace.id);
    std::fs::create_dir_all(&state).unwrap();
    let record = serde_json::json!({ "argv": argv });
    std::fs::write(state.join("make.command.json"), record.to_string()).unwrap();
}

/// A claude agent of `workspace` running in the zmx session `session`.
async fn placed_agent(manager: &Manager, workspace: &Workspace, session: &str) -> ExecutionId {
    let origin = Origin::created(HostName::Zmx, session.into());
    let request = crate::daemon::executions::ExecutionRequest {
        place: Some(crate::daemon::places::ExecutionPlace {
            origin,
            agent: true,
        }),
        ..Default::default()
    };
    let selector = SpaceSelector::id(&workspace.id);
    let kind = ExecutionKind::Command;
    let started = (manager.agents)
        .begin_execution_placed(manager, &selector, None, kind, None, request)
        .await
        .unwrap();
    let id = started.plan.id;
    let agent = TrackedAgent::named("claude");
    (manager.agents)
        .track_agent(
            manager,
            &id,
            &agent,
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    id
}

/// An interrupted turn continues with a message that wakes its agent,
/// which the agent's wrapper prompts it with, and refuses a permission
/// change first, since the running agent keeps the launch it runs under.
#[tokio::test]
async fn an_interrupted_turn_continues_with_a_message_that_wakes_its_agent() {
    let (root, manager) = manager().await;
    let workspace = workspace(&manager, &root, "fix").await;
    let id = placed_agent(&manager, &workspace, "fix-session").await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.agents)
        .set_agent_state(
            &*manager,
            &selector,
            AgentState::Working,
            Some(id.clone()),
            None,
        )
        .await
        .unwrap();
    (manager.agents)
        .interrupt_turn(&*manager, &selector, id)
        .await
        .unwrap();

    let changing = AgentResume {
        change: Some(PermissionChange::assigned("push=direct").unwrap()),
        ..AgentResume::default()
    };
    assert!(resume(&manager, &workspace, changing).await.is_err());
    let unread = || manager.agents.unread_messages(&workspace.id);
    assert!(!unread().await.unwrap().wakes);

    let outcome = resume(&manager, &workspace, AgentResume::default()).await;
    assert_eq!(
        outcome.unwrap(),
        ResumeOutcome::Continued {
            agent: Some("claude".into())
        }
    );
    let unread = unread().await.unwrap();
    let messages: Vec<_> = (unread.alerts.iter())
        .map(|alert| alert.message.as_str())
        .collect();
    assert_eq!(
        (unread.wakes, messages),
        (true, vec![shoal_agents::resumes::CONTINUE_MESSAGE])
    );
}

/// A launch that failed to start queues again as it was, waiting for the
/// queue, or starting at once with `now`.
#[tokio::test]
async fn a_failed_launch_queues_again() {
    let (root, manager) = manager().await;
    let repo = register(&manager, &root, "repo").await;
    let mut failed = Vec::new();
    for name in ["later", "now"] {
        let entry = add(&manager, &repo, name).await.entry.unwrap();
        (manager.agents)
            .fail_queue_entry(&*manager, &entry.id, "the place did not open")
            .await
            .unwrap();
        failed.push(entry);
    }
    let spaces = manager.spaces.list_workspaces().await.unwrap();

    let mut states = Vec::new();
    for (entry, now) in failed.iter().zip([false, true]) {
        let space = spaces.iter().find(|space| Some(&space.id) == entry.space());
        let resume_it = AgentResume {
            now,
            ..AgentResume::default()
        };
        let entries = queued(resume(&manager, space.unwrap(), resume_it).await.unwrap());
        states.push((entries[0].id.clone(), entries[0].state));
    }

    let expected = [
        (failed[0].id.clone(), QueueState::Queued),
        (failed[1].id.clone(), QueueState::Starting),
    ];
    assert_eq!(states, expected);
}

/// A stopped agent's restore queues as the limits allow, or starts at once
/// with `now`, under the permissions the resume changes.
#[tokio::test]
async fn a_stopped_agent_queues_its_restore() {
    let (root, manager) = manager().await;
    let later = workspace(&manager, &root, "later").await;
    let now = workspace(&manager, &root, "now").await;
    stop(&manager, &later, "later-agent");
    stop(&manager, &now, "now-agent");
    let change = PermissionChange::assigned("push=direct").unwrap();

    let resumed = AgentResume {
        change: Some(change.clone()),
        ..AgentResume::default()
    };
    let later_entries = queued(resume(&manager, &later, resumed).await.unwrap());
    let resumed_now = AgentResume {
        now: true,
        ..AgentResume::default()
    };
    let now_entries = queued(resume(&manager, &now, resumed_now).await.unwrap());

    let described = |entries: &[QueueEntry]| {
        (entries.iter())
            .map(|entry| (entry.kind, entry.execution.clone().unwrap(), entry.state))
            .collect::<Vec<_>>()
    };
    let (restore, later_agent) = (QueueKind::Restore, ExecutionId::from("later-agent"));
    assert_eq!(
        described(&later_entries),
        [(restore, later_agent.clone(), QueueState::Queued)]
    );
    assert_eq!(
        described(&now_entries),
        [(restore, "now-agent".into(), QueueState::Starting)]
    );
    let path = recovery::record_path(&manager.paths, &later.id, &later_agent);
    let record = recovery::Record::load(&path).unwrap().unwrap();
    assert_eq!(record.narrowing, change.set);
}

/// A last session continues at once when its space resumes alone, since
/// it holds no heavy slot, and otherwise waits in the queue.
#[tokio::test]
async fn a_last_session_resumed_alone_continues_at_once() {
    let (root, manager) = manager().await;
    let alone = workspace(&manager, &root, "alone").await;
    let together = workspace(&manager, &root, "together").await;
    let agent = TrackedAgent::named("claude");
    for (workspace, execution) in [(&alone, "alone-last"), (&together, "together-last")] {
        let execution = ExecutionId::from(execution);
        LastSession::save(&manager.paths, &workspace.id, &agent, &execution, None).unwrap();
    }

    let resumed_alone = AgentResume {
        alone: true,
        ..AgentResume::default()
    };
    let alone_entries = queued(resume(&manager, &alone, resumed_alone).await.unwrap());
    let together_entries = queued(
        resume(&manager, &together, AgentResume::default())
            .await
            .unwrap(),
    );

    let states = [&alone_entries[0], &together_entries[0]].map(|entry| entry.state);
    assert_eq!(states, [QueueState::Starting, QueueState::Queued]);
}

/// Stopped commands without an agent go to the user once, and then
/// nothing of the space resumes.
#[tokio::test]
async fn stopped_commands_alone_are_reported_once() {
    let (root, manager) = manager().await;
    let workspace = workspace(&manager, &root, "fix").await;
    stop_command(&manager, &workspace, &["make", "test"]);

    let first = resume(&manager, &workspace, AgentResume::default()).await;
    let second = resume(&manager, &workspace, AgentResume::default()).await;

    let commands = vec![vec!["make".to_owned(), "test".to_owned()]];
    assert_eq!(first.unwrap(), ResumeOutcome::Commands { commands });
    assert!(matches!(second.unwrap(), ResumeOutcome::Nothing { .. }));
}

/// A workspace whose setup failed is refused, naming `shoal setup`, even
/// with a stopped agent to restore.
#[tokio::test]
async fn a_workspace_whose_setup_failed_is_refused() {
    let (root, manager) = manager().await;
    let workspace = workspace(&manager, &root, "fix").await;
    stop(&manager, &workspace, "agent");
    (manager.spaces)
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();

    let refused = resume(&manager, &workspace, AgentResume::default()).await;

    let error = format!("{:#}", refused.unwrap_err());
    assert!(error.contains("shoal setup"), "{error}");
    assert!(
        manager
            .agents
            .queue_entries(&*manager)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A scoped process resumes the agents of the spaces below its own, not
/// its own, starting nothing past the limits its permission does not let
/// it.
#[tokio::test]
async fn a_scoped_process_resumes_only_the_spaces_below_its_own() {
    let (root, manager) = manager().await;
    let parent = workspace(&manager, &root, "parent").await;
    let child = workspace(&manager, &root, "child").await;
    let (below, above) = (child.id.clone(), parent.id.clone());
    (manager.store)
        .write(move |tx| crate::daemon::tree::set_parent(tx, &below, Some(&above)))
        .await
        .unwrap();
    let token = shoal_protocol::scope_tokens::ScopeToken::from("parent-token");
    let scope = crate::daemon::scope::Caller::of_space(parent.id.clone());
    (manager.agents.executions())
        .issue_scope(token.clone(), scope)
        .await;

    let mut allowed = Vec::new();
    for space in [&parent, &child] {
        let mut request = crate::protocol::AgentMethod::ResumeAgent {
            space: SpaceSelector::id(&space.id),
            resume: AgentResume::default(),
        };
        let authorized = crate::daemon::scope::authorize(&manager, Some(&token), &mut request);
        allowed.push(authorized.await.is_ok());
    }
    // Its `limits` permission does not let it start a last session past
    // the limits without asking, so its resume of one space alone queues.
    let mut alone = crate::protocol::AgentMethod::ResumeAgent {
        space: SpaceSelector::id(&child.id),
        resume: AgentResume {
            alone: true,
            ..AgentResume::default()
        },
    };
    (crate::daemon::scope::authorize(&manager, Some(&token), &mut alone))
        .await
        .unwrap();

    assert_eq!(allowed, [false, true]);
    let crate::protocol::AgentMethod::ResumeAgent { resume, .. } = alone else {
        unreachable!("the request stays a resume");
    };
    assert!(!resume.alone);
}

/// A discard of a space's agent takes its failed launch with its stopped
/// agent, so neither resumes any longer.
#[tokio::test]
async fn a_discard_takes_the_failed_launch_with_the_stopped_agent() {
    let (root, manager) = manager().await;
    let repo = register(&manager, &root, "repo").await;
    let entry = add(&manager, &repo, "work").await.entry.unwrap();
    (manager.agents)
        .fail_queue_entry(&*manager, &entry.id, "the place did not open")
        .await
        .unwrap();
    let workspaces = manager.spaces.list_workspaces().await.unwrap();
    let workspace = (workspaces.iter())
        .find(|space| Some(&space.id) == entry.space())
        .unwrap();
    stop(&manager, workspace, "agent");

    let selector = SpaceSelector::id(&workspace.id);
    let discarded = (manager.agents)
        .discard_space_agent(&*manager, &selector, None)
        .await
        .unwrap();

    let launch = discarded.launch.map(|launch| launch.id);
    assert_eq!(
        (discarded.executions, launch),
        (vec![ExecutionId::from("agent")], Some(entry.id))
    );
    let outcome = resume(&manager, workspace, AgentResume::default()).await;
    assert!(matches!(outcome.unwrap(), ResumeOutcome::Nothing { .. }));
}

/// A discard leaves an agent its waiting wrapper restores, and discards
/// nothing in a space with only such an agent.
#[tokio::test]
async fn a_discard_leaves_an_agent_its_wrapper_waits_to_restore() {
    let (root, manager) = manager().await;
    let workspace = workspace(&manager, &root, "asleep").await;
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    let path = recovery::record_path(&manager.paths, &workspace.id, &"asleep-agent".into());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record = serde_json::json!({
        "agent": "claude", "mode": "worker", "stage": "stopped", "waiting": wrapper,
    });
    std::fs::write(&path, record.to_string()).unwrap();

    let selector = SpaceSelector::id(&workspace.id);
    let discarded = (manager.agents)
        .discard_space_agent(&*manager, &selector, None)
        .await
        .unwrap();

    assert!(discarded.is_empty());
    assert!(path.exists());
}
