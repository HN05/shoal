use super::*;
use crate::config::hooks::HookKind;
use crate::config::{self, named_commands, repo::parse};
use serde_json::Value;

/// Every option set, so an option that does not survive layering reports
/// another layer than the one holding the fixture.
const FULL: &str = "\
git_profile = 'work'\n\
[agent]\ndefault = 'claude'\nmodel = 'sonnet'\neffort = 'low'\nmax_running = 2\ndirs = ['/tmp', '~/cache']\ngit_profile = 'agent'\n\
[modes.worker]\nkind = 'workspace'\nrepository = true\nrules = ['chat']\ninstructions = 'work'\n[modes.worker.permissions]\nissue_open = 'propose'\npr_open = 'propose'\nissue_other_repositories = 'deny'\nmerge = 'propose'\npush = 'deny'\napi_writes = 'deny'\n[modes.chat]\nkind = 'workspace'\nrepository = true\nrules = ['worker']\ninstructions = 'talk'\n[modes.chat.permissions]\nissue_open = 'propose'\npr_open = 'propose'\nissue_other_repositories = 'deny'\nmerge = 'propose'\npush = 'deny'\napi_writes = 'deny'\n\
[modes.plan]\nkind = 'workspace'\nrepository = true\nrules = ['review']\ninstructions = 'plan'\n[modes.plan.permissions]\nissue_open = 'propose'\npr_open = 'propose'\nissue_other_repositories = 'deny'\nmerge = 'propose'\npush = 'deny'\napi_writes = 'deny'\n\
[modes.orchestrate]\nkind = 'chat'\nrepository = false\nrules = ['orchestrate']\ninstructions = 'manage'\n[modes.orchestrate.permissions]\nissue_open = 'propose'\npr_open = 'propose'\nissue_other_repositories = 'deny'\nmerge = 'propose'\npush = 'deny'\napi_writes = 'deny'\n\
[rules.work]\ntext = 'work'\n[rules.worker]\ntext = 'agent'\n[rules.chat]\ntext = 'chat'\n[rules.review]\ntext = 'review'\n[rules.orchestrate]\ntext = 'orchestrate'\n\
[terminal]\nfocus = false\nclose_when_done = false\ndefault_host = 'rex'\n\
[terminal.herdr]\nenabled = false\ntab_name = '{branch}'\nnew_tab = false\nworkspace_name = 'work'\nclose_when_done = false\n\
[terminal.rex]\nenabled = false\nnew_window = false\nplacement = 'split'\nwindow_name = '{branch}'\nsession_name = 'work'\nclose_when_done = false\n\
[terminal.tmux]\nenabled = true\nnew_window = false\nplacement = 'split'\nwindow_name = '{branch}'\nsession_name = 'work'\nclose_when_done = false\n\
[terminal.zellij]\nenabled = true\nnew_tab = false\nplacement = 'split'\ntab_name = '{branch}'\nclose_when_done = false\n\
[setup]\ncommand = 'setup'\ncopy_ignored = false\npresets = ['cargo']\n[setup.seed]\ncommand = 'scripts/seed'\npaths = ['target']\n\
[hooks]\npre_setup = 'pre-setup'\npost_remove = 'post-remove'\npost_done = 'post-done'\npost_ready = 'post-ready'\npost_agent_exit = 'agent-exit'\n\
post_resource_acquire = 'acquire'\npre_resource_release = 'release'\npost_setup = 'attach'\npre_remove = 'detach'\n\
[env]\nSESSION = '{workspace}'\n[caches.go]\nscope = 'shared'\n[demos.web]\npreset = 'web'\ncmd = ['npm', 'run', 'dev']\n[secrets.TOKEN]\ncommand = ['token']\n\
[commands]\ncheck = ['check']\n[ports]\non_conflict = 'auto'\nstart = 3000\nend = 3100\n[ports.web]\nport = 3000\n\
[resources.lock]\ncapacity = 1\n[resource_pools.devices]\ncapacity = 2\n\
[resource_pools.devices.resources.phone]\ncapacity = 1\n\
[simulators]\nrequires_approval = true\napproval_lifetime = 'workspace'\npreferred = ['phone']\n\
[cleanup]\ngrace_minutes = 5\n[cleanup.auto]\nenabled = false\nidle_minutes = 30\n[cleanup.pr]\nenabled = false\n[cleanup.merged]\nenabled = false\nidle_minutes = 60\n\
[cleanup.restore]\nretention_days = 7\narchive_max_mb = 5\n[cleanup.done]\nenabled = false\n[idle_stop]\nenabled = true\n\
[pr]\ndraft = true\nrequired_labels = ['area/']\nundraft_on_ready = false\ndraft_on_unready = true\nprogress = false\n\
[issue]\nrequired_labels = ['type/']\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n[review]\npost = false\nagent = 'codex'\nfallback_agent = 'claude'\nrequire_for_pr = true\nrequire_for_ready = true\n[messages]\nsettle_seconds = 2\n[merge]\nenabled = false\nconfirm = false\npush = true\n[conflicts]\nsensitive = ['version']\n\
[permissions]\nissue_open = 'propose'\npr_open = 'propose'\nissue_other_repositories = 'direct'\nmerge = 'direct'\npush = 'propose'\napi_writes = 'direct'\n[runner]\nenabled = true\nlabels = ['rust']\npermit = 'builds'\n[app]\ndefault = 'codex'\n[agents.reviewer]\ncommand = ['reviewer', '{prompt}']\nresume_command = ['reviewer', '--resume']\nenabled = true\n\
base = 'claude'\ninstructions = ['--system', '{instructions}']\nresume = ['--resume', '{session}']\nresume_latest = ['--continue']\n\
fork = ['--fork', '{session}']\nadd_dir = ['--dir', '{directory}']\nreview = ['-p', '{prompt}', '{schema}']\nenv = { MODE = 'review' }\nenv_remove = ['TOKEN']\nretry = true\n\
prompt_cache_minutes = 5\nmodel = 'opus'\neffort = 'high'\nmodel_args = ['--model', '{model}']\neffort_args = ['--effort', '{effort}']\n[agents.claude]\nretry = false\n[agents.codex]\nretry = false\n";

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

fn stack(global: &str, worktree: &str, saved: &str) -> Stack {
    let global = Config::from_toml(global).unwrap();
    let layers = ConfigLayers {
        worktree_file: parse(worktree).unwrap(),
        local_config: parse(saved).unwrap(),
        ..Default::default()
    };
    Stack::new(&global, &layers)
}

#[test]
fn former_template_files_are_not_read() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["worker-template.md", "chat-template.md"] {
        std::fs::write(directory.path().join(name), name).unwrap();
    }
    let (config, files) = config::repo::load(directory.path()).unwrap();
    assert!(config.rules.is_empty());
    assert!(files.is_empty());
}

#[test]
fn a_rule_repeating_the_guidance_warns() {
    let stack = stack(
        "rules.worker.text = 'You are working in Shoal workspace {workspace} on branch {branch} at {path}.'",
        "rules.worker.text = 'Babysit {branch}.'",
        "",
    );
    let warnings = stack.warnings();
    assert!(
        matches!(warnings, [warning] if warning.starts_with("global config: rules.worker.text repeats")),
        "{warnings:?}"
    );
}

#[test]
fn a_mode_naming_an_undefined_rule_is_refused() {
    let undefined = stack("", "[modes.triage]\nrules = ['triage']\n", "")
        .resolve()
        .unwrap();
    let error = modes::check(&undefined.modes, &undefined.rules).unwrap_err();
    assert!(error.to_string().contains("names rule triage"), "{error}");
    let settings = stack(
        "[rules.triage]\ntext = 'Label it.'\n",
        "[modes.triage]\nrules = ['triage']\n",
        "",
    )
    .resolve()
    .unwrap();
    let mode = &settings.modes["triage"];
    assert_eq!(mode.kind, crate::model::SpaceKind::Workspace);
    assert_eq!(mode.instructions, "");
    // A repository changes a built-in mode option by option.
    let settings = stack("", "[modes.plan]\nkind = 'workspace'\n", "")
        .resolve()
        .unwrap();
    assert_eq!(
        settings.modes["plan"].kind,
        crate::model::SpaceKind::Workspace
    );
    assert_eq!(settings.modes["plan"].rules, ["work", "chat"]);
}

#[test]
fn only_a_chat_mode_may_have_no_repository() {
    let settings = stack("", "", "").resolve().unwrap();
    let orchestrate = &settings.modes[modes::ModeName::ORCHESTRATE.as_str()];
    assert!(!orchestrate.repository);
    assert_eq!(orchestrate.kind, crate::model::SpaceKind::Chat);
    modes::check(&settings.modes, &settings.rules).unwrap();
    for (config, refused) in [
        ("[modes.setup]\nkind = 'chat'\nrepository = false\n", false),
        (
            "[modes.setup]\nkind = 'workspace'\nrepository = false\n",
            true,
        ),
        ("[modes.orchestrate]\nkind = 'workspace'\n", true),
    ] {
        let settings = stack(config, "", "").resolve();
        let checked = settings.and_then(|settings| modes::check(&settings.modes, &settings.rules));
        assert_eq!(checked.is_err(), refused, "{config}");
    }
}

#[test]
fn templates_append_one_repository_value_and_report_the_composed_text() {
    for key in [
        "rules.worker.text",
        "rules.chat.text",
        "rules.review.text",
        "modes.plan.instructions",
    ] {
        let default = match key {
            "rules.review.text" => Some(config::templates::review_base()),
            "modes.plan.instructions" => Some(include_str!("../modes/plan.md")),
            _ => Some(""),
        };
        let config = |value: Option<&str>| {
            value.map_or_else(String::new, |value| {
                format!("{key} = {}", toml::Value::String(value.into()))
            })
        };
        for (global, worktree, saved, expected, layer) in [
            (
                None,
                None,
                None,
                default.map(str::to_owned),
                Layer::BuiltInDefault,
            ),
            (
                Some("global"),
                None,
                None,
                Some("global".into()),
                Layer::GlobalConfig,
            ),
            (
                Some("global"),
                Some("file"),
                None,
                Some("global\n\nfile".into()),
                Layer::WorktreeFile,
            ),
            (
                Some("global"),
                Some("file"),
                Some("saved"),
                Some("global\n\nsaved".into()),
                Layer::LocalConfig,
            ),
            (
                Some("global"),
                Some("file"),
                Some(""),
                Some("global".into()),
                Layer::LocalConfig,
            ),
            (
                Some("global"),
                Some(""),
                None,
                Some("global".into()),
                Layer::WorktreeFile,
            ),
            (
                Some(""),
                Some("file"),
                None,
                Some("file".into()),
                Layer::WorktreeFile,
            ),
            (
                Some(""),
                None,
                None,
                Some(String::new()),
                Layer::GlobalConfig,
            ),
            (
                None,
                Some("file"),
                None,
                Some(default.filter(|base| !base.is_empty()).map_or_else(
                    || "file".into(),
                    |base| format!("{}\n\nfile", base.trim_end()),
                )),
                Layer::WorktreeFile,
            ),
            (
                None,
                Some("file"),
                Some(""),
                default.map(str::to_owned).or(Some(String::new())),
                Layer::LocalConfig,
            ),
        ] {
            let stack = stack(&config(global), &config(worktree), &config(saved));
            let effective = json(&stack.clone().resolve().unwrap());
            let pointer = format!("/{}", key.replace('.', "/"));
            assert_eq!(
                effective.pointer(&pointer).unwrap(),
                &json(&expected),
                "{key}: {global:?}, {worktree:?}, {saved:?}"
            );
            let entry = stack
                .report()
                .unwrap()
                .into_iter()
                .find(|entry| entry.key == key)
                .unwrap();
            assert_eq!(&entry.value, effective.pointer(&pointer).unwrap());
            assert_eq!(entry.layer, layer);
        }
    }
}

#[test]
fn a_template_reports_the_file_of_the_layer_it_came_from() {
    let file = |path: &str| config::modes::Files::from([("rules.worker.text".into(), path.into())]);
    let mut global = Config::from_toml("").unwrap();
    global
        .repository
        .rules
        .entry("worker".into())
        .or_default()
        .text = Some("Global".into());
    global.template_files = file("/global/rules/worker.md");
    let report = |layers: &ConfigLayers| {
        Stack::new(&global, layers)
            .report()
            .unwrap()
            .into_iter()
            .map(|entry| (entry.key, (entry.layer, entry.file)))
            .collect::<BTreeMap<_, _>>()
    };
    let mut layers = ConfigLayers::default();
    let entries = report(&layers);
    assert_eq!(
        entries["rules.worker.text"],
        (Layer::GlobalConfig, Some("/global/rules/worker.md".into()))
    );
    assert_eq!(entries["rules.chat.text"], (Layer::BuiltInDefault, None));
    // An inline value is not from a file.
    layers.worktree_file = parse("rules.worker.text = 'Worktree'").unwrap();
    assert_eq!(
        report(&layers)["rules.worker.text"],
        (Layer::WorktreeFile, None)
    );
    layers.worktree_template_files = file("/worktree/.shoal/rules/worker.md");
    assert_eq!(
        report(&layers)["rules.worker.text"],
        (
            Layer::WorktreeFile,
            Some("/worktree/.shoal/rules/worker.md".into())
        )
    );
}

#[test]
fn herdr_options_resolve_independently() {
    let settings = stack(
        "[terminal.herdr]\nenabled = false\nnew_tab = false\nworkspace_name = 'work'\n",
        "[terminal.herdr]\nnew_tab = true\nclose_when_done = false\n",
        "",
    )
    .resolve()
    .unwrap()
    .terminal
    .herdr;
    assert!(!settings.enabled && settings.new_tab);
    assert_eq!(settings.close_when_done, Some(false));
    assert_eq!(settings.workspace_name, "work");
}

#[test]
fn rex_options_resolve_independently() {
    let settings = stack(
        "terminal.default_host = 'rex'\n[terminal.rex]\nenabled = false\nsession_name = 'work'\n",
        "[terminal.rex]\nenabled = true\nplacement = 'split'\nclose_when_done = false\n",
        "",
    )
    .resolve()
    .unwrap();
    assert_eq!(
        settings.terminal.default_host,
        crate::sessions::HostName::Rex
    );
    assert!(settings.terminal.rex.enabled);
    assert_eq!(settings.terminal.rex.session_name, "work");
    assert_eq!(
        settings.terminal.rex.placement,
        config::repo::Placement::Split
    );
    assert_eq!(settings.terminal.rex.close_when_done, Some(false));
    let herdr = stack("terminal.default_host = 'herdr'\n", "", "")
        .resolve()
        .unwrap();
    assert_eq!(
        herdr.terminal.default_host,
        crate::sessions::HostName::Herdr
    );
    assert!(Config::from_toml("terminal.default_host = 'screen'\n").is_err());
    assert!(Config::from_toml("terminal.default_host = 'terminal'\n").is_err());
    assert!(Config::from_toml("[terminal.rex]\nplacement = 'tab'\n").is_err());
}

#[test]
fn a_later_layer_turns_closing_places_back_on() {
    assert!(
        !stack("terminal.close_when_done = false\n", "", "")
            .resolve()
            .unwrap()
            .terminal
            .close_when_done
    );
    assert!(
        stack(
            "terminal.close_when_done = false\n",
            "",
            "terminal.close_when_done = true\n"
        )
        .resolve()
        .unwrap()
        .terminal
        .close_when_done
    );
}

#[test]
fn presets_come_from_the_global_config_unless_the_repository_names_its_own() {
    use crate::config::presets::Preset;
    let presets = |global, worktree, saved| {
        stack(global, worktree, saved)
            .resolve()
            .unwrap()
            .setup
            .presets
            .unwrap_or_default()
    };
    assert!(presets("", "", "").is_empty());
    assert_eq!(
        presets("setup.presets = ['xcode']\n", "", ""),
        [Preset::Xcode]
    );
    assert_eq!(
        presets(
            "setup.presets = ['xcode']\n",
            "setup.presets = ['cargo']\n",
            ""
        ),
        [Preset::Cargo]
    );
    assert_eq!(
        presets(
            "setup.presets = ['xcode']\n",
            "setup.presets = ['cargo']\n",
            "setup.presets = ['bun']\n"
        ),
        [Preset::Bun]
    );
    // An empty list turns the inherited presets off.
    assert!(presets("setup.presets = ['xcode']\n", "setup.presets = []\n", "").is_empty());
}

#[test]
fn focus_layers_per_repository() {
    assert_eq!(
        stack("terminal.focus = false\n", "", "")
            .resolve()
            .unwrap()
            .terminal
            .focus,
        Some(false)
    );
    assert_eq!(
        stack("terminal.focus = false\n", "", "terminal.focus = true\n")
            .resolve()
            .unwrap()
            .terminal
            .focus,
        Some(true)
    );
}

#[test]
fn herdr_tab_names_resolve_and_report_the_winning_layer() {
    for (global, worktree, saved, expected, layer) in [
        ("", "", "", None, Layer::BuiltInDefault),
        (
            "[terminal.herdr]\ntab_name = '{branch}'",
            "",
            "",
            Some("{branch}"),
            Layer::GlobalConfig,
        ),
        (
            "[terminal.herdr]\ntab_name = '{branch}'",
            "[terminal.herdr]\ntab_name = '{repo}'",
            "[terminal.herdr]\nnew_tab = true",
            Some("{repo}"),
            Layer::WorktreeFile,
        ),
        (
            "[terminal.herdr]\ntab_name = '{branch}'",
            "[terminal.herdr]\ntab_name = '{repo}'",
            "[terminal.herdr]\ntab_name = 'Work'",
            Some("Work"),
            Layer::LocalConfig,
        ),
    ] {
        let stack = stack(global, worktree, saved);
        assert_eq!(
            stack
                .clone()
                .resolve()
                .unwrap()
                .terminal
                .herdr
                .tab_name
                .as_deref(),
            expected
        );
        let entry = stack
            .report()
            .unwrap()
            .into_iter()
            .find(|entry| entry.key == "terminal.herdr.tab_name")
            .unwrap();
        assert_eq!(entry.value, json(&expected));
        assert_eq!(entry.layer, layer);
    }
}

#[test]
fn every_option_survives_layering_in_both_directions() {
    // Read as a layer, not a repository's file, which refuses a mode's
    // `repository` that the global layer takes.
    let full: RepoConfig = toml::from_str(FULL).unwrap();
    assert_eq!(json(&full.clone().over(RepoConfig::default())), json(&full));
    assert_eq!(json(&RepoConfig::default().over(full.clone())), json(&full));
    // The same values reported from any single layer name that layer.
    for (index, layer) in [
        (1, Layer::GlobalConfig),
        (2, Layer::WorktreeFile),
        (3, Layer::LocalConfig),
    ] {
        let mut stack = stack("", "", "");
        stack.layers[index].0 = full.clone();
        for entry in stack.report().unwrap() {
            // The fixture leaves the built-in agents' own settings alone.
            let built_in_agent = entry.key.strip_prefix("agents.").is_some_and(|key| {
                named_commands::defaults()
                    .keys()
                    .any(|name| key.starts_with(&format!("{name}.")))
            });
            if !built_in_agent {
                assert_eq!(entry.layer, layer, "{}", entry.key);
            }
        }
    }
}

#[test]
fn reported_provenance_names_the_layer_whose_value_is_in_effect() {
    let cases = [
        ("", "", ""),
        // Explicit defaults win their layer; omitted values fall through.
        (
            "[cleanup.auto]\nenabled = true\n[agents.codex]\nretry = false\n",
            "[cleanup.auto]\nidle_minutes = 5\n",
            "",
        ),
        // Partial nested tables merge per option across all three layers.
        (
            "[ports]\nstart = 3000\nend = 4000\n[commands]\nreview = ['global']\ncheck = ['check']\n",
            "[ports]\nstart = 3500\n[ports.web]\nport = 3600\n[commands]\nreview = ['file']\n",
            "[ports]\nend = 3900\n[ports.web]\nenv = 'WEB'\n[commands]\nreview = ['saved']\n\
                 [simulators]\npreferred = ['tablet']\n[agent]\ngit_profile = 'saved'\n",
        ),
        (
            "[agent]\ngit_profile = 'global'\n[resources.lock]\ncapacity = 1\n",
            "[simulators]\npreferred = ['phone']\nrequires_approval = true\n",
            "[resources.lock]\ncapacity = 3\n[cleanup.pr]\nenabled = false\n",
        ),
    ];
    for (global, worktree, saved) in cases {
        let stack = stack(global, worktree, saved);
        let effective = json(&stack.clone().resolve().unwrap());
        let entries = stack.report().unwrap();
        assert!(!entries.is_empty());
        for entry in &entries {
            let pointer = format!("/{}", entry.key.replace('.', "/"));
            let in_effect = effective.pointer(&pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                entry.value, in_effect,
                "{global}|{worktree}|{saved}: {pointer}"
            );
            // Nothing below the reported layer decides the value: the
            // same stack without the higher layers reports it unchanged.
            let layer_of = |layer: Layer| stack.layers.iter().position(|(_, l)| *l == layer);
            let mut lower = stack.clone();
            for (config, layer) in &mut lower.layers {
                if layer_of(*layer) > layer_of(entry.layer) {
                    *config = RepoConfig::default();
                }
            }
            let below = lower.report().unwrap();
            let same = below.iter().find(|e| e.key == entry.key).unwrap();
            assert_eq!(
                (&same.value, same.layer),
                (&entry.value, entry.layer),
                "{pointer}"
            );
        }
    }
    let entries = stack(
        "[cleanup.auto]\nenabled = true\n",
        "[cleanup.auto]\nidle_minutes = 5\n",
        "",
    )
    .report()
    .unwrap();
    let entry = |key: &str| entries.iter().find(|e| e.key == key).unwrap();
    assert_eq!(entry("cleanup.auto.enabled").layer, Layer::GlobalConfig);
    assert_eq!(
        entry("cleanup.auto.idle_minutes").layer,
        Layer::WorktreeFile
    );
    assert_eq!(entry("cleanup.pr.enabled").layer, Layer::BuiltInDefault);
}

#[test]
fn conflicting_ranges_fail_after_the_layers_combine() {
    for (global, worktree, saved) in [
        (
            "[ports]\nstart = 3000\nend = 3100\n",
            "",
            "[ports]\nstart = 3200\n",
        ),
        (
            "[ports]\nstart = 3000\nend = 3100\n",
            "[ports]\nend = 2000\n",
            "",
        ),
        ("", "[ports]\nstart = 5000\n", "[ports]\nend = 4000\n"),
        (
            "",
            "[resource_pools.lock]\ncapacity = 2\n[resource_pools.lock.resources.a]\ncapacity = 1\n",
            "[resources.lock]\ncapacity = 1\n",
        ),
    ] {
        let stack = stack(global, worktree, saved);
        assert!(
            stack.clone().resolve().is_err(),
            "{global}|{worktree}|{saved}"
        );
        assert!(stack.report().is_err(), "{global}|{worktree}|{saved}");
    }
    let split = stack(
        "[ports]\nstart = 3000\nend = 3100\n",
        "",
        "[ports]\nstart = 3050\n",
    );
    let ports = split.resolve().unwrap().ports;
    assert_eq!((ports.start, ports.end), (3050, 3100));
    // A repository name shadowing a global one is checked at allocation,
    // so it never blocks the settings that hooks and removal read.
    let shadowed = stack(
        "[resource_pools.lock]\ncapacity = 2\n[resource_pools.lock.resources.a]\ncapacity = 1\n",
        "[resources.lock]\ncapacity = 1\n",
        "",
    );
    assert!(shadowed.resolve().is_ok());
}

#[test]
fn post_setup_and_pre_remove_hooks_use_global_defaults_below_repository_layers() {
    for kind in [HookKind::PostSetup, HookKind::PreRemove] {
        let key = kind.key();
        let global = format!("{key} = 'global/hook'\n");
        let worktree = format!("{key} = 'file/hook'\n");
        let saved = format!("{key} = 'saved/hook'\n");
        for (global, worktree, saved, expected, layer) in [
            ("", "", "", None, Layer::BuiltInDefault),
            (
                global.as_str(),
                "",
                "",
                Some("global/hook"),
                Layer::GlobalConfig,
            ),
            (
                global.as_str(),
                worktree.as_str(),
                "",
                Some("file/hook"),
                Layer::WorktreeFile,
            ),
            (
                global.as_str(),
                worktree.as_str(),
                saved.as_str(),
                Some("saved/hook"),
                Layer::LocalConfig,
            ),
        ] {
            let stack = stack(global, worktree, saved);
            let settings = stack.clone().resolve().unwrap();
            assert_eq!(kind.command(&settings).map(String::as_str), expected);
            assert!(settings.setup.command.is_none());
            assert!(settings.hooks.pre_setup.is_none());
            let entries = stack.report().unwrap();
            let entry = entries.iter().find(|entry| entry.key == key).unwrap();
            assert_eq!(entry.value, serde_json::json!(expected));
            assert_eq!(entry.layer, layer);
        }
    }
}

#[test]
fn hooks_resolve_by_kind_through_the_settings() {
    let settings = stack(
        "hooks.pre_setup = 'global/pre'\nhooks.post_remove = 'global/remove'\n",
        "hooks.post_setup = 'scripts/attach.sh'\nhooks.pre_setup = 'file/pre'\n",
        "hooks.pre_remove = '/opt/detach'\n",
    )
    .resolve()
    .unwrap();
    for (kind, expected) in [
        (HookKind::Setup, None),
        (HookKind::PreSetup, Some("file/pre")),
        (HookKind::PostSetup, Some("scripts/attach.sh")),
        (HookKind::PreRemove, Some("/opt/detach")),
        (HookKind::PostRemove, Some("global/remove")),
        (HookKind::PostResourceAcquire, None),
    ] {
        assert_eq!(
            kind.command(&settings).map(String::as_str),
            expected,
            "{kind:?}"
        );
    }
}

#[test]
fn environment_values_layer_and_report_each_name() {
    let layered = stack(
        "[env]\nGLOBAL = 'global'\nSESSION = 'global'\nEMPTY = 'global'",
        "[env]\nFILE = 'file'\nSESSION = 'file'",
        "[env]\nSESSION = '{workspace}'\nEMPTY = ''",
    );
    let effective = layered.clone().resolve().unwrap();
    assert_eq!(
        effective.env,
        BTreeMap::from([
            ("GLOBAL".into(), "global".into()),
            ("FILE".into(), "file".into()),
            ("SESSION".into(), "{workspace}".into()),
            ("EMPTY".into(), "".into()),
        ])
    );
    let entries = layered.report().unwrap();
    for (name, value, layer) in [
        ("GLOBAL", "global", Layer::GlobalConfig),
        ("FILE", "file", Layer::WorktreeFile),
        ("SESSION", "{workspace}", Layer::LocalConfig),
        ("EMPTY", "", Layer::LocalConfig),
    ] {
        let entry = entries
            .iter()
            .find(|entry| entry.key == format!("env.{name}"))
            .unwrap();
        assert_eq!(entry.value, value);
        assert_eq!(entry.layer, layer);
    }
    assert!(stack("", "", "").resolve().unwrap().env.is_empty());
}

#[test]
fn named_tables_report_replacement_by_name() {
    let stack = stack(
        "[commands]\nreview = ['global']\ncheck = ['check']\n",
        "[commands]\nreview = ['file']\nlint = ['lint']\n",
        "[commands]\nreview = ['saved', 'two words']\n",
    );
    let entries = stack.report().unwrap();
    let command = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.key == format!("commands.{name}"))
            .map(|entry| (entry.value.clone(), entry.layer))
            .unwrap()
    };
    assert_eq!(
        command("review"),
        (json(&["saved", "two words"]), Layer::LocalConfig)
    );
    assert_eq!(command("check").1, Layer::GlobalConfig);
    assert_eq!(command("lint").1, Layer::WorktreeFile);
    let agent_command = entries
        .iter()
        .find(|entry| entry.key == "agents.codex.command")
        .unwrap();
    assert_eq!(agent_command.layer, Layer::BuiltInDefault);
    let commands = stack.named(|config| &config.commands);
    for (name, (value, layer)) in &commands {
        assert_eq!((json(value), *layer), command(name), "{name}");
    }
    // Only the configured names: agents' launchers sit under [agents].
    assert_eq!(commands.len(), 3);
    let effective = stack.resolve().unwrap();
    assert_eq!(effective.commands["review"], ["saved", "two words"]);
    assert_eq!(effective.commands["check"], ["check"]);
    assert!(!effective.commands.contains_key("claude"));
}
