use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn invoked_path_keeps_an_installation_symlink_to_the_running_binary() {
    let root = tempfile::tempdir().unwrap();
    let keg = root.path().join("keg");
    let bin = root.path().join("bin");
    fs::create_dir_all(&keg).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let binary = keg.join("shoal");
    let other = keg.join("other");
    for file in [&binary, &other] {
        fs::write(file, "").unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let link = bin.join("shoal");
    std::os::unix::fs::symlink(&binary, &link).unwrap();
    let resolve = |arg: &str| {
        invoked_path(
            Some(arg.into()),
            bin.as_os_str(),
            root.path(),
            binary.clone(),
        )
    };
    assert_eq!(resolve(link.to_str().unwrap()), link);
    assert_eq!(resolve("shoal"), link, "found on PATH");
    assert_eq!(
        resolve("bin/shoal"),
        link,
        "relative to the current directory"
    );
    assert_eq!(
        invoked_path(
            Some("shoal".into()),
            "bin".as_ref(),
            root.path(),
            binary.clone()
        ),
        link,
        "found through a relative PATH entry"
    );
    assert_eq!(resolve(other.to_str().unwrap()), binary);
    assert_eq!(resolve("missing"), binary);
    assert_eq!(
        invoked_path(None, bin.as_os_str(), root.path(), binary.clone()),
        binary
    );
}
