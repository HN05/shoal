use super::conflicted_paths;

#[test]
fn only_a_tree_id_with_paths_is_a_conflict_result() {
    let tree = "26b77343eb53870bab71a317fc36bfeb93a4b67b";
    assert_eq!(
        conflicted_paths(&format!("{tree}\0f\0g\0")).unwrap(),
        ["f", "g"]
    );
    for failure in ["", &format!("{tree}\0"), "not a tree\0f\0"] {
        assert_eq!(conflicted_paths(failure), None);
    }
}
