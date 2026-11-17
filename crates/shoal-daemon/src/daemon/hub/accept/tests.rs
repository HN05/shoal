use super::newer;

#[test]
fn a_release_is_newer_by_its_dotted_numbers() {
    assert!(newer("0.19.0", "0.18.1"));
    assert!(newer("0.18.10", "0.18.9"));
    assert!(!newer("0.18.1", "0.18.1"));
    assert!(!newer("0.9.0", "0.18.1"));
    assert!(!newer("of another release", "0.18.1"));
}
