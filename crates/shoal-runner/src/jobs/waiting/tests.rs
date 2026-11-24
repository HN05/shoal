use shoal_forge_types::ci::runner::{JobRun, Trigger};

use super::*;

#[test]
fn only_a_run_on_the_known_default_branch_is_on_it() {
    let run = |trigger| JobRun {
        url: "https://forge.example/team/repo/actions/runs/1"
            .parse()
            .unwrap(),
        workflow: None,
        commit: "abc".into(),
        trigger,
        reference: None,
    };
    let main = run(Trigger::Branch("main".into()));
    assert!(on_branch(&main, Some("main")));
    assert!(!on_branch(&main, None));
    assert!(!on_branch(
        &run(Trigger::Branch("topic".into())),
        Some("main")
    ));
    assert!(!on_branch(&run(Trigger::Other), Some("main")));
}

#[tokio::test]
async fn waiting_jobs_are_read_every_interval_while_none_could_start() {
    use std::sync::atomic::Ordering;

    use shoal_forge::test_support::Server;
    use shoal_forge_types::backends::ForgeKind;

    use crate::{jobs::INTERVAL, stand_in::Fixture};

    let fixture = Fixture::new("[runner]\nenabled = true\n").await;
    let server = Server::start(|_| (404, Vec::new(), b"{}".to_vec()));
    let url = format!("http://{}/team/repo", server.host);
    fixture.place_on_forge(&url, ForgeKind::Forgejo);
    fixture.configure(&format!(
        "[machine.forges.\"{}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n",
        server.host
    ));
    let repository = fixture.repository().clone();
    (fixture.runner.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO runners(repository_id,forge_repository,forge_id,uuid,token)
                 VALUES (?1,?2,5,'u','t')",
                rusqlite::params![repository, url],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.daemon.could_start.store(false, Ordering::Relaxed);
    let reads = || {
        (server.received().iter())
            .filter(|request| {
                (request.line).starts_with("GET /api/v1/repos/team/repo/actions/runners/jobs?")
            })
            .count()
    };
    let poll = || async {
        let daemon = fixture.daemon();
        fixture.runner.poll_waiting_jobs(&*daemon).await.unwrap();
    };

    // Half the interval leaves no read due, however slow the runner.
    fixture.runner.jobs.age_waiting_read(INTERVAL / 2);
    poll().await;
    assert_eq!(reads(), 0);
    fixture.runner.jobs.age_waiting_read(INTERVAL);
    poll().await;
    assert_eq!(reads(), 1);
}
