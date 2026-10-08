use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
        land: None,
    };
    let (client, mut server) = UnixStream::pair().unwrap();
    let mut bytes = Vec::new();
    for control in [
        Control::Started,
        Control::OverloadStop {
            recover: true,
            reason: "critical memory pressure".into(),
        },
        Control::Stop,
    ] {
        protocol::write(&mut bytes, &control).await.unwrap();
    }
    server.write_all(&bytes).await.unwrap();
    let mut stream = BufReader::new(client);
    // Force read-ahead before supervision, independent of socket scheduling.
    assert_eq!(stream.fill_buf().await.unwrap(), bytes);
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
            &mut stream,
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
                recover: true,
                reason: Some(reason),
            } if reason == "critical memory pressure"
        ));
        assert!(recovery::wait(&mut stream).await.unwrap().is_none());
        assert!(
            report_completion(&mut stream, 143, &Mode::Command { record: false })
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
