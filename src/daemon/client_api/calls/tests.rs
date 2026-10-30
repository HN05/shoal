use shoal_client::{
    AcquireHold, AcquireSimulator, AddLink, AnswerQuestion, DecideApproval, ErrorCode,
    GetWorkspace, ListRepositories, MarkDone, QueueAgent, ReleaseHold, ReleaseSimulator,
    RemoveLink, RemoveWorkspace, RenameWorkspace, StopAgent, SyncRepository, UndoDone,
    records::{AgentState, ApprovalLifetime, ApprovalStatus, ItemKind, Relation},
};

use super::super::tests::{TestDaemon, code};
use crate::{
    config::repo::ConflictPolicy,
    daemon::{
        approvals::{ApprovalRequest, Lifetime, PortSpecification, Specification, Target},
        questions::Ask,
    },
};

fn get(workspace: &str) -> GetWorkspace {
    GetWorkspace {
        workspace: workspace.into(),
    }
}

#[tokio::test]
async fn repositories_are_listed_and_an_unknown_one_is_not_found() {
    let daemon = TestDaemon::start().await;
    let client = daemon.client().await;
    let listed = client.call(ListRepositories {}).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, daemon.repository.id);
    let unknown = client
        .call(SyncRepository {
            repository: "missing".into(),
        })
        .await;
    assert_eq!(code(unknown), ErrorCode::NotFound);
}

#[tokio::test]
async fn a_workspace_is_read_by_name_or_id_and_scoped_to_its_tree() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    let other = daemon.workspace("other").await;
    let client = daemon.client().await;
    let found = client.call(get(&own.id)).await.unwrap();
    assert_eq!((found.name.as_str(), found.branch.as_str()), ("own", "own"));
    assert_eq!(found.agent, None);
    assert_eq!(code(client.call(get("missing")).await), ErrorCode::NotFound);
    let scoped = daemon.scoped(&own).await;
    assert_eq!(scoped.call(get("own")).await.unwrap().id, own.id);
    assert_eq!(
        code(scoped.call(get(&other.id)).await),
        ErrorCode::ScopeDenied
    );
}

#[tokio::test]
async fn removal_refuses_unsaved_work_unless_the_call_discards_it() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("dirty").await;
    std::fs::write(workspace.path.join("draft"), "unsaved\n").unwrap();
    let client = daemon.client().await;
    let remove = |discard_unsaved| RemoveWorkspace {
        workspace: "dirty".into(),
        discard_unsaved,
    };
    assert_eq!(
        code(client.call(remove(false)).await),
        ErrorCode::UnsavedWork
    );
    let removal = client.call(remove(true)).await.unwrap();
    assert_eq!(removal.branch.as_deref(), Some("dirty"));
    assert_eq!(code(client.call(get("dirty")).await), ErrorCode::NotFound);
}

#[tokio::test]
async fn done_is_recorded_and_withdrawn() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("finished").await;
    let client = daemon.client().await;
    let completion = client
        .call(MarkDone {
            workspace: workspace.id.clone(),
            cleanup: false,
        })
        .await
        .unwrap();
    assert!(!completion.head.is_empty());
    assert!(client.call(get("finished")).await.unwrap().done);
    client
        .call(UndoDone {
            workspace: workspace.id.clone(),
        })
        .await
        .unwrap();
    assert!(!client.call(get("finished")).await.unwrap().done);
}

#[tokio::test]
async fn renaming_to_a_taken_branch_is_refused_with_name_taken() {
    let daemon = TestDaemon::start().await;
    daemon.workspace("first").await;
    daemon.workspace("second").await;
    let client = daemon.client().await;
    let rename = |branch: &str| RenameWorkspace {
        workspace: "first".into(),
        branch: branch.into(),
    };
    assert_eq!(
        code(client.call(rename("second")).await),
        ErrorCode::NameTaken
    );
    let renamed = client.call(rename("third")).await.unwrap();
    assert_eq!(
        (renamed.name.as_str(), renamed.branch.as_str()),
        ("third", "third")
    );
}

#[tokio::test]
async fn links_are_added_and_removed_and_a_missing_one_is_not_found() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("linked").await;
    let client = daemon.client().await;
    client
        .call(AddLink {
            workspace: "linked".into(),
            kind: ItemKind::Issue,
            item: "40".into(),
            relation: Relation::Related,
        })
        .await
        .unwrap();
    let linked = daemon.manager.workspace(&workspace.id).await.unwrap().links;
    assert_eq!(
        linked.related[0].url,
        "https://github.com/team/repo/issues/40"
    );
    let remove = || RemoveLink {
        workspace: "linked".into(),
        kind: ItemKind::Issue,
        item: "40".into(),
        relation: Relation::Related,
    };
    client.call(remove()).await.unwrap();
    assert_eq!(code(client.call(remove()).await), ErrorCode::NotFound);
    let unknown = client
        .call(AddLink {
            relation: Relation::Unknown("blocking".into()),
            ..AddLink {
                workspace: "linked".into(),
                kind: ItemKind::Issue,
                item: "40".into(),
                relation: Relation::Related,
            }
        })
        .await;
    assert_eq!(code(unknown), ErrorCode::InvalidRequest);
}

#[tokio::test]
async fn holds_belong_to_the_user() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("held").await;
    let client = daemon.client().await;
    let acquire = || AcquireHold {
        workspace: "held".into(),
        name: "quay".into(),
        reason: Some("open in Quay".into()),
    };
    let hold = client.call(acquire()).await.unwrap();
    assert_eq!(hold.name, "quay");
    assert_eq!(client.call(get("held")).await.unwrap().holds, [hold]);
    let scoped = daemon.scoped(&workspace).await;
    assert_eq!(code(scoped.call(acquire()).await), ErrorCode::ScopeDenied);
    client
        .call(ReleaseHold {
            workspace: "held".into(),
            name: "quay".into(),
        })
        .await
        .unwrap();
    assert!(client.call(get("held")).await.unwrap().holds.is_empty());
}

#[tokio::test]
async fn simulator_leases_follow_the_workspace_scope() {
    let daemon = TestDaemon::start().await;
    let own = daemon.workspace("own").await;
    daemon.workspace("other").await;
    let client = daemon.client().await;
    let acquire = |workspace: &str| AcquireSimulator {
        workspace: workspace.into(),
        ..Default::default()
    };
    assert_eq!(
        code(client.call(acquire("missing")).await),
        ErrorCode::NotFound
    );
    let scoped = daemon.scoped(&own).await;
    assert_eq!(
        code(scoped.call(acquire("other")).await),
        ErrorCode::ScopeDenied
    );
    let release = client
        .call(ReleaseSimulator {
            workspace: "missing".into(),
            name: "default".into(),
        })
        .await;
    assert_eq!(code(release), ErrorCode::NotFound);
}

#[tokio::test]
async fn a_queued_agent_shows_as_the_workspace_agent_until_it_is_stopped() {
    let daemon = TestDaemon::start().await;
    daemon.workspace("worker").await;
    let client = daemon.client().await;
    let queue = |agent: &str| QueueAgent {
        workspace: "worker".into(),
        agent: agent.into(),
        prompt: Some("Fix the build".into()),
    };
    assert_eq!(
        code(client.call(queue("Not An Agent")).await),
        ErrorCode::InvalidRequest
    );
    let queued = client.call(queue("claude")).await.unwrap();
    assert!(!queued.id.is_empty());
    let agent = client.call(get("worker")).await.unwrap().agent.unwrap();
    assert!(
        matches!(agent.state, AgentState::Queued | AgentState::Starting),
        "{agent:?}"
    );
    assert_eq!(agent.agent.as_deref(), Some("claude"));
    client
        .call(StopAgent {
            workspace: "worker".into(),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn questions_are_answered_and_an_unknown_one_is_not_found() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("asker").await;
    let question = daemon
        .manager
        .ask_question(
            &workspace.name,
            Ask {
                question: "Which database?".into(),
                recommendation: "SQLite".into(),
                reason: None,
                choices: Vec::new(),
            },
        )
        .await
        .unwrap();
    let client = daemon.client().await;
    let answer = |id, shown| AnswerQuestion {
        id,
        answer: "Postgres".into(),
        shown,
    };
    let mut stale = super::super::records::question(question.clone());
    stale.question = "Which cache?".into();
    assert_eq!(
        code(client.call(answer(question.id, Some(stale))).await),
        ErrorCode::NotFound
    );
    let shown = super::super::records::question(question.clone());
    let answered = client.call(answer(question.id, Some(shown))).await.unwrap();
    assert_eq!(answered.question, "Which database?");
    assert_eq!(
        code(client.call(answer(999, None)).await),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_workspace_agent_cannot_decide_a_request_not_passed_to_it() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("requester").await;
    let request = ApprovalRequest::new(
        &workspace.id,
        Target::Port("web".into()),
        "web",
        Specification::Port(PortSpecification {
            env: "PORT".into(),
            on_conflict: ConflictPolicy::Suggest,
            preferred: None,
            range: [4000, 4100],
        }),
        Lifetime::Workspace,
        Some("serve the app"),
    );
    let pending = daemon
        .manager
        .request_approval(request)
        .await
        .unwrap()
        .unwrap();
    let decide = |id, shown| DecideApproval {
        id,
        approve: true,
        shown,
    };
    let scoped = daemon.scoped(&workspace).await;
    assert_eq!(
        code(scoped.call(decide(pending.id, None)).await),
        ErrorCode::ScopeDenied
    );
    let client = daemon.client().await;
    let mut stale = super::super::records::approval(pending.clone());
    stale.key = "a released request's key".into();
    assert_eq!(
        code(client.call(decide(pending.id, Some(stale))).await),
        ErrorCode::NotFound
    );
    let shown = super::super::records::approval(pending.clone());
    let decided = client.call(decide(pending.id, Some(shown))).await.unwrap();
    assert_eq!(decided.status, ApprovalStatus::Approved);
    assert_eq!(decided.lifetime, ApprovalLifetime::Workspace);
    assert_eq!(decided.subject, "port web (workspace)");
    assert_eq!(decided.proposal, None);
    assert_eq!(
        code(client.call(decide(999, None)).await),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn an_agent_stopped_with_a_session_to_resume_shows_as_stopped() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("paused").await;
    crate::execution::recovery::LastSession::save(
        &daemon.manager.paths,
        &workspace.id,
        "claude",
        "execution",
    )
    .unwrap();
    let client = daemon.client().await;
    let agent = client.call(get("paused")).await.unwrap().agent.unwrap();
    assert_eq!(agent.state, AgentState::Stopped);
    assert_eq!(agent.agent.as_deref(), Some("claude"));
}
