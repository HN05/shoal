use super::*;

#[test]
fn the_rex_section_sets_windows_and_the_session_outside_rex() {
    let config = crate::config::Rex {
        enabled: false,
        new_window: false,
        window_name: Some("{branch}".into()),
        session_name: "{repo}".into(),
        close_when_done: Some(false),
    };
    let options = options(&config);
    assert!(!options.enabled && !options.opens_places);
    assert_eq!(options.name_template.as_deref(), Some("{branch}"));
    assert_eq!(options.group_template.as_deref(), Some("{repo}"));
    assert_eq!(options.close_when_done, Some(false));
}

#[test]
fn queued_starts_from_rex_open_windows_and_others_do_not() {
    let place = |host| Origin::created(host, "block:1".into());
    assert!(Rex.takes_queued(Some(&place(HostName::Rex))));
    assert!(!Rex.takes_queued(Some(&place(HostName::Herdr))));
    assert!(!Rex.takes_queued(None));
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
