use super::*;
use crate::config::{
    MachineLimit,
    machine::KeepAwake,
    repo::{ConfigLayer, ConfigLayers},
    resolve::Stack,
};
use crate::test_support::write_beside_global_config;

/// A temporary home whose global files hold `own` and `shared`.
fn home(own: &str, shared: &str) -> (tempfile::TempDir, Paths) {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    write_beside_global_config(&paths, "config.toml", own);
    write_beside_global_config(&paths, FILE, shared);
    (home, paths)
}

#[test]
fn each_option_takes_the_global_file_over_the_shared_one() {
    let (_home, paths) = home(
        "[cleanup.auto]\nidle_minutes = 45\n",
        "[cleanup.auto]\nidle_minutes = 30\nenabled = false\n",
    );
    let config = Config::load(&paths).unwrap();
    let auto = config.global_settings().unwrap().cleanup.auto;
    assert_eq!((auto.enabled, auto.idle_minutes), (false, 45));
    let entries = Stack::new(&config, &ConfigLayers::default())
        .unwrap()
        .report()
        .unwrap();
    let layer = |key: &str| entries.iter().find(|entry| entry.key == key).unwrap().layer;
    assert_eq!(layer("cleanup.auto.enabled"), ConfigLayer::SharedConfig);
    assert_eq!(
        layer("cleanup.auto.idle_minutes"),
        ConfigLayer::GlobalConfig
    );
}

#[test]
fn machine_keys_merge_table_by_table_with_references_across_files() {
    let (_home, paths) = home(
        "[machine]\nmax_heavy_agents = 4\n[machine.hub]\nescalate_after_minutes = 5\n",
        "[machine]\nkeep_awake = 'finish'\nmax_jobs = { same_as = 'max_heavy_agents' }\n\
         [machine.hub]\nauto_update = false\nescalate_after_minutes = 60\n",
    );
    let machine = Config::load(&paths).unwrap().machine;
    assert_eq!(machine.keep_awake, KeepAwake::Finish);
    assert_eq!(machine.max_jobs, MachineLimit::Fixed(4));
    assert!(!machine.hub.auto_update);
    assert_eq!(machine.hub.escalate_after_minutes, 5);
    // A value in the global file replaces the shared file's reference.
    write_beside_global_config(&paths, "config.toml", "[machine]\nmax_jobs = 2\n");
    let machine = Config::load(&paths).unwrap().machine;
    assert_eq!(machine.max_jobs, MachineLimit::Fixed(2));
}

#[test]
fn the_shared_config_refuses_how_a_machine_reaches_the_hub() {
    let paths = Paths::for_test("/home/test");
    for setting in [
        "enabled = true",
        "listen = '127.0.0.1:7000'",
        "url = 'https://hub.example'",
        "connect = ['ssh', 'hub']",
    ] {
        let text = format!("[machine.hub]\n{setting}\n");
        assert!(Shared::parse(&text, &paths).is_err(), "{setting}");
        assert!(Config::parse(&text, &paths).is_ok(), "{setting}");
    }
    Shared::parse("[machine.hub]\nescalate_after_minutes = 5\n", &paths).unwrap();
}

#[test]
fn the_shared_config_must_hold_without_a_global_file() {
    let paths = Paths::for_test("/home/test");
    let ends_low = "[ports]\nend = 4000\n";
    assert!(Shared::parse(ends_low, &paths).is_err());
    let shared = Shared::parse("[ports]\nstart = 3000\n", &paths).unwrap();
    let ports = Config::parse_over(ends_low, &shared, &paths)
        .unwrap()
        .global_settings()
        .unwrap()
        .ports;
    assert_eq!((ports.start, ports.end), (3000, 4000));
}

#[test]
fn the_global_files_texts_win_over_the_shared_ones() {
    let (_home, paths) = home("", "[rules.inline]\ntext = 'Shared inline.'\n");
    write_beside_global_config(&paths, "shared/rules/only.md", "Shared only.");
    write_beside_global_config(&paths, "shared/rules/both.md", "Shared both.");
    write_beside_global_config(&paths, "rules/both.md", "Own both.");
    write_beside_global_config(&paths, "rules/inline.md", "Own file.");
    let config = Config::load_with_templates(&paths).unwrap();
    let rules = config.global_settings().unwrap().rules;
    assert_eq!(rules["only"].text, "Shared only.");
    assert_eq!(rules["both"].text, "Own both.");
    assert_eq!(rules["inline"].text, "Own file.");
    let entries = Stack::new(&config, &ConfigLayers::default())
        .unwrap()
        .report()
        .unwrap();
    let only = entries.iter().find(|entry| entry.key == "rules.only.text");
    let only = only.unwrap();
    assert_eq!(only.layer, ConfigLayer::SharedConfig);
    assert!(
        only.file
            .as_ref()
            .unwrap()
            .ends_with("shared/rules/only.md")
    );
}

#[test]
fn a_global_trigger_replaces_the_shared_one_of_its_name() {
    let trigger = |task: &str| format!("on = 'timer'\nat = '02:00'\ntask = '{task}'\n");
    let (_home, paths) = home(
        &format!("[triggers.nightly]\n{}", trigger("Own")),
        &format!(
            "[triggers.nightly]\n{}[triggers.weekly]\n{}",
            trigger("Shared"),
            trigger("Weekly")
        ),
    );
    let triggers = Config::load(&paths).unwrap().triggers();
    let task = |name: &str| {
        triggers[name]
            .launch
            .task
            .as_ref()
            .map(|task| task.as_str())
    };
    assert_eq!(
        (task("nightly"), task("weekly")),
        (Some("Own"), Some("Weekly"))
    );
}

#[test]
fn the_shared_files_are_the_config_and_its_mode_and_rule_files() {
    let (_home, paths) = home("", "agent.default = 'claude'\n");
    for (name, text) in [
        ("shared/modes/review.md", "Review it."),
        ("shared/rules/tests.md", "Test it."),
        ("shared/modes/review.md.backup", "Older."),
        ("shared/modes/notes.txt", "Not a mode."),
        ("shared/modes/bad name.md", "No name."),
        ("modes/own.md", "This machine's."),
    ] {
        write_beside_global_config(&paths, name, text);
    }
    let files = Shared::files(&paths).unwrap();
    let listed: Vec<_> = (files.iter())
        .map(|(name, text)| (name.to_string(), text.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("shared.toml".to_owned(), "agent.default = 'claude'\n"),
            ("modes/review.md".to_owned(), "Review it."),
            ("rules/tests.md".to_owned(), "Test it."),
        ]
    );
    let mode = FileName::Mode("review".into());
    assert_eq!(
        Shared::file(&paths, &mode),
        Config::path(&paths).with_file_name("shared/modes/review.md")
    );
}
