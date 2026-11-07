use super::*;
use crate::config::{edit, repo::ConfigLayers};

const TEMPLATE: &str = include_str!("../../crates/shoal-client/configs/default.toml");

#[test]
fn install_writes_the_template_once_and_keeps_edits() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let expected = home.path().join(".config/shoal/config.toml");
    let (path, created) = Config::install(&paths).unwrap();
    assert!(created && path == expected);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);
    std::fs::write(&path, "agent.default = 'claude'\n").unwrap();
    let (same, created) = Config::install(&paths).unwrap();
    assert!(!created && same == expected);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        Config::parse(&text, &paths)
            .unwrap()
            .repository
            .agent
            .default,
        "claude".parse::<crate::agent::Agent>().ok()
    );
}

#[test]
fn replace_keeps_the_replaced_file_as_a_backup() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join(".config/shoal/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let edited = "agent.default = 'claude'\n";
    std::fs::write(&path, edited).unwrap();
    let (same, backup) = replace(path.clone(), "template\n").unwrap();
    let backup = backup.unwrap();
    assert!(same == path && backup == home.path().join(".config/shoal/config.toml.backup"));
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), edited);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "template\n");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(replace(path.clone(), "template\n").unwrap().1, None);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), edited);
}

fn change(key: &str, value: Option<&str>) -> edit::Change {
    edit::Change {
        key: key.into(),
        value: value.map(Into::into),
    }
}

fn edit_one(paths: &Paths, key: &str, value: Option<&str>) -> Result<()> {
    Config::edit(paths, &[change(key, value)]).map(drop)
}

#[test]
fn global_edits_must_leave_a_valid_range_with_the_defaults() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    assert!(edit_one(&paths, "ports.end", Some("4000")).is_err());
    assert!(edit_one(&paths, "ports.end", Some("0")).is_err());
    edit_one(&paths, "ports.start", Some("3000")).unwrap();
    edit_one(&paths, "ports.end", Some("4000")).unwrap();
    let ports = Config::load(&paths)
        .unwrap()
        .resolve(&ConfigLayers::default())
        .unwrap()
        .into_settings()
        .ports;
    assert_eq!((ports.start, ports.end), (3000, 4000));
    // Removing the start leaves the default start above the saved end.
    assert!(edit_one(&paths, "ports.start", None).is_err());
    edit_one(&paths, "ports.end", None).unwrap();
    edit_one(&paths, "ports.start", None).unwrap();
    assert!(Config::parse("[ports]\nstart = 60000\n", &paths).is_ok());
}

#[test]
fn dependent_global_edits_validate_together() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let profile = [
        change("machine.simulators.profiles.phone.runtime", Some("iOS 27")),
        change("machine.simulators.default", Some("phone")),
        change(
            "machine.simulators.profiles.phone.device",
            Some("iPhone 17"),
        ),
    ];
    for single in &profile[..2] {
        assert!(Config::edit(&paths, std::slice::from_ref(single)).is_err());
    }
    assert!(!Config::path(&paths).exists());
    Config::edit(&paths, &profile).unwrap();
    let simulators = Config::load(&paths).unwrap().machine.simulators;
    assert_eq!(simulators.default.as_deref(), Some("phone"));
    assert_eq!(simulators.profiles["phone"].runtime, "iOS 27");
    // Removing a profile its default still names fails unless both go at once.
    assert!(edit_one(&paths, "machine.simulators.profiles.phone", None).is_err());
    Config::edit(
        &paths,
        &[
            change("machine.simulators.default", None),
            change("machine.simulators.profiles.phone", None),
        ],
    )
    .unwrap();
    assert!(
        Config::load(&paths)
            .unwrap()
            .machine
            .simulators
            .profiles
            .is_empty()
    );
}
