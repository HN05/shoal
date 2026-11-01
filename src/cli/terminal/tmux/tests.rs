use super::*;

fn config(enabled: Option<bool>) -> crate::config::Tmux {
    crate::config::Tmux {
        enabled,
        new_window: true,
        placement: Placement::Split,
        window_name: Some("{branch}".into()),
        session_name: "{repo}".into(),
        close_when_done: Some(false),
    }
}

#[test]
fn tmux_serves_its_panes_only_once_config_opts_in() {
    assert!(!options(&config(None), HostName::Zmx).enabled);
    assert!(options(&config(None), HostName::Tmux).enabled);
    assert!(options(&config(Some(true)), HostName::Zmx).enabled);
    assert!(!options(&config(Some(false)), HostName::Tmux).enabled);
}

#[test]
fn the_tmux_section_sets_windows_and_the_session_outside_tmux() {
    let options = options(&config(Some(true)), HostName::Zmx);
    assert!(options.opens_places && options.opens_splits);
    assert_eq!(options.name_template.as_deref(), Some("{branch}"));
    assert_eq!(options.group_template.as_deref(), Some("{repo}"));
    assert_eq!(options.close_when_done, Some(false));
}

#[tokio::test]
async fn a_pane_on_a_server_that_is_gone_is_no_place_to_rerun_in() {
    let root = tempfile::tempdir().unwrap();
    let mut place = Origin::created(HostName::Tmux, "%1".into());
    place.data = Server::data(&root.path().join("gone").display().to_string());
    let ran = Tmux
        .run_in_place(root.path(), &place, &["/opt/shoal".into()], &[])
        .await;
    // Without tmux the lookup fails instead; either way nothing runs there.
    assert!(!ran.unwrap_or(false));
}

#[tokio::test]
async fn keys_reach_no_pane_whose_server_instance_is_unknown() {
    let root = tempfile::tempdir().unwrap();
    let mut place = Origin::created(HostName::Tmux, "%1".into());
    place.data = Server::data(&root.path().join("sock").display().to_string());
    let error = Tmux.send_keys(&place, "hello\r").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("records no server instance"),
        "{error:#}"
    );
}
