use super::editor;
use std::ffi::OsString;

#[test]
fn editor_prefers_visual_splits_on_whitespace_and_requires_a_value() {
    assert_eq!(
        editor([Some(" code  --wait ".into()), Some("vi".into())]).unwrap(),
        [OsString::from("code"), OsString::from("--wait")]
    );
    for visual in [None, Some(" ".into())] {
        assert_eq!(editor([visual, Some("vi".into())]).unwrap(), ["vi"]);
    }
    for missing in [None, Some(OsString::new()), Some(" ".into())] {
        assert!(editor([missing.clone(), missing]).is_err());
    }
}
