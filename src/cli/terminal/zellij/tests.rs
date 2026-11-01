use super::*;

fn config(enabled: Option<bool>) -> crate::config::Zellij {
    crate::config::Zellij {
        enabled,
        new_tab: true,
        placement: TabPlacement::Split,
        tab_name: Some("{branch}".into()),
        close_when_done: Some(false),
    }
}

#[test]
fn zellij_serves_its_panes_only_once_config_opts_in() {
    assert!(!options(&config(None), HostName::Zmx).enabled);
    assert!(options(&config(None), HostName::Zellij).enabled);
    assert!(options(&config(Some(true)), HostName::Zmx).enabled);
    assert!(!options(&config(Some(false)), HostName::Zellij).enabled);
}

#[test]
fn the_zellij_section_sets_tabs_and_keeps_no_group() {
    let options = options(&config(Some(true)), HostName::Zmx);
    assert!(options.opens_places && options.opens_splits);
    assert_eq!(options.name_template.as_deref(), Some("{branch}"));
    assert_eq!(options.group_template, None);
    assert_eq!(options.close_when_done, Some(false));
}

#[test]
fn a_pane_is_at_its_prompt_while_the_users_shell_runs_in_front() {
    let shell = var("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let name = Path::new(&shell).file_name().unwrap().to_str().unwrap();
    assert!(at_prompt(false, Some(&shell)));
    assert!(at_prompt(false, Some(&format!("-{name}"))));
    assert!(at_prompt(false, Some(&format!("{shell} -l"))));
    assert!(!at_prompt(true, Some(&shell)));
    assert!(!at_prompt(false, Some("claude --resume")));
    assert!(!at_prompt(false, None));
}

#[tokio::test]
async fn keys_reach_no_pane_whose_server_instance_is_unknown() {
    let place = Origin::created(HostName::Zellij, "repo:terminal_1".into());
    let error = Zellij.send_keys(&place, "hello\r").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("records no server instance"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_pane_recorded_without_its_instance_is_no_place_to_rerun_in() {
    let root = tempfile::tempdir().unwrap();
    let place = Origin::created(HostName::Zellij, "repo:terminal_1".into());
    let ran = Zellij
        .run_in_place(root.path(), &place, &["/opt/shoal".into()], &[])
        .await
        .unwrap();
    assert!(!ran);
}
