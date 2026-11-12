use super::*;

#[test]
fn pages_end_at_a_short_page() {
    let full = serde_json::json!([1, 2]);
    assert!(Forgejo.page(&full, 2).unwrap().1);
    assert!(!Forgejo.page(&full, 3).unwrap().1);
}

#[test]
fn a_limit_without_a_page_reads_the_first_page() {
    for (endpoint, read) in [
        (
            "repos/t/r/actions/runs?limit=8",
            "repos/t/r/actions/runs?limit=8&page=1",
        ),
        (
            "repos/t/r/actions/runs?status=success&limit=8#x",
            "repos/t/r/actions/runs?status=success&limit=8&page=1#x",
        ),
        (
            "repos/t/r/actions/runs?limit=8&page=3",
            "repos/t/r/actions/runs?limit=8&page=3",
        ),
        ("repos/t/r/actions/runs", "repos/t/r/actions/runs"),
        (
            "repos/t/r/branches?unlimited=1",
            "repos/t/r/branches?unlimited=1",
        ),
    ] {
        assert_eq!(Forgejo.raw_read(endpoint.into()), read);
    }
}
