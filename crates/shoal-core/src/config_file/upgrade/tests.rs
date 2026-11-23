use super::*;
use crate::config::{Config, Shared, machine::KeepAwake};
use crate::paths::Paths;

#[test]
fn a_boolean_keep_awake_becomes_its_level_keeping_the_comment() {
    let text = "[machine]\nkeep_awake = true # while agents work\nload = 'light'\n";
    assert_eq!(
        upgrade(text).unwrap(),
        "[machine]\nkeep_awake = \"on\" # while agents work\nload = 'light'\n"
    );
    let dotted = upgrade("machine.keep_awake = false\n").unwrap();
    assert_eq!(dotted, "machine.keep_awake = \"off\"\n");
    assert_eq!(upgrade("[machine]\nkeep_awake = 'finish'\n"), None);
}

#[test]
fn loading_the_global_files_saves_them_upgraded() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let own = "[machine]\nkeep_awake = true\n";
    crate::test_support::write_beside_global_config(&paths, "config.toml", own);
    let shared = "[machine]\nkeep_awake = false\n";
    crate::test_support::write_beside_global_config(&paths, "shared.toml", shared);
    let machine = Config::load(&paths).unwrap().machine;
    assert_eq!(machine.keep_awake, KeepAwake::On);
    let saved = |path| std::fs::read_to_string(path).unwrap();
    assert_eq!(
        saved(Config::path(&paths)),
        "[machine]\nkeep_awake = \"on\"\n"
    );
    assert_eq!(
        saved(Shared::path(&paths)),
        "[machine]\nkeep_awake = \"off\"\n"
    );
}

#[test]
fn a_file_left_unwritten_fails_to_load_naming_the_levels() {
    let paths = Paths::for_test("/home/test");
    let error = Config::parse("[machine]\nkeep_awake = true\n", &paths).unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("\"handover\""), "{error}");
}

#[test]
fn a_load_under_the_edit_locks_upgrades_the_files_it_holds() {
    use crate::config_file::ConfigExt;
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(home.path());
    let own = "[machine]\nkeep_awake = true\n";
    crate::test_support::write_beside_global_config(&paths, "config.toml", own);
    let machine = Config::load_settled(&paths).unwrap().machine;
    assert_eq!(machine.keep_awake, KeepAwake::On);
}
