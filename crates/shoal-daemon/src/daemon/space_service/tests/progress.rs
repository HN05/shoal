use crate::daemon::manager::Manager;
use shoal_protocol::progress::ProgressChange;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
        test_support::{manager, repository},
    },
    model::{SpaceKind, Workspace},
    protocol::AgentMethod,
};

/// A workspace for each of `names`, in one repository.
async fn workspaces<const N: usize>(
    manager: &Manager,
    temp: &tempfile::TempDir,
    names: [&str; N],
) -> [Workspace; N] {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut created = Vec::new();
    for name in names {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        created.push(workspace);
    }
    created.try_into().unwrap()
}

#[tokio::test]
async fn scoped_callers_set_only_their_own_progress_read_below_and_unblock_below() {
    let (temp, manager) = manager().await;
    let [own, other] = workspaces(&manager, &temp, ["own", "other"]).await;
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: own.id.clone(),
                execution: None,
                granted: None,
                read_only: false,
                vouched: None,
            },
        )
        .await;
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&own.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let allowed = async |method: &mut AgentMethod| {
        scope::authorize(&manager, Some(&"scope".into()), method)
            .await
            .is_ok()
    };
    let set = |space: &Workspace| AgentMethod::SetProgress {
        space: SpaceSelector::id(&space.id),
        change: ProgressChange::Status(Some("Working".into())),
    };
    let read = |space: &Workspace| AgentMethod::Progress {
        space: SpaceSelector::id(&space.id),
    };
    assert!(allowed(&mut set(&own)).await);
    assert!(!allowed(&mut set(&child)).await);
    assert!(allowed(&mut read(&own)).await);
    assert!(allowed(&mut read(&child)).await);
    assert!(!allowed(&mut read(&other)).await);
    // A coordinator removes what blocks a space below, and only removes.
    let blocker = shoal_protocol::progress::Blocker::Issue("7".parse().unwrap());
    let unblock = |space: &Workspace| AgentMethod::SetProgress {
        space: SpaceSelector::id(&space.id),
        change: ProgressChange::RemoveBlocker {
            blocker: blocker.clone(),
            step: None,
        },
    };
    let block = AgentMethod::SetProgress {
        space: SpaceSelector::id(&child.id),
        change: ProgressChange::AddBlocker {
            blocker: blocker.clone(),
            step: None,
            reason: "Needs it".into(),
        },
    };
    assert!(allowed(&mut unblock(&child)).await);
    assert!(!allowed(&mut unblock(&other)).await);
    assert!(!allowed(&mut { block }).await);
}

#[tokio::test]
async fn a_chats_progress_is_its_own_and_goes_with_it() {
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
    let selector = SpaceSelector::id(&chat.id);
    manager
        .spaces
        .set_progress(
            &*manager,
            &selector,
            ProgressChange::Status(Some("Drafting".into())),
        )
        .await
        .unwrap();
    let listed = manager.spaces.chats().await.unwrap();
    let progress = listed[0].progress.as_ref().unwrap();
    assert_eq!(progress.status.as_deref(), Some("Drafting"));
    let events: Vec<_> = (manager.space_events(None, 100).await.unwrap())
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::ProgressChanged => {
                Some(event.details)
            }
            _ => None,
        })
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (&events[0].space_id, events[0].space.kind()),
        (&chat.id, SpaceKind::Chat)
    );
    assert_eq!(events[0].repository_id.as_ref(), Some(&repo.id));

    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM spaces WHERE id=?1", [&id])?;
            Ok(
                db.query_row("SELECT COUNT(*) FROM space_progress", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .map(|left| assert_eq!(left, 0))
        .unwrap();
}
