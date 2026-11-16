use super::*;

#[test]
fn preset_directories_go_first_on_the_exported_path() {
    let exported = export_configured(
        BTreeMap::from([(PATH_PREFIX.to_owned(), "/cache/xcode/bin".to_owned())]),
        Some("/usr/bin".into()),
    )
    .unwrap();
    assert_eq!(
        exported,
        BTreeMap::from([("PATH".to_owned(), "/cache/xcode/bin:/usr/bin".to_owned())])
    );
    // A PATH `[env]` sets is the one the directories go before.
    let exported = export_configured(
        BTreeMap::from([
            (PATH_PREFIX.to_owned(), "/cache/xcode/bin".to_owned()),
            ("PATH".to_owned(), "/tools".to_owned()),
        ]),
        Some("/usr/bin".into()),
    )
    .unwrap();
    assert_eq!(exported["PATH"], "/cache/xcode/bin:/tools");
}
