use super::*;
use crate::test_support::assert_pending;
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

fn owned_daemon(root: &std::path::Path) -> Result<(Paths, File)> {
    let paths = Paths::for_test(root);
    fs::create_dir_all(&paths.state)?;
    let owner = File::create(paths.daemon_lock())?;
    fs2::FileExt::lock_exclusive(&owner)?;
    Ok((paths, owner))
}

#[tokio::test(start_paused = true)]
async fn draining_waits_for_ownership_and_keeps_it_through_job_removal() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let (paths, owner) = owned_daemon(root.path())?;
    let waiting = await_exit(&paths, true);
    tokio::pin!(waiting);
    assert_pending(&mut waiting).await;
    tokio::time::advance(timing::WORKSPACE_STOP_TIMEOUT).await;
    assert_pending(&mut waiting).await;
    fs2::FileExt::unlock(&owner)?;
    let drained = waiting.await?.context("expected ownership lock")?;
    assert!(fs2::FileExt::try_lock_exclusive(&owner).is_err());
    drop(drained);
    fs2::FileExt::try_lock_exclusive(&owner)?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn failed_signals_and_stalled_drains_leave_the_service_loaded() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let (paths, _owner) = owned_daemon(root.path())?;
    assert!(
        await_exit(&paths, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("could not signal")
    );
    let started = Instant::now();
    let error = await_exit(&paths, true).await.unwrap_err();
    assert!(error.to_string().contains("service left loaded"));
    assert_eq!(started.elapsed(), SERVICE_STOP_TIMEOUT);
    Ok(())
}

#[tokio::test]
async fn legacy_service_jobs_are_drained_before_their_stop_deadline_starts() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let path = std::env::join_paths(std::iter::once(root.path().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?;
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        crate::test_support::isolated_test(
            root.path(),
            "service::shutdown::tests::legacy_service_child",
        )
        .env("PATH", path)
        .output(),
    )
    .await??;
    ensure!(output.status.success(), "{output:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "isolated test with legacy fake service managers"]
async fn legacy_service_child() -> Result<()> {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return Ok(());
    }
    let root = std::env::current_dir()?;
    fs::create_dir(root.join("bin"))?;
    for name in ["launchctl", "systemctl"] {
        let script = root.join("bin").join(name);
        fs::write(
            &script,
            r#"#!/usr/bin/env python3
import sys
from pathlib import Path
args = sys.argv[1:]
if args[0] == '--user':
    args = args[1:]
with Path('calls').open('a') as calls:
    calls.write(' '.join(args) + '\n')
if args[0] == 'kill':
    Path('signaled').touch()
elif args[0] in ('bootout', 'stop'):
    # Model the already-loaded job's old 20-second limit: an undrained
    # daemon would be force-killed, even if its plist on disk was updated.
    if not Path('drained').exists():
        sys.exit('legacy service deadline cut the daemon drain short')
"#,
        )?;
        fs::set_permissions(script, fs::Permissions::from_mode(0o755))?;
    }
    for platform in [Platform::Mac, Platform::Linux] {
        let (paths, owner) = owned_daemon(&root)?;
        let stopping = tokio::spawn(async move { stop(&paths, platform).await });
        tokio::time::timeout(Duration::from_secs(60), async {
            while !root.join("signaled").exists() && !stopping.is_finished() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        ensure!(
            root.join("signaled").exists(),
            "job stopped before signaling daemon"
        );
        fs::write(root.join("drained"), "")?;
        fs2::FileExt::unlock(&owner)?;
        stopping.await??;
        let calls = fs::read_to_string(root.join("calls"))?;
        let operations: Vec<_> = calls
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        let expected = match platform {
            Platform::Mac => vec!["print", "kill", "bootout"],
            Platform::Linux => vec!["kill", "stop"],
        };
        assert_eq!(operations, expected);
        for file in ["calls", "signaled", "drained"] {
            fs::remove_file(root.join(file))?;
        }
    }
    Ok(())
}
