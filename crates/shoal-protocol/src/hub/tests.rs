use super::{
    ExchangeKey, HubUrl, MemberKind, Nonce, PublicKey,
    link::{Hello, Intent, Joining, Signer, Transcript, Updates},
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
fn the_installer_line_passes_the_url_and_code_as_one_word_each() {
    let code = PairingCode {
        url: url("https://host/a&b'c"),
        key: PublicKey::try_from([7u8; 32].as_slice())
            .unwrap()
            .fingerprint(),
        code: "ABC".into(),
    };
    let words = shlex::split(&code.installer()).unwrap();
    assert_eq!(words[2], "https://host/a&b'c/install");
    assert_eq!(words.last().unwrap(), &code.to_string());
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

/// A transcript of a join by `key`.
fn joining(key: u8) -> Transcript {
    let bytes = |byte: u8| [byte; 32];
    Transcript {
        hello: Hello {
            version: "1.0.0".into(),
            nonce: Nonce::try_from(bytes(1).as_slice()).unwrap(),
            exchange: ExchangeKey::try_from(bytes(3).as_slice()).unwrap(),
            instance: "desk".into(),
            updates: Updates::Follows,
            intent: Intent::Join(Joining {
                kind: MemberKind::Machine,
                name: "laptop".into(),
                key: PublicKey::try_from(bytes(key).as_slice()).unwrap(),
            }),
        },
        hub_nonce: Nonce::try_from(bytes(2).as_slice()).unwrap(),
        hub_exchange: ExchangeKey::try_from(bytes(4).as_slice()).unwrap(),
    }
}

#[test]
fn each_side_signs_its_own_role_over_the_whole_transcript() {
    let transcript = joining(7);
    assert_ne!(
        transcript.signed(Signer::Hub),
        transcript.signed(Signer::Member)
    );
    let mut other = transcript.clone();
    other.hub_exchange = ExchangeKey::try_from([5u8; 32].as_slice()).unwrap();
    assert_ne!(transcript.signed(Signer::Hub), other.signed(Signer::Hub));
}

#[test]
fn what_a_joiner_signs_names_its_key() {
    assert_ne!(
        joining(7).signed(Signer::Member),
        joining(8).signed(Signer::Member)
    );
}

#[test]
fn a_stream_s_frames_and_call_keep_their_json() {
    use super::{
        link::{Call, Frame},
        routing::{Forwarded, Vouched},
    };
    let frames = [
        Frame::Data {
            stream: 3,
            data: "AQI".into(),
            up: false,
        },
        Frame::Data {
            stream: 3,
            data: "AQI".into(),
            up: true,
        },
        Frame::End {
            stream: 3,
            error: None,
            up: false,
        },
        Frame::End {
            stream: 3,
            error: Some("lost".into()),
            up: true,
        },
        Frame::Credit {
            stream: 3,
            bytes: 131072,
            up: false,
        },
        Frame::Credit {
            stream: 3,
            bytes: 131072,
            up: true,
        },
        Frame::Close {
            stream: 3,
            up: false,
        },
        Frame::Close {
            stream: 3,
            up: true,
        },
        Frame::Call {
            id: 7,
            call: Call::Stream {
                machine: "laptop".into(),
                request: Forwarded {
                    caller: Vouched::User,
                    method: serde_json::json!({"follow_session": {"space": "s"}}),
                    summary: "follow_session for space s".into(),
                },
                stream: 3,
            },
        },
    ];
    let json: Vec<_> = (frames.iter())
        .map(|frame| serde_json::to_string(frame).unwrap())
        .collect();
    assert_eq!(
        json,
        [
            r#"{"data":{"stream":3,"data":"AQI"}}"#,
            r#"{"data":{"stream":3,"data":"AQI","up":true}}"#,
            r#"{"end":{"stream":3}}"#,
            r#"{"end":{"stream":3,"error":"lost","up":true}}"#,
            r#"{"credit":{"stream":3,"bytes":131072}}"#,
            r#"{"credit":{"stream":3,"bytes":131072,"up":true}}"#,
            r#"{"close":{"stream":3}}"#,
            r#"{"close":{"stream":3,"up":true}}"#,
            r#"{"call":{"id":7,"call":{"stream":{"machine":"laptop","request":{"caller":"user","method":{"follow_session":{"space":"s"}},"summary":"follow_session for space s"},"stream":3}}}}"#,
        ]
    );
}

#[test]
fn a_forwarded_path_prefix_starts_and_ends_with_a_slash() {
    use super::app_servers::PathPrefix;
    let quay: PathPrefix = "/quay/".parse().unwrap();
    assert!(quay.covers("/quay/board"));
    assert!(!quay.covers("/quay"));
    for refused in ["quay/", "/quay", "/a b/", "/a?b/"] {
        assert!(refused.parse::<PathPrefix>().is_err(), "{refused}");
    }
}
