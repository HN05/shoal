//! Triggers firing through the daemon's launch: when their times come,
//! across restarts, against their overlap, key and filter.
use std::time::Duration;

use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};
use shoal_protocol::triggers::{Fired, RunOrigin, RunOutcome, TriggerRun};

use super::{firing::Firing, state};
use crate::{Runner, stand_in::Fixture, time::UnixTime};

const HOUR: Duration = Duration::from_secs(3600);

/// Set the runtime state of the repository's trigger `name`: counting
/// from `counted_from`, last fired at `fired`.
async fn set_state(fixture: &Fixture, name: &str, counted_from: UnixTime, fired: Option<UnixTime>) {
    let (scope, name) = (fixture.repository().to_string(), name.to_owned());
    fixture
        .runner
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

/// The runs of the repository's trigger `name` that `runner` recorded,
/// oldest first.
async fn runs_of(runner: &Runner, fixture: &Fixture, name: &str) -> Vec<TriggerRun> {
    let (scope, name) = (fixture.repository().to_string(), name.to_owned());
    runner
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
                    queue_entry: None,
                })
            })?;
            Ok(runs.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap()
}

/// The runs of the repository's trigger `name`, oldest first.
pub(super) async fn runs(fixture: &Fixture, name: &str) -> Vec<TriggerRun> {
    runs_of(&fixture.runner, fixture, name).await
}

/// Fire the repository's trigger `name` now, as `shoal trigger run` does:
/// the run it made, none when its filter dropped it.
async fn fire(fixture: &Fixture, name: &str) -> Option<TriggerRun> {
    let daemon = fixture.daemon();
    let triggers = fixture.runner.configured_triggers(&*daemon).await;
    let trigger = (triggers.into_iter())
        .find(|trigger| trigger.repository.as_ref() == Some(fixture.repository()))
        .filter(|trigger| trigger.name == name)
        .unwrap();
    let firing = Firing {
        origin: RunOrigin::Manual,
        input: serde_json::json!({ "trigger": name }).into(),
        at: UnixTime::now(),
    };
    match fixture.runner.fire_trigger(&daemon, &trigger, firing).await {
        Ok(Fired::Ran { run }) => Some(run),
        Ok(Fired::Dropped { .. }) => None,
        Err(error) => panic!("{error:#}"),
    }
}

/// Run the triggers' pass at `now`, returning when a timer comes next.
async fn pass(fixture: &Fixture, now: UnixTime) -> Option<UnixTime> {
    fixture.runner.trigger_pass(&fixture.daemon(), now).await
}

/// Pause or resume the repository's trigger `name`.
async fn pause(
    fixture: &Fixture,
    name: &str,
    paused: bool,
) -> shoal_protocol::triggers::TriggerStatus {
    let daemon = fixture.daemon();
    (fixture.runner)
        .pause_trigger(&*daemon, fixture.context(), name, paused, None)
        .await
        .unwrap()
}

pub(super) fn outcomes(runs: &[TriggerRun]) -> Vec<RunOutcome> {
    runs.iter().map(|run| run.outcome).collect()
}

const HOURLY: &str = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\n\
                      agent = 'claude'\ntask = 'Check since {{ previous_at }}'\n";

#[tokio::test]
async fn a_due_time_fires_once_through_the_launch() {
    let fixture = Fixture::new(HOURLY).await;
    let fired = UnixTime::now() - HOUR - Duration::from_secs(60);
    set_state(&fixture, "check", fired, Some(fired)).await;

    let now = UnixTime::now();
    let next = pass(&fixture, now).await;
    pass(&fixture, now).await;

    let runs = runs(&fixture, "check").await;
    assert_eq!(outcomes(&runs), [RunOutcome::Started]);
    assert_eq!(runs[0].origin, RunOrigin::Source);
    // The run names the space its launch opened.
    let opened: Vec<_> = (fixture.daemon.opened_spaces().into_iter())
        .map(Some)
        .collect();
    assert_eq!(opened, [runs[0].space_id.clone()]);
    let tasks = fixture.daemon.tasks();
    assert!(!tasks[0][0].contains("{{"), "{:?}", tasks[0]);
    // The next hour counts from this firing.
    assert_eq!(next, Some(now + HOUR));
}

#[tokio::test]
async fn every_keeps_counting_across_a_restart_and_catches_up_once() {
    let fixture = Fixture::new(HOURLY).await;
    let fired = UnixTime::now() - 3 * HOUR - Duration::from_secs(30 * 60);
    set_state(&fixture, "check", fired, Some(fired)).await;

    // A daemon started again reads the last firing, so 3½ hours later the
    // times 1h and 2h after it are too old to catch up and 3h is not.
    let restarted = fixture.restarted().await;
    restarted
        .trigger_pass(&fixture.daemon(), UnixTime::now())
        .await;

    let runs = runs_of(&restarted, &fixture, "check").await;
    assert_eq!(outcomes(&runs), [RunOutcome::Skipped, RunOutcome::Started]);
    assert_eq!(runs[0].at, fired + HOUR);
    assert_eq!(fixture.daemon.tasks().len(), 1);
}

#[tokio::test]
async fn a_time_beyond_catch_up_is_skipped() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\ncatch_up = '1m'\n\
                  mode = 'chat'\nagent = 'claude'\ntask = 'Check'\n";
    let fixture = Fixture::new(config).await;
    let fired = UnixTime::now() - 3 * HOUR - Duration::from_secs(30 * 60);
    set_state(&fixture, "check", fired, Some(fired)).await;

    pass(&fixture, UnixTime::now()).await;

    let runs = runs(&fixture, "check").await;
    assert_eq!(outcomes(&runs), [RunOutcome::Skipped]);
    assert!(fixture.daemon.tasks().is_empty());
}

#[tokio::test]
async fn a_paused_trigger_waits_and_resumes_without_catching_up() {
    let fixture = Fixture::new(HOURLY).await;
    let status = pause(&fixture, "check", true).await;
    assert!(status.paused && status.next_at.is_none());
    let fired = UnixTime::now() - 2 * HOUR;
    set_state(&fixture, "check", fired, Some(fired)).await;
    pause(&fixture, "check", true).await;
    pass(&fixture, UnixTime::now()).await;
    assert!(runs(&fixture, "check").await.is_empty());

    let status = pause(&fixture, "check", false).await;
    pass(&fixture, UnixTime::now()).await;
    assert!(runs(&fixture, "check").await.is_empty());
    assert!(status.next_at.unwrap() > UnixTime::now());
}

/// Record a started run of the repository's trigger `name` that launched
/// `space`, else the launch waiting in the agent queue as `entry`.
async fn record_started(
    fixture: &Fixture,
    name: &str,
    space_id: Option<SpaceId>,
    queue_entry: Option<QueueEntryId>,
) {
    let (scope, name) = (fixture.repository().to_string(), name.to_owned());
    let run = TriggerRun {
        at: UnixTime::now(),
        origin: RunOrigin::Source,
        outcome: RunOutcome::Started,
        reason: None,
        space_id,
        queue_entry,
    };
    fixture
        .runner
        .store
        .write(move |tx| state::record(tx, &scope, &name, &run, None))
        .await
        .unwrap();
}

/// A space whose agent runs, which a started run of the repository's
/// trigger `name` launched.
async fn last_agent(fixture: &Fixture, name: &str) -> SpaceId {
    let last = SpaceId::generate();
    fixture.daemon.run_agent(&last);
    record_started(fixture, name, Some(last.clone()), None).await;
    last
}

fn overlapping(overlap: &str) -> String {
    format!(
        "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
         task = 'Check'\noverlap = '{overlap}'\n"
    )
}

#[tokio::test]
async fn overlap_skip_drops_a_firing_while_the_last_agent_runs() {
    let fixture = Fixture::new(&overlapping("skip")).await;
    last_agent(&fixture, "check").await;

    let run = fire(&fixture, "check").await;
    let run = run.unwrap();
    assert_eq!(run.outcome, RunOutcome::Skipped);
    assert_eq!(run.origin, RunOrigin::Manual);
    assert!(fixture.daemon.tasks().is_empty());
}

#[tokio::test]
async fn overlap_queue_launches_once_the_last_agent_stops() {
    let fixture = Fixture::new(&overlapping("queue")).await;
    let last = last_agent(&fixture, "check").await;

    let first = fire(&fixture, "check").await;
    assert_eq!(first.unwrap().outcome, RunOutcome::Queued);
    // A second firing finds one waiting already.
    let second = fire(&fixture, "check").await;
    assert_eq!(second.unwrap().outcome, RunOutcome::Skipped);
    pass(&fixture, UnixTime::now()).await;
    assert!(fixture.daemon.tasks().is_empty());

    fixture.daemon.stop_agent(&last);
    // Once that agent stopped, a firing before the queued run launches still
    // finds it waiting, so only the queued run starts.
    let third = fire(&fixture, "check").await;
    assert_eq!(third.unwrap().outcome, RunOutcome::Skipped);
    pass(&fixture, UnixTime::now()).await;

    let runs = runs(&fixture, "check").await;
    assert_eq!(
        outcomes(&runs),
        [
            RunOutcome::Started,
            RunOutcome::Started,
            RunOutcome::Skipped,
            RunOutcome::Skipped
        ]
    );
    assert_eq!(fixture.daemon.tasks(), [vec!["Check".to_owned()]]);
}

#[tokio::test]
async fn a_launch_waiting_in_the_agent_queue_overlaps_until_it_ends() {
    let fixture = Fixture::new(&overlapping("skip")).await;
    let entry = QueueEntryId::generate();
    fixture.daemon.queue.lock().unwrap().insert(entry.clone());
    record_started(&fixture, "check", None, Some(entry.clone())).await;

    let waiting = fire(&fixture, "check").await;
    assert_eq!(waiting.unwrap().outcome, RunOutcome::Skipped);
    fixture.daemon.queue.lock().unwrap().remove(&entry);
    let ended = fire(&fixture, "check").await;
    assert_eq!(ended.unwrap().outcome, RunOutcome::Started);
}

#[tokio::test]
async fn a_queued_launch_overlaps_through_the_space_it_opened() {
    let fixture = Fixture::new(&overlapping("skip")).await;
    let entry = QueueEntryId::generate();
    let opened = SpaceId::generate();
    record_started(&fixture, "check", None, Some(entry.clone())).await;
    // The launch left the queue for the space it opened, whose agent runs.
    fixture
        .daemon
        .opened
        .lock()
        .unwrap()
        .insert(entry, opened.clone());
    fixture.daemon.run_agent(&opened);

    let running = fire(&fixture, "check").await;
    assert_eq!(running.unwrap().outcome, RunOutcome::Skipped);
    fixture.daemon.stop_agent(&opened);
    let stopped = fire(&fixture, "check").await;
    assert_eq!(stopped.unwrap().outcome, RunOutcome::Started);
}

#[tokio::test]
async fn overlap_outlasts_a_history_longer_than_it_keeps() {
    let fixture = Fixture::new(&overlapping("skip")).await;
    last_agent(&fixture, "check").await;
    for _ in 0..=state::HISTORY {
        let skipped = fire(&fixture, "check").await;
        assert_eq!(skipped.unwrap().outcome, RunOutcome::Skipped);
    }

    let run = fire(&fixture, "check").await;

    assert_eq!(run.unwrap().outcome, RunOutcome::Skipped);
    assert!(fixture.daemon.tasks().is_empty());
}

#[tokio::test]
async fn a_paused_trigger_holds_its_queued_run() {
    let fixture = Fixture::new(&overlapping("queue")).await;
    let last = last_agent(&fixture, "check").await;
    let queued = fire(&fixture, "check").await;
    assert_eq!(queued.unwrap().outcome, RunOutcome::Queued);
    pause(&fixture, "check", true).await;
    fixture.daemon.stop_agent(&last);

    pass(&fixture, UnixTime::now()).await;
    assert!(fixture.daemon.tasks().is_empty());
    pause(&fixture, "check", false).await;
    pass(&fixture, UnixTime::now()).await;
    assert_eq!(fixture.daemon.tasks().len(), 1);
}

#[tokio::test]
async fn overlap_parallel_launches_at_once() {
    let fixture = Fixture::new(&overlapping("parallel")).await;
    last_agent(&fixture, "check").await;

    let run = fire(&fixture, "check").await;
    assert_eq!(run.unwrap().outcome, RunOutcome::Started);
    assert_eq!(fixture.daemon.tasks().len(), 1);
}

#[tokio::test]
async fn a_key_drops_a_repeat_within_its_window() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
                  task = 'Check'\noverlap = 'parallel'\nkey = '{{ trigger }}'\nkey_window = '1h'\n";
    let fixture = Fixture::new(config).await;

    let first = fire(&fixture, "check").await;
    let second = fire(&fixture, "check").await;

    assert_eq!(first.unwrap().outcome, RunOutcome::Started);
    assert_eq!(second.unwrap().outcome, RunOutcome::Duplicate);
    assert_eq!(fixture.daemon.tasks().len(), 1);
}

#[tokio::test]
async fn a_filter_drops_a_firing_without_a_run() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
                  task = 'Check'\nfilter = [{ path = 'trigger', equals = 'other' }]\n";
    let fixture = Fixture::new(config).await;

    let run = fire(&fixture, "check").await;

    assert_eq!(run, None);
    assert!(runs(&fixture, "check").await.is_empty());
}

#[tokio::test]
async fn a_missing_value_fails_the_firing_with_its_path() {
    let config = "[triggers.check]\non = 'timer'\nevery = '1h'\nmode = 'chat'\nagent = 'claude'\n\
                  task = 'Fix {{ payload.issue }}'\n";
    let fixture = Fixture::new(config).await;

    let run = fire(&fixture, "check").await;

    let run = run.unwrap();
    assert_eq!(run.outcome, RunOutcome::Failed);
    assert!(run.reason.unwrap().contains("payload.issue"));
    assert!(fixture.daemon.tasks().is_empty());
}

#[tokio::test]
async fn a_global_trigger_is_listed_beside_the_repositorys() {
    let fixture = Fixture::new(HOURLY).await;
    fixture.configure(
        "[triggers.nightly]\non = 'timer'\nat = '02:00'\n\
         mode = 'chat'\nagent = 'claude'\ntask = 'Nightly'\n",
    );
    let daemon = fixture.daemon();

    let listed = (fixture.runner.list_triggers(&*daemon, fixture.context()))
        .await
        .unwrap();
    let names: Vec<_> = (listed.iter())
        .map(|status| (status.name.as_str(), status.repository.is_some()))
        .collect();
    assert_eq!(names, [("nightly", false), ("check", true)]);
    assert!(listed.iter().all(|status| status.next_at.is_some()));
    let global = fixture.runner.list_triggers(&*daemon, None).await.unwrap();
    assert_eq!(global.len(), 1);
}

#[tokio::test]
async fn a_contribution_fires_only_its_local_configs_triggers() {
    let fixture = Fixture::new(HOURLY).await;
    *fixture.daemon.tracked.lock().unwrap() = "[triggers.upstream]\non = 'timer'\n\
        every = '1h'\nmode = 'chat'\nagent = 'claude'\ntask = 'Push'\n"
        .to_owned();
    let names = || async {
        let triggers = fixture.runner.configured_triggers(&*fixture.daemon()).await;
        let mut names: Vec<_> = triggers.into_iter().map(|trigger| trigger.name).collect();
        names.sort();
        names
    };
    assert_eq!(names().await, ["check", "upstream"]);

    (fixture.daemon.contribution).store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(names().await, ["check"]);
}
