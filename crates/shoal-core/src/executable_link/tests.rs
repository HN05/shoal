use super::{aim, path};

#[test]
fn aiming_the_link_points_it_at_the_new_binary() {
    let root = tempfile::tempdir().unwrap();
    let link = path(&root.path().join("state"));
    let (old, new) = (root.path().join("old"), root.path().join("new"));

    aim(&link, &old).unwrap();
    aim(&link, &new).unwrap();

    assert_eq!(std::fs::read_link(&link).unwrap(), new);
    let left: Vec<_> = std::fs::read_dir(link.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["shoal"]);
}

#[test]
fn a_daemon_started_through_the_link_keeps_its_installation() {
    let root = tempfile::tempdir().unwrap();
    let installed = root.path().join("installed");
    let state = root.path().join("state");
    let through = root.path().join("state-alias");
    let link = path(&state);
    aim(&link, &installed).unwrap();
    std::os::unix::fs::symlink(&state, &through).unwrap();

    aim(&link, &link).unwrap();
    aim(&link, &path(&through)).unwrap();

    assert_eq!(std::fs::read_link(&link).unwrap(), installed);
}
