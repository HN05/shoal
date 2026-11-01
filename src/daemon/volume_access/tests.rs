use super::*;

const WORKSPACE: &str = "/Volumes/SSD/shoal/repo/feature";

fn never() -> impl Future<Output = Result<()>> {
    std::future::pending()
}

#[test]
fn a_path_under_volumes_stands_for_its_volume_and_any_other_for_itself() {
    assert_eq!(volume(Path::new(WORKSPACE)), Path::new("/Volumes/SSD"));
    assert_eq!(
        volume(Path::new("/Users/me/shoal/repo")),
        Path::new("/Users/me/shoal/repo")
    );
    assert_eq!(volume(Path::new("/Volumes")), Path::new("/Volumes"));
}

#[tokio::test(start_paused = true)]
async fn a_check_that_runs_out_stalls_its_volume_once_until_a_recheck_finishes() {
    let access = VolumeAccess::default();

    let (result, stall) = access.check(Path::new(WORKSPACE), never()).await;
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains("file access on /Volumes/SSD stalled; a macOS privacy prompt"),
        "{error}"
    );
    let stall = stall.expect("the first stall is new");
    assert_eq!(stall.volume, Path::new("/Volumes/SSD"));
    assert_eq!(stall.path, Path::new(WORKSPACE));
    assert_eq!(stall.reason, "did not finish within 30s");
    assert_eq!(access.stalls(), std::slice::from_ref(&stall));

    // Another path on the volume skips its check meanwhile.
    let other = Path::new("/Volumes/SSD/shoal/repo/other");
    let (result, again) = access
        .check::<()>(other, async { panic!("a stalled volume skips its checks") })
        .await;
    assert!(result.is_err());
    assert_eq!(again, None);
    assert_eq!(access.stalled(other), Some(stall.clone()));

    tokio::time::advance(RECHECK_AFTER).await;
    let (result, again) = access.check(other, never()).await;
    assert!(result.is_err());
    assert_eq!(again, None, "a stall that goes on is reported once");
    assert_eq!(access.stalls(), [stall]);

    tokio::time::advance(RECHECK_AFTER).await;
    let (result, _) = access.check(other, async { Ok(7) }).await;
    assert_eq!(result.unwrap(), 7);
    assert_eq!(access.stalls(), []);
    assert_eq!(access.stalled(Path::new(WORKSPACE)), None);
}

#[tokio::test(start_paused = true)]
async fn one_check_at_a_time_rechecks_a_stalled_volume() {
    let access = VolumeAccess::default();
    let (_, stall) = access.check(Path::new(WORKSPACE), never()).await;
    assert!(stall.is_some());
    tokio::time::advance(RECHECK_AFTER).await;

    let recheck = access.check(Path::new(WORKSPACE), never());
    let other = async {
        // Past RECHECK_AFTER since the stall, while the recheck still runs.
        tokio::time::advance(RECHECK_AFTER).await;
        access
            .check::<()>(Path::new(WORKSPACE), async {
                panic!("a recheck runs already")
            })
            .await
    };
    let ((rechecked, _), (skipped, _)) = tokio::join!(recheck, other);
    assert!(rechecked.is_err());
    assert!(skipped.is_err());

    // A recheck that was dropped unfinished lets the next one run.
    tokio::time::advance(RECHECK_AFTER).await;
    drop(
        tokio::time::timeout(
            Duration::from_secs(1),
            access.check(Path::new(WORKSPACE), never()),
        )
        .await,
    );
    let (result, _) = access.check(Path::new(WORKSPACE), async { Ok(()) }).await;
    result.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_interrupted_check_stalls_its_volume_and_keeps_its_error() {
    let access = VolumeAccess::default();
    let (result, stall) = access
        .check(Path::new(WORKSPACE), async {
            Err::<(), _>(anyhow!(
                "git failed (exit status: 128): fatal: Unable to read current working directory: Interrupted system call"
            ))
        })
        .await;
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.starts_with("file access on /Volumes/SSD stalled"),
        "{error}"
    );
    assert!(error.ends_with("Interrupted system call"), "{error}");
    assert_eq!(stall.unwrap().reason, "was interrupted");

    let access = VolumeAccess::default();
    let (_, stall) = access
        .check(Path::new(WORKSPACE), async {
            Err::<(), _>(io::Error::from(io::ErrorKind::Interrupted).into())
        })
        .await;
    assert!(stall.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_check_that_fails_otherwise_leaves_the_volume_answering() {
    let access = VolumeAccess::default();
    let (result, stall) = access
        .check(Path::new(WORKSPACE), async {
            Err::<(), _>(anyhow!("fatal: not a git repository"))
        })
        .await;
    assert_eq!(
        format!("{:#}", result.unwrap_err()),
        "fatal: not a git repository"
    );
    assert_eq!(stall, None);
    assert_eq!(access.stalls(), []);
}

#[tokio::test(start_paused = true)]
async fn waiting_names_the_check_that_has_run_longest() {
    let access = VolumeAccess::default();
    assert_eq!(access.waiting(), None);
    let started = tokio::sync::Notify::new();
    let (finish, finished) = tokio::sync::oneshot::channel::<()>();
    let check = access.check(Path::new(WORKSPACE), async {
        started.notify_one();
        let _ = finished.await;
        Ok(())
    });
    let observe = async {
        started.notified().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        let waiting = access.waiting();
        let _ = finish.send(());
        waiting
    };
    let ((result, _), waiting) = tokio::join!(check, observe);
    result.unwrap();
    assert_eq!(
        waiting,
        Some((PathBuf::from(WORKSPACE), Duration::from_secs(5)))
    );
    assert_eq!(access.waiting(), None);
}

#[tokio::test]
async fn the_daemon_alerts_once_while_a_volume_stays_stalled() {
    let (temp, manager) = crate::test_support::manager().await;
    let path = temp.path().join("workspace");
    tokio::time::pause();
    for _ in 0..2 {
        tokio::time::advance(RECHECK_AFTER).await;
        let result = manager.check_volume(&path, never()).await;
        assert!(result.is_err());
    }
    tokio::time::resume();

    let alerts = manager.notifications(false, 10).await.unwrap();
    let [alert] = alerts.as_slice() else {
        panic!("expected one alert: {alerts:?}");
    };
    assert_eq!(alert.kind, NotificationKind::VolumeStalled);
    assert_eq!(
        alert.message,
        format!(
            "File access on {} stalled: a check did not finish within 30s. A macOS privacy prompt may be waiting for an answer.",
            path.display()
        )
    );
    assert_eq!(manager.volume_access.stalls().len(), 1);
}
