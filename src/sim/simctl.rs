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
    #[serde(default, skip_serializing)]
    pub supported_device_types: Option<Vec<DeviceType>>,
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

pub async fn run(args: &[&str]) -> Result<String> {
    ensure!(cfg!(target_os = "macos"), "Xcode simulators require macOS");
    let mut command = Command::new(Tool::Xcrun.program());
    command.arg("simctl").args(args);
    let output = crate::subprocess::Run::new(command)
        .timeout(Duration::from_secs(180))
        .output()
        .await
        .context("simctl failed; allocation retained for reconciliation")?;
    Ok(output.trim().to_owned())
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
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{fs, path::Path, process::Command};

    // Exercise the real fixture output on every platform without invoking xcrun.
    fn fixture(root: &Path, args: &[&str]) -> String {
        let output = Command::new("python3")
            .args([
                "-c",
                include_str!("../../tests/fixtures/simctl.py"),
                "simctl",
            ])
            .args(args)
            .env("HOME", root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn native_states_preserve_wire_spelling_and_conservative_capacity() {
        let root = tempfile::tempdir().unwrap();
        let udid = fixture(root.path(), &["create", "shoal-test", "Phone", "iOS Test"]);
        for (command, native, expected) in [
            ("shutdown", "Shutdown", DeviceState::Shutdown),
            ("bootstatus", "Creating", DeviceState::Creating),
            ("bootstatus", "Booting", DeviceState::Booting),
            ("bootstatus", "Booted", DeviceState::Booted),
            ("shutdown", "Shutting Down", DeviceState::ShuttingDown),
            (
                "shutdown",
                "Future State",
                DeviceState::Unknown("Future State".into()),
            ),
            (
                "shutdown",
                "shutdown",
                DeviceState::Unknown("shutdown".into()),
            ),
            (
                "bootstatus",
                "booted",
                DeviceState::Unknown("booted".into()),
            ),
            ("shutdown", "", DeviceState::Unknown(String::new())),
        ] {
            fs::write(
                root.path().join("sim-state-overrides.json"),
                json!({command: native}).to_string(),
            )
            .unwrap();
            fixture(root.path(), &[command, &udid]);
            let wire: Value =
                serde_json::from_str(&fixture(root.path(), &["list", "--json"])).unwrap();
            let inventory: Inventory = serde_json::from_value(wire.clone()).unwrap();
            let state = &inventory.device(&udid).unwrap().state;
            assert_eq!(state, &expected);
            assert_eq!(state.is_booted(), native == "Booted");
            assert_eq!(state.is_shutdown(), native == "Shutdown");
            assert_eq!(inventory.running_count(), usize::from(native != "Shutdown"));
            assert_eq!(serde_json::to_value(&inventory).unwrap(), wire);
        }
    }

    #[test]
    fn malformed_native_states_reject_the_inventory() {
        let root = tempfile::tempdir().unwrap();
        for malformed in [
            json!(null),
            json!(1),
            json!(true),
            json!([]),
            json!({"Booted": null}),
        ] {
            let device = json!({"name": "shoal-test", "udid": "test", "state": malformed, "isAvailable": true});
            fs::write(
                root.path().join("sim-devices.json"),
                json!([device]).to_string(),
            )
            .unwrap();
            let wire = fixture(root.path(), &["list", "--json"]);
            assert!(serde_json::from_str::<Inventory>(&wire).is_err(), "{wire}");
        }
        fs::write(
            root.path().join("sim-devices.json"),
            json!([{"name": "shoal-test", "udid": "test", "isAvailable": true}]).to_string(),
        )
        .unwrap();
        assert!(
            serde_json::from_str::<Inventory>(&fixture(root.path(), &["list", "--json"])).is_err()
        );
    }
}
