use super::*;
use crate::{
    client::{
        assignments::Done,
        events::{BlockerEvent, EventKind},
        progress::{ProgressChange, RecordedBlocker},
    },
    daemon::{
        events::EventItem,
        test_support::{manager, repository},
    },
    model::{Blocker, Workspace},
};

const ISSUE: &str = "https://github.com/team/app/issues/7";
const PR: &str = "https://github.com/team/app/pull/8";
const RUN: &str = "https://github.com/team/app/actions/runs/9";

async fn workspaces<const N: usize>(
    manager: &Manager,
    temp: &tempfile::TempDir,
    names: [&str; N],
) -> [Workspace; N] {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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

async fn block(manager: &Manager, space: &Workspace, blocker: Blocker) {
    let change = ProgressChange::AddBlocker {
        blocker,
        step: None,
        reason: "Needs it".into(),
    };
    let selector = SpaceSelector::id(&space.id);
    manager.set_progress(&selector, change).await.unwrap();
}

async fn blockers(manager: &Manager, space: &Workspace) -> Vec<RecordedBlocker> {
    let selector = SpaceSelector::id(&space.id);
    let progress = manager.space_progress(&selector).await.unwrap();
    progress.map_or_else(Vec::new, |progress| progress.blockers)
}

#[tokio::test]
async fn forge_blockers_clear_as_their_lookups_say_and_failures_keep_them() {
    let (temp, manager) = manager().await;
    let [blocked] = workspaces(&manager, &temp, ["blocked"]).await;
    for blocker in [
        Blocker::Issue(ISSUE.into()),
        Blocker::Pr(PR.into()),
        Blocker::CiRun(RUN.into()),
    ] {
        block(&manager, &blocked, blocker).await;
    }
    let targets = manager.progress_forge_blockers().await.unwrap();
    assert_eq!(targets.len(), 3);
    let lookups: Lookups = [
        (
            (BlockerKind::Issue, ISSUE.into()),
            Lookup::Cleared("it closed".into()),
        ),
        (
            (BlockerKind::Pr, PR.into()),
            Lookup::Failed("forge unreachable".into()),
        ),
        (
            (BlockerKind::CiRun, RUN.into()),
            Lookup::Cleared("run 12 passed on main at abc".into()),
        ),
    ]
    .into();
    let cleared = manager.clear_progress_blockers(&lookups).await.unwrap();
    let how: Vec<_> = (cleared[0].cleared.iter())
        .map(|event| event.cleared.clone().unwrap_or_default())
        .collect();
    assert_eq!(how, ["it closed", "run 12 passed on main at abc"]);
    let kept = blockers(&manager, &blocked).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].lookup_failed.as_deref(), Some("forge unreachable"));
    // A later lookup that succeeds forgets the failure.
    let open: Lookups = [((BlockerKind::Pr, PR.into()), Lookup::Open)].into();
    assert!(
        manager
            .clear_progress_blockers(&open)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(blockers(&manager, &blocked).await[0].lookup_failed, None);

    let events: Vec<_> = (manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter())
    .filter_map(|item| match item {
        EventItem::Event(event) if event.details.kind == EventKind::BlockerCleared => {
            event.details.blocker.map(|blocker| blocker.blocker.target)
        }
        _ => None,
    })
    .collect();
    assert_eq!(events, [ISSUE, RUN]);
}

#[tokio::test]
async fn a_space_blocker_clears_once_that_space_is_done_or_removed() {
    let (temp, manager) = manager().await;
    let [blocked, api, docs] = workspaces(&manager, &temp, ["blocked", "api", "docs"]).await;
    block(&manager, &blocked, Blocker::Space("api".into())).await;
    block(&manager, &blocked, Blocker::Space("docs".into())).await;
    let none = Lookups::new();
    assert!(
        manager
            .clear_progress_blockers(&none)
            .await
            .unwrap()
            .is_empty()
    );
    manager
        .mark_done(
            &SpaceSelector::id(&api.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&docs.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    let cleared = manager.clear_progress_blockers(&none).await.unwrap();
    assert_eq!(cleared[0].cleared.len(), 2);
    assert!(blockers(&manager, &blocked).await.is_empty());
}

#[test]
fn the_cleared_message_names_what_cleared_and_what_still_blocks() {
    let blocker = |kind, target: &str| RecordedBlocker {
        kind,
        target: target.into(),
        name: target.into(),
        reason: "Needs it".into(),
        lookup_failed: None,
    };
    let cleared = Cleared {
        space: "space".into(),
        cleared: vec![BlockerEvent {
            blocker: blocker(BlockerKind::CiRun, RUN),
            step: Some("Fix".into()),
            cleared: Some("run 12 passed on main at abc".into()),
        }],
    };
    let progress = Progress {
        status: None,
        steps: Vec::new(),
        current: None,
        blockers: vec![blocker(BlockerKind::Pr, PR)],
        updated_at: crate::time::UnixTime::EPOCH,
    };
    let text = cleared_message(&cleared, Some(&progress));
    assert!(text.contains(&format!("CI run {RUN} (step Fix), Needs it: run 12 passed")));
    assert!(text.contains(&format!("Still open:\n- PR {PR}")));
    assert!(!cleared_message(&cleared, None).contains("Still open"));
}
