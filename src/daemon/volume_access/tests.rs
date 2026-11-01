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

    tokio::time::advance(RECHECK_AFTER).await;
    let (result, again) = access.check(other, never()).await;
    assert!(result.is_err());
    assert_eq!(again, None, "a stall that goes on is reported once");
    assert_eq!(access.stalls(), [stall]);

    tokio::time::advance(RECHECK_AFTER).await;
    let (result, _) = access.check(other, async { Ok(7) }).await;
    assert_eq!(result.unwrap(), 7);
    assert_eq!(access.stalls(), []);
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
