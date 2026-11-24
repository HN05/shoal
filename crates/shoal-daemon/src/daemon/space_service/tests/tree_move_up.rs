use shoal_refs::{BranchName, CommitId};

use shoal_spaces::daemon::{Onto, Restack};

use crate::{
    daemon::{
        events::{EventItem, EventKind},
        manager::Manager,
        test_support::{manager, repository},
    },
    model::{EventCause, Workspace},
};
use shoal_protocol::space_names::SpaceSelector;

/// A workspace `root`, `middle` below it and `leaf` below `middle`, built on
/// its branch.
async fn tree(manager: &Manager, root_dir: &std::path::Path) -> [Workspace; 3] {
    let checkout = repository(root_dir, "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let root = manager
        .create_workspace(&repo.id, "root".into(), None, None, None)
        .await
        .unwrap();
    let middle = child(manager, &root, "middle", None).await;
    let leaf = child(manager, &middle, "leaf", Some("middle")).await;
    [root, middle, leaf]
}

/// A new workspace `name` below `parent`, built on `base`.
async fn child(manager: &Manager, parent: &Workspace, name: &str, base: Option<&str>) -> Workspace {
    let parent = SpaceSelector::id(&parent.id);
    let base = base.map(str::to_owned);
    (manager
        .spaces
        .create_child(manager, &parent, name.into(), base, Default::default()))
    .await
    .unwrap()
}

fn head(workspace: &Workspace) -> CommitId {
    crate::test_support::git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .into()
}

#[tokio::test]
async fn a_space_moves_below_its_grandparent_and_its_branch_past_its_old_base() {
    let (temp, manager) = manager().await;
    let [root, middle, leaf] = tree(&manager, temp.path()).await;
    let middle = manager.spaces.workspace_by_id(&middle.id).await.unwrap();
    let (target, head) = (BranchName::from("root"), head(&middle));
    let restack = Restack {
        base: &middle,
        head: &head,
        merged_heads: &[BranchName::from("middle")],
        target: &target,
        onto: Onto::Local,
        new_base: Some(root.id.clone()),
        cause: EventCause::Absorbed,
        account: &manager.forges.user_account(),
    };

    let above = manager
        .spaces
        .move_up_one_level(&*manager, &leaf.id, Some(&restack))
        .await
        .unwrap();

    assert_eq!(above.map(|above| above.id), Some(root.id.clone()));
    let leaf = manager.spaces.workspace_by_id(&leaf.id).await.unwrap();
    assert_eq!(
        leaf.parent.as_ref().map(|parent| &parent.id),
        Some(&root.id)
    );
    assert_eq!(
        leaf.base_workspace.as_ref().map(|base| &base.id),
        Some(&root.id)
    );
    // Without a linked item, its agent hears of the rebase as a message.
    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&leaf.id))
        .await
        .unwrap();
    let rebase = format!("git rebase --onto root {head}");
    assert!(
        messages
            .iter()
            .any(|message| message.message.contains(&rebase)),
        "{messages:?}"
    );
    let events = manager.space_events(None, 100).await.unwrap();
    let changes: Vec<_> = (events.iter())
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.space_id == leaf.id => {
                Some((event.details.kind, event.details.cause))
            }
            _ => None,
        })
        .filter(|(kind, _)| matches!(kind, EventKind::ParentChanged | EventKind::BaseChanged))
        .collect();
    assert_eq!(
        changes,
        [
            (EventKind::ParentChanged, None),
            (EventKind::BaseChanged, Some(EventCause::Absorbed)),
        ]
    );
}
