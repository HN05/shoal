use super::*;

#[test]
fn changed_file_preview_bounds_entries_and_encoded_bytes() {
    let status = "?? file\n".repeat(5000);
    let (files, omitted) = changed_files_preview(&status);
    assert_eq!(files.len(), 50);
    assert_eq!(omitted, 4950);

    let status = format!("?? {}\n", "\\".repeat(1000)).repeat(50);
    let (files, omitted) = changed_files_preview(&status);
    assert!(!files.is_empty());
    assert!(files.len() < 50);
    assert_eq!(files.len() + omitted, 50);
    assert!(serde_json::to_vec(&files).unwrap().len() <= 16 * 1024 + 1);

    let (files, omitted) = changed_files_preview(&format!("?? {}\n", "x".repeat(20_000)));
    assert!(files.is_empty());
    assert_eq!(omitted, 1);
}
