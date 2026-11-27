use super::{path, replace};

#[test]
fn replacing_the_link_points_it_at_the_new_binary() {
    let root = tempfile::tempdir().unwrap();
    let link = path(&root.path().join("state"));
    let (old, new) = (root.path().join("old"), root.path().join("new"));

    replace(&link, &old).unwrap();
    replace(&link, &new).unwrap();

    assert_eq!(std::fs::read_link(&link).unwrap(), new);
    let left: Vec<_> = std::fs::read_dir(link.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["shoal"]);
}
