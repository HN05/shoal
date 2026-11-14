use super::*;

fn attachment(path: &str, alt: Option<&str>) -> Attachment {
    Attachment {
        path: PathBuf::from(path),
        alt: alt.map(str::to_owned),
    }
}

#[test]
fn arguments_split_the_alt_text_at_the_first_hash() {
    let cwd = Path::new("/work");
    assert_eq!(
        Attachment::parse("shots/a.png#The login error", cwd),
        attachment("/work/shots/a.png", Some("The login error"))
    );
    assert_eq!(
        Attachment::parse("/tmp/a.png#", cwd),
        attachment("/tmp/a.png", None)
    );
}

#[test]
fn checks_refuse_empty_and_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, len: u64| {
        let path = dir.path().join(name);
        std::fs::File::create(&path).unwrap().set_len(len).unwrap();
        Attachment { path, alt: None }
    };
    assert!(write("a.PNG", 10).check().is_ok());
    assert!(write("b.mp4", MAX_IMAGE_BYTES + 1).check().is_ok());
    assert_eq!(write("b.mp4", 1).media(), Media::Video);
    let ledger = write("c.tsv", MAX_IMAGE_BYTES + 1);
    assert!(ledger.check().is_ok());
    assert_eq!(ledger.media(), Media::File);
    assert_eq!(write("c.unknown", 1).content_type(), OTHER_TYPE);
    assert!(write("d.png", 0).check().is_err());
    assert!(write("e.png", MAX_IMAGE_BYTES + 1).check().is_err());
    assert!(write("f.webm", MAX_FILE_BYTES + 1).check().is_err());
    assert!(attachment("relative.png", None).check().is_err());
}
