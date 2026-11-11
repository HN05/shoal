use std::{ffi::OsString, path::Path};

use super::*;

#[test]
fn the_claude_app_link_encodes_the_folder_and_takes_no_arguments() {
    assert_eq!(
        percent_encode("/a b/ä+%_.-~".as_bytes()),
        "%2Fa%20b%2F%C3%A4%2B%25_.-~"
    );
    assert_eq!(
        new_session(Path::new("/work/x y"), Vec::new()),
        [OsString::from("claude://code/new?folder=%2Fwork%2Fx%20y")]
    );
    assert_eq!(
        macos_without_arguments(&[]).is_ok(),
        cfg!(target_os = "macos")
    );
    assert!(macos_without_arguments(&["--model".into()]).is_err());
}
