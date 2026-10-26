use crate::tools::Tool;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
use tokio::process::Command;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing)]
    pub supported_device_types: Option<Vec<DeviceType>>,
}

impl Runtime {
    pub fn supports(&self, device: &DeviceType) -> bool {
        self.supported_device_types
            .as_ref()
            .is_none_or(|types| types.iter().any(|d| d.identifier == device.identifier))
    }

    pub fn ios_version(&self) -> Option<Vec<u64>> {
        self.identifier
            .strip_prefix("com.apple.CoreSimulator.SimRuntime.iOS-")?;
        self.version
            .as_deref()?
            .split('.')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub udid: String,
    pub name: String,
    pub state: DeviceState,
    pub is_available: bool,
}

/// Native simctl state, independent of Shoal's ownership/lease lifecycle.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "String")]
pub enum DeviceState {
    Creating,
    Shutdown,
    Booting,
    Booted,
    #[serde(rename = "Shutting Down")]
    ShuttingDown,
    /// Preserve unfamiliar spellings without treating them as confirmed states.
    #[serde(untagged)]
    Unknown(String),
}

impl From<String> for DeviceState {
    fn from(value: String) -> Self {
        match value.as_str() {
            "Creating" => Self::Creating,
            "Shutdown" => Self::Shutdown,
            "Booting" => Self::Booting,
            "Booted" => Self::Booted,
            "Shutting Down" => Self::ShuttingDown,
            _ => Self::Unknown(value),
        }
    }
}

impl DeviceState {
    pub fn is_booted(&self) -> bool {
        matches!(self, Self::Booted)
    }

    pub fn is_shutdown(&self) -> bool {
        matches!(self, Self::Shutdown)
    }

    /// Transitional and unknown states may still occupy running capacity.
    pub fn counts_as_running(&self) -> bool {
        !self.is_shutdown()
    }
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
            .filter(|d| d.state.counts_as_running())
            .count()
    }
}

async fn run(args: &[&str]) -> Result<String> {
    ensure!(cfg!(target_os = "macos"), "Xcode simulators require macOS");
    let mut command = Command::new(Tool::Xcrun.program());
    command.arg("simctl").args(args);
    let output = crate::subprocess::Run::new(command)
        .timeout(Duration::from_secs(180))
        .output()
        .await
        .with_context(|| format!("simctl {}", args.join(" ")))?;
    Ok(output.trim().to_owned())
}

pub async fn create(name: &str, device: &str, runtime: &str) -> Result<String> {
    run(&["create", name, device, runtime]).await
}

pub async fn shutdown(udid: &str) -> Result<()> {
    run(&["shutdown", udid]).await.map(|_| ())
}

pub async fn delete(udid: &str) -> Result<()> {
    run(&["delete", udid]).await.map(|_| ())
}

pub async fn erase(udid: &str) -> Result<()> {
    run(&["erase", udid]).await.map(|_| ())
}

pub async fn wait_for_boot(udid: &str) -> Result<()> {
    run(&["bootstatus", udid, "-b"]).await.map(|_| ())
}

pub async fn inventory() -> Result<Inventory> {
    serde_json::from_str(&run(&["list", "--json"]).await?).context("parse simctl inventory")
}

/// listapps returns a plist, and requires a booted device. Cache counts at
/// release so allocation need not boot an idle device just to estimate cost.
pub async fn user_app_count(udid: &str) -> Result<usize> {
    let apps = run(&["listapps", udid]).await?;
    let mut convert = Command::new(Tool::Plutil.program());
    convert.args(["-convert", "json", "-o", "-", "--", "-"]);
    let output = crate::subprocess::Run::new(convert)
        .input(apps.into_bytes())
        .timeout(Duration::from_secs(10))
        .checked()
        .await
        .context("cannot parse simctl application list")?;
    let apps: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&output.stdout)?;
    // Count all non-system entries conservatively if metadata is incomplete.
    Ok(apps
        .values()
        .filter(|app| app.get("ApplicationType").and_then(|t| t.as_str()) != Some("System"))
        .count())
}

#[cfg(test)]
mod tests;
