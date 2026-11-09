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
    assert_eq!(waiting_job(&waiting).unwrap(), Some(job()));
    let running =
        json!({"handle": "h2", "id": 8, "run_id": 3, "name": "rust", "status": "running"});
    assert_eq!(waiting_job(&running).unwrap(), None);
    assert!(waiting_job(&json!({"id": 9, "run_id": 3, "name": "rust"})).is_err());
}

fn job() -> WaitingJob {
    WaitingJob {
        handle: "h1".into(),
        id: JobId::new(7),
        run: RunId::new(3),
        name: "rust".into(),
        labels: vec!["shoal".into()],
    }
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
            id: RunnerId::new(4),
            uuid: "u".into(),
            token: "t".into(),
        }
    );
    let request = delete(&repo(), RunnerId::new(4));
    assert_eq!(request.method, HttpMethod::Delete);
    assert_eq!(request.endpoint, "repos/team/repo/actions/runners/4");
}

#[test]
fn one_job_claims_the_handle_on_the_host_backend_with_its_files_in_the_work_directory() {
    let runner = RegisteredRunner {
        id: RunnerId::new(4),
        uuid: "u".into(),
        token: "t".into(),
    };
    let labels = ["shoal".to_owned(), "shoal-macos-arm64".to_owned()];
    let job = job();
    let launch = JobLaunch {
        runner: Some(&runner),
        program: Path::new("/bin/forgejo-runner"),
        name: "shoal-host-1",
        work_dir: Path::new("/work dir"),
        workspace: Path::new("/work/ci rust"),
        labels: &labels,
        job: &job,
        env: &[("SHOAL_WORKSPACE_PATH".into(), "/work/ci rust".into())],
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
            "/bin/forgejo-runner",
            "--config",
            "/work dir/config.yaml",
            "one-job",
            "--url",
            "https://forge.example",
            "--uuid",
            "u",
            "--token-url",
            "file:/work dir/token",
            "--handle",
            "h1",
            "--label",
            "shoal:host",
            "--label",
            "shoal-macos-arm64:host",
        ]
    );
    let [(token_file, token), (config_file, config)] = &command.files[..] else {
        panic!("{:?}", command.files);
    };
    assert_eq!(
        (token_file.as_path(), token.as_str()),
        (Path::new("/work dir/token"), "t")
    );
    assert_eq!(config_file, Path::new("/work dir/config.yaml"));
    assert!(
        config.contains("workdir_parent: \"/work dir/host\""),
        "{config}"
    );
    assert!(config.contains("file: \"/work dir/.runner\""), "{config}");
    assert!(
        config.contains("  envs:\n    \"SHOAL_WORKSPACE_PATH\": \"/work/ci rust\"\nhost:"),
        "{config}"
    );
    assert_eq!(
        (command.installation, command.runner),
        (None, None),
        "Forgejo's runner runs from where it is installed, as the repository's"
    );
}
