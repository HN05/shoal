use super::{
    HubUrl, Nonce, PublicKey,
    link::{Signer, signed},
    pairing::PairingCode,
};

fn url(text: &str) -> HubUrl {
    text.parse().unwrap()
}

#[test]
fn a_hub_url_takes_http_or_https_with_a_host() {
    assert_eq!(
        url("https://hub.example.com/").as_str(),
        "https://hub.example.com"
    );
    for text in [
        "ftp://hub.example.com",
        "hub.example.com",
        "https://",
        "https://hub.example.com/?a=1",
        "https://hub.example.com/#x",
    ] {
        assert!(text.parse::<HubUrl>().is_err(), "{text}");
    }
}

#[test]
fn the_link_takes_the_websocket_scheme_of_the_url() {
    assert_eq!(
        url("https://hub.example.com/shoal").link(),
        "wss://hub.example.com/shoal/hub"
    );
    assert_eq!(
        url("http://127.0.0.1:7474").link(),
        "ws://127.0.0.1:7474/hub"
    );
}

#[test]
fn only_http_to_another_machine_is_unencrypted_remote() {
    assert!(url("http://desk.tailnet.ts.net:7474").unencrypted_remote());
    assert!(url("http://192.168.1.4").unencrypted_remote());
    assert!(!url("https://desk.tailnet.ts.net").unencrypted_remote());
    assert!(!url("http://127.0.0.1:7474").unencrypted_remote());
    assert!(!url("http://localhost:7474/x").unencrypted_remote());
    assert!(!url("http://[::1]:7474").unencrypted_remote());
}

#[test]
fn a_pairing_code_reads_back_what_it_writes() {
    let code = PairingCode {
        url: url("https://hub.example.com/shoal"),
        key: PublicKey::try_from([7u8; 32].as_slice())
            .unwrap()
            .fingerprint(),
        code: "Ab12".into(),
    };
    let text = code.to_string();
    assert!(text.starts_with("https://hub.example.com/shoal/pair#code=Ab12&key="));
    assert_eq!(text.parse::<PairingCode>().unwrap(), code);
}

#[test]
fn a_pairing_code_needs_its_code_and_key() {
    let key = PublicKey::try_from([7u8; 32].as_slice())
        .unwrap()
        .fingerprint();
    for text in [
        format!("https://hub.example.com/pair#key={key}"),
        format!("https://hub.example.com/pair#code=&key={key}"),
        "https://hub.example.com/pair#code=Ab12".to_owned(),
        format!("https://hub.example.com/#code=Ab12&key={key}"),
        format!("https://hub.example.com/pair#code=A%20b&key={key}"),
    ] {
        assert!(text.parse::<PairingCode>().is_err(), "{text}");
    }
}

#[test]
fn each_side_signs_its_own_role() {
    let first = Nonce::try_from([1u8; 32].as_slice()).unwrap();
    let second = Nonce::try_from([2u8; 32].as_slice()).unwrap();
    assert_ne!(
        signed(Signer::Hub, &first, &second),
        signed(Signer::Member, &first, &second)
    );
    assert_ne!(
        signed(Signer::Hub, &first, &second),
        signed(Signer::Hub, &second, &first)
    );
}
