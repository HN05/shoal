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
