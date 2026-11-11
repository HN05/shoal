use super::*;

#[test]
fn split_placement_opens_splits() {
    let config = crate::config::Rex {
        enabled: true,
        new_window: true,
        placement: Placement::Split,
        window_name: None,
        session_name: "{repo}".into(),
        close_when_done: None,
    };
    assert!(options(&config).opens_splits);
}

#[tokio::test]
async fn a_block_whose_rex_is_gone_is_no_place_to_rerun_in() {
    let root = tempfile::tempdir().unwrap();
    // The recorded `rex` is gone; one found elsewhere could not reach the
    // recorded server either, which never is the user's.
    let mut place = Origin::created(HostName::Rex, "block:1".into());
    place.data = [
        (
            "bin_dir".to_owned(),
            root.path().join("gone").display().to_string(),
        ),
        (
            "server".to_owned(),
            root.path().join("rex.sock").display().to_string(),
        ),
    ]
    .into();
    let ran = Rex
        .run_in_place(root.path(), &place, &["/opt/shoal".into()], &[])
        .await
        .unwrap();
    assert!(!ran);
}
