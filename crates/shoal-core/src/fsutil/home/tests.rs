use super::*;
use std::{os::unix::ffi::OsStrExt, path::Path};

#[test]
fn home_expansion_only_replaces_a_leading_tilde_component() {
    let home = Path::new("/home/test");
    for (input, expected) in [
        ("~", "/home/test"),
        ("~/dir", "/home/test/dir"),
        ("~/../dir", "/home/test/../dir"),
        ("~someone/dir", "~someone/dir"),
        ("dir/~", "dir/~"),
        ("/dir", "/dir"),
        ("", ""),
    ] {
        assert_eq!(expand_home(Path::new(input), home), Path::new(expected));
    }
    let path = Path::new(OsStr::from_bytes(b"~/non-utf8-\xff"));
    assert_eq!(
        expand_home(path, home),
        home.join(OsStr::from_bytes(b"non-utf8-\xff"))
    );
}
