use super::*;
use crate::{process::identity::Identity, protocol};
use tokio::{io::AsyncWriteExt, net::UnixStream};

#[tokio::test]
async fn adjacent_controls_survive_start_and_recovery_transitions() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = timeout(
        Duration::from_secs(60),
        crate::test_support::isolated_test(
            root.path(),
            "execution::tests::adjacent_controls_child",
        )
        .output(),
    )
    .await
    .expect("isolated execution control test stalled")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[tokio::test]
#[ignore = "isolated wrapper launched by control regression"]
async fn adjacent_controls_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = Paths::for_test(&root);
    let plan = ExecutionPlan {
        id: uuid::Uuid::new_v4().to_string(),
        workspace: Workspace::new_record(
            "repo".into(),
            "worker".into(),
            root.clone(),
            "worker".into(),
            crate::state::WorkspaceState::Ready,
        ),
        scope_token: "test-scope".into(),
        setup_cmd: None,
        ports: vec![],
        environment: Default::default(),
        land: None,
    };
    let (client, mut server) = UnixStream::pair().unwrap();
    let mut bytes = Vec::new();
    for control in [
        Control::Started,
        Control::OverloadStop {
            recover: true,
            reason: "critical memory pressure".into(),
            resumes_when: "system load is healthy".into(),
        },
        Control::Stop {
            reason: Some("stopped by shoal stop".into()),
        },
    ] {
        protocol::write(&mut bytes, &control).await.unwrap();
    }
    // Deliver every control in one write, so they share a socket read.
    server.write_all(&bytes).await.unwrap();
    let (reader, writer) = client.into_split();
    let mut link = Link::new(BufReader::new(reader), writer);
    let daemon = async {
        let mut server = BufReader::new(server);
        assert!(matches!(
            protocol::read_buffered::<ExecutionEvent>(&mut server)
                .await
                .unwrap(),
            ExecutionEvent::Started { .. }
        ));
        assert!(matches!(
            protocol::read_buffered::<ExecutionEvent>(&mut server)
                .await
                .unwrap(),
            ExecutionEvent::Finished { exit_code: 143 }
        ));
        protocol::write(server.get_mut(), &Control::Finished { complete: true })
            .await
            .unwrap();
    };
    let wrapper = async {
        let outcome = supervise(
            &mut link,
            &paths,
            &plan,
            &[
                "/bin/sh".into(),
                "-c".into(),
                "while :; do sleep 1; done".into(),
            ],
            &Mode::Command { record: false },
            None,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::Paused {
                code: 143,
                stop: Halt::Protection { recover: true, reason, .. },
            } if reason == "critical memory pressure"
        ));
        assert!(matches!(
            recovery::wait(&mut link).await.unwrap(),
            recovery::Waited::Cancelled(Some(reason)) if reason == "stopped by shoal stop"
        ));
        assert!(
            report_completion(&mut link, 143, &Mode::Command { record: false })
                .await
                .unwrap()
        );
    };
    timeout(Duration::from_secs(60), async {
        tokio::join!(daemon, wrapper)
    })
    .await
    .expect("execution control exchange stalled");
}

#[tokio::test]
async fn wrappers_reattach_report_late_exits_and_stop_when_refused() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let output = timeout(
        Duration::from_secs(120),
        crate::test_support::isolated_test(root.path(), "execution::tests::reattachment_child")
            .output(),
    )
    .await
    .expect("isolated reattachment test stalled")
    .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// The daemon end of a wrapper's first connection, and the wrapper's link.
fn connected(paths: &Paths, plan: &ExecutionPlan) -> (Link, BufReader<UnixStream>) {
    let (client, server) = UnixStream::pair().unwrap();
    let (reader, writer) = client.into_split();
    let mut link = Link::new(BufReader::new(reader), writer);
    link.allow_reattach(
        paths,
        Reattach {
            execution: plan.id.clone(),
            wrapper: process::identity::capture(std::process::id())
                .unwrap()
                .unwrap(),
            child: None,
            group_id: 0,
            scope_token: plan.scope_token.clone(),
            agent: Some("codex".into()),
            recover: false,
            running_ms: 0,
        },
    );
    (link, BufReader::new(server))
}

/// Acknowledge the wrapper's child, returning its identity and group.
async fn acknowledge_start(daemon: &mut BufReader<UnixStream>) -> (Option<Identity>, u32) {
    let ExecutionEvent::Started { child, group_id } =
        protocol::read_buffered(daemon).await.unwrap()
    else {
        panic!("wrapper did not register its child");
    };
    protocol::write(daemon.get_mut(), &Control::Started)
        .await
        .unwrap();
    (child, group_id)
}

async fn accept_reattach(listener: &tokio::net::UnixListener) -> (Reattach, BufReader<UnixStream>) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut stream = BufReader::new(stream);
    let request: protocol::Request = protocol::read_buffered(&mut stream).await.unwrap();
    let protocol::Method::Reattach(reattach) = request.method else {
        panic!("wrapper did not reattach");
    };
    assert!(request.scope.is_none());
    (reattach, stream)
}

async fn acknowledge_finish(daemon: &mut BufReader<UnixStream>, expected: i32) {
    let event = protocol::read_buffered::<ExecutionEvent>(daemon)
        .await
        .unwrap();
    assert!(
        matches!(event, ExecutionEvent::Finished { exit_code } if exit_code == expected),
        "{event:?}"
    );
    protocol::write(daemon.get_mut(), &Control::Finished { complete: true })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "isolated wrapper launched by reattachment regression"]
async fn reattachment_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    let root = std::env::current_dir().unwrap();
    let paths = Paths::for_test(&root);
    std::fs::create_dir_all(&paths.state).unwrap();
    let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    let plan = ExecutionPlan {
        id: uuid::Uuid::new_v4().to_string(),
        workspace: Workspace::new_record(
            "repo".into(),
            "worker".into(),
            root.clone(),
            "worker".into(),
            crate::state::WorkspaceState::Ready,
        ),
        scope_token: "test-scope".into(),
        setup_cmd: None,
        ports: vec![],
        environment: Default::default(),
        land: None,
    };
    let mode = Mode::Command { record: false };
    let waiting = |marker: &str| -> Vec<OsString> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("while [ ! -f {marker} ]; do sleep 0.05; done; touch {marker}-exited; exit 7")
                .into(),
        ]
    };

    // A planned restart: detach, reattach with the recorded identities, and
    // obey the new daemon's stop.
    let (mut link, mut daemon) = connected(&paths, &plan);
    let restarted = async {
        let (child, group_id) = acknowledge_start(&mut daemon).await;
        protocol::write(daemon.get_mut(), &Control::Detach)
            .await
            .unwrap();
        drop(daemon);
        let (reattach, mut daemon) = accept_reattach(&listener).await;
        assert_eq!(
            (reattach.execution, reattach.child),
            (plan.id.clone(), child)
        );
        assert_eq!(reattach.group_id, group_id);
        assert_eq!(reattach.scope_token, plan.scope_token);
        assert_eq!(reattach.agent.as_deref(), Some("codex"));
        protocol::write(
            daemon.get_mut(),
            &protocol::Response::new(1, protocol::Body::Ok),
        )
        .await
        .unwrap();
        protocol::write(
            daemon.get_mut(),
            &Control::Stop {
                reason: Some("the workspace is being removed".into()),
            },
        )
        .await
        .unwrap();
        acknowledge_finish(&mut daemon, 143).await;
    };
    let wrapper = async {
        let outcome = supervise(
            &mut link,
            &paths,
            &plan,
            &waiting("never"),
            &mode,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::Stopped { code: 143, reason } if reason == "the workspace is being removed"
        ));
        assert!(report_completion(&mut link, 143, &mode).await.unwrap());
    };
    tokio::join!(restarted, wrapper);

    // A crash during which the command exits: its status waits for the next daemon.
    let (mut link, mut daemon) = connected(&paths, &plan);
    let crashed = async {
        acknowledge_start(&mut daemon).await;
        drop(daemon);
        std::fs::write(root.join("release"), "").unwrap();
        while !root.join("release-exited").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (_, mut daemon) = accept_reattach(&listener).await;
        protocol::write(
            daemon.get_mut(),
            &protocol::Response::new(1, protocol::Body::Ok),
        )
        .await
        .unwrap();
        acknowledge_finish(&mut daemon, 7).await;
    };
    let wrapper = async {
        let outcome = supervise(
            &mut link,
            &paths,
            &plan,
            &waiting("release"),
            &mode,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, Outcome::Exited(7)));
        assert!(report_completion(&mut link, 7, &mode).await.unwrap());
    };
    tokio::join!(crashed, wrapper);

    // A daemon that no longer knows the execution stops the command, which
    // then saves its session as a stop does.
    let (mut link, mut daemon) = connected(&paths, &plan);
    let refused = async {
        acknowledge_start(&mut daemon).await;
        drop(daemon);
        let (_, mut daemon) = accept_reattach(&listener).await;
        let body = protocol::Body::error(
            protocol::ErrorCode::ExecutionFailed,
            "execution record is missing",
        );
        protocol::write(daemon.get_mut(), &protocol::Response::new(1, body))
            .await
            .unwrap();
    };
    let wrapper = async {
        let outcome = supervise(
            &mut link,
            &paths,
            &plan,
            &waiting("never"),
            &mode,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            Outcome::Paused {
                code: 143,
                stop: Halt::Pause { reason: Some(reason) },
            } if reason.contains("execution record is missing")
        ));
        let refusal = link.refusal().unwrap().to_string();
        assert!(refusal.contains("execution record is missing"), "{refusal}");
    };
    tokio::join!(refused, wrapper);
}

#[test]
fn stopped_lines_name_the_cause_and_only_protection_saves_it() {
    let restore = "restore with shoal resume worker --execution id";
    let protection = Halt::Protection {
        recover: false,
        reason: "free disk space is below 2 GiB".into(),
        resumes_when: "free disk space reaches 5 GiB".into(),
    };
    assert_eq!(
        protection.saved_reason(),
        Some("free disk space is below 2 GiB")
    );
    assert_eq!(
        protection.manual_restore(restore),
        "shoal: agent stopped: free disk space is below 2 GiB; once free disk space reaches 5 GiB, restore with shoal resume worker --execution id"
    );
    let stop = Halt::Pause {
        reason: Some("stopped by shoal stop".into()),
    };
    assert_eq!(stop.saved_reason(), None);
    assert_eq!(stop.stated_reason(), ": stopped by shoal stop");
    assert_eq!(
        stop.manual_restore(restore),
        "shoal: agent stopped: stopped by shoal stop; restore with shoal resume worker --execution id"
    );
    assert_eq!(Halt::Pause { reason: None }.stated_reason(), "");
}
