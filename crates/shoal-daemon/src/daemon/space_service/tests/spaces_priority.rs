use crate::daemon::scope::Caller;
use crate::daemon::spaces::priority::*;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::priority::Priority;

use crate::daemon::test_support::{chat, manager, repository, workspace_below};

#[tokio::test]
async fn a_scoped_caller_gives_at_most_its_own_spaces_priority() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let top = chat(&manager, "top", "chat", Some(&repo), None).await;
    let child = workspace_below(&manager, &repo, "child", &top.id, None).await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let caller = |space_id: &SpaceId| Caller {
        space_id: space_id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let selector = SpaceSelector::id(&child.id);
    let urgent = |caller: Option<Caller>| {
        let (manager, selector) = (manager.clone(), selector.clone());
        async move {
            (manager
                .spaces
                .set_priority(&*manager, caller.as_ref(), &selector, Priority::URGENT))
            .await
        }
    };

    // The parent's own priority is the default, below urgent.
    assert!(urgent(Some(caller(&top.id))).await.is_err());
    (manager
        .spaces
        .set_priority(&*manager, Some(&caller(&top.id)), &selector, Priority::LOW))
    .await
    .unwrap();
    // The user and a master session give any value.
    assert_eq!(urgent(None).await.unwrap().priority, Priority::URGENT);
    (manager
        .spaces
        .set_priority(&*manager, None, &SpaceSelector::id(&top.id), Priority::LOW))
    .await
    .unwrap();
    let highest = Priority::try_from(100).unwrap();
    (manager
        .spaces
        .set_priority(&*manager, Some(&caller(&master.id)), &selector, highest))
    .await
    .unwrap();
    let id = child.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), highest);
}

#[tokio::test]
async fn a_chat_started_with_a_priority_records_it() {
    let (_temp, manager) = manager().await;
    let new = crate::daemon::chats::NewChat {
        priority: Some(Priority::URGENT),
        name: "urgent".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
    };
    let chat = manager
        .spaces
        .start_new_chat(&*manager, None, None, new)
        .await
        .unwrap();
    let chat = manager.run_chat_agent(chat, None).await.unwrap();
    let stored = manager.store.read(move |tx| current(tx, &chat.id)).await;
    assert_eq!(stored.unwrap(), Priority::URGENT);
}

/// What preparing a started workspace does in the tests: nothing more.
fn bare() -> crate::daemon::starts::Preparing<crate::protocol::operations::LaunchRequest> {
    crate::daemon::starts::Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: None,
        blockers: Vec::new(),
        unfollowed: false,
        env: None,
    }
}

#[tokio::test]
async fn a_started_or_split_workspace_records_the_priority_it_is_given() {
    use crate::daemon::starts::{Child, Opening};
    use crate::protocol::operations::WorkspaceStart;
    use shoal_protocol::protocol::CreationOptions;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let options = CreationOptions {
        priority: Some(Priority::URGENT),
        ..Default::default()
    };
    let opening = |name: &str| Opening {
        start: WorkspaceStart::Name(name.into()),
        base: Some("HEAD".into()),
        new: crate::daemon::workspace::NewWorkspace {
            options: options.clone(),
            ..Default::default()
        },
        parent: None,
    };
    let started = manager
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repo.id).into(),
            opening("started"),
            bare(),
            None,
        )
        .await
        .unwrap();
    let _worker = crate::daemon::operations::fake::take(started.operation.as_deref().unwrap());
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), Some("HEAD".into()), None, None)
        .await
        .unwrap();
    let split = Child {
        name: Some("split".into()),
        copy: None,
        base: None,
        options: options.clone(),
    };
    let split = manager
        .spaces
        .start_child(
            &manager.daemon(),
            &SpaceSelector::id(&parent.id),
            split,
            bare(),
            None,
        )
        .await
        .unwrap();
    let _split_worker = crate::daemon::operations::fake::take(split.operation.as_deref().unwrap());
    for workspace in [&started.workspace, &split.workspace] {
        let id = workspace.id.clone();
        let stored = manager.store.read(move |tx| current(tx, &id)).await;
        assert_eq!(stored.unwrap(), Priority::URGENT);
    }
    // The parent's agent has the default priority, below urgent.
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let refused = manager
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repo.id).into(),
            opening("refused"),
            bare(),
            Some(&caller),
        )
        .await;
    let refused = refused.err().unwrap().to_string();
    assert!(refused.contains("priority"), "{refused}");
}

#[tokio::test]
async fn a_scoped_caller_that_names_no_priority_gives_its_own_when_lower() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let top = chat(&manager, "top", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: top.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let given = |caller| manager.spaces.given_priority(&*manager, caller, None);
    assert_eq!(given(Some(&caller)).await.unwrap(), None);
    (manager
        .spaces
        .set_priority(&*manager, None, &SpaceSelector::id(&top.id), Priority::LOW))
    .await
    .unwrap();
    assert_eq!(given(Some(&caller)).await.unwrap(), Some(Priority::LOW));
    assert_eq!(given(None).await.unwrap(), None);
}

#[tokio::test]
async fn a_reopened_workspace_keeps_its_priority_unless_one_is_asked_for() {
    use crate::daemon::starts::Opening;
    use crate::protocol::operations::WorkspaceStart;
    use shoal_protocol::protocol::CreationOptions;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), Some("HEAD".into()), None, None)
        .await
        .unwrap();
    (manager.spaces.set_priority(
        &*manager,
        None,
        &SpaceSelector::id(&parent.id),
        Priority::LOW,
    ))
    .await
    .unwrap();
    let opening = |start, priority, base: Option<&str>| Opening {
        start,
        base: base.map(Into::into),
        new: crate::daemon::workspace::NewWorkspace {
            options: CreationOptions {
                priority,
                ..Default::default()
            },
            ..Default::default()
        },
        parent: Some(parent.id.clone()),
    };
    let child = manager
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repo.id).into(),
            opening(
                WorkspaceStart::Name("child".into()),
                Some(Priority::URGENT),
                Some("HEAD"),
            ),
            bare(),
            None,
        )
        .await
        .unwrap();
    let _worker = crate::daemon::operations::fake::take(child.operation.as_deref().unwrap());
    let selector = SpaceSelector::id(&child.workspace.id);
    manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let branch = child.workspace.branch().cloned().unwrap();
    let reopened = manager
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repo.id).into(),
            opening(WorkspaceStart::Branch(branch.into()), None, None),
            bare(),
            Some(&caller),
        )
        .await
        .unwrap();
    let _reopened = crate::daemon::operations::fake::take(reopened.operation.as_deref().unwrap());
    assert_eq!(reopened.workspace.id, child.workspace.id);
    let id = child.workspace.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), Priority::URGENT);
}

/// A workspace at a commit records the priority its creation asks for.
#[tokio::test]
async fn a_workspace_at_a_commit_records_the_priority_it_is_given() {
    use shoal_protocol::protocol::{CommitStart, CreationOptions};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let options = CreationOptions {
        priority: Some(Priority::URGENT),
        ..Default::default()
    };

    let workspace = manager
        .spaces
        .create_commit_workspace(
            &*manager,
            (&repo.id).into(),
            (None, None),
            CommitStart::Revision(None),
            options,
        )
        .await
        .unwrap();

    let id = workspace.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), Priority::URGENT);
}
