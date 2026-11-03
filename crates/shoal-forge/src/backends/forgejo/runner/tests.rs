use std::path::Path;

use serde_json::json;

use super::*;

fn repo() -> ForgeRepo {
    ForgeRepo::parse("https://forge.example/team/repo").unwrap()
}

fn run(event: &str, payload: Value) -> Value {
    json!({
        "id": 24857,
        "index_in_repo": 15252,
        "event": event,
        "commit_sha": "abc123",
        "workflow_id": "ci.yml",
        "event_payload": payload.to_string(),
    })
}

#[test]
fn the_job_search_lists_waiting_jobs_and_skips_others() {
    assert_eq!(
        waiting_jobs_path(&["shoal".into(), "shoal-linux-x86_64".into()]),
        "actions/runners/jobs?labels=shoal%2Cshoal-linux-x86_64"
    );
    let waiting = json!({
        "handle": "h1", "id": 7, "run_id": 3, "name": "rust",
        "runs_on": ["shoal"], "status": "waiting",
    });
    assert_eq!(
        waiting_job(&waiting).unwrap(),
        Some(WaitingJob {
            handle: "h1".into(),
            id: 7,
            run: 3,
            name: "rust".into(),
            labels: vec!["shoal".into()],
        })
    );
    let running =
        json!({"handle": "h2", "id": 8, "run_id": 3, "name": "rust", "status": "running"});
    assert_eq!(waiting_job(&running).unwrap(), None);
    let error = waiting_job(&json!({"id": 9, "run_id": 3, "name": "rust"})).unwrap_err();
    assert!(error.to_string().contains("handle"), "{error}");
}

#[test]
fn a_run_names_the_pr_or_branch_whose_ref_serves_its_commit() {
    let pull = run(
        "pull_request",
        json!({"number": 12, "pull_request": {"number": 12}}),
    );
    let read = job_run(&repo(), &pull).unwrap();
    assert_eq!(
        read.url,
        "https://forge.example/team/repo/actions/runs/15252"
    );
    assert_eq!(read.workflow.as_deref(), Some("ci.yml"));
    assert_eq!(read.commit, "abc123");
    assert_eq!(read.trigger, Trigger::Pull { number: 12 });
    assert_eq!(read.reference.as_deref(), Some("refs/pull/12/head"));

    let push = job_run(&repo(), &run("push", json!({"ref": "refs/heads/main"}))).unwrap();
    assert_eq!(push.trigger, Trigger::Branch("main".into()));
    assert_eq!(push.reference.as_deref(), Some("refs/heads/main"));
    let dispatch = run("workflow_dispatch", json!({"ref": "refs/heads/topic"}));
    let dispatch = job_run(&repo(), &dispatch).unwrap();
    assert_eq!(dispatch.trigger, Trigger::Branch("topic".into()));
    let tag = job_run(&repo(), &run("push", json!({"ref": "refs/tags/v1"}))).unwrap();
    assert_eq!(tag.trigger, Trigger::Other);
    assert_eq!(tag.reference.as_deref(), Some("refs/tags/v1"));
    let schedule = job_run(&repo(), &run("schedule", json!({}))).unwrap();
    assert_eq!(
        (schedule.trigger, schedule.reference),
        (Trigger::Other, None)
    );
}

#[test]
fn registration_and_deletion_act_on_the_repository_runner() {
    let request = register(&repo(), "shoal-host");
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(request.endpoint, "repos/team/repo/actions/runners");
    assert_eq!(request.body.as_ref().unwrap()["name"], "shoal-host");
    let runner = registered(&json!({"id": 4, "uuid": "u", "token": "t"})).unwrap();
    assert_eq!(
        runner,
        RegisteredRunner {
            id: 4,
            uuid: "u".into(),
            token: "t".into(),
        }
    );
    let request = delete(&repo(), 4);
    assert_eq!(request.method, HttpMethod::Delete);
    assert_eq!(request.endpoint, "repos/team/repo/actions/runners/4");
}

#[test]
fn one_job_claims_the_handle_on_the_host_backend_with_its_files_in_the_work_directory() {
    let runner = RegisteredRunner {
        id: 4,
        uuid: "u".into(),
        token: "t".into(),
    };
    let labels = ["shoal".to_owned(), "shoal-macos-arm64".to_owned()];
    let launch = JobLaunch {
        runner: &runner,
        token_file: Path::new("/state/runner/token"),
        config_file: Path::new("/work/config.yaml"),
        work_dir: Path::new("/work dir"),
        labels: &labels,
        handle: "h1",
    };
    let command = job_command(&repo(), &launch).unwrap();
    let argv: Vec<_> = command
        .argv
        .iter()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(
        argv,
        [
            "forgejo-runner",
            "--config",
            "/work/config.yaml",
            "one-job",
            "--url",
            "https://forge.example",
            "--uuid",
            "u",
            "--token-url",
            "file:/state/runner/token",
            "--handle",
            "h1",
            "--label",
            "shoal:host",
            "--label",
            "shoal-macos-arm64:host",
        ]
    );
    assert!(
        command
            .config
            .contains("workdir_parent: \"/work dir/host\""),
        "{}",
        command.config
    );
    assert!(
        command.config.contains("file: \"/work dir/.runner\""),
        "{}",
        command.config
    );
}
