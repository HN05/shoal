use serde_json::json;

use crate::stand_in::Fixture;

#[tokio::test]
async fn the_sweep_keeps_the_directory_of_a_recorded_claim() {
    let fixture = Fixture::new("").await;
    let job = serde_json::from_value(json!({
        "job": {"handle": "h", "id": 1, "run": 1, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": "ci.yml", "commit": "abc", "trigger": "other"},
    }))
    .unwrap();
    let claim = fixture.record_claim("claimed", &job, None, None).await;
    // A claim still running keeps its job, so its directory stays too.
    fixture.runner.jobs.begin_claim(&claim.id);
    let kept = fixture.runner.job_directory(&claim.id);
    let stray = fixture.runner.job_directory(&"stray".into());
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::create_dir_all(&stray).unwrap();

    let daemon = fixture.daemon();
    fixture.runner.finish_ended_jobs(&*daemon).await.unwrap();

    assert!(kept.exists());
    assert!(!stray.exists());
}
