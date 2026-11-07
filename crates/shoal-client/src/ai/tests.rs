use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    time::Duration,
};

use super::*;
use crate::config::named_commands::Commands;

fn parse(text: &str) -> Result<Tools> {
    let config = crate::config::Config::from_toml(text)?;
    validate::skill_dirs(&config.machine.agents, Path::new("/home"))?;
    Ok(config.resolve(&Default::default())?.tools)
}

/// The built-in tools without configuration.
fn built_in() -> Tools {
    tools(&Default::default())
}

/// The launch profile of `name`, which runs as an agent.
fn launch<'a>(tools: &'a Tools, name: &str) -> &'a LaunchProfile {
    &tools[name].launch
}

fn arguments<'a>(tools: &'a Tools, name: &str) -> &'a Arguments {
    &launch(tools, name).arguments
}

fn runs_as_agent(tools: &Tools, name: &str) -> bool {
    agents(tools).any(|agent| agent == name)
}

#[test]
fn configured_providers_add_launchers_and_skill_directories() {
    let agents = parse(
        "[agents.droid]\ncommand = ['droid', '{args}']\n[machine.agents.claude]\nskill_dir = '~/claude-skills'\n[machine.agents.notes]\nskill_dir = '/skills'\n",
    )
    .unwrap();
    assert!(runs_as_agent(&agents, "droid"));
    // Skill-only tools are not agents, so `shoal notes` stays a command error.
    assert!(!runs_as_agent(&agents, "notes"));
    let directories = skill_dirs(&agents, Path::new("/home")).unwrap();
    assert_eq!(directories["claude"], Some("/home/claude-skills".into()));
    assert_eq!(directories["notes"], Some("/skills".into()));
    assert_eq!(directories["droid"], None);
}

#[test]
fn instruction_arguments_preserve_empty_and_literal_text() {
    let agents = built_in();
    for name in ["claude", "codex"] {
        assert_eq!(
            arguments(&agents, name).instruction_args(""),
            Some(Vec::new())
        );
    }
    assert_eq!(arguments(&agents, "pi").instruction_args("text"), None);
    let text = "quotes: \" and newlines\n$(false)";
    assert_eq!(
        arguments(&agents, "claude").instruction_args(text),
        Some(vec![OsString::from("--append-system-prompt"), text.into()])
    );
    // Codex reads a literal TOML string.
    let text = "quotes: \"'''\\\nUnicode: 日本語 $(false)";
    let args = arguments(&agents, "codex").instruction_args(text).unwrap();
    let value: toml::Value = toml::from_str(args[1].to_str().unwrap()).unwrap();
    assert_eq!(value["developer_instructions"].as_str(), Some(text));
}

#[test]
fn configured_arguments_replace_built_in_ones_entry_by_entry() {
    let agents = parse(
        "[agents.claude]\nresume = ['--resume={session}']\n[agents.droid]\ncommand = ['droid']\ninstructions = ['--system', '{instructions}']\n",
    )
    .unwrap();
    let claude = arguments(&agents, "claude");
    assert_eq!(
        claude.resume_args(Some("s1")),
        Some(vec![OsString::from("--resume=s1")])
    );
    assert_eq!(claude.resume_args(None), Some(vec!["--continue".into()]));
    let droid = arguments(&agents, "droid");
    assert_eq!(
        droid.instruction_args("be brief"),
        Some(vec!["--system".into(), "be brief".into()])
    );
    assert_eq!(droid.resume_args(None), None);
    for config in [
        "[agents.droid]\ninstructions = ['--system']\n",
        "[agents.droid]\nresume = ['--resume']\n",
    ] {
        assert!(parse(config).is_err(), "{config}");
    }
}

#[test]
fn fork_arguments_name_the_forked_session() {
    let agents = parse(
        "[agents.droid]\ncommand = ['droid']\nfork = ['--fork', '{session}']\n[agents.claude-direct]\nbase = 'claude'\n",
    )
    .unwrap();
    assert_eq!(
        arguments(&agents, "claude-direct").fork_args("s1", "/work/child"),
        Some(vec![
            OsString::from("--resume"),
            "s1".into(),
            "--fork-session".into()
        ])
    );
    assert_eq!(
        arguments(&agents, "codex").fork_args("s1", "/work/child"),
        Some(vec![
            OsString::from("fork"),
            "s1".into(),
            "--cd".into(),
            "/work/child".into()
        ])
    );
    assert_eq!(
        arguments(&agents, "droid").fork_args("s1", "/work/child"),
        Some(vec![OsString::from("--fork"), "s1".into()])
    );
    assert_eq!(
        arguments(&agents, "pi").fork_args("s1", "/work/child"),
        None
    );
    let error = format!(
        "{:#}",
        parse("[agents.droid]\ncommand = ['droid']\nfork = ['--fork']\n").unwrap_err()
    );
    assert!(error.contains("agents.droid.fork"), "{error}");
}

#[test]
fn argument_templates_alone_make_no_agent_of_a_shortcut() {
    let agents =
        parse("[commands]\nhelper = ['helper']\n[agents.helper]\nresume_latest = ['--continue']\n")
            .unwrap();
    // Without a `command` or `base`, the entry makes no agent of the shortcut.
    assert!(!runs_as_agent(&agents, "helper"));
    assert_eq!(
        arguments(&agents, "helper").resume_args(None),
        Some(vec![OsString::from("--continue")])
    );
}

#[test]
fn directory_arguments_name_one_directory_each() {
    let directory = Path::new("/shoal/repo");
    let agents = parse(
        "[agents.codex]\nadd_dir = []\n[agents.droid]\ncommand = ['droid']\nadd_dir = ['--dir', '{directory}']\n",
    )
    .unwrap();
    assert_eq!(
        arguments(&agents, "claude").add_dir_args(directory),
        [OsString::from("--add-dir=/shoal/repo")]
    );
    assert_eq!(
        arguments(&built_in(), "codex").add_dir_args(directory),
        [OsString::from("--add-dir"), "/shoal/repo".into()]
    );
    assert!(
        arguments(&agents, "codex")
            .add_dir_args(directory)
            .is_empty()
    );
    assert_eq!(
        arguments(&agents, "droid").add_dir_args(directory),
        [OsString::from("--dir"), "/shoal/repo".into()]
    );
    assert!(arguments(&agents, "pi").add_dir_args(directory).is_empty());
    let error = format!(
        "{:#}",
        parse("[agents.droid]\nadd_dir = ['--dir']\n").unwrap_err()
    );
    assert!(
        error.contains("agents.droid.add_dir") && error.contains("{directory}"),
        "{error}"
    );
}

#[test]
fn provider_launchers_need_reachable_unique_names() {
    for config in [
        "[agents.work]\ncommand = ['work']\n",
        "[agents.Droid]\ncommand = ['droid']\n",
        "[agents.droid]\ncommand = []\n",
        "[commands]\ndroid = ['droid']\n[agents.droid]\ncommand = ['droid']\n",
    ] {
        assert!(parse(config).is_err(), "{config}");
    }
    // Built-in providers may replace their launcher.
    parse("[agents.codex]\ncommand = ['codex']\n").unwrap();
}

#[test]
fn retry_defaults_to_on_for_claude_and_its_profiles() {
    let agents = parse(
        "[agents.claude-direct]\nbase = 'claude'\n[agents.claude-fast]\nbase = 'claude-direct'\n[agents.droid]\ncommand = ['droid']\n",
    )
    .unwrap();
    assert!(agents["claude-fast"].retry);
    assert!(!agents["droid"].retry);
}

#[test]
fn retry_uses_the_nearest_explicit_setting() {
    let agents = parse(
        "[agents.claude]\nretry = false\n[agents.claude-direct]\nbase = 'claude'\n[agents.claude-auto]\nbase = 'claude-direct'\nretry = true\n[agents.claude-manual]\nbase = 'claude-auto'\nretry = false\n[agents.droid]\ncommand = ['droid']\nretry = true\n[agents.droid-profile]\nbase = 'droid'\n",
    )
    .unwrap();
    assert!(!agents["claude"].retry);
    assert!(!agents["claude-direct"].retry);
    assert!(agents["claude-auto"].retry);
    assert!(!agents["claude-manual"].retry);
    assert!(agents["droid"].retry);
    assert!(agents["droid-profile"].retry);
    assert!(parse("[agents.notes]\nretry = true\n").is_err());
}

#[test]
fn launch_profiles_inherit_along_their_bases() {
    let agents = parse(
        "[agents.claude-direct]\nbase = 'claude'\nresume = ['--resume={session}']\nenv = { CLAUDE_CONFIG_DIR = '~/.claude-direct', MODE = 'direct' }\nenv_remove = ['ANTHROPIC_BASE_URL', 'MODE_OLD']\n[agents.claude-fast]\nbase = 'claude-direct'\nenv = { MODE_OLD = 'kept' }\nenv_remove = ['MODE']\n",
    )
    .unwrap();
    assert!(runs_as_agent(&agents, "claude-fast"));
    assert_eq!(agents["claude-fast"].integration, IntegrationKind::Claude);
    assert_eq!(agents["claude-fast"].bases, ["claude-direct", "claude"]);
    // The nearest entry wins, then the built-in provider's.
    let fast = arguments(&agents, "claude-fast");
    assert_eq!(
        fast.resume_args(Some("s1")),
        Some(vec!["--resume=s1".into()])
    );
    assert_eq!(fast.resume_args(None), Some(vec!["--continue".into()]));
    assert_eq!(fast.instructions, arguments(&agents, "claude").instructions);
    let fast = &launch(&agents, "claude-fast").environment;
    assert_eq!(
        fast.values(Path::new("/home")),
        BTreeMap::from([
            (
                "CLAUDE_CONFIG_DIR".to_owned(),
                OsString::from("/home/.claude-direct")
            ),
            ("MODE_OLD".to_owned(), "kept".into()),
        ])
    );
    assert_eq!(
        fast.remove,
        BTreeSet::from(["ANTHROPIC_BASE_URL".to_owned(), "MODE".to_owned()])
    );
    assert_eq!(
        launch(&agents, "claude").environment,
        Environment::default()
    );
    let directories = skill_dirs(&agents, Path::new("/home")).unwrap();
    assert_eq!(
        directories["claude-fast"],
        Some("/home/.claude-direct/skills".into())
    );
}

#[test]
fn launch_profiles_follow_the_launcher_in_effect() {
    let agents = parse(
        "[agents.droid]\ncommand = ['droid']\n[agents.droid-direct]\nbase = 'droid'\n[agents.claude-direct]\nbase = 'claude'\n[agents.claude-own]\nbase = 'claude'\ncommand = ['claude-own']\n",
    )
    .unwrap();
    let mut commands = Commands::from([
        ("claude".to_owned(), vec!["repo-claude".to_owned()]),
        ("droid".to_owned(), vec!["droid".to_owned()]),
        ("claude-own".to_owned(), vec!["claude-own".to_owned()]),
    ]);
    inherit_launchers(&agents, &mut commands);
    assert_eq!(commands["claude-direct"], ["repo-claude"]);
    assert_eq!(commands["droid-direct"], ["droid"]);
    assert_eq!(commands["claude-own"], ["claude-own"]);
}

#[test]
fn launch_profiles_need_a_reachable_base_and_valid_variables() {
    for config in [
        "[agents.direct]\nbase = 'missing'\n",
        "[agents.direct]\nbase = 'notes'\n[machine.agents.notes]\nskill_dir = '/s'\n",
        "[agents.one]\nbase = 'two'\n[agents.two]\nbase = 'one'\n",
        "[agents.self]\nbase = 'self'\n",
        "[agents.work]\nbase = 'claude'\n",
        "[commands]\ndirect = ['claude']\n[agents.direct]\nbase = 'claude'\n",
        "[agents.direct]\nbase = 'claude'\nenv = { 'bad-name' = 'x' }\n",
        "[agents.direct]\nbase = 'claude'\nenv_remove = ['CLAUDE_CONFIG_DIR']\n",
        "[agents.direct]\nbase = 'claude'\nenv = { CODEX_HOME = 'relative' }\n",
        "[agents.notes]\nenv = { MODE = 'x' }\n",
    ] {
        assert!(parse(config).is_err(), "{config}");
    }
    // Built-in tools take an environment of their own.
    parse("[agents.claude]\nenv_remove = ['ANTHROPIC_BASE_URL']\n").unwrap();
}

#[test]
fn prompt_cache_lifetime_prefers_configured_then_reported_then_the_default() {
    let agents = parse(
        "[agents.claude-short]\nbase = 'claude'\nprompt_cache_minutes = 5\n[agents.claude-shorter]\nbase = 'claude-short'\n[agents.codex-long]\nbase = 'codex'\nprompt_cache_minutes = 720\n",
    )
    .unwrap();
    let minutes = |minutes: u64| Duration::from_secs(minutes * 60);
    assert_eq!(
        agents["claude"].prompt_cache(Some(minutes(60))),
        minutes(60)
    );
    assert_eq!(
        agents["claude-shorter"].prompt_cache(Some(minutes(60))),
        minutes(5)
    );
    assert_eq!(agents["codex-long"].prompt_cache(None), minutes(720));
    // Every tool has a lifetime, so idle stop never takes a cache for gone
    // the moment a turn ends.
    for tool in ["claude", "codex", "pi", "opencode", "grok"] {
        assert!(agents[tool].prompt_cache(None) > Duration::ZERO, "{tool}");
    }
    for config in [
        "[agents.notes]\nprompt_cache_minutes = 5\n",
        "[agents.codex]\nprompt_cache_minutes = 0\n",
    ] {
        assert!(parse(config).is_err(), "{config}");
    }
}

#[test]
fn model_arguments_select_the_model_and_effort_or_name_the_missing_template() {
    let agents = parse(
        "[agents.droid]\ncommand = ['droid']\nmodel_args = ['--model={model}']\n[agents.claude-fast]\nbase = 'claude'\neffort_args = ['--effort', '{effort}', '--fast']\n",
    )
    .unwrap();
    let choice = |model: Option<&str>, effort: Option<&str>| crate::agent::ModelChoice {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
    };
    let both = choice(Some("m1"), Some("high"));
    assert_eq!(
        arguments(&agents, "claude").model_args(&both),
        Ok(["--model", "m1", "--effort", "high"]
            .map(OsString::from)
            .to_vec())
    );
    // A profile keeps its base's templates unless it sets its own.
    assert_eq!(
        arguments(&agents, "claude-fast").model_args(&both),
        Ok(["--model", "m1", "--effort", "high", "--fast"]
            .map(OsString::from)
            .to_vec())
    );
    let droid = arguments(&agents, "droid");
    assert_eq!(
        droid.model_args(&choice(Some("m1"), None)),
        Ok(vec![OsString::from("--model=m1")])
    );
    assert_eq!(droid.model_args(&both), Err("effort_args"));
    assert_eq!(
        arguments(&agents, "pi").model_args(&both),
        Err("model_args")
    );
    assert_eq!(
        arguments(&agents, "pi").model_args(&Default::default()),
        Ok(Vec::new())
    );
    // Claude's effort variable overrides its flag, so a launch sets both.
    let claude = launch(&agents, "claude-fast");
    assert_eq!(
        claude.environment_with(&both).values(Path::new("/home")),
        BTreeMap::from([("CLAUDE_CODE_EFFORT_LEVEL".to_owned(), "high".into())])
    );
    assert_eq!(
        launch(&agents, "codex").environment_with(&both),
        Environment::default()
    );
    assert_eq!(
        claude.effort_environment(&both),
        Some(("CLAUDE_CODE_EFFORT_LEVEL", "high".to_owned()))
    );
    assert_eq!(launch(&agents, "codex").effort_environment(&both), None);
}

#[test]
fn configured_models_follow_the_nearest_entry_per_option() {
    let agents = parse(
        "[agents.claude]\nmodel = 'opus'\neffort = 'high'\n[agents.claude-fast]\nbase = 'claude'\nmodel = 'haiku'\n",
    )
    .unwrap();
    let model = &agents["claude-fast"].model;
    assert_eq!(model.model.as_deref(), Some("haiku"));
    assert_eq!(model.effort.as_deref(), Some("high"));
    assert!(agents["codex"].model.is_empty());
    for config in [
        "[agents.notes]\nmodel = 'x'\n",
        "[agents.codex]\neffort = ''\n",
        "[agents.droid]\ncommand = ['droid']\nmodel_args = ['--model']\n",
        "[agents.droid]\ncommand = ['droid']\neffort_args = ['-e']\n",
    ] {
        assert!(parse(config).is_err(), "{config}");
    }
}

#[test]
fn reviews_run_with_their_template_after_the_launcher_before_its_args() {
    let agents = parse(
        "[agents.codex-review]\nbase = 'codex'\n[agents.grok]\nreview = ['--prompt', '{prompt}', '--schema', '{schema_json}']\n",
    )
    .unwrap();
    let fields = ReviewFields {
        prompt: "Review it",
        schema: Path::new("/tmp/schema.json"),
        schema_json: "{}",
        output: Path::new("/tmp/review.json"),
    };
    let codex = arguments(&agents, "codex-review");
    let args = codex.review_args(&fields).unwrap();
    assert!(codex.review_writes_output());
    assert_eq!(args.first(), Some(&OsString::from("exec")));
    assert!(args.contains(&"/tmp/review.json".into()));
    assert_eq!(args.last(), Some(&OsString::from("Review it")));
    let claude = arguments(&agents, "claude");
    assert!(!claude.review_writes_output());
    let args = claude.review_args(&fields).unwrap();
    assert!(args.contains(&"{}".into()));
    // Read-only whatever settings it would otherwise load.
    for flag in ["--setting-sources=", "--permission-mode=dontAsk"] {
        assert!(args.contains(&flag.into()), "{args:?}");
    }
    let sandbox = args
        .iter()
        .find_map(|arg| arg.to_str()?.strip_prefix("--settings="))
        .unwrap();
    let sandbox: serde_json::Value = serde_json::from_str(sandbox).unwrap();
    assert_eq!(sandbox["sandbox"]["enabled"], true);
    assert_eq!(sandbox["sandbox"]["allowUnsandboxedCommands"], false);
    assert!(arguments(&agents, "grok").review_args(&fields).is_some());
    assert_eq!(arguments(&agents, "pi").review_args(&fields), None);

    let launcher = [
        "codex",
        "-m",
        "gpt",
        "{args}",
        "--sandbox",
        "danger-full-access",
    ]
    .map(String::from);
    let command = oneshot_command(&launcher, vec!["exec".into()]);
    assert_eq!(command, ["codex", "-m", "gpt", "exec"].map(OsString::from));
    let command = oneshot_command(
        &["claude".into(), "--model".into(), "opus".into()],
        vec!["-p".into()],
    );
    assert_eq!(
        command,
        ["claude", "--model", "opus", "-p"].map(OsString::from)
    );
}

#[test]
fn review_templates_need_a_prompt_and_a_schema() {
    for template in ["['{schema}']", "['{prompt}']"] {
        assert!(parse(&format!("[agents.claude]\nreview = {template}\n")).is_err());
    }
}
