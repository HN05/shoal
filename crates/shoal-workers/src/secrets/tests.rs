use super::*;

#[test]
fn scoped_runs_keep_their_token_to_fetch_secrets() {
    assert!(!must_authorize("tool", false, false).unwrap());
    assert!(must_authorize("tool", true, true).unwrap());
    assert!(must_authorize("tool", true, false).is_err());
}

#[test]
fn launchers_search_only_later_path_entries() {
    let path = OsStr::new("/inner:/outer:/bin");
    assert_eq!(
        path_after(path, Path::new("/inner")).unwrap(),
        "/outer:/bin"
    );
    assert_eq!(path_after(path, Path::new("/outer")).unwrap(), "/bin");
    assert_eq!(
        path_after(path, Path::new("/elsewhere")).unwrap(),
        "/inner:/outer:/bin"
    );
}
