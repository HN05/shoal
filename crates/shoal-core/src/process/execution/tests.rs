use super::*;

#[tokio::test]
async fn unreadable_evidence_blocks_only_while_the_same_process_is_alive() {
    let workspace = tempfile::tempdir().unwrap();
    // Reading it again shows the marker where macOS does not hide it.
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .current_dir(workspace.path())
        .env_clear()
        .env(crate::env::EXECUTION_ID, "test-execution")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = process::capture(child.id().unwrap()).unwrap().unwrap();
    let execution = Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: "test-execution".into(),
        space_id: "test-workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: Some(identity.clone()),
        child: None,
        group_id: Some(identity.pid),
        origin: None,
        agent: None,
        stopped: None,
    };
    let scan = process::Scan {
        unreadable: vec![process::Unattributed::alone(identity)],
        ..Default::default()
    };
    let live = Processes::inspect(&execution, workspace.path(), &scan, &[])
        .await
        .unwrap();
    assert_eq!(live.unreadable.len(), 1);
    assert!(!live.visibility_complete());
    let issue = live.completion_issue().unwrap();
    let pid = child.id().unwrap();
    assert!(issue.contains(&format!("kill {pid}")), "{issue}");
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    let exited = Processes::inspect(&execution, workspace.path(), &scan, &[])
        .await
        .unwrap();
    assert!(exited.unreadable.is_empty());
    assert!(exited.visibility_complete());
    assert!(exited.completion_issue().is_none());
}

/// Like a loop launchd adopted from a removed workspace, which no
/// execution of another workspace can have left.
#[tokio::test]
async fn unreadable_evidence_outside_the_workspace_does_not_block() -> anyhow::Result<()> {
    use anyhow::Context;
    let (workspace, elsewhere) = (tempfile::tempdir()?, tempfile::tempdir()?);
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .current_dir(elsewhere.path())
        .env(crate::env::EXECUTION_ID, "test-execution")
        .kill_on_drop(true)
        .spawn()?;
    let identity = process::capture(child.id().context("child PID")?)?.context("child")?;
    let execution = Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: "test-execution".into(),
        space_id: "test-workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: Some(identity.clone()),
        child: None,
        group_id: None,
        origin: None,
        agent: None,
        stopped: None,
    };
    let scan = process::Scan {
        unreadable: vec![process::Unattributed::alone(identity)],
        ..Default::default()
    };
    let inspected = Processes::inspect(&execution, workspace.path(), &scan, &[]).await;
    child.kill().await?;
    child.wait().await?;
    let inspected = inspected?;
    assert!(inspected.unreadable.is_empty());
    assert!(inspected.completion_issue().is_none());
    Ok(())
}

/// Like a detached helper that execs its own program after the scan.
#[tokio::test]
async fn unreadable_evidence_settles_once_it_shows_no_marker() -> anyhow::Result<()> {
    use anyhow::Context;
    let workspace = tempfile::tempdir()?;
    let mut child = crate::test_support::spawn_marked_child(
        crate::test_support::marked_child()
            .current_dir(workspace.path())
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1"),
    );
    let identity = process::capture(child.id().context("child PID")?)?.context("child")?;
    let execution = Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: "test-execution".into(),
        space_id: "test-workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: Some(identity.clone()),
        child: None,
        group_id: None,
        origin: None,
        agent: None,
        stopped: None,
    };
    let scan = process::Scan {
        unreadable: vec![process::Unattributed::alone(identity)],
        ..Default::default()
    };
    let settled = Processes::inspect(&execution, workspace.path(), &scan, &[]).await;
    child.kill().await?;
    child.wait().await?;
    assert!(settled?.unreadable.is_empty());
    Ok(())
}

#[tokio::test]
async fn unreadable_evidence_blocks_after_its_head_exits_unless_the_head_is_older()
-> anyhow::Result<()> {
    use anyhow::Context;
    let workspace = tempfile::tempdir()?;
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .current_dir(workspace.path())
        .env(crate::env::EXECUTION_ID, "test-execution")
        .kill_on_drop(true)
        .spawn()?;
    let identity = process::capture(child.id().context("child PID")?)?.context("child")?;
    let execution = Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: "test-execution".into(),
        space_id: "test-workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: Some(identity.clone()),
        child: None,
        group_id: None,
        origin: None,
        agent: None,
        stopped: None,
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
    let (exited_head, older_head) = (
        scan(head(identity.birth.clone())),
        scan(head(format!("{prefix}:0"))),
    );
    let exited_head = Processes::inspect(&execution, workspace.path(), &exited_head, &[]).await;
    let older_head = Processes::inspect(&execution, workspace.path(), &older_head, &[]).await;
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
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: "agent".into(),
        space_id: "workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper: None,
        child: None,
        group_id: Some(std::process::id()),
        origin: None,
        agent: None,
        stopped: None,
    };
    let marked = |execution: &str| process::Scan {
        processes: vec![process::OwnedProcess {
            owner: execution.into(),
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
    let mut hook = crate::test_support::spawn_marked_child(
        crate::test_support::marked_child()
            .env(crate::env::WAITING_HOOK, "1")
            .process_group(0),
    );
    let hook_identity = process::capture(hook.id().unwrap()).unwrap().unwrap();
    let waiting = process::Scan {
        processes: vec![process::OwnedProcess {
            owner: "agent".into(),
            identity: hook_identity,
        }],
        ..Default::default()
    };
    assert!(!Processes::runs_outside_group(&execution, &waiting));
    hook.kill().await.unwrap();
    hook.wait().await.unwrap();
}

/// The agent wrapper a test starts in a process group of its own, as an
/// agent's, and the process it runs in the background.
async fn agent_wrapper(script: &str) -> anyhow::Result<(tokio::process::Child, Vec<u32>)> {
    use anyhow::Context;
    use tokio::io::AsyncBufReadExt;
    let mut wrapper = tokio::process::Command::new("sh")
        .args(["-c", script])
        .stdout(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = tokio::io::BufReader::new(wrapper.stdout.take().context("stdout")?);
    let mut pids = Vec::new();
    for _ in 0..script.matches("echo").count() {
        let mut line = String::new();
        stdout.read_line(&mut line).await?;
        pids.push(line.trim().parse()?);
    }
    Ok((wrapper, pids))
}

/// An execution of `id` with `wrapper`, outside whose process group its
/// tests' processes run.
fn agent_execution(id: &str, wrapper: Option<process::Identity>) -> Execution {
    Execution {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        id: id.into(),
        space_id: "workspace".into(),
        state: crate::state::ExecutionState::Running,
        wrapper,
        child: None,
        group_id: Some(std::process::id()),
        origin: None,
        agent: None,
        stopped: None,
    }
}

/// A scan that finds `process` carrying the marker of `owner`.
fn marked_by(owner: &str, process: &process::Identity) -> process::Scan {
    process::Scan {
        processes: vec![process::OwnedProcess {
            owner: owner.into(),
            identity: process.clone(),
        }],
        ..Default::default()
    }
}

/// A process that hides its environment under another execution's agent
/// wrapper, as `sh` does on macOS under a scoped caller's `start --here`,
/// takes the marker the wrapper inherited from its starter, whose work it is
/// not.
#[tokio::test]
async fn a_foreign_agent_wrappers_processes_are_not_the_starters() -> anyhow::Result<()> {
    use anyhow::Context;
    let (mut wrapper, pids) = agent_wrapper("sleep 60 & echo $!; wait").await?;
    let wrapper_identity =
        process::capture(wrapper.id().context("wrapper PID")?)?.context("wrapper")?;
    let agent = process::capture(pids[0])?.context("agent")?;
    let execution = agent_execution("starter", None);
    let scan = marked_by("starter", &agent);
    let workspace = tempfile::tempdir()?;
    let wrappers = std::slice::from_ref(&wrapper_identity);
    let alone = Processes::inspect(&execution, workspace.path(), &scan, &[]).await;
    let beside = Processes::inspect(&execution, workspace.path(), &scan, wrappers).await;
    process::signal(&agent, libc::SIGKILL)?;
    wrapper.wait().await?;
    assert_eq!(alone?.owned, [agent]);
    assert!(beside?.owned.is_empty());
    Ok(())
}

/// An agent a scoped caller runs in its own terminal runs below the caller's
/// agent wrapper too; what it runs stays its own.
#[tokio::test]
async fn an_agent_below_another_agents_wrapper_keeps_its_processes() -> anyhow::Result<()> {
    use anyhow::Context;
    let script = "sh -c 'echo $$; sleep 60 & echo $!; wait'; :";
    let (mut outer, pids) = agent_wrapper(script).await?;
    let outer_identity =
        process::capture(outer.id().context("outer PID")?)?.context("outer wrapper")?;
    let inner = process::capture(pids[0])?.context("inner wrapper")?;
    let job = process::capture(pids[1])?.context("job")?;
    let wrappers = [outer_identity, inner.clone()];
    let execution = agent_execution("child", Some(inner));
    let scan = marked_by("child", &job);
    let workspace = tempfile::tempdir()?;
    let inspected = Processes::inspect(&execution, workspace.path(), &scan, &wrappers).await;
    process::signal(&job, libc::SIGKILL)?;
    outer.wait().await?;
    assert!(inspected?.owned.contains(&job));
    Ok(())
}
