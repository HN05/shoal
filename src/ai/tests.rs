use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    time::Duration,
};

use super::*;
use crate::config::named_commands::Commands;

fn parse(text: &str) -> Result<Tools> {
    let config: crate::config::Config = toml::from_str(text)?;
    validate(&config.ai.entries, &config.commands, Path::new("/home"))?;
    Ok(config.ai.tools)
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
        "[ai.droid]\ncommand = ['droid', '{args}']\n\
             [ai.claude]\nskill_dir = '~/claude-skills'\n\
             [ai.notes]\nskill_dir = '/skills'\n",
    )
    .unwrap();
    assert!(runs_as_agent(&agents, "droid"));
    // Skill-only tools are not agents, so `shoal notes` stays a command error.
    assert!(!runs_as_agent(&agents, "notes"));
    let directories = skill_dirs(&agents, Path::new("/home")).unwrap();
    assert_eq!(directories["claude"], Some("/home/claude-skills".into()));
    assert_eq!(directories["codex"], Some("/home/.agents/skills".into()));
    assert_eq!(directories["opencode"], directories["codex"]);
    assert_eq!(directories["grok"], Some("/home/.grok/skills".into()));
    assert_eq!(directories["notes"], Some("/skills".into()));
    assert_eq!(directories["droid"], None);
}

#[test]
fn instruction_arguments_preserve_empty_and_literal_text() {
    let agents = Table::default().tools;
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
        "[ai.claude]\nresume = ['--resume={session}']\n\
             [ai.droid]\ncommand = ['droid']\ninstructions = ['--system', '{instructions}']\n",
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
    for (config, message) in [
        (
            "[ai.droid]\ninstructions = ['--system']\n",
            "{instructions}",
        ),
        ("[ai.droid]\nresume = ['--resume']\n", "{session}"),
    ] {
        let error = format!("{:#}", parse(config).unwrap_err());
        assert!(error.contains(message), "{config}: {error}");
    }
}

#[test]
fn fork_arguments_name_the_forked_session() {
    let agents = parse(
        "[ai.droid]\ncommand = ['droid']\nfork = ['--fork', '{session}']\n\
             [ai.claude-direct]\nbase = 'claude'\n",
    )
    .unwrap();
    assert_eq!(
        arguments(&agents, "claude-direct").fork_args("s1"),
        Some(vec![
            OsString::from("--resume"),
            "s1".into(),
            "--fork-session".into()
        ])
    );
    assert_eq!(
        arguments(&agents, "codex").fork_args("s1"),
        Some(vec![OsString::from("fork"), "s1".into()])
    );
    assert_eq!(
        arguments(&agents, "droid").fork_args("s1"),
        Some(vec![OsString::from("--fork"), "s1".into()])
    );
    assert_eq!(arguments(&agents, "pi").fork_args("s1"), None);
    let error = format!(
        "{:#}",
        parse("[ai.droid]\ncommand = ['droid']\nfork = ['--fork']\n").unwrap_err()
    );
    assert!(error.contains("ai.droid.fork"), "{error}");
}

#[test]
fn arguments_describe_a_launcher_defined_in_commands() {
    let agents = parse(
        "[ai.helper]\nresume_latest = ['--continue']\n\
             [commands]\nhelper = ['helper']\n",
    )
    .unwrap();
    // `[commands]` defines the launcher, so `[ai]` alone makes no agent of it.
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
        "[ai.codex]\nadd_dir = []\n\
             [ai.droid]\ncommand = ['droid']\nadd_dir = ['--dir', '{directory}']\n",
    )
    .unwrap();
    assert_eq!(
        arguments(&agents, "claude").add_dir_args(directory),
        [OsString::from("--add-dir=/shoal/repo")]
    );
    assert_eq!(
        arguments(&Table::default().tools, "codex").add_dir_args(directory),
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
        parse("[ai.droid]\nadd_dir = ['--dir']\n").unwrap_err()
    );
    assert!(
        error.contains("ai.droid.add_dir") && error.contains("{directory}"),
        "{error}"
    );
}

#[test]
fn provider_launchers_need_reachable_unique_names() {
    for (config, message) in [
        ("[ai.work]\ncommand = ['work']\n", "built-in Shoal command"),
        ("[ai.Droid]\ncommand = ['droid']\n", "cannot name an agent"),
        ("[ai.droid]\ncommand = []\n", "nonempty executable"),
        (
            "[ai.droid]\ncommand = ['droid']\n[commands]\ndroid = ['droid']\n",
            "keep one launcher",
        ),
    ] {
        let error = format!("{:#}", parse(config).unwrap_err());
        assert!(error.contains(message), "{config}: {error}");
    }
    // Built-in providers may replace their launcher here as in [commands].
    parse("[ai.codex]\ncommand = ['codex']\n").unwrap();
}

#[test]
fn retry_defaults_to_on_for_claude_and_its_profiles() {
    let agents = parse(
        "[ai.claude-direct]\nbase = 'claude'\n\
         [ai.claude-fast]\nbase = 'claude-direct'\n\
         [ai.droid]\ncommand = ['droid']\n",
    )
    .unwrap();
    assert!(Table::default().tools["claude"].retry);
    assert!(agents["claude-direct"].retry);
    assert!(agents["claude-fast"].retry);
    for name in ["codex", "opencode", "pi", "grok", "droid"] {
        assert!(!agents[name].retry, "{name}");
    }
}

#[test]
fn retry_uses_the_nearest_explicit_setting() {
    let agents = parse(
        "[ai.claude]\nretry = false\n[ai.claude-direct]\nbase = 'claude'\n\
         [ai.claude-auto]\nbase = 'claude-direct'\nretry = true\n\
         [ai.claude-manual]\nbase = 'claude-auto'\nretry = false\n\
         [ai.droid]\ncommand = ['droid']\nretry = true\n\
         [ai.droid-profile]\nbase = 'droid'\n",
    )
    .unwrap();
    assert!(!agents["claude"].retry);
    assert!(!agents["claude-direct"].retry);
    assert!(agents["claude-auto"].retry);
    assert!(!agents["claude-manual"].retry);
    assert!(agents["droid"].retry);
    assert!(agents["droid-profile"].retry);
    let error = format!("{:#}", parse("[ai.notes]\nretry = true\n").unwrap_err());
    assert!(error.contains("ai.notes.retry needs"), "{error}");
}

#[test]
fn launch_profiles_inherit_along_their_bases() {
    let agents = parse(
        "[ai.claude-direct]\nbase = 'claude'\nresume = ['--resume={session}']\n\
             env = { CLAUDE_CONFIG_DIR = '~/.claude-direct', MODE = 'direct' }\n\
             env_remove = ['ANTHROPIC_BASE_URL', 'MODE_OLD']\n\
             [ai.claude-fast]\nbase = 'claude-direct'\nenv = { MODE_OLD = 'kept' }\n\
             env_remove = ['MODE']\n",
    )
    .unwrap();
    assert!(runs_as_agent(&agents, "claude-fast"));
    assert_eq!(agents["claude-fast"].integration, IntegrationKind::Claude);
    assert_eq!(agents["claude-fast"].bases, ["claude-direct", "claude"]);
    assert_eq!(agents["codex"].integration, IntegrationKind::Codex);
    assert_eq!(agents["pi"].integration, IntegrationKind::None);
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
        "[ai.droid]\ncommand = ['droid']\n\
             [ai.droid-direct]\nbase = 'droid'\n\
             [ai.claude-direct]\nbase = 'claude'\n\
             [ai.claude-own]\nbase = 'claude'\ncommand = ['claude-own']\n",
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
    for (config, message) in [
        ("[ai.direct]\nbase = 'missing'\n", "not a built-in AI tool"),
        (
            "[ai.notes]\nskill_dir = '/s'\n[ai.direct]\nbase = 'notes'\n",
            "not a built-in AI tool",
        ),
        (
            "[ai.one]\nbase = 'two'\n[ai.two]\nbase = 'one'\n",
            "form a cycle",
        ),
        ("[ai.self]\nbase = 'self'\n", "form a cycle"),
        ("[ai.work]\nbase = 'claude'\n", "built-in Shoal command"),
        (
            "[ai.direct]\nbase = 'claude'\n[commands]\ndirect = ['claude']\n",
            "keep one launcher",
        ),
        (
            "[ai.direct]\nbase = 'claude'\nenv = { 'bad-name' = 'x' }\n",
            "invalid env",
        ),
        (
            "[ai.direct]\nbase = 'claude'\nenv_remove = ['CLAUDE_CONFIG_DIR']\n",
            "cannot remove CLAUDE_CONFIG_DIR",
        ),
        (
            "[ai.direct]\nbase = 'claude'\nenv = { CODEX_HOME = 'relative' }\n",
            "absolute path",
        ),
        (
            "[ai.notes]\nenv = { MODE = 'x' }\n",
            "need a command or base",
        ),
    ] {
        let error = format!("{:#}", parse(config).unwrap_err());
        assert!(error.contains(message), "{config}: {error}");
    }
    // Built-in tools take an environment of their own.
    parse("[ai.claude]\nenv_remove = ['ANTHROPIC_BASE_URL']\n").unwrap();
}

#[test]
fn prompt_cache_lifetime_prefers_configured_then_reported_then_built_in() {
    let agents = parse(
        "[ai.claude-short]\nbase = 'claude'\nprompt_cache_minutes = 5\n\
         [ai.claude-shorter]\nbase = 'claude-short'\n\
         [ai.codex-long]\nbase = 'codex'\nprompt_cache_minutes = 720\n",
    )
    .unwrap();
    let minutes = |minutes: u64| Some(Duration::from_secs(minutes * 60));
    assert_eq!(agents["claude"].prompt_cache(None), None);
    assert_eq!(agents["claude"].prompt_cache(minutes(60)), minutes(60));
    assert_eq!(
        agents["claude-shorter"].prompt_cache(minutes(60)),
        minutes(5)
    );
    assert_eq!(agents["codex"].prompt_cache(None), minutes(30));
    assert_eq!(agents["codex-long"].prompt_cache(None), minutes(720));
    assert_eq!(agents["pi"].prompt_cache(None), None);
    for (config, message) in [
        (
            "[ai.notes]\nprompt_cache_minutes = 5\n",
            "needs a command or base",
        ),
        (
            "[ai.codex]\nprompt_cache_minutes = 0\n",
            "must be at least 1",
        ),
    ] {
        let error = format!("{:#}", parse(config).unwrap_err());
        assert!(error.contains(message), "{config}: {error}");
    }
}
