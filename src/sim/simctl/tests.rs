use super::*;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

// Exercise the real fixture output on every platform without invoking xcrun.
fn fixture(root: &Path, args: &[&str]) -> String {
    let output = Command::new("python3")
        .args([
            "-c",
            include_str!("../../../tests/fixtures/simctl.py"),
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
        let wire: Value = serde_json::from_str(&fixture(root.path(), &["list", "--json"])).unwrap();
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
        let device =
            json!({"name": "shoal-test", "udid": "test", "state": malformed, "isAvailable": true});
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
    assert!(serde_json::from_str::<Inventory>(&fixture(root.path(), &["list", "--json"])).is_err());
}
