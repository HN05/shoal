use super::*;

#[test]
fn process_identity_unavailable_includes_inaccessible_processes() {
    assert!(process_identity_unavailable(Some(libc::ESRCH)));
    assert!(process_identity_unavailable(Some(libc::ENOENT)));
    assert!(process_identity_unavailable(Some(libc::EPERM)));
    assert!(!process_identity_unavailable(Some(libc::EACCES)));
    assert!(!process_identity_unavailable(None));
}

#[test]
fn lineage_follows_unique_ids_through_exited_parents_only_to_the_root() {
    let process = |pid, unique, parent| Lineage {
        identity: Identity {
            pid,
            birth: pid.to_string(),
            unique: None,
        },
        unique: Some(unique),
        parent: Some(parent),
        parent_pid: 1,
        adopted: false,
    };
    // 10 has exited; 11 descends from it and 13 from 11, while 12 is
    // another branch whose parent 9 is no descendant of 10.
    let found = lineage_descendants(
        10,
        vec![process(13, 13, 11), process(12, 12, 9), process(11, 11, 10)],
    );
    let pids: Vec<_> = found.iter().map(|identity| identity.pid).collect();
    assert_eq!(pids, [11, 13]);
}

#[test]
fn a_unique_id_recorded_in_another_boot_roots_no_lineage() -> Result<()> {
    let this = capture(std::process::id())?.context("identify this process")?;
    assert_eq!(
        this.lineage_root(),
        this.unique.as_ref().map(|unique| unique.id)
    );
    let earlier = Identity {
        unique: Some(UniqueId {
            boot: "an earlier boot".into(),
            id: 1,
        }),
        ..this
    };
    assert_eq!(earlier.lineage_root(), None);
    Ok(())
}

/// A group member whose parent exited, with a child that left the group,
/// while the leader lives.
#[tokio::test]
async fn children_of_an_orphaned_group_member_are_owned_with_a_live_leader() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let script =
        r#"( sh -c 'perl -e "setpgrp(0,0); \$| = 1; print qq(\$\$\n); sleep 30"; :' & ); read _"#;
    let mut leader = tokio::process::Command::new("sh")
        .args(["-c", script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let identity = capture(leader.id().context("leader PID")?)?.context("leader")?;
    let mut stdout = tokio::io::BufReader::new(leader.stdout.take().context("stdout")?);
    let mut line = String::new();
    stdout.read_line(&mut line).await?;
    let child = capture(line.trim().parse()?)?.context("child outside the group")?;
    let member = parent(child.pid).context("orphaned group member")?;
    // The subshell that started the member exits on its own.
    while parent(member).and_then(parent) == Some(identity.pid) {
        tokio::task::yield_now().await;
    }
    let (owned, _) = related(Some(&identity), Some(identity.pid)).await?;
    signal(&child, libc::SIGKILL)?;
    leader.stdin.take().context("stdin")?.shutdown().await?;
    assert!(owned.contains(&child));
    Ok(())
}

#[test]
fn identity_equality_ignores_a_missing_unique_id() {
    let recorded = Identity {
        pid: 7,
        birth: "1:2".into(),
        unique: None,
    };
    let captured = Identity {
        unique: Some(UniqueId {
            boot: "boot".into(),
            id: 42,
        }),
        ..recorded.clone()
    };
    assert_eq!(recorded, captured);
    let stored: Identity = serde_json::from_str(r#"{"pid":7,"birth":"1:2"}"#).unwrap();
    assert_eq!(stored.unique, None);
}

/// A command that started a background job and exited: launchd adopts
/// the job, whose recorded lineage still leads to the command.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_exited_commands_background_job_is_owned_through_its_lineage() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut command = tokio::process::Command::new("sh")
        .args(["-c", "sleep 30 & echo $!; read _"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let identity = capture(command.id().context("command PID")?)?.context("command")?;
    let mut stdout = tokio::io::BufReader::new(command.stdout.take().context("stdout")?);
    let mut line = String::new();
    stdout.read_line(&mut line).await?;
    let job = capture(line.trim().parse()?)?.context("background job")?;
    // Replacing its image after launchd adopts it would end its lineage.
    while executable(job.pid).as_deref() != Some(std::path::Path::new("/bin/sleep")) {
        tokio::task::yield_now().await;
    }
    command.stdin.take().context("stdin")?.shutdown().await?;
    command.wait().await?;
    let (owned, candidates) = related(Some(&identity), Some(identity.pid)).await?;
    signal(&job, libc::SIGKILL)?;
    assert!(owned.contains(&job));
    assert!(!candidates.contains(&job));
    Ok(())
}

/// A process born in PID order, as the parents below precede children.
fn observed(pid: u32, parent: u32, adopted: bool, evidence: Evidence) -> (u32, Observed) {
    let identity = Identity {
        pid,
        birth: format!("1:{pid}"),
        unique: None,
    };
    let lineage = Lineage {
        identity,
        unique: None,
        parent: None,
        parent_pid: parent,
        adopted,
    };
    (pid, Observed { lineage, evidence })
}

#[test]
fn a_hidden_environment_takes_the_attribution_of_the_nearest_ancestor_that_tells() -> Result<()> {
    use Evidence::{Hidden, Marked, Unmarked};
    // A live parent the inventory missed, which `cat` keeps until its stdin closes.
    let mut unlisted = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    let inventory = Inventory {
        observed: HashMap::from([
            // A shell and its job under a marked agent.
            observed(10, 9, false, Marked("execution".into())),
            observed(11, 10, false, Hidden),
            observed(12, 11, false, Hidden),
            // A shell under an unmarked terminal.
            observed(20, 1, false, Unmarked),
            observed(21, 20, false, Hidden),
            // A job launchd adopted after its shell exited, and its child.
            observed(31, 1, true, Hidden),
            observed(32, 31, false, Hidden),
            // A job launchd started.
            observed(30, 1, false, Hidden),
            // A shell whose listed parent exited before it was observed.
            // Above every system's highest PID, so no process has it.
            observed(41, 4_194_305, false, Hidden),
            // A shell whose parent exited and left its PID to a later
            // marked process.
            observed(51, 60, false, Hidden),
            observed(60, 9, false, Marked("execution".into())),
            // A shell whose live parent is missing from the inventory.
            observed(u32::MAX >> 1, unlisted.id(), false, Hidden),
        ]),
    };
    let mut attributions = HashMap::new();
    let mut resolve = |pid| match inventory.resolve(pid, &mut attributions) {
        Attribution::Owned(id) => format!("owned by {id}"),
        Attribution::Foreign => "foreign".into(),
        Attribution::Unknown(head) => format!("unknown under {}", head.pid),
    };
    assert_eq!(resolve(12), "owned by execution");
    assert_eq!(resolve(11), "owned by execution");
    assert_eq!(resolve(21), "foreign");
    assert_eq!(resolve(32), "unknown under 31");
    assert_eq!(resolve(30), "foreign");
    assert_eq!(resolve(41), "unknown under 41");
    assert_eq!(resolve(51), "unknown under 51");
    assert_eq!(resolve(60), "owned by execution");
    let missed = resolve(u32::MAX >> 1);
    drop(unlisted.stdin.take());
    unlisted.wait()?;
    assert_eq!(missed, format!("unknown under {}", u32::MAX >> 1));
    Ok(())
}

/// A shell under a setuid program the user started, such as a terminal's
/// `login` or an agent's `sudo -u`, which runs as root yet accepts the user's
/// signals, takes the attribution of the setuid program's parent.
#[cfg(target_os = "macos")]
#[test]
fn a_hidden_environment_takes_its_attribution_through_a_setuid_parent() -> Result<()> {
    use Evidence::{Hidden, Marked, Unmarked};
    // `top` runs setuid root like `login`; it samples until killed.
    let mut setuid = std::process::Command::new("/usr/bin/top")
        .args(["-l", "0", "-s", "60"])
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let elevated = observe_elevated(setuid.id());
    setuid.kill()?;
    setuid.wait()?;
    let elevated = elevated?.context("observe the setuid program")?;
    ensure!(elevated.lineage.parent_pid == std::process::id());
    let shell = || {
        let (pid, mut shell) =
            observed(u32::MAX >> 1, elevated.lineage.identity.pid, false, Hidden);
        // Started after the setuid program, as its child.
        shell.lineage.identity.birth = format!("{}:0", u32::MAX);
        (pid, shell)
    };
    let starter = |evidence| Inventory {
        observed: HashMap::from([
            shell(),
            (
                elevated.lineage.identity.pid,
                Observed {
                    lineage: elevated.lineage.clone(),
                    evidence: Evidence::Elevated,
                },
            ),
            observed(std::process::id(), 1, false, evidence),
        ]),
    };
    let resolve = |inventory: Inventory| match inventory.resolve(u32::MAX >> 1, &mut HashMap::new())
    {
        Attribution::Owned(id) => format!("owned by {id}"),
        Attribution::Foreign => "foreign".into(),
        Attribution::Unknown(head) => format!("unknown under {}", head.pid),
    };
    assert_eq!(resolve(starter(Unmarked)), "foreign");
    assert_eq!(
        resolve(starter(Marked("execution".into()))),
        "owned by execution"
    );
    // The setuid program is never reported itself.
    let scan = starter(Marked("execution".into())).attribute()?;
    assert!(scan.unreadable.is_empty());
    assert!(
        scan.processes
            .iter()
            .all(|owned| owned.identity.pid != elevated.lineage.identity.pid)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn a_start_time_matches_the_birth_of_the_identity() -> Result<()> {
    let own = capture(std::process::id())?.context("own identity")?;
    assert_eq!(start_time(own.pid)?, Some(own.birth));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn launchd_starts_a_process_only_by_a_read_and_matching_version() {
    let process = |parent_unique, original_parent_version| UniqueIdentifiers {
        uuid: [0; 16],
        unique: 2,
        parent_unique,
        id_version: 0,
        original_parent_version,
        reserved: [0; 2],
    };
    assert!(started_by_launchd(&process(1, 7), Some(7)));
    assert!(!started_by_launchd(&process(1, 7), None));
    assert!(!started_by_launchd(&process(1, 8), Some(7)));
    assert!(!started_by_launchd(&process(5, 7), Some(7)));
}

#[cfg(target_os = "macos")]
fn executable(pid: u32) -> Option<PathBuf> {
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    path.truncate(usize::try_from(len).ok()?);
    Some(PathBuf::from(OsStr::from_bytes(&path)))
}

/// A shell's background job that launchd adopted, on a macOS that hides
/// a platform binary's environment.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_adopted_job_with_a_hidden_environment_is_unreadable() -> Result<()> {
    let output = tokio::process::Command::new("sh")
        .args(["-c", "sleep 30 </dev/null >/dev/null 2>&1 & echo $!"])
        .output()
        .await?;
    let pid = std::str::from_utf8(&output.stdout)?.trim().parse()?;
    let job = capture(pid)?.context("background job")?;
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    let hidden = matches!(
        visibility(&job, deadline)?,
        Visibility::Environment(environment) if environment.is_empty()
    ) && hides_environment(pid);
    let scan = scan(HashSet::from(["unrelated-execution".into()])).await;
    signal(&job, libc::SIGKILL)?;
    let unreadable = scan?.unreadable.iter().any(|u| u.identity == job);
    assert_eq!(unreadable, hidden);
    Ok(())
}

#[test]
fn exited_children_stay_unreaped_until_waited_for() -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .args(["-c", "read line"])
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    let observed = observe_unreaped_child(&mut child);
    // Close and reap even if an observation failed, without reaping during polling.
    drop(child.stdin.take());
    child.wait()?;
    let identity = observed?;
    assert!(!unreaped(&identity)?);
    Ok(())
}

fn observe_unreaped_child(child: &mut std::process::Child) -> Result<Identity> {
    let identity = capture(child.id())?.context("identify live child")?;
    ensure!(!unreaped(&identity)?, "live child reported as unreaped");
    // Closing its input ends the shell; nothing waits for it yet.
    drop(child.stdin.take());
    // A hang guard well past any exit.
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !unreaped(&identity)? {
        ensure!(std::time::Instant::now() < deadline, "child never exited");
        std::thread::sleep(Duration::from_millis(10));
    }
    ensure!(!alive(&identity)?, "exited child reported as alive");
    let changed = Identity {
        birth: "different-process".into(),
        ..identity.clone()
    };
    ensure!(
        !unreaped(&changed)?,
        "mismatched identity reported as unreaped"
    );
    // Workspace usage reads the time an exited child awaits its parent with.
    #[cfg(target_os = "linux")]
    ensure!(super::super::usage::read(identity.pid)?.is_some());
    Ok(identity)
}

#[test]
#[ignore = "helper child launched by ownership test"]
fn marked_child() {
    std::thread::sleep(Duration::from_secs(30));
}
#[cfg(target_os = "linux")]
#[tokio::test]
async fn stat_read_after_process_exit_is_not_an_inventory_failure() {
    use std::os::fd::AsRawFd;

    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let stat = std::fs::File::open(format!("/proc/{pid}/stat")).unwrap();
    assert!(capture(pid).unwrap().is_some());
    child.kill().await.unwrap();

    // Keep the proc inode open across exit to deterministically exercise
    // the race between opening stat and reading it during an inventory.
    let path = format!("/proc/self/fd/{}", stat.as_raw_fd());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_err().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(read_process_stat(&path).unwrap().is_none());
    assert!(capture(pid).unwrap().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn empty_environment_waits_only_for_a_loading_image() {
    let stat = |flags: u64, env_end: u64| {
        let mut fields = vec![0; 50];
        fields[6] = flags;
        fields[48] = env_end;
        let fields: Vec<_> = fields.iter().map(u64::to_string).collect();
        format!("7 (odd) name) S {}", fields[1..].join(" "))
    };
    assert!(matches!(stat_image(&stat(0, 0)).unwrap(), Image::Loading));
    assert!(matches!(
        stat_image(&stat(0, 4096)).unwrap(),
        Image::Settled
    ));
    assert!(matches!(stat_image(&stat(0x4, 0)).unwrap(), Image::Ended));
    assert!(matches!(
        stat_image(&stat(0x20_0000, 0)).unwrap(),
        Image::Ended
    ));
}

#[tokio::test]
async fn spawned_process_environment_is_visible_until_it_exits() {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env_clear()
        .env("SHOAL_TEST_PROCESS", "1")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = capture(child.id().unwrap()).unwrap().unwrap();
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    // Spawning returns once exec is committed, possibly before the new
    // environment is published.
    let Visibility::Environment(environment) = visibility(&identity, deadline).unwrap() else {
        panic!("spawned process environment is not visible");
    };
    assert!(
        environment
            .iter()
            .any(|entry| entry == b"SHOAL_TEST_PROCESS=1")
    );
    child.kill().await.unwrap();
    assert!(matches!(
        visibility(&identity, deadline).unwrap(),
        Visibility::Gone
    ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn refusal_after_an_empty_read_waits_for_the_published_environment() {
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = capture(child.id().unwrap()).unwrap().unwrap();
    // The order macOS reports while a spawned process execs.
    let mut reads = [
        Ok(vec![]),
        Err(anyhow::anyhow!("cannot inspect process environment")),
        Ok(vec![b"SHOAL_TEST_PROCESS=1".to_vec()]),
    ]
    .into_iter();
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    let visibility = visibility_with(&identity, deadline, |_| reads.next().unwrap()).unwrap();
    let Visibility::Environment(environment) = visibility else {
        panic!("published environment is not visible");
    };
    assert_eq!(environment, [b"SHOAL_TEST_PROCESS=1".to_vec()]);
    child.kill().await.unwrap();
}

#[tokio::test]
async fn settled_empty_environment_is_readable_and_has_no_execution_marker() {
    let mut child = tokio::process::Command::new("cat")
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = capture(child.id().unwrap()).unwrap().unwrap();
    let deadline = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
    let Visibility::Environment(environment) = visibility(&identity, deadline).unwrap() else {
        panic!("settled empty environment must be readable");
    };
    assert!(environment.is_empty());
    let scan = scan(HashSet::from(["unrelated-execution".into()]))
        .await
        .unwrap();
    assert!(scan.unreadable.iter().all(|u| u.identity != identity));
    assert!(scan.processes.iter().all(|p| p.identity != identity));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}

#[tokio::test]
async fn discovers_marked_processes_and_rejects_changed_identity() {
    let id = uuid::Uuid::new_v4().to_string();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env(crate::env::EXECUTION_ID, &id)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let identity = capture(child.id().unwrap()).unwrap().unwrap();
    let inventory = scan(HashSet::from([id.clone()])).await.unwrap();
    assert!(
        inventory
            .processes
            .iter()
            .any(|p| p.identity == identity && p.execution_id == id)
    );

    let changed = Identity {
        birth: "different-process".into(),
        ..identity.clone()
    };
    signal(&changed, libc::SIGKILL).unwrap();
    assert!(alive(&identity).unwrap());
    signal(&identity, libc::SIGKILL).unwrap();
    child.wait().await.unwrap();
    assert!(!alive(&identity).unwrap());
}
