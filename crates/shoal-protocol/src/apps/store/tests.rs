use super::{AppData, ContentType, Entry, EntryKey, EntryPiece, KeyPrefix, Revision, Stored};
use crate::time::UnixTime;

#[test]
fn an_entry_key_is_slash_separated_segments_none_empty_or_relative() {
    for key in ["layout/main", "exports/2026-11.png", "a b/c"] {
        assert!(key.parse::<EntryKey>().is_ok(), "{key}");
    }
    for key in ["", "/a", "a/", "a//b", "a/./b", "../a", "a\tb"] {
        assert!(key.parse::<EntryKey>().is_err(), "{key:?}");
    }
    assert!("a".repeat(EntryKey::MAX + 1).parse::<EntryKey>().is_err());
}

#[test]
fn a_prefix_covers_the_keys_that_start_with_it() {
    let exports: KeyPrefix = "exports/".parse().unwrap();
    assert!(exports.covers(&"exports/a.png".parse().unwrap()));
    assert!(!exports.covers(&"exports".parse().unwrap()));
    assert!(KeyPrefix::default().covers(&"layout".parse().unwrap()));
}

#[test]
fn a_content_type_is_printable_ascii() {
    assert!("text/plain; charset=utf-8".parse::<ContentType>().is_ok());
    assert!("".parse::<ContentType>().is_err());
    assert!("text/plain\n".parse::<ContentType>().is_err());
    assert!("tëxt/plain".parse::<ContentType>().is_err());
}

fn stored(bytes: Vec<u8>) -> Stored {
    Stored {
        entry: Entry {
            key: "exports/big.bin".parse().unwrap(),
            content_type: "application/octet-stream".parse().unwrap(),
            revision: Revision(7),
            size: bytes.len() as u64,
            updated_at: UnixTime::from_seconds(1_800_000_000),
        },
        bytes,
    }
}

#[test]
fn an_entry_larger_than_a_frame_comes_back_whole_from_its_pieces() {
    let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let pieces = EntryPiece::split(stored(bytes.clone()));
    assert!(pieces.len() > 2);
    let joined = EntryPiece::join(pieces).unwrap().unwrap();
    assert_eq!(joined, stored(bytes));
    assert_eq!(EntryPiece::join(Vec::new()).unwrap(), None);
}

#[test]
fn pieces_that_hold_less_than_their_entry_are_refused() {
    let mut pieces = EntryPiece::split(stored(vec![1; 10]));
    pieces.pop();
    assert!(EntryPiece::join(pieces).is_err());
}

#[test]
fn a_write_s_bytes_come_back_from_their_pieces() {
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
    let pieces: Vec<AppData> = AppData::split(&bytes).collect();
    let joined: Vec<u8> = (pieces.iter())
        .flat_map(|piece| piece.bytes().unwrap())
        .collect();
    assert_eq!(joined, bytes);
}
