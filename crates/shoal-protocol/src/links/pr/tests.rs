use serde_json::json;

use super::*;

#[test]
fn registrations_round_trip() {
    let url = "https://forge.example/team/repo/pulls/7";
    for (kind, record) in [
        (
            RegistrationKind::Watch {
                urls: vec![url.parse().unwrap()],
                merged_head: None,
            },
            json!({"urls": [url], "head": null}),
        ),
        (
            RegistrationKind::Acknowledgement {
                head: "abc123".into(),
            },
            json!({"head": "abc123"}),
        ),
    ] {
        for error in [None, Some("lookup failed")] {
            let mut record = record.clone();
            record["error"] = json!(error);
            let registration: Registration = serde_json::from_value(record.clone()).unwrap();
            assert_eq!(registration.kind, kind);
            assert_eq!(registration.error.as_deref(), error);
            assert_eq!(serde_json::to_value(&registration).unwrap(), record);
        }
    }
}

#[test]
fn multiple_watches_round_trip_and_reject_ambiguous_or_empty_sets() {
    let (first, second) = (
        "https://github.com/team/repo/pull/1",
        "https://github.com/team/repo/pull/2",
    );
    let value = json!({"urls": [first, second], "head": null,
            "merged_head": "commit", "error": null});
    let registration: Registration = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(registration).unwrap(), value);
    for value in [
        json!({"urls": []}),
        json!({"urls": [first, first]}),
        json!({"urls": [""]}),
        json!({"urls": [first], "head": "commit"}),
        json!({"head": "commit", "merged_head": "commit"}),
    ] {
        assert!(serde_json::from_value::<Registration>(value).is_err());
    }
}

#[test]
fn registration_rejects_ambiguous_records() {
    for record in [json!({}), json!({"head": null}), json!({"url": "url"})] {
        assert!(serde_json::from_value::<Registration>(record).is_err());
    }
}
