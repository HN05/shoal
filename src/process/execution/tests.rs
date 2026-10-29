use super::*;

#[tokio::test]
async fn unreadable_evidence_blocks_only_while_the_same_process_is_alive() {
    // Reading it again shows the marker where macOS does not hide it.
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .env_clear()
        .env(crate::env::EXECUTION_ID, "test-execution")
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
        origin: None,
        agent: None,
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

/// Like a detached helper that execs its own program after the scan.
#[tokio::test]
async fn unreadable_evidence_settles_once_it_shows_no_marker() -> anyhow::Result<()> {
    use anyhow::Context;
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1")
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
        origin: None,
        agent: None,
    };
    let scan = process::Scan {
        unreadable: vec![process::Unattributed::alone(identity)],
        ..Default::default()
    };
    let settled = Processes::inspect(&execution, &scan).await;
    child.kill().await?;
    child.wait().await?;
    assert!(settled?.unreadable.is_empty());
    Ok(())
}

#[tokio::test]
async fn unreadable_evidence_blocks_after_its_head_exits_unless_the_head_is_older()
-> anyhow::Result<()> {
    use anyhow::Context;
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .env(crate::env::EXECUTION_ID, "test-execution")
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
        origin: None,
        agent: None,
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

#[tokio::test]
async fn only_marked_processes_outside_the_group_run_outside_it() {
    let mut background = tokio::process::Command::new("sleep")
        .arg("60")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = process::capture(background.id().unwrap()).unwrap().unwrap();
    let mut execution = Execution {
        id: "agent".into(),
        workspace_id: "workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: None,
        child: None,
        group_id: Some(std::process::id()),
        origin: None,
        agent: None,
    };
    let marked = |execution_id: &str| process::Scan {
        processes: vec![process::OwnedProcess {
            execution_id: execution_id.into(),
            identity: identity.clone(),
        }],
        ..Default::default()
    };
    assert!(Processes::runs_outside_group(&execution, &marked("agent")));
    assert!(!Processes::runs_outside_group(&execution, &marked("other")));
    execution.group_id = Some(identity.pid);
    assert!(!Processes::runs_outside_group(&execution, &marked("agent")));

    execution.group_id = Some(std::process::id());
    background.kill().await.unwrap();
    background.wait().await.unwrap();
    assert!(!Processes::runs_outside_group(&execution, &marked("agent")));

    // An agent hook waiting for the next update does no work.
    let mut hook = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env(crate::env::WAITING_HOOK, "1")
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let hook_identity = process::capture(hook.id().unwrap()).unwrap().unwrap();
    let waiting = process::Scan {
        processes: vec![process::OwnedProcess {
            execution_id: "agent".into(),
            identity: hook_identity,
        }],
        ..Default::default()
    };
    assert!(!Processes::runs_outside_group(&execution, &waiting));
    hook.kill().await.unwrap();
    hook.wait().await.unwrap();
}
