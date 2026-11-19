use shoal_protocol::hub::{
    Nonce,
    link::{Hello, Intent, Signer, Transcript},
};

use super::{Exchange, Keys};

/// Each side's keys of one handshake.
fn linked() -> (Keys, Keys) {
    let (member, hub) = (Exchange::new().unwrap(), Exchange::new().unwrap());
    let transcript = Transcript {
        hello: Hello {
            version: "1.0.0".into(),
            nonce: Nonce::try_from([1u8; 32].as_slice()).unwrap(),
            exchange: member.public.clone(),
            instance: "desk".into(),
            intent: Intent::Connect {
                member: "m1".to_owned().into(),
            },
        },
        hub_nonce: Nonce::try_from([2u8; 32].as_slice()).unwrap(),
        hub_exchange: hub.public.clone(),
    };
    (
        member.keys(&transcript, Signer::Member).unwrap(),
        hub.keys(&transcript, Signer::Hub).unwrap(),
    )
}

#[test]
fn each_side_opens_what_the_other_sealed_in_order() {
    let (mut member, mut hub) = linked();
    for frame in ["\"ping\"", "\"pong\""] {
        let sealed = member.sending.seal(frame.into());
        assert_eq!(hub.receiving.open(sealed).unwrap(), frame);
    }
    let sealed = hub.sending.seal("\"pong\"".into());
    assert_eq!(member.receiving.open(sealed).unwrap(), "\"pong\"");
}

#[test]
fn a_sealed_frame_hides_its_text_and_a_changed_one_does_not_open() {
    let (mut member, mut hub) = linked();
    let mut sealed = member.sending.seal("{\"code\":\"ABCDEFGH\"}".into());
    assert!(!sealed.data.contains("ABCDEFGH"));
    let mut data = sealed.data.into_bytes();
    data[0] = if data[0] == b'A' { b'B' } else { b'A' };
    sealed.data = String::from_utf8(data).unwrap();
    assert!(hub.receiving.open(sealed).is_err());
}

#[test]
fn a_replayed_or_skipped_frame_does_not_open() {
    let (mut member, mut hub) = linked();
    let first = member.sending.seal("\"ping\"".into());
    hub.receiving.open(first.clone()).unwrap();
    assert!(hub.receiving.open(first).is_err());
    member.sending.seal("\"ping\"".into());
    let third = member.sending.seal("\"ping\"".into());
    assert!(hub.receiving.open(third).is_err());
}

#[test]
fn a_frame_sealed_without_the_link_key_does_not_open() {
    let (_, mut hub) = linked();
    let (mut stranger, _) = linked();
    let forged = stranger.sending.seal("\"ping\"".into());
    assert!(hub.receiving.open(forged).is_err());
}
