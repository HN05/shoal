use std::{fs, path::Path};

use shoal_protocol::hub::sleep::Power;

use super::{powers_system_from_battery, system_battery_power};

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

#[test]
fn a_discharging_system_battery_runs_the_machine_on_battery_at_the_mean_level() {
    let class = tempfile::tempdir().unwrap();
    let battery = |name, capacity, status| {
        let attributes = [
            ("type", "Battery"),
            ("capacity", capacity),
            ("status", status),
        ];
        supply(class.path(), name, &attributes);
    };
    battery("BAT0", "80", "Full");
    supply(
        class.path(),
        "hidpp_battery_0",
        &[
            ("type", "Battery"),
            ("scope", "Device"),
            ("capacity", "3"),
            ("status", "Discharging"),
        ],
    );
    let on_external = Power {
        on_battery: false,
        percent: 80,
    };
    assert_eq!(system_battery_power(class.path()), Some(on_external));
    battery("BAT1", "4", "Discharging");
    let on_battery = Power {
        on_battery: true,
        percent: 42,
    };
    assert_eq!(system_battery_power(class.path()), Some(on_battery));
}

#[test]
fn a_machine_whose_battery_says_no_level_reports_no_power() {
    let class = tempfile::tempdir().unwrap();
    supply(class.path(), "BAT0", &[("type", "Battery")]);
    assert_eq!(system_battery_power(class.path()), None);
}
