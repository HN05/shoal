//! Triggers firing through the launch `shoal start` uses: when their times
//! come, across restarts, against their overlap, key and filter, and as a
//! scoped caller's permission allows.
use std::{sync::Arc, time::Duration};

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::triggers::{RunOrigin, RunOutcome, TriggerRun};

use crate::{
    daemon::{
        approvals::Undecided,
        chats::launches::tests::STARTED,
        scope::Caller,
        test_support::{chat, configure, manager, repository},
        triggers::firing::Firing,
        workspace::Manager,
    },
    model::Repository,
    paths::Paths,
    protocol::ItemContext,
    time::UnixTime,
};

const HOUR: Duration = Duration::from_secs(3600);

/// A manager with a registered repository whose local config defines
/// `triggers`.
async fn with_triggers(triggers: &str) -> (tempfile::TempDir, Arc<Manager>, Repository) {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    (manager.write_local_config(&repo.id, Some(triggers.into())))
        .await
        .unwrap();
    (temp, manager, repo)
}

/// The context of a request made in `repo`'s checkout.
fn context(repo: &Repository) -> Option<ItemContext> {
    Some(ItemContext::Checkout(repo.id.clone()))
}

/// Set the runtime state of the repository's trigger `name`: counting
/// from `counted_from`, last fired at `fired`.
async fn set_state(
    manager: &Manager,
    repo: &Repository,
    name: &str,
    counted_from: UnixTime,
    fired: Option<UnixTime>,
) {
    let (scope, name) = (repo.id.to_string(), name.to_owned());
    manager
        .store
        .write(move |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO trigger_states(scope,name,created_at,counted_from,last_fired_at)
                    VALUES (?1,?2,?3,?3,?4)",
                rusqlite::params![scope, name, counted_from, fired],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The runs of the repository's trigger `name`, oldest first.
async fn runs(manager: &Manager, repo: &Repository, name: &str) -> Vec<TriggerRun> {
    let (scope, name) = (repo.id.to_string(), name.to_owned());
    manager
        .store
        .read(move |tx| {
            let mut statement = tx.prepare(
                "SELECT at,origin,outcome,reason,space_id FROM trigger_runs
                    WHERE scope=?1 AND name=?2 ORDER BY id",
            )?;
            let runs = statement.query_map(rusqlite::params![scope, name], |row| {
                Ok(TriggerRun {
                    at: row.get(0)?,
                    origin: row.get(1)?,
                    outcome: row.get(2)?,
                    reason: row.get(3)?,
                    space_id: row.get(4)?,
                })
            })?;
            Ok(runs.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
}

/// Fire the repository's trigger `name` now, as `shoal trigger run` does.
async fn fire(manager: &Arc<Manager>, repo: &Repository, name: &str) -> Option<TriggerRun> {
    let trigger = (manager.configured_triggers().await.into_iter())
        .find(|trigger| trigger.repository.as_ref() == Some(&repo.id) && trigger.name == name)
        .unwrap();
    let firing = Firing {
        origin: RunOrigin::Manual,
        input: serde_json::json!({ "trigger": name }),
        at: UnixTime::now(),
    };
    manager.fire_trigger(&trigger, firing).await.unwrap()
}

fn outcomes(runs: &[TriggerRun]) -> Vec<RunOutcome> {
    runs.iter().map(|run| run.outcome).collect()
}

/// The tasks of the chats the triggers started, oldest first.
fn started_tasks() -> Vec<Vec<String>> {
    let started = STARTED.lock().unwrap();
    started
        .iter()
        .map(|(start, _)| start.args.clone())
        .collect()
}

const HOURLY: &str = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\n\
                      agent = 'claude'\ntask = 'Check since {{ previous_at }}'\n";

#[tokio::test]
async fn a_due_time_fires_once_through_the_launch() {
    let (_temp, manager, repo) = with_triggers(HOURLY).await;
    let fired = UnixTime::now() - HOUR - Duration::from_secs(60);
    set_state(&manager, &repo, "check", fired, Some(fired)).await;

    let now = UnixTime::now();
    let next = manager.trigger_pass(now).await;
    manager.trigger_pass(now).await;

    let runs = runs(&manager, &repo, "check").await;
    assert_eq!(outcomes(&runs), [RunOutcome::Started]);
    assert_eq!(runs[0].origin, RunOrigin::Source);
    let tasks = started_tasks();
    assert_eq!(tasks.len(), 1);
    assert!(!tasks[0][0].contains("{{"), "{:?}", tasks[0]);
    // The next hour counts from this firing.
    assert_eq!(next, Some(now + HOUR));
}

#[tokio::test]
async fn every_keeps_counting_across_a_restart_and_catches_up_once() {
    let (temp, manager, repo) = with_triggers(HOURLY).await;
    let fired = UnixTime::now() - 3 * HOUR - Duration::from_secs(30 * 60);
    set_state(&manager, &repo, "check", fired, Some(fired)).await;
    drop(manager);

    // A daemon started again reads the last firing, so 3½ hours later the
    // times 1h and 2h after it are too old to catch up and 3h is not.
    let manager = Manager::open(Paths::for_test(temp.path())).await.unwrap();
    manager.trigger_pass(UnixTime::now()).await;

    let runs = runs(&manager, &repo, "check").await;
    assert_eq!(outcomes(&runs), [RunOutcome::Skipped, RunOutcome::Started]);
    assert_eq!(runs[0].at, fired + HOUR);
    assert_eq!(started_tasks().len(), 1);
}

#[tokio::test]
async fn a_time_beyond_catch_up_is_skipped() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\ncatch_up = '1m'\n\
                  mode = 'chat'\nagent = 'claude'\ntask = 'Check'\n";
    let (_temp, manager, repo) = with_triggers(config).await;
    let fired = UnixTime::now() - 3 * HOUR - Duration::from_secs(30 * 60);
    set_state(&manager, &repo, "check", fired, Some(fired)).await;

    manager.trigger_pass(UnixTime::now()).await;

    let runs = runs(&manager, &repo, "check").await;
    assert_eq!(outcomes(&runs), [RunOutcome::Skipped]);
    assert!(started_tasks().is_empty());
}

#[tokio::test]
async fn a_paused_trigger_waits_and_resumes_without_catching_up() {
    let (_temp, manager, repo) = with_triggers(HOURLY).await;
    let status = (manager.pause_trigger(context(&repo), "check", true, None))
        .await
        .unwrap();
    assert!(status.paused && status.next_at.is_none());
    let fired = UnixTime::now() - 2 * HOUR;
    set_state(&manager, &repo, "check", fired, Some(fired)).await;
    (manager.pause_trigger(context(&repo), "check", true, None))
        .await
        .unwrap();
    manager.trigger_pass(UnixTime::now()).await;
    assert!(runs(&manager, &repo, "check").await.is_empty());

    let status = (manager.pause_trigger(context(&repo), "check", false, None))
        .await
        .unwrap();
    manager.trigger_pass(UnixTime::now()).await;
    assert!(runs(&manager, &repo, "check").await.is_empty());
    assert!(status.next_at.unwrap() > UnixTime::now());
}

/// Record a started run of the repository's trigger `name` in `space`.
async fn started_in(manager: &Manager, repo: &Repository, name: &str, space: &SpaceId) {
    let (scope, name) = (repo.id.to_string(), name.to_owned());
    let run = TriggerRun {
        at: UnixTime::now(),
        origin: RunOrigin::Source,
        outcome: RunOutcome::Started,
        reason: None,
        space_id: Some(space.clone()),
    };
    manager
        .store
        .write(move |tx| super::state::record(tx, &scope, &name, &run, None))
        .await
        .unwrap();
}

fn overlapping(overlap: &str) -> String {
    format!(
        "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
         task = 'Check'\noverlap = '{overlap}'\n"
    )
}

#[tokio::test]
async fn overlap_skip_drops_a_firing_while_the_last_agent_runs() {
    let (_temp, manager, repo) = with_triggers(&overlapping("skip")).await;
    let last = chat(&manager, "last", "chat", Some(&repo), None).await;
    started_in(&manager, &repo, "check", &last.id).await;

    let run = fire(&manager, &repo, "check").await;
    let run = run.unwrap();
    assert_eq!(run.outcome, RunOutcome::Skipped);
    assert_eq!(run.origin, RunOrigin::Manual);
    assert!(started_tasks().is_empty());
}

#[tokio::test]
async fn overlap_queue_launches_once_the_last_agent_stops() {
    let (_temp, manager, repo) = with_triggers(&overlapping("queue")).await;
    let last = chat(&manager, "last", "chat", Some(&repo), None).await;
    started_in(&manager, &repo, "check", &last.id).await;

    let first = fire(&manager, &repo, "check").await;
    assert_eq!(first.unwrap().outcome, RunOutcome::Queued);
    // A second firing finds one waiting already.
    let second = fire(&manager, &repo, "check").await;
    assert_eq!(second.unwrap().outcome, RunOutcome::Skipped);
    manager.trigger_pass(UnixTime::now()).await;
    assert!(started_tasks().is_empty());

    let id = last.id.clone();
    manager
        .store
        .write(move |tx| {
            tx.execute("DELETE FROM executions WHERE space_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    // Once that agent stopped, a firing before the queued run launches still
    // finds it waiting, so only the queued run starts.
    let third = fire(&manager, &repo, "check").await;
    assert_eq!(third.unwrap().outcome, RunOutcome::Skipped);
    manager.trigger_pass(UnixTime::now()).await;

    let runs = runs(&manager, &repo, "check").await;
    assert_eq!(
        outcomes(&runs),
        [
            RunOutcome::Started,
            RunOutcome::Started,
            RunOutcome::Skipped,
            RunOutcome::Skipped
        ]
    );
    assert_eq!(started_tasks(), [vec!["Check".to_owned()]]);
}

#[tokio::test]
async fn overlap_parallel_launches_at_once() {
    let (_temp, manager, repo) = with_triggers(&overlapping("parallel")).await;
    let last = chat(&manager, "last", "chat", Some(&repo), None).await;
    started_in(&manager, &repo, "check", &last.id).await;

    let run = fire(&manager, &repo, "check").await;
    assert_eq!(run.unwrap().outcome, RunOutcome::Started);
    assert_eq!(started_tasks().len(), 1);
}

#[tokio::test]
async fn a_key_drops_a_repeat_within_its_window() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
                  task = 'Check'\noverlap = 'parallel'\nkey = '{{ trigger }}'\nkey_window = '1h'\n";
    let (_temp, manager, repo) = with_triggers(config).await;

    let first = fire(&manager, &repo, "check").await;
    let second = fire(&manager, &repo, "check").await;

    assert_eq!(first.unwrap().outcome, RunOutcome::Started);
    assert_eq!(second.unwrap().outcome, RunOutcome::Duplicate);
    assert_eq!(started_tasks().len(), 1);
}

#[tokio::test]
async fn a_filter_drops_a_firing_without_a_run() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
                  task = 'Check'\nfilter = [{ path = 'trigger', equals = 'other' }]\n";
    let (_temp, manager, repo) = with_triggers(config).await;

    let run = fire(&manager, &repo, "check").await;

    assert_eq!(run, None);
    assert!(runs(&manager, &repo, "check").await.is_empty());
}

#[tokio::test]
async fn a_missing_value_fails_the_firing_with_its_path() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
                  task = 'Fix {{ payload.issue }}'\n";
    let (_temp, manager, repo) = with_triggers(config).await;

    let run = fire(&manager, &repo, "check").await;

    let run = run.unwrap();
    assert_eq!(run.outcome, RunOutcome::Failed);
    assert!(run.reason.unwrap().contains("payload.issue"));
    assert!(started_tasks().is_empty());
}

#[tokio::test]
async fn a_global_trigger_is_listed_beside_the_repositorys() {
    let (_temp, manager, repo) = with_triggers(HOURLY).await;
    configure(
        &manager,
        "[cleanup]\ngrace_minutes = 0\n[triggers.nightly]\non = 'timer'\nat = '02:00'\n\
         mode = 'chat'\nagent = 'claude'\ntask = 'Nightly'\n",
    )
    .await;

    let listed = manager.list_triggers(context(&repo)).await.unwrap();
    let names: Vec<_> = (listed.iter())
        .map(|status| (status.name.as_str(), status.repository.is_some()))
        .collect();
    assert_eq!(names, [("nightly", false), ("check", true)]);
    assert!(listed.iter().all(|status| status.next_at.is_some()));
    let global = manager.list_triggers(None).await.unwrap();
    assert_eq!(global.len(), 1);
}

#[tokio::test]
async fn a_scoped_run_follows_the_triggers_permission() {
    let (_temp, manager, repo) = with_triggers(HOURLY).await;
    let agent = chat(&manager, "agent", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: agent.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };

    // The built-in modes propose it: an approval request, and no run.
    let error = (manager.run_trigger(context(&repo), "check", Some(&caller)))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_some(), "{error:#}");
    assert!(runs(&manager, &repo, "check").await.is_empty());

    let config = format!("{HOURLY}[permissions]\ntriggers = 'direct'\n");
    (manager.write_local_config(&repo.id, Some(config)))
        .await
        .unwrap();
    let run = manager
        .run_trigger(context(&repo), "check", Some(&caller))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (run.outcome, run.origin),
        (RunOutcome::Started, RunOrigin::Manual)
    );
}
