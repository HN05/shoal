use super::*;

const GIB: u64 = 1 << 30;

/// A store with one repository, `repo`.
fn database() -> Connection {
    let db = shoal_store::test_support::migrated().unwrap();
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )
    .unwrap();
    db
}

fn repository() -> RepositoryId {
    RepositoryId::from("repo")
}

fn job(name: &str) -> RunKey {
    RunKey::CiJob {
        workflow: "ci.yml".into(),
        name: name.into(),
    }
}

/// A run that ended at `ended_at` seconds, reaching `peak` bytes.
fn run(ended_at: i64, peak: u64) -> RunUsage {
    RunUsage {
        ended_at: UnixTime::from_seconds(ended_at),
        peak_memory: peak,
        duration: Duration::from_secs(600),
        whole: true,
        oom_killed: false,
    }
}

/// Keep `runs` of the job `rust`, the last one latest.
fn record(db: &Connection, runs: impl IntoIterator<Item = RunUsage>) {
    for run in runs {
        record_run(db, &repository(), &job("rust"), &run).unwrap();
    }
}

fn estimate(db: &Connection) -> Option<u64> {
    expected_memory(db, &repository(), &job("rust")).unwrap()
}

fn kept_runs(db: &Connection) -> i64 {
    (db.query_row("SELECT count(*) FROM run_usage", [], |row| row.get(0))).unwrap()
}

#[test]
fn a_key_counts_at_the_95th_percentile_of_its_whole_runs() {
    let db = database();
    assert_eq!(estimate(&db), None);

    // With few runs, the estimate stays close to the highest.
    record(&db, [run(1, 10 * GIB), run(2, 20 * GIB)]);
    assert_eq!(estimate(&db), Some(GIB * 39 / 2));

    // One outlier among the latest 30 runs no longer sets the estimate.
    record(
        &db,
        (3..=32).map(|n| run(n, if n == 3 { 23 } else { 15 } * GIB)),
    );
    assert_eq!(estimate(&db), Some(15 * GIB));
    assert_eq!(
        expected_memory(&db, &repository(), &job("docs")).unwrap(),
        None
    );
}

#[test]
fn only_the_latest_whole_runs_count_whatever_their_age() {
    let db = database();
    record(&db, [run(1, 40 * GIB)]);
    record(&db, (2..=31).map(|n| run(n, 10 * GIB)));
    assert_eq!(estimate(&db), Some(10 * GIB));
    assert_eq!(kept_runs(&db), WHOLE_RUNS);
}

#[test]
fn runs_cut_short_bound_the_estimate_only_until_a_whole_run() {
    let db = database();
    let cut_short = |ended_at, peak| RunUsage {
        whole: false,
        ..run(ended_at, peak)
    };
    record(&db, [cut_short(1, 6 * GIB), cut_short(2, 4 * GIB)]);
    assert_eq!(estimate(&db), Some(6 * GIB));

    record(&db, [run(3, 9 * GIB), cut_short(4, GIB)]);
    assert_eq!(estimate(&db), Some(9 * GIB));
    assert_eq!(kept_runs(&db), 1, "the runs cut short are forgotten");
}

#[test]
fn a_run_the_oom_killer_hit_counts_as_whole_above_its_peak() {
    let db = database();
    record(
        &db,
        [RunUsage {
            whole: false,
            oom_killed: true,
            ..run(1, 10 * GIB)
        }],
    );
    assert_eq!(estimate(&db), Some(12 * GIB));
}

#[test]
fn a_key_that_has_not_run_for_the_kept_period_is_forgotten() {
    let db = database();
    record(&db, [run(1, 10 * GIB)]);
    let later = 2 + i64::try_from(KEPT_FOR.as_secs()).unwrap();
    record_run(&db, &repository(), &job("docs"), &run(later, GIB)).unwrap();
    assert_eq!(estimate(&db), None);
    assert_eq!(kept_runs(&db), 1);
}
