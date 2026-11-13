use super::*;

#[test]
fn each_hook_is_read_at_the_key_it_names() {
    for &(kind, field) in FIELD_KEYS {
        assert_eq!(kind.key(), field.replace(' ', ""));
    }
}
