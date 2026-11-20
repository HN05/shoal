use super::*;

#[test]
fn a_limit_is_a_size_or_a_share_of_ram() {
    assert_eq!(
        MemoryLimit::parse("24GiB"),
        Ok(MemoryLimit::Bytes(24 << 30))
    );
    assert_eq!(
        MemoryLimit::parse("512 MiB"),
        Ok(MemoryLimit::Bytes(512 << 20))
    );
    assert_eq!(MemoryLimit::parse("75%"), Ok(MemoryLimit::Percent(75)));
    for invalid in ["24", "24GB", "1.5GiB", "GiB", "0GiB", "0%", "101%", "-1GiB"] {
        assert!(MemoryLimit::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn a_share_of_ram_resolves_against_the_machines_ram() {
    assert_eq!(MemoryLimit::Percent(75).bytes(32 << 30), 24 << 30);
    assert_eq!(MemoryLimit::Bytes(8 << 30).bytes(32 << 30), 8 << 30);
}

#[test]
fn a_limit_writes_back_as_it_reads() {
    for written in ["24GiB", "1536MiB", "75%", "1000B"] {
        let limit = MemoryLimit::parse(written).unwrap();
        assert_eq!(limit.to_string(), written);
    }
}
