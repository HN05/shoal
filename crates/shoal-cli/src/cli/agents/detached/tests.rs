use super::*;

#[test]
fn a_workspace_session_gets_the_workspace_directories_before_its_own_arguments() {
    let config = crate::config::Config::from_toml("").unwrap();
    let settings = config
        .resolve(&crate::config::repo::ConfigLayers::default())
        .unwrap();
    let happy = crate::agents::agent_integration(&settings, &"happy-claude".into())
        .detached()
        .expect("Happy runs its agents detached");
    let directories = vec![OsString::from("--add-dir=/root/repo")];
    let (_, args) = workspace_input(
        happy,
        &settings,
        "Guidance".into(),
        directories,
        None,
        vec!["--own".into()],
    );
    let at = |arg: &str| args.iter().position(|candidate| candidate == arg);
    let directory = at("--add-dir=/root/repo").expect("the directories are given");
    assert!(directory < at("--own").expect("its own arguments follow"));
}
