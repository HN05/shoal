use super::*;
use crate::{
    client::{assignments::Done, space_names::SpaceSelector},
    config::modes::ModeName,
    daemon::{
        test_support::{chat, configure, manager, repository},
        workspace_files::Attacher,
    },
    model::{Repository, Workspace},
    state::{AgentState, Sleep},
};

const HOUR: Duration = Duration::from_secs(3600);

#[test]
fn an_agent_is_due_each_period_it_stays_idle_counted_from_its_last_turn() {
    let id = SpaceId::generate();
    let since = UnixTime::from_seconds(1_000);
    let mut woken = Woken::default();
    assert!(!woken.due(&id, since, HOUR, since + HOUR - Duration::from_secs(1)));
    assert!(woken.due(&id, since, HOUR, since + HOUR));
    woken.0.insert(id.clone(), since + HOUR);
    // Without a turn, the next wake waits another period, without backoff.
    assert!(!woken.due(&id, since, HOUR, since + HOUR + HOUR / 2));
    assert!(woken.due(&id, since, HOUR, since + HOUR * 2));
    // A turn ending later counts from its end.
    let later = since + HOUR * 3;
    assert!(!woken.due(&id, later, HOUR, later + HOUR / 2));
    assert!(woken.due(&id, later, HOUR, later + HOUR));
}

/// A registered repository with a workspace for each of `names`, its agent
/// recorded in the worker mode and idle.
async fn idle_workers<const N: usize>(
    manager: &Manager,
    temp: &tempfile::TempDir,
    names: [&str; N],
) -> (Repository, [Workspace; N]) {
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
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET mode=?2 WHERE id=?1",
                    rusqlite::params![id, ModeName::DEFAULT],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        report(manager, &workspace.id, AgentState::Idle).await;
        created.push(workspace);
    }
    (repo, created.try_into().unwrap())
}

async fn report(manager: &Manager, id: &SpaceId, state: AgentState) {
    (manager.set_agent_state(&SpaceSelector::id(id), state, None, None))
        .await
        .unwrap();
}

/// The unread messages of the space `id`.
async fn messages(manager: &Manager, id: &SpaceId) -> Vec<String> {
    (manager
        .space_messages(&SpaceSelector::id(id))
        .await
        .unwrap())
    .into_iter()
    .map(|message| message.message)
    .collect()
}

/// Run a sweep an hour and a minute from now.
async fn sweep_later(manager: &Manager, woken: &mut Woken) {
    let later = UnixTime::now() + HOUR + Duration::from_secs(60);
    manager.wake_idle_agents(woken, later).await.unwrap();
}

#[tokio::test]
async fn a_worker_idle_for_its_period_is_woken_unless_done_or_sleeping() {
    let (temp, manager) = manager().await;
    let names = ["idle", "done", "sleeping", "permanent"];
    let (_, [idle, done, sleeping, permanent]) = idle_workers(&manager, &temp, names).await;
    let selector = |workspace: &Workspace| SpaceSelector::id(&workspace.id);
    manager
        .mark_space_done(&selector(&done), Done::default(), &Attacher::User)
        .await
        .unwrap();
    let slept = manager
        .sleep(&selector(&sleeping), Some(Sleep::Turn))
        .await
        .unwrap();
    assert_eq!(
        (slept.sleep, slept.idle_wake_minutes),
        (Some(Sleep::Turn), Some(60))
    );
    (manager.sleep(&selector(&permanent), Some(Sleep::Permanent)))
        .await
        .unwrap();
    let mut woken = Woken::default();
    // Not yet idle for its period.
    manager
        .wake_idle_agents(&mut woken, UnixTime::now())
        .await
        .unwrap();
    assert!(messages(&manager, &idle.id).await.is_empty());
    sweep_later(&manager, &mut woken).await;
    let wake = messages(&manager, &idle.id).await;
    assert_eq!(wake.len(), 1);
    assert!(wake[0].contains("idle for 60 minutes"), "{wake:?}");
    for quiet in [&done, &sleeping, &permanent] {
        assert!(messages(&manager, &quiet.id).await.is_empty());
    }
    // A turn ends a plain sleep, so the agent idle after it is woken again;
    // a permanent sleep lasts.
    for agent in [&sleeping, &permanent] {
        report(&manager, &agent.id, AgentState::Working).await;
        report(&manager, &agent.id, AgentState::Idle).await;
    }
    sweep_later(&manager, &mut Woken::default()).await;
    assert_eq!(messages(&manager, &sleeping.id).await.len(), 1);
    assert!(messages(&manager, &permanent.id).await.is_empty());
    // Undo ends a permanent sleep.
    let undone = (manager.sleep(&selector(&permanent), None)).await.unwrap();
    assert_eq!(undone.sleep, None);
    sweep_later(&manager, &mut Woken::default()).await;
    assert_eq!(messages(&manager, &permanent.id).await.len(), 1);
}

#[tokio::test]
async fn a_sleep_lasts_through_its_turn_and_a_wait_for_the_user() {
    let (temp, manager) = manager().await;
    let (_, [workspace]) = idle_workers(&manager, &temp, ["waits"]).await;
    let selector = SpaceSelector::id(&workspace.id);
    report(&manager, &workspace.id, AgentState::Working).await;
    manager.sleep(&selector, Some(Sleep::Turn)).await.unwrap();
    // A permission prompt in the turn that slept starts no turn.
    report(&manager, &workspace.id, AgentState::Waiting).await;
    report(&manager, &workspace.id, AgentState::Working).await;
    report(&manager, &workspace.id, AgentState::Idle).await;
    let recorded = manager.space(&selector).await.unwrap().sleep();
    assert_eq!(recorded, Some(Sleep::Turn));
}

#[tokio::test]
async fn a_chat_sleeps_only_in_a_mode_that_wakes_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    configure(&manager, "[modes.plan]\nidle_wake_minutes = 30\n").await;
    let talks = chat(&manager, "talks", ModeName::CHAT, Some(&repo), None).await;
    let plans = chat(&manager, "plans", ModeName::PLAN, Some(&repo), None).await;
    let slept = (manager.sleep(&SpaceSelector::id(&talks.id), Some(Sleep::Permanent)))
        .await
        .unwrap();
    assert_eq!((slept.sleep, slept.idle_wake_minutes), (None, None));
    for chat in [&talks, &plans] {
        report(&manager, &chat.id, AgentState::Idle).await;
    }
    sweep_later(&manager, &mut Woken::default()).await;
    assert!(messages(&manager, &talks.id).await.is_empty());
    let wake = messages(&manager, &plans.id).await;
    assert!(wake[0].contains("idle for 30 minutes"), "{wake:?}");
}

#[tokio::test]
async fn a_wake_read_before_the_agent_worked_again_is_not_queued() {
    let (temp, manager) = manager().await;
    let (_, [worked]) = idle_workers(&manager, &temp, ["worked"]).await;
    let mut idle = (manager.store.read(|db| agent_state::idle(db)))
        .await
        .unwrap();
    // Before the wake is queued, the agent works and goes idle again, within
    // the same second.
    report(&manager, &worked.id, AgentState::Working).await;
    report(&manager, &worked.id, AgentState::Idle).await;
    let later = UnixTime::now() + HOUR * 2;
    (manager.wake_if_due(&idle.remove(0), &mut Woken::default(), later))
        .await
        .unwrap();
    assert!(messages(&manager, &worked.id).await.is_empty());
}
