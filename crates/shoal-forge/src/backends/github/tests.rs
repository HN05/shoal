use super::*;

#[test]
fn graphql_errors_fail_a_successful_response() {
    let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
    assert_eq!(
        graphql_errors(&failed).unwrap(),
        "not allowed; GraphQL error"
    );
    assert_eq!(
        graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
        None
    );
    assert_eq!(graphql_errors(&serde_json::json!([])), None);
}
