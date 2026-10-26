use super::*;

#[test]
fn deletes_only_the_selected_projects_folders() {
    let root = tempfile::tempdir().unwrap();
    let location = root.path();
    let removed = folder(location, "App-a", Path::new("/w/removed/App.xcodeproj"));
    let kept = folder(location, "App-b", Path::new("/w/kept/App.xcodeproj"));
    fs::create_dir(location.join("ModuleCache.noindex")).unwrap();
    let deleted = delete(location, |project| project.starts_with("/w/removed")).unwrap();
    assert_eq!(deleted, std::slice::from_ref(&removed));
    assert!(!removed.exists());
    assert!(kept.exists());
    assert!(location.join("ModuleCache.noindex").exists());
    assert!(
        delete(&location.join("missing"), |_| true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn location_follows_an_absolute_custom_location() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        location(home.path()),
        Some(home.path().join(DEFAULT_LOCATION))
    );
    let preferences = home.path().join(PREFERENCES);
    fs::create_dir_all(preferences.parent().unwrap()).unwrap();
    let write = |location: &str| {
        let preferences_value = plist::Dictionary::from_iter([(
            "IDECustomDerivedDataLocation".to_owned(),
            plist::Value::String(location.to_owned()),
        )]);
        plist::Value::Dictionary(preferences_value)
            .to_file_binary(&preferences)
            .unwrap();
    };
    write("/Volumes/Builds/DerivedData");
    assert_eq!(
        location(home.path()),
        Some(PathBuf::from("/Volumes/Builds/DerivedData"))
    );
    write("DerivedData");
    assert_eq!(location(home.path()), None);
}
