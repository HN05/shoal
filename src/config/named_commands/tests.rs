use crate::config::{self, Config};

#[test]
fn commands_layer_by_name_and_replace_whole_argument_arrays() {
    let global =
        Config::from_toml("[commands]\nreview = ['global', '--flag']\ncheck = ['check']\n")
            .unwrap();
    let layers = config::repo::ConfigLayers {
        worktree_file: config::repo::parse("[commands]\nreview = ['file']\n").unwrap(),
        saved_repository_config: config::repo::parse(
            "[commands]\nreview = ['saved', 'two words']\n",
        )
        .unwrap(),
    };
    let effective = global.resolve(&layers).unwrap();
    assert_eq!(effective.commands["review"], ["saved", "two words"]);
    assert_eq!(effective.commands["check"], ["check"]);
}

#[test]
fn invalid_commands_are_rejected() {
    for config in [
        "[commands]\nreview = []",
        "[commands]\nreview = ['{args}']",
        "[commands]\nreview = ['tool', '{args}', '{args}']",
        "[commands]\nreview = ['']",
        "[commands]\nreview = ['   ']",
        "[commands]\nreview = ['tool', \"\\u0000\"]",
        "[commands]\n'bad name' = ['tool']",
        "[commands]\nreview = 'shell command'",
    ] {
        assert!(config::repo::parse(config).is_err(), "{config}");
    }
}

#[test]
fn outside_a_workspace_only_the_directory_placeholder_has_a_value() {
    let argv = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let placeholder = |args: &[&str]| super::workspace_only_placeholder(&argv(args));
    assert_eq!(placeholder(&["tool", "{path}", "{prompt}", "{args}"]), None);
    assert_eq!(
        placeholder(&["tool", "--name={workspace}"]),
        Some("{workspace}")
    );
    assert_eq!(placeholder(&["tool", "{branch}"]), Some("{branch}"));
    assert_eq!(
        placeholder(&["tool", "{diff_base}..HEAD"]),
        Some("{diff_base}")
    );
}

#[test]
fn built_in_names_are_valid_for_explicit_run() {
    for name in ["run", "work"] {
        let config = format!("[commands]\n{name} = ['tool']\n");
        assert!(config::repo::parse(&config).is_ok(), "{config}");
    }
}
