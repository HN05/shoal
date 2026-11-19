use super::*;

#[test]
fn load_sets_the_thresholds_the_file_leaves_out() {
    let mut balanced = Overload::default();
    balanced.follow_load(Load::Balanced, &toml::Table::new());
    assert_eq!(balanced, Overload::default());
    for load in [Load::Light, Load::Balanced, Load::Full] {
        let mut settings = Overload::default();
        settings.follow_load(load, &toml::Table::new());
        settings.validate().unwrap();
        assert!(settings.warning.cpu_used_percent < settings.cpu.used_percent);
        assert!(settings.warning.memory_used_percent < settings.memory.used_percent);
    }
    let text =
        "[warning]\ncpu_used_percent = 70\nmemory_stall_percent = 15\n[memory]\nused_percent = 96";
    let mut settings: Overload = toml::from_str(text).unwrap();
    settings.follow_load(Load::Full, &toml::from_str(text).unwrap());
    assert_eq!(settings.warning.cpu_used_percent, 70);
    assert_eq!(settings.warning.memory_stall_percent, 15);
    assert_eq!(settings.memory.used_percent, 96);
    // What the file leaves out follows the load.
    assert_eq!(settings.cpu.used_percent, Load::Full.cpu().stop);
    assert_eq!(
        settings.recovery.memory_used_percent,
        Load::Full.memory().recovery
    );
    assert_eq!(
        settings.memory.stall_percent,
        Load::Full.memory_stall().stop
    );
    assert_eq!(
        settings.recovery.memory_stall_percent,
        Load::Full.memory_stall().recovery
    );
}

#[test]
fn settings_are_bounded() {
    let defaults = Overload::default();
    defaults.validate().unwrap();
    for text in [
        "cooldown_seconds = 0",
        "[recovery]\nmemory_used_percent = 95",
        "[recovery]\ncpu_used_percent = 90",
        "[recovery]\nsustained_seconds = 0",
        "[cpu]\nused_percent = 0",
        "[cpu]\nused_percent = 101",
        "[cpu]\nsustained_seconds = 0",
        "[memory]\nused_percent = 0",
        "[memory]\nused_percent = 100",
        "[memory]\nsustained_seconds = 86401",
        "[memory]\nstall_percent = 0",
        "[memory]\nstall_percent = 100",
        "[recovery]\nmemory_stall_percent = 0",
        "[memory]\nstall_percent = 30\n[recovery]\nmemory_stall_percent = 30",
        "[warning]\nmemory_stall_percent = 0",
        "[memory]\nstall_percent = 30\n[warning]\nmemory_stall_percent = 31",
        "[disk]\nstop_free_gib = 0",
        "[disk]\ncleanup_free_gib = 1",
        "[disk]\ncleanup_free_gib = 1048577",
        "[warning]\nmemory_used_percent = 100",
        "[warning]\ncpu_used_percent = 0",
        "[warning]\ncpu_sustained_seconds = 0",
        "[warning]\ndisk_free_gib = 0",
        "[warning]\nrepeat_minutes = 0",
    ] {
        let config: Overload = toml::from_str(text).unwrap();
        assert!(config.validate().is_err(), "{text}");
    }
    // Lowering a stop threshold below the default warning stays valid.
    let config: Overload =
        toml::from_str("[memory]\nused_percent = 85\n[recovery]\nmemory_used_percent = 80")
            .unwrap();
    config.validate().unwrap();
    // A stall warning may equal the stop: it measures some tasks, the stop all.
    let config: Overload =
        toml::from_str("[memory]\nstall_percent = 30\n[warning]\nmemory_stall_percent = 30")
            .unwrap();
    config.validate().unwrap();
    let config: Overload = toml::from_str("[memory]\nenabled = false").unwrap();
    assert!(!config.memory.enabled);
    assert_eq!(config.memory.used_percent, defaults.memory.used_percent);
    assert!(toml::from_str::<Overload>("[memory]\nunknown = true").is_err());
    assert!(crate::config::repo::parse("[overload.memory]\nenabled = false").is_err());
}
