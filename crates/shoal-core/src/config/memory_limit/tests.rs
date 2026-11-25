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

#[test]
fn a_left_out_limit_follows_load_where_the_kernel_enforces_it() {
    let percent = |load| {
        let effect = MemoryLimitSetting::Load.where_enforced(load, true);
        assert_eq!(effect.source, MemoryLimitSource::Load);
        effect.limit
    };
    assert_eq!(percent(Load::Light), Some(MemoryLimit::Percent(60)));
    assert_eq!(percent(Load::Balanced), Some(MemoryLimit::Percent(75)));
    assert_eq!(percent(Load::Full), Some(MemoryLimit::Percent(85)));
    let unenforced = MemoryLimitSetting::Load.where_enforced(Load::Full, false);
    assert_eq!(unenforced.limit, None);
}

#[test]
fn a_written_limit_or_none_overrides_load_wherever_it_runs() {
    let written = MemoryLimitSetting::Limit(MemoryLimit::Bytes(8 << 30));
    let off = MemoryLimitSetting::Off;
    for enforced in [true, false] {
        let effect = written.where_enforced(Load::Full, enforced);
        assert_eq!(effect.limit, Some(MemoryLimit::Bytes(8 << 30)));
        assert_eq!(effect.source, MemoryLimitSource::Config);
        let effect = off.where_enforced(Load::Full, enforced);
        assert_eq!(effect.limit, None);
        assert_eq!(effect.source, MemoryLimitSource::Config);
    }
}

#[test]
fn the_machine_reads_none_a_limit_or_nothing() {
    let read = |text: &str| {
        crate::test_support::global_config(text)
            .machine
            .memory_limit
    };
    assert_eq!(read(""), MemoryLimitSetting::Load);
    assert_eq!(
        read("[machine]\nmemory_limit = \"none\"\n"),
        MemoryLimitSetting::Off
    );
    assert_eq!(
        read("[machine]\nmemory_limit = \"75%\"\n"),
        MemoryLimitSetting::Limit(MemoryLimit::Percent(75))
    );
}
