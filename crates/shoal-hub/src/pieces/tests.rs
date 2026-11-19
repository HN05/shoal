use shoal_protocol::hub::link::{Call, Reply};

use super::*;

#[test]
fn a_message_larger_than_a_piece_arrives_whole_from_its_pieces() {
    let message = "é".repeat(5_000);
    let frame = Frame::Reply {
        id: 7,
        reply: Reply::Failed { message },
    };
    let pieces = split(&frame, 1_000).unwrap();
    assert!(pieces.len() > 1);
    let mut assembly = Assembly::default();
    let mut whole = None;
    for piece in pieces {
        let Frame::Piece { id, data, more } = piece else {
            panic!("expected a piece");
        };
        assert!(data.len() <= 1_000);
        whole = assembly.take(id, data, more).unwrap();
    }
    let Some(Frame::Reply { id: 7, reply }) = whole else {
        panic!("expected the reply");
    };
    assert!(matches!(reply, Reply::Failed { message } if message.chars().count() == 5_000));
}

#[test]
fn a_message_that_fits_goes_as_it_is() {
    let frame = Frame::Call {
        id: 1,
        call: Call::Members,
    };
    assert!(matches!(
        split(&frame, 1_000).unwrap().as_slice(),
        [Frame::Call { id: 1, .. }]
    ));
}
