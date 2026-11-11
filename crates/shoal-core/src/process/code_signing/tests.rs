use super::*;

/// A signature superblob with a blob of `magic` and `content` bytes in each
/// listed slot.
fn superblob(slots: &[(u32, u32, usize)]) -> Vec<u8> {
    let mut index = Vec::new();
    let mut blobs = Vec::new();
    let start = 12 + 8 * slots.len();
    for &(slot, magic, content) in slots {
        index.extend(slot.to_be_bytes());
        index.extend(u32::try_from(start + blobs.len()).unwrap().to_be_bytes());
        blobs.extend(magic.to_be_bytes());
        blobs.extend(u32::try_from(8 + content).unwrap().to_be_bytes());
        blobs.extend(vec![0x30; content]);
    }
    let mut signature = Vec::new();
    signature.extend(0xfade_0cc0_u32.to_be_bytes());
    signature.extend(u32::try_from(start + blobs.len()).unwrap().to_be_bytes());
    signature.extend(u32::try_from(slots.len()).unwrap().to_be_bytes());
    signature.extend(index);
    signature.extend(blobs);
    signature
}

const CODE_DIRECTORY: (u32, u32, usize) = (0, 0xfade_0c02, 40);
const REQUIREMENTS: (u32, u32, usize) = (2, 0xfade_0c01, 4);

#[test]
fn only_a_cms_signature_counts_as_a_certificate() {
    let linker_signed = superblob(&[CODE_DIRECTORY]);
    assert!(!carries_certificate(&linker_signed));
    let ad_hoc = superblob(&[CODE_DIRECTORY, REQUIREMENTS, (0x1_0000, 0xfade_0b01, 0)]);
    assert!(!carries_certificate(&ad_hoc));
    let certificate = superblob(&[CODE_DIRECTORY, REQUIREMENTS, (0x1_0000, 0xfade_0b01, 900)]);
    assert!(carries_certificate(&certificate));
}

#[test]
fn malformed_signatures_carry_no_certificate() {
    let certificate = superblob(&[(0x1_0000, 0xfade_0b01, 900)]);
    assert!(!carries_certificate(&[]));
    assert!(!carries_certificate(&certificate[..20]));
    let mut wrong_magic = certificate.clone();
    wrong_magic[0] = 0;
    assert!(!carries_certificate(&wrong_magic));
    let mut past_the_end = certificate;
    past_the_end[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(!carries_certificate(&past_the_end));
}

#[test]
fn test_builds_are_not_signed_with_a_certificate() {
    assert!(!own_signature_has_certificate());
}
