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
    let error = |attachment: Attachment| format!("{:#}", attachment.check().unwrap_err());
    assert!(write("a.PNG", 10).check().is_ok());
    assert!(write("b.mp4", MAX_IMAGE_BYTES + 1).check().is_ok());
    assert_eq!(write("b.mp4", 1).media(), Media::Video);
    let ledger = write("c.tsv", MAX_IMAGE_BYTES + 1);
    assert!(ledger.check().is_ok());
    assert_eq!(ledger.media(), Media::File);
    assert_eq!(write("c.unknown", 1).content_type(), OTHER_TYPE);
    assert!(error(write("d.png", 0)).contains("is empty"));
    assert!(error(write("e.png", MAX_IMAGE_BYTES + 1)).contains("at most 10 MiB"));
    assert!(error(write("f.webm", MAX_FILE_BYTES + 1)).contains("at most 100 MiB"));
    assert!(error(write("f.zip", MAX_FILE_BYTES + 1)).contains("files must be"));
    assert!(error(attachment("relative.png", None)).contains("absolute"));
    let twice = [write("g.png", 1), write("g.png", 1)];
    assert!(format!("{:#}", check_all(&twice).unwrap_err()).contains("twice"));
}

#[test]
fn references_resolve_against_the_first_directory_that_has_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    std::fs::create_dir(&notes).unwrap();
    std::fs::write(notes.join("a.png"), "x").unwrap();
    std::fs::write(dir.path().join("b.mp4"), "x").unwrap();
    let attachments = [
        Attachment::parse("notes/a.png", dir.path()),
        Attachment::parse("b.mp4", dir.path()),
    ];
    let body = "See ![login](./a.png) and ![](b.mp4 \"demo\"), not ![c](c.png).";
    let resolved = resolve_references(body, &attachments, &[&notes, dir.path()]);
    assert_eq!(
        resolved,
        format!(
            "See ![login](<{}>) and ![](<{}>), not ![c](c.png).",
            attachments[0].path.display(),
            attachments[1].path.display()
        )
    );
}

#[test]
fn embedding_replaces_references_and_appends_the_rest() {
    let attachments = [
        attachment("/t/a.png", Some("given")),
        attachment("/t/b.png", None),
        attachment("/t/c.mp4", None),
    ];
    let body = "Before ![shown](</t/a.png>) and ![](/t/b.png).\n";
    let embedded = embed(body, &attachments, |index, alt| {
        format!("[{alt}|{}]", attachments[index].name())
    });
    assert_eq!(
        embedded,
        "Before [shown|a.png] and [b.png|b.png].\n\n[c.mp4|c.mp4]"
    );
    let only = embed("", &attachments[..1], |_, alt| alt.to_owned());
    assert_eq!(only, "given");
}

#[test]
fn forges_keep_uploads_per_repository_item_or_comment() {
    let png = attachment("/t/a b.png", None);
    let video = attachment("/t/c.mp4", None);
    let ledger = attachment("/t/ledger.tsv", None);
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
    let request = gitlab
        .kind
        .upload(&gitlab, Holder::Repository, &png, None)
        .unwrap();
    assert_eq!(request.endpoint, "projects/group%2Frepo/uploads");
    assert_eq!(request.upload.unwrap().field, Some("file"));
    assert_eq!(
        gitlab.kind.markup(&video, "c", "/uploads/x/c.mp4"),
        "![c](/uploads/x/c.mp4)"
    );
    assert_eq!(
        gitlab
            .kind
            .markup(&ledger, "ledger.tsv", "/uploads/x/ledger.tsv"),
        "[ledger.tsv](/uploads/x/ledger.tsv)"
    );

    let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    assert!(
        github
            .kind
            .upload(&github, Holder::Repository, &png, None)
            .is_err()
    );
    let request = github
        .kind
        .upload(&github, Holder::Repository, &png, Some(42))
        .unwrap();
    assert_eq!(
        request.endpoint,
        "https://uploads.github.com/user-attachments/assets?name=a%20b.png&content_type=image%2Fpng&repository_id=42"
    );
    assert_eq!(request.upload.unwrap().field, None);
    assert_eq!(github.kind.markup(&png, "a [b]", "u"), "![a b](u)");
    assert_eq!(github.kind.markup(&ledger, "a [b]", "u"), "[a b](u)");

    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    assert!(
        forgejo
            .kind
            .upload(&forgejo, Holder::Repository, &png, None)
            .is_err()
    );
    assert_eq!(forgejo.kind.markup(&ledger, "ledger", "u"), "[ledger](u)");
}

#[tokio::test]
async fn uploads_are_checked_where_the_forge_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.png");
    std::fs::write(&path, "png").unwrap();
    let files = [Attachment { path, alt: None }];
    let account = Account::default();
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
    assert!(!gitlab.check_uploads(&account, &files).await.unwrap());
    let bitbucket = ForgeRepo::parse("git@bitbucket.org:team/repo.git").unwrap();
    assert!(!bitbucket.check_uploads(&account, &[]).await.unwrap());
    let error = bitbucket.check_uploads(&account, &files).await.unwrap_err();
    assert!(format!("{error:#}").contains("no API for attaching files"));
}

#[test]
fn images_need_a_target_and_a_closing_parenthesis_on_their_line() {
    let targets = |body| -> Vec<String> {
        images(body)
            .iter()
            .map(|image| image.target.to_owned())
            .collect()
    };
    assert_eq!(
        targets("![a](x.png) ![b](<y z.png> 'title')"),
        ["x.png", "y z.png"]
    );
    assert!(targets("![a](x.png\n)").is_empty());
    assert!(targets("![a] (x.png)").is_empty());
    assert!(targets("![a](x.png trailing)").is_empty());
    assert_eq!(targets("![[nested]](x.png) ![b](y.png)"), ["y.png"]);
}

#[test]
fn images_in_code_are_text() {
    let targets = |body| -> Vec<String> {
        images(body)
            .iter()
            .map(|image| image.target.to_owned())
            .collect()
    };
    assert_eq!(
        targets("`![a](x.png)` ``a ` ![b](y.png)`` ![c](z.png)"),
        ["z.png"]
    );
    let fenced = "```md\n![a](x.png)\n````\n~~~\n![b](y.png)\n~~~\n![c](z.png)\n";
    assert_eq!(targets(fenced), ["z.png"]);
    assert!(targets("```\n![a](x.png)\n").is_empty());
    assert_eq!(targets("a ` b ![c](z.png)"), ["z.png"]);
    let attachments = [attachment("/t/a.png", None)];
    let embedded = embed("Use `![](/t/a.png)`.", &attachments, |_, alt| {
        alt.to_owned()
    });
    assert_eq!(embedded, "Use `![](/t/a.png)`.\n\na.png");
}
