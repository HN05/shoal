use super::*;

#[test]
fn available_bytes_reads_the_filesystem_or_fails() {
    let root = tempfile::tempdir().unwrap();
    assert!(available_bytes(root.path()).unwrap() > 0);
    assert!(available_bytes(&root.path().join("missing")).is_err());
}
