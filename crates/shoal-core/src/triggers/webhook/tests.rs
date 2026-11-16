use serde_json::json;
use shoal_protocol::triggers::WebhookRequest;

use super::{Signature, input};
use crate::{
    config::repo,
    time::UnixTime,
    triggers::{Source, Trigger},
};

fn request(headers: &[(&str, &str)], body: &str) -> WebhookRequest {
    WebhookRequest {
        headers: (headers.iter())
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        query: vec![("page".into(), "2".into())],
        body: body.to_owned(),
    }
}

#[test]
fn a_json_request_is_its_payload_beside_its_text_headers_and_query() {
    let body = r#"{"action":"labeled","issue":{"number":42}}"#;
    let request = request(
        &[
            ("Content-Type", "application/json"),
            ("X-Forgejo-Event", "issues"),
            ("Accept", "a"),
            ("accept", "b"),
        ],
        body,
    );

    let input = input("triage", &request, UnixTime::now());

    let document = &input.document;
    assert_eq!(document["payload"]["issue"]["number"], 42);
    assert_eq!(document["body"], body);
    assert_eq!(document["headers"]["x-forgejo-event"], "issues");
    assert_eq!(document["headers"]["accept"], "a, b");
    assert_eq!(document["query"], json!({ "page": "2" }));
    assert_eq!(document["trigger"], "triage");
    assert!(document["received_at"].is_string());
    assert!(input.default_key.is_none());
    // A task marks what the request sent, never the trigger's own name.
    let path = |text: &str| text.parse().unwrap();
    assert!(input.request_read_by(&path("payload.issue")).is_some());
    assert!(input.request_read_by(&path("headers")).is_some());
    assert!(input.request_read_by(&path("trigger")).is_none());
}

#[test]
fn a_form_is_its_fields_and_other_text_has_no_payload() {
    let form = request(
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "payload=%7B%22a%22%3A1%7D&name=a+b",
    );
    let document = input("t", &form, UnixTime::now()).document;
    assert_eq!(
        document["payload"],
        json!({ "payload": "{\"a\":1}", "name": "a b" })
    );

    let text = input("t", &request(&[], "not json"), UnixTime::now()).document;
    assert!(text.get("payload").is_none());
    assert_eq!(text["body"], "not json");
}

#[test]
fn a_delivery_header_is_the_default_key() {
    for header in [
        "X-Forgejo-Delivery",
        "X-Gitea-Delivery",
        "X-GitHub-Delivery",
    ] {
        let input = input("t", &request(&[(header, "d-1")], "{}"), UnixTime::now());
        assert_eq!(input.default_key.as_deref(), Some("d-1"), "{header}");
    }
}

fn trigger(text: &str) -> anyhow::Result<Trigger> {
    let config = repo::parse(text)?;
    Ok(config.triggers.into_values().next().unwrap())
}

#[test]
fn a_webhook_reads_its_signature_and_writes_what_it_reads() {
    let signed = trigger(
        "[triggers.a]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'hmac-sha256'\n\
         signature_header = 'X-Signature'\ntask = 'Fix {{ payload.issue.number }}'",
    )
    .unwrap();
    let Source::Webhook(webhook) = &signed.source else {
        panic!("{signed:?}");
    };
    let signed_by = webhook.signed.as_ref().unwrap();
    assert_eq!(signed_by.secret, "HOOK");
    assert_eq!(signed_by.signature, Signature::Header("x-signature".into()));

    for trigger in [
        signed,
        trigger("[triggers.a]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'forgejo'").unwrap(),
        trigger("[triggers.a]\non = 'webhook'").unwrap(),
    ] {
        let written = toml::to_string(&trigger).unwrap();
        assert_eq!(toml::from_str::<Trigger>(&written).unwrap(), trigger);
    }
}

#[test]
fn reading_refuses_a_signature_no_request_could_carry() {
    for text in [
        // A secret's signature says where requests carry it.
        "[triggers.a]\non = 'webhook'\nsecret = 'HOOK'",
        "[triggers.a]\non = 'webhook'\nsignature = 'github'",
        "[triggers.a]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'hmac-sha256'",
        "[triggers.a]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'github'\nsignature_header = 'X-S'",
        "[triggers.a]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'hmac-sha256'\nsignature_header = 'X S'",
        // Each source takes only its own keys.
        "[triggers.a]\non = 'webhook'\nevery = '1h'",
        "[triggers.a]\non = 'timer'\nevery = '1h'\nsecret = 'HOOK'\nsignature = 'github'",
    ] {
        assert!(trigger(text).is_err(), "{text}");
    }
}
