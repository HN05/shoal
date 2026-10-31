use super::*;

#[test]
fn curl_output_splits_into_the_file_and_its_status() {
    let (body, status) = split_status(b"\x89PNG\n\0\xff\n200".to_vec()).unwrap();
    assert_eq!(body, b"\x89PNG\n\0\xff");
    assert_eq!(status, 200);
    let (body, status) = split_status(b"\n404".to_vec()).unwrap();
    assert!(body.is_empty());
    assert_eq!(status, 404);
    assert!(split_status(b"no status".to_vec()).is_err());
}
