use crate::forge::numbers::PullNumber;
use shoal_ids::space_ids::SpaceId;

use crate::daemon::workspace::NewWorkspace;
use crate::{
    daemon::{
        scope::{Caller, ScopedExecution},
        test_support::{commit, git, manager, repository},
    },
    protocol::{CommitStart, WorkspaceMethod},
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn workspace_processes_create_workspaces_at_a_commit_only_below_their_own() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create_branch =
        |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let own = create_branch("own").await.unwrap();
    let other = create_branch("other").await.unwrap();
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: own.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: crate::daemon::executions::ExecutionKind::Command,
                    agent_execution: None,
                }),
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    for (parent, allowed) in [
        (Some(own.id.clone()), true),
        (Some(other.id.clone()), false),
        // Without one it goes below the caller's own workspace.
        (None, true),
    ] {
        let mut request = WorkspaceMethod::CreateCommitWorkspace {
            repository: (&repo.id).into(),
            parent: parent.clone().map(SpaceSelector::from),
            start: CommitStart::Revision(None),
            options: Default::default(),
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut request).await;
        assert_eq!(authorized.is_ok(), allowed, "{parent:?}");
        if parent.is_none() {
            let WorkspaceMethod::CreateCommitWorkspace { parent, .. } = request else {
                unreachable!()
            };
            assert_eq!(parent, Some(SpaceSelector::id(&own.id)));
        }
    }
}

#[tokio::test]
async fn a_workspace_at_a_commit_goes_below_the_chat_or_workspace_it_names() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let create = |parent: Option<SpaceId>| {
        manager.spaces.create_commit_workspace(
            &*manager,
            (&repo.id).into(),
            (parent, None),
            CommitStart::Revision(None),
            Default::default(),
        )
    };
    let below_chat = create(Some(chat.id.clone())).await.unwrap();
    assert_eq!(
        below_chat.parent.map(|parent| parent.id),
        Some(chat.id.clone())
    );
    let child = create(Some(parent.id.clone())).await.unwrap();
    assert_eq!(
        child.parent.map(|parent| parent.id),
        Some(parent.id.clone())
    );
    let top = create(None).await.unwrap();
    assert_eq!(top.parent, None);
}

#[tokio::test]
async fn reopening_a_prs_workspace_gives_it_to_its_opener() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    std::fs::write(origin.join("fork"), "fork\n").unwrap();
    commit(&origin, "fork");
    let head = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&origin, &["update-ref", "refs/pull/4/head", &head]);
    let repo = manager
        .repositories
        .register(
            temp.path().join("clone").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    let start =
        |name: &str| manager.start_chat(repo.id.clone().into(), name.into(), "claude".into(), None);
    let (first, second) = (
        start("first").await.unwrap(),
        start("second").await.unwrap(),
    );
    let (manager, repo) = (&manager, &repo);
    // The chat's own agent opens it `by_agent`, and the user otherwise.
    let open = |chat: Option<SpaceId>, by_agent: bool| {
        let caller = (chat.clone().filter(|_| by_agent)).map(|space_id| Caller {
            space_id,
            execution: None,
            granted: None,
            read_only: false,
            vouched: None,
        });
        let head = head.clone();
        async move {
            let head = crate::protocol::PullHead {
                number: PullNumber::new(4),
                reference: "refs/pull/4/head".into(),
                commit: head.into(),
                base: None,
            };
            let url = "https://forge.example/team/repo/pulls/4".parse().unwrap();
            (manager.spaces.open_pull_head(
                &**manager,
                (&repo.id).into(),
                url,
                head,
                NewWorkspace {
                    parent: chat,
                    ..Default::default()
                },
                caller.as_ref(),
            ))
            .await
        }
    };
    let opened = open(Some(first.id.clone()), true).await.unwrap().workspace;
    assert_eq!(
        opened.parent.map(|parent| parent.id),
        Some(first.id.clone())
    );
    // Another chat's own agent cannot take it from the first.
    open(Some(second.id.clone()), true).await.unwrap_err();
    let kept = manager.spaces.workspace_by_id(&opened.id).await.unwrap();
    assert_eq!(kept.parent.map(|parent| parent.id), Some(first.id.clone()));
    let reopened = open(Some(second.id.clone()), false)
        .await
        .unwrap()
        .workspace;
    assert_eq!(
        reopened.parent.map(|parent| parent.id),
        Some(second.id.clone())
    );
    let by_user = open(None, false).await.unwrap().workspace;
    assert_eq!(by_user.parent, None);
}

#[tokio::test]
async fn a_chat_opens_no_workspace_at_a_commit_in_another_repository() {
    let (temp, manager) = manager().await;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .repositories
                .register(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    let chat = manager
        .start_chat(
            repos[0].id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let other = &repos[1];
    manager
        .spaces
        .create_commit_workspace(
            &*manager,
            (&other.id).into(),
            (Some(chat.id.clone()), None),
            CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap_err();
    manager
        .spaces
        .open_pull_head(
            &*manager,
            (&other.id).into(),
            "https://forge.example/team/other/pulls/3".parse().unwrap(),
            crate::protocol::PullHead {
                number: PullNumber::new(3),
                reference: "refs/pull/3/head".into(),
                commit: "0000000".into(),
                base: None,
            },
            NewWorkspace {
                parent: Some(chat.id.clone()),
                ..Default::default()
            },
            Some(&Caller {
                space_id: chat.id.clone(),
                execution: None,
                granted: None,
                read_only: false,
                vouched: None,
            }),
        )
        .await
        .unwrap_err();
    // Refused before anything was created.
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
}
