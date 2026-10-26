use super::*;

fn settings(enabled: bool, allowed_types: &str, max_size: u64) -> Settings {
    Settings {
        enabled,
        allowed_types: allowed_types.to_owned(),
        max_size,
    }
}

#[test]
fn settings_refuse_disabled_uploads_other_types_and_large_files() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, head: &[u8], len: u64| {
        let path = dir.path().join(name);
        std::fs::write(&path, head).unwrap();
        std::fs::File::options()
            .append(true)
            .open(&path)
            .unwrap()
            .set_len(len)
            .unwrap();
        Attachment { path, alt: None }
    };
    let png = write("a.PNG", b"\x89PNG\x0D\x0A\x1A\x0A", 10);
    let svg = write("b.svg", b"<svg/>", 10);
    let video = write(
        "c.mp4",
        b"\x00\x00\x00\x10ftypmp42\x00\x00\x00\x00",
        2 * 1024 * 1024,
    );
    // Forgejo detects the type in the file, not from its name.
    let ledger = write("ledger.tsv", b"a\tb\n", 4);
    let notes = write("NOTES", b"text", 4);
    let named = write("d.png", b"text", 4);
    let error = |settings: Settings, attachment: &Attachment| {
        format!("{:#}", settings.check(attachment).unwrap_err())
    };
    for allowed in ["", "*/*", ".png", "image/png", "image/*", ".txt|image/*"] {
        assert!(settings(true, allowed, 1).check(&png).is_ok(), "{allowed}");
    }
    assert!(error(settings(false, "", 1), &png).contains("does not accept"));
    assert!(error(settings(true, ".png,.mp4", 1), &svg).contains("only these"));
    assert!(error(settings(true, "image/", 1), &png).contains("only these"));
    assert!(error(settings(true, "", 1), &video).contains("at most 1 MiB"));
    assert!(settings(true, "video/*", 2).check(&video).is_ok());
    assert!(settings(true, "text/plain", 1).check(&ledger).is_ok());
    assert!(settings(true, "text/*", 1).check(&notes).is_ok());
    assert!(error(settings(true, "image/*", 1), &named).contains("only these"));
    assert!(settings(true, ".png", 1).check(&named).is_ok());
}
