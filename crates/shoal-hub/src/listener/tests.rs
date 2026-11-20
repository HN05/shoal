use serde_json::json;

use super::opening;

#[test]
fn an_opening_names_its_protocol_and_hello_whatever_else_it_holds() {
    let request =
        json!({"protocol": 7, "id": 1, "method": {"hub_link": {"anything": true}}, "new": 1});
    let (protocol, hello) = opening(request).unwrap();
    assert_eq!(protocol, 7);
    assert_eq!(hello, json!({"anything": true}));
}

#[test]
fn a_request_that_opens_no_link_is_refused() {
    assert!(opening(json!({"protocol": 7, "method": "status"})).is_err());
    assert!(opening(json!({"method": {"hub_link": {}}})).is_err());
}
