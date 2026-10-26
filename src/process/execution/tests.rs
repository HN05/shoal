use super::*;

#[tokio::test]
async fn unreadable_evidence_blocks_only_while_the_same_process_is_alive() {
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = process::capture(child.id().unwrap()).unwrap().unwrap();
    let execution = Execution {
        id: "test-execution".into(),
        workspace_id: "test-workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: Some(identity.clone()),
        child: None,
        group_id: Some(identity.pid),
    };
    let scan = process::Scan {
        unreadable: vec![process::Unattributed::alone(identity)],
        ..Default::default()
    };
    let live = Processes::inspect(&execution, &scan).await.unwrap();
    assert_eq!(live.unreadable.len(), 1);
    assert!(!live.visibility_complete());
    assert!(live.completion_issue().unwrap().contains(&format!(
        "unreadable live process environments (PID {})",
        child.id().unwrap()
    )));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let exited = Processes::inspect(&execution, &scan).await.unwrap();
    assert!(exited.unreadable.is_empty());
    assert!(exited.visibility_complete());
    assert!(exited.completion_issue().is_none());
}

#[tokio::test]
async fn unreadable_evidence_blocks_after_its_head_exits_unless_the_head_is_older()
-> anyhow::Result<()> {
    use anyhow::Context;
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()?;
    let identity = process::capture(child.id().context("child PID")?)?.context("child")?;
    let execution = Execution {
        id: "test-execution".into(),
        workspace_id: "test-workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: Some(identity.clone()),
        child: None,
        group_id: None,
    };
    let (prefix, _) = identity.birth.rsplit_once(':').context("birth")?;
    let head = |birth: String| process::Identity {
        pid: u32::MAX >> 1,
        birth,
        unique: None,
    };
    let scan = |head| process::Scan {
        unreadable: vec![process::Unattributed {
            identity: identity.clone(),
            head,
        }],
        ..Default::default()
    };
    let exited_head = Processes::inspect(&execution, &scan(head(identity.birth.clone()))).await;
    let older_head = Processes::inspect(&execution, &scan(head(format!("{prefix}:0")))).await;
    child.kill().await?;
    child.wait().await?;
    assert_eq!(exited_head?.unreadable, std::slice::from_ref(&identity));
    assert!(older_head?.unreadable.is_empty());
    Ok(())
}
