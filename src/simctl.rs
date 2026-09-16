use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
use tokio::{process::Command, time::timeout};

#[derive(Debug, Deserialize, Serialize)]
pub struct Inventory {
    pub devicetypes: Vec<DeviceType>,
    pub runtimes: Vec<Runtime>,
    pub devices: BTreeMap<String, Vec<Device>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeviceType {
    pub identifier: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub identifier: String,
    pub name: String,
    pub is_available: bool,
    #[serde(default, skip_serializing)]
    pub supported_device_types: Option<Vec<DeviceType>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub udid: String,
    pub name: String,
    pub state: String,
    pub is_available: bool,
}

impl Inventory {
    pub fn device(&self, udid: &str) -> Option<&Device> {
        self.devices
            .values()
            .flatten()
            .find(|d| d.udid.eq_ignore_ascii_case(udid))
    }
    pub fn running_count(&self) -> usize {
        self.devices
            .values()
            .flatten()
            .filter(|d| d.state != "Shutdown")
            .count()
    }
}

pub async fn run(args: &[&str]) -> Result<String> {
    ensure!(cfg!(target_os = "macos"), "Xcode simulators require macOS");
    let output = timeout(
        Duration::from_secs(180),
        Command::new("xcrun")
            .arg("simctl")
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("simctl timed out; allocation retained for reconciliation")??;
    ensure!(
        output.status.success(),
        "simctl {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub async fn inventory() -> Result<Inventory> {
    serde_json::from_str(&run(&["list", "--json"]).await?).context("parse simctl inventory")
}

/// listapps returns a plist, and requires a booted device. Cache counts at
/// release so allocation need not boot an idle device just to estimate cost.
pub async fn user_app_count(udid: &str) -> Result<usize> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let apps = run(&["listapps", udid]).await?;
    let output = timeout(Duration::from_secs(10), async {
        let mut convert = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-", "--", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdin = convert.stdin.take().context("plutil stdin unavailable")?;
        // Write concurrently with draining stdout to avoid pipe-buffer deadlocks.
        let write = async move {
            stdin.write_all(apps.as_bytes()).await?;
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let (output, ()) = tokio::try_join!(convert.wait_with_output(), write)?;
        Ok::<_, anyhow::Error>(output)
    })
    .await
    .context("plist conversion timed out")??;
    ensure!(
        output.status.success(),
        "cannot parse simctl application list"
    );
    let apps: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    // Count all non-system entries conservatively if metadata is incomplete.
    Ok(apps
        .values()
        .filter(|app| app.get("ApplicationType").and_then(|t| t.as_str()) != Some("System"))
        .count())
}
