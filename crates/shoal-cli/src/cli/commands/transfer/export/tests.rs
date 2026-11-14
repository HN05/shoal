use super::*;

#[test]
fn a_checkpoint_copies_only_one_space() {
    copy::ensure_one_at_checkpoint(1, Some(3.into())).unwrap();
    copy::ensure_one_at_checkpoint(2, None).unwrap();
    assert!(copy::ensure_one_at_checkpoint(2, Some(3.into())).is_err());
}
