use std::{fs, path::Path};

use super::powers_system_from_battery;

fn supply(class: &Path, name: &str, attributes: &[(&str, &str)]) {
    let supply = class.join(name);
    fs::create_dir_all(&supply).unwrap();
    for (attribute, value) in attributes {
        fs::write(supply.join(attribute), format!("{value}\n")).unwrap();
    }
}

#[test]
fn a_system_battery_counts_and_a_device_s_or_mains_supply_does_not() {
    let class = tempfile::tempdir().unwrap();
    supply(class.path(), "AC", &[("type", "Mains")]);
    supply(
        class.path(),
        "hidpp_battery_0",
        &[("type", "Battery"), ("scope", "Device")],
    );
    assert!(!powers_system_from_battery(class.path()));
    supply(class.path(), "BAT0", &[("type", "Battery")]);
    assert!(powers_system_from_battery(class.path()));
}

#[test]
fn a_machine_without_the_class_has_no_battery() {
    let root = tempfile::tempdir().unwrap();
    assert!(!powers_system_from_battery(&root.path().join("absent")));
}
