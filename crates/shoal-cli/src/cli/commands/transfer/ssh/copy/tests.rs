use super::*;

#[test]
fn a_copy_under_another_name_is_named() {
    let renamed = copied_message("talk", "talk-2", "other");
    assert!(renamed.contains("as talk-2"), "{renamed}");
    let same = copied_message("talk", "talk", "other");
    assert!(!same.contains(" as "), "{same}");
}
