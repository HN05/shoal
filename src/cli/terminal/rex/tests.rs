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
