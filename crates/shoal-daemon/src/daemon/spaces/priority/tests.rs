use super::*;
use crate::daemon::test_support::{ProcessGroup, chat, manager, repository, workspace_below};

#[tokio::test]
async fn a_scoped_caller_gives_at_most_its_own_spaces_priority() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let child = workspace_below(&manager, &repo, "child", &top.id, None).await;
    let (master, _master) = chat(&manager, "master", "orchestrate", None, None).await;
    let caller = |space_id: &SpaceId| Caller {
        space_id: space_id.clone(),
        execution: None,
    };
    let selector = SpaceSelector::id(&child.id);
    let urgent = |caller: Option<Caller>| {
        let (manager, selector) = (manager.clone(), selector.clone());
        async move { (manager.set_priority(caller.as_ref(), &selector, Priority::URGENT)).await }
    };

    // The parent's own priority is the default, below urgent.
    assert!(urgent(Some(caller(&top.id))).await.is_err());
    (manager.set_priority(Some(&caller(&top.id)), &selector, Priority::LOW))
        .await
        .unwrap();
    // The user and a master session give any value.
    assert_eq!(urgent(None).await.unwrap().priority, Priority::URGENT);
    (manager.set_priority(None, &SpaceSelector::id(&top.id), Priority::LOW))
        .await
        .unwrap();
    let highest = Priority::try_from(100).unwrap();
    (manager.set_priority(Some(&caller(&master.id)), &selector, highest))
        .await
        .unwrap();
    let id = child.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), highest);
}

#[tokio::test]
async fn a_chat_started_with_a_priority_records_it() {
    let (_temp, manager) = manager().await;
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = crate::daemon::chats::NewChat {
        priority: Some(Priority::URGENT),
        name: "urgent".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: None,
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    let stored = manager.store.read(move |tx| current(tx, &chat.id)).await;
    assert_eq!(stored.unwrap(), Priority::URGENT);
}

/// What preparing a started workspace does in the tests: nothing more.
fn bare() -> crate::daemon::operations::starts::Preparing<crate::protocol::operations::LaunchRequest>
{
    crate::daemon::operations::starts::Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: None,
        env: None,
    }
}

#[tokio::test]
async fn a_started_or_split_workspace_records_the_priority_it_is_given() {
    use crate::client::protocol::CreationOptions;
    use crate::daemon::operations::starts::{Opening, Split};
    use crate::protocol::operations::WorkspaceStart;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
        .start_workspace((&repo.id).into(), opening("started"), bare(), None)
        .await
        .unwrap();
    let _worker = crate::daemon::operations::fake::take(&started.operation);
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), Some("HEAD".into()), None, None)
        .await
        .unwrap();
    let split = Split {
        name: Some("split".into()),
        copy: None,
        base: None,
        options: options.clone(),
    };
    let split = manager
        .start_split(&SpaceSelector::id(&parent.id), split, bare(), None)
        .await
        .unwrap();
    let _split_worker = crate::daemon::operations::fake::take(&split.operation);
    for workspace in [&started.workspace, &split.workspace] {
        let id = workspace.id.clone();
        let stored = manager.store.read(move |tx| current(tx, &id)).await;
        assert_eq!(stored.unwrap(), Priority::URGENT);
    }
    // The parent's agent has the default priority, below urgent.
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
    };
    let refused = manager
        .start_workspace((&repo.id).into(), opening("refused"), bare(), Some(&caller))
        .await;
    let refused = refused.err().unwrap().to_string();
    assert!(refused.contains("priority"), "{refused}");
}

#[tokio::test]
async fn a_scoped_caller_that_names_no_priority_gives_its_own_when_lower() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: top.id.clone(),
        execution: None,
    };
    let given = |caller| manager.given_priority(caller, None);
    assert_eq!(given(Some(&caller)).await.unwrap(), None);
    (manager.set_priority(None, &SpaceSelector::id(&top.id), Priority::LOW))
        .await
        .unwrap();
    assert_eq!(given(Some(&caller)).await.unwrap(), Some(Priority::LOW));
    assert_eq!(given(None).await.unwrap(), None);
}

#[tokio::test]
async fn a_reopened_workspace_keeps_its_priority_unless_one_is_asked_for() {
    use crate::client::protocol::CreationOptions;
    use crate::daemon::operations::starts::Opening;
    use crate::protocol::operations::WorkspaceStart;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), Some("HEAD".into()), None, None)
        .await
        .unwrap();
    (manager.set_priority(None, &SpaceSelector::id(&parent.id), Priority::LOW))
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
        .start_workspace(
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
    let _worker = crate::daemon::operations::fake::take(&child.operation);
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
    };
    let branch = child.workspace.branch.clone().unwrap();
    let reopened = manager
        .start_workspace(
            (&repo.id).into(),
            opening(WorkspaceStart::Branch(branch), None, None),
            bare(),
            Some(&caller),
        )
        .await
        .unwrap();
    let _reopened = crate::daemon::operations::fake::take(&reopened.operation);
    assert_eq!(reopened.workspace.id, child.workspace.id);
    let id = child.workspace.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), Priority::URGENT);
}
