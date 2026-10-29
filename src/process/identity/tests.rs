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
        |_| Ok(false),
    )
    .unwrap();
    let pids: Vec<_> = found.iter().map(|identity| identity.pid).collect();
    assert_eq!(pids, [11, 13]);
    // 11 left the execution, as a demo does, so 13 below it is not found.
    let found = lineage_descendants(
        10,
        vec![
            process(13, 13, 11),
            process(14, 14, 10),
            process(11, 11, 10),
        ],
        |identity| Ok(identity.pid == 11),
    )
    .unwrap();
    let pids: Vec<_> = found.iter().map(|identity| identity.pid).collect();
    assert_eq!(pids, [14]);
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

#[test]
fn only_a_process_recorded_in_another_boot_is_of_an_earlier_one() -> Result<()> {
    let this = capture(std::process::id())?.context("identify this process")?;
    assert!(!this.of_earlier_boot());
    assert!(
        this.clone()
            .recorded_in("an-earlier-boot")
            .of_earlier_boot()
    );
    let unnamed = Identity {
        pid: this.pid,
        birth: "1".into(),
        unique: None,
    };
    assert!(!unnamed.of_earlier_boot());
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
    let (owned, _) = related(Some(&identity), Some(identity.pid), Some("execution")).await?;
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
        // The job inherits the marker, as a tracked command's jobs do.
        .env(crate::env::EXECUTION_ID, "execution")
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
    let (owned, candidates) =
        related(Some(&identity), Some(identity.pid), Some("execution")).await?;
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

/// `observed` with the unique IDs of the process and its parent.
fn unique((pid, mut process): (u32, Observed), unique: u64, parent: u64) -> (u32, Observed) {
    process.lineage.unique = Some(unique);
    process.lineage.parent = Some(parent);
    (pid, process)
}

fn marked(execution: &str) -> Evidence {
    Evidence::Marked(Marks {
        execution: Some(execution.into()),
        workspace: None,
    })
}

fn inventory<const N: usize>(
    owners: &owners::Owners,
    observed: [(u32, Observed); N],
) -> Inventory<'_> {
    Inventory {
        observed: HashMap::from(observed),
        ..Inventory::new(owners, std::time::Instant::now())
    }
}

fn describe(attribution: Attribution) -> String {
    match attribution {
        Attribution::Owned(marks) => format!("owned by {}", marks.execution.unwrap_or_default()),
        Attribution::Foreign => "foreign".into(),
        Attribution::Unknown(head) => format!("unknown under {}", head.pid),
    }
}

#[test]
fn a_hidden_environment_takes_the_attribution_of_the_nearest_ancestor_that_tells() -> Result<()> {
    use Evidence::{Hidden, Unmarked};
    // A live parent the inventory missed, which `cat` keeps until its stdin closes.
    let mut unlisted = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    let owners = owners::Owners::default();
    let inventory = inventory(
        &owners,
        [
            // A shell and its job under a marked agent.
            observed(10, 9, false, marked("execution")),
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
            observed(60, 9, false, marked("execution")),
            // A shell whose live parent is missing from the inventory.
            observed(u32::MAX >> 1, unlisted.id(), false, Hidden),
        ],
    );
    let mut attributions = HashMap::new();
    let mut resolve = |pid| describe(inventory.resolve(pid, &mut attributions));
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

#[test]
fn an_orphan_takes_the_owner_remembered_for_it_or_its_exited_parent() -> Result<()> {
    use Evidence::Hidden;
    let owners = owners::Owners::default();
    let ids = HashSet::from(["execution".to_owned()]);
    // An agent's shell and its job, while the agent lives.
    inventory(
        &owners,
        [
            unique(observed(10, 9, false, marked("execution")), 100, 90),
            unique(observed(11, 10, false, Hidden), 101, 100),
            unique(observed(12, 11, false, Hidden), 102, 101),
        ],
    )
    .attribute(Marker::Execution, &ids)?;
    // The agent and its shell exited and launchd adopted what the shell
    // started: the job, another job no inventory saw, and an unrelated one.
    let later = inventory(
        &owners,
        [
            unique(observed(12, 1, true, Hidden), 102, 101),
            unique(observed(13, 1, true, Hidden), 103, 101),
            unique(observed(14, 1, true, Hidden), 104, 999),
        ],
    );
    let mut attributions = HashMap::new();
    let mut resolve = |pid| describe(later.resolve(pid, &mut attributions));
    assert_eq!(resolve(12), "owned by execution");
    assert_eq!(resolve(13), "owned by execution");
    assert_eq!(resolve(14), "unknown under 14");
    Ok(())
}

#[test]
fn owners_are_forgotten_once_no_live_process_names_them() -> Result<()> {
    use Evidence::Hidden;
    let owners = owners::Owners::default();
    let ids = HashSet::from(["execution".to_owned()]);
    inventory(
        &owners,
        [
            unique(observed(10, 9, false, marked("execution")), 100, 90),
            unique(observed(11, 10, false, Hidden), 101, 100),
            unique(observed(12, 11, false, Hidden), 102, 101),
        ],
    )
    .attribute(Marker::Execution, &ids)?;
    assert_eq!(owners.len(), 3);
    // An inventory that started after them sees only the adopted job, which
    // names itself and its exited shell.
    let after = || std::time::Instant::now() + Duration::from_secs(1);
    Inventory {
        started: after(),
        ..inventory(&owners, [unique(observed(12, 1, true, Hidden), 102, 101)])
    }
    .attribute(Marker::Execution, &ids)?;
    assert_eq!(owners.len(), 2);
    assert!(owners.recall(100).is_none());
    // Once the job has exited too, nothing can name them again.
    Inventory {
        started: after(),
        ..inventory(&owners, [])
    }
    .attribute(Marker::Execution, &ids)?;
    assert_eq!(owners.len(), 0);
    Ok(())
}

#[test]
fn an_inventory_keeps_owners_recorded_after_it_began() {
    let owners = owners::Owners::default();
    let marks = Marks {
        execution: Some("execution".into()),
        workspace: None,
    };
    let began = std::time::Instant::now();
    // An overlapping inventory records a process this one's listing missed.
    owners.update(
        std::time::Instant::now(),
        vec![(100, marks.clone())],
        &HashSet::new(),
    );
    owners.update(began, Vec::new(), &HashSet::new());
    assert_eq!(owners.recall(100), Some(marks));
}

/// An agent's job whose shell and agent were killed stays the agent's: the
/// daemon remembers who owned it from an inventory made while they lived.
#[tokio::test]
async fn a_job_stays_owned_after_its_shell_and_agent_exit() -> Result<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut agent = tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "process::identity::tests::marked_parent_of_a_shell_job",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(crate::env::EXECUTION_ID, &id)
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = agent.stdout.take().context("agent stdout")?;
    let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(stdout));
    let (shell, job) = loop {
        let line = lines.next_line().await?.context("the agent exited")?;
        if let Some((shell, job)) = line
            .split_once(' ')
            .and_then(|(shell, job)| Some((shell.parse().ok()?, job.parse().ok()?)))
        {
            break (shell, job);
        }
    };
    let shell = capture(shell)?.context("shell")?;
    let job = capture(job)?.context("job")?;
    let ids = HashSet::from([id.clone()]);
    let owned = |scan: &Scan| scan.processes.iter().any(|p| p.identity == job);

    let before = scan(ids.clone()).await?;
    agent.kill().await?;
    agent.wait().await?;
    signal(&shell, libc::SIGKILL)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while lineage(job.pid)?.is_some_and(|lineage| lineage.parent_pid != 1) {
        ensure!(
            std::time::Instant::now() < deadline,
            "the job was never adopted"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let after = scan(ids).await;
    signal(&job, libc::SIGKILL)?;
    let after = after?;

    assert!(owned(&before));
    assert!(owned(&after));
    assert!(after.unreadable.iter().all(|u| u.identity != job));
    Ok(())
}

#[test]
#[ignore = "helper agent launched by ownership test"]
fn marked_parent_of_a_shell_job() {
    // The shell prints its PID and its job's, then waits for the job.
    let mut shell = std::process::Command::new("/bin/sh")
        .args(["-c", "sleep 300 </dev/null & echo $$ $!; wait"])
        .spawn()
        .unwrap();
    shell.wait().unwrap();
}

/// A shell under a setuid program the user started, such as a terminal's
/// `login` or an agent's `sudo -u`, which runs as root yet accepts the user's
/// signals, takes the attribution of the setuid program's parent.
#[cfg(target_os = "macos")]
#[test]
fn a_hidden_environment_takes_its_attribution_through_a_setuid_parent() -> Result<()> {
    use Evidence::{Hidden, Unmarked};
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
    let owners = owners::Owners::default();
    let starter = |evidence| {
        inventory(
            &owners,
            [
                shell(),
                (
                    elevated.lineage.identity.pid,
                    Observed {
                        lineage: elevated.lineage.clone(),
                        evidence: Evidence::Elevated,
                    },
                ),
                observed(std::process::id(), 1, false, evidence),
            ],
        )
    };
    let resolve =
        |inventory: Inventory| describe(inventory.resolve(u32::MAX >> 1, &mut HashMap::new()));
    assert_eq!(resolve(starter(Unmarked)), "foreign");
    assert_eq!(resolve(starter(marked("execution"))), "owned by execution");
    // The setuid program is never reported itself.
    let scan = starter(marked("execution"))
        .attribute(Marker::Execution, &HashSet::from(["execution".into()]))?;
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

#[cfg(target_os = "macos")]
#[test]
fn an_exiting_or_exited_process_has_ended_its_image() {
    let bsd = |status: u32, flags: u32| {
        let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        bsd.pbi_status = status;
        bsd.pbi_flags = flags;
        bsd
    };
    // SRUN = 2; PROC_FLAG_LP64 = 0x10; PROC_FLAG_INEXIT = 0x4; SZOMB = 5.
    assert!(matches!(bsd_image(&bsd(2, 0x10)), Image::Settled));
    assert!(matches!(bsd_image(&bsd(2, 0x14)), Image::Ended));
    assert!(matches!(bsd_image(&bsd(5, 0)), Image::Ended));
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
