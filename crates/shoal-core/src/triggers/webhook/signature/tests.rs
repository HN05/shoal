use shoal_protocol::triggers::WebhookRequest;

use super::Signature;

// GitHub's example of a webhook signature.
const SECRET: &[u8] = b"It's a Secret to Everybody";
const BODY: &[u8] = b"Hello, World!";
const HEX: &str = "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";

fn signed(header: &str, value: &str) -> WebhookRequest {
    WebhookRequest {
        headers: vec![(header.to_owned(), value.to_owned())],
        ..WebhookRequest::default()
    }
}

#[test]
fn each_scheme_reads_the_hmac_of_the_body_from_its_header() {
    let github = format!("sha256={HEX}");
    for (signature, request) in [
        (Signature::Forgejo, signed("X-Forgejo-Signature", HEX)),
        (Signature::Github, signed("X-Hub-Signature-256", &github)),
        (
            Signature::Header("x-signature".into()),
            signed("X-Signature", HEX),
        ),
        (
            Signature::Header("x-signature".into()),
            signed("X-Signature", &github),
        ),
    ] {
        assert!(signature.verifies(SECRET, &request, BODY), "{signature:?}");
        assert!(
            !signature.verifies(b"another secret", &request, BODY),
            "{signature:?}"
        );
        assert!(
            !signature.verifies(SECRET, &request, b"Hello, World?"),
            "{signature:?}"
        );
    }
}

#[test]
fn a_missing_or_malformed_signature_fails() {
    let github = Signature::Github;
    // GitHub's header names its algorithm.
    assert!(!github.verifies(SECRET, &signed("X-Hub-Signature-256", HEX), BODY));
    assert!(!github.verifies(SECRET, &WebhookRequest::default(), BODY));
    let forgejo = Signature::Forgejo;
    assert!(!forgejo.verifies(SECRET, &signed("X-Forgejo-Signature", &HEX[1..]), BODY));
    let plus = format!("+{}", &HEX[2..]);
    assert!(!forgejo.verifies(SECRET, &signed("X-Forgejo-Signature", &plus), BODY));
    // Another scheme's header is not this one's.
    assert!(!forgejo.verifies(SECRET, &signed("X-Gitea-Signature", HEX), BODY));
}
