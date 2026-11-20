use shoal_forge_types::raw::{HttpMethod, RawRequest};

use super::*;

#[test]
fn a_long_request_names_its_start_and_a_digest_of_all_of_it() {
    let write = |body: &str| RawRequest {
        method: HttpMethod::Patch,
        path: "repos/{owner}/{repo}/issues/3".into(),
        body: Some(serde_json::json!({ "body": body.repeat(100) })),
    };
    let named = |body| Action::request(PermissionKey::ApiWrites, &write(body), None).action;
    let (first, other) = (named("a"), named("b"));
    assert!(
        first.starts_with(r#"{"method":"PATCH","path":"repos/{owner}/{repo}/issues/3""#),
        "{first}"
    );
    assert_ne!(first, other);
}
