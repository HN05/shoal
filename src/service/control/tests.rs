use super::*;
use crate::test_support::install_test_script;
use std::fs;

#[tokio::test]
async fn synchronous_service_stops_allow_the_daemon_drain() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let path = std::env::join_paths(std::iter::once(root.path().join("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?;
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        crate::test_support::isolated_test(
            root.path(),
            "service::control::tests::slow_service_manager_child",
        )
        .env("PATH", path)
        .output(),
    )
    .await??;
    anyhow::ensure!(output.status.success(), "{output:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "isolated test with fake service managers; never calls the OS manager"]
async fn slow_service_manager_child() -> Result<()> {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return Ok(());
    }
    let root = std::env::current_dir()?;
    fs::create_dir(root.join("bin"))?;
    for name in ["launchctl", "systemctl"] {
        let script = root.join("bin").join(name);
        install_test_script(
            &script,
            r#"#!/usr/bin/env python3
import time
from pathlib import Path
Path('ready').touch()
while not Path('release').exists():
    time.sleep(0.01)
"#,
        );
    }
    for (tool, args) in [
        (Tool::Launchctl, &["bootout", "test-service"][..]),
        (Tool::Systemctl, &["--user", "stop", "test.service"][..]),
        (Tool::Systemctl, &["--user", "restart", "test.service"][..]),
    ] {
        let stopping = tokio::spawn(async move { stop_command(tool, args).await });
        tokio::time::timeout(Duration::from_secs(60), async {
            while !root.join("ready").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        // The fake manager remains blocked on an explicit release while virtual
        // time passes. No real delay or process scheduling speed is asserted.
        tokio::time::pause();
        tokio::time::advance(timing::WORKSPACE_STOP_TIMEOUT).await;
        tokio::task::yield_now().await;
        tokio::time::resume();
        fs::write(root.join("release"), "")?;
        anyhow::ensure!(stopping.await??);
        fs::remove_file(root.join("ready"))?;
        fs::remove_file(root.join("release"))?;
    }
    Ok(())
}
