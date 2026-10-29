use super::*;
use crate::config::repo::parse;

/// Every option set, so a field missing from the table shows up as a
/// value that does not survive layering. Adding a field to `RepoConfig`
/// fails the destructuring below until the fixture states it too.
const FULL: &str = "\
worker_template = 'agent'\nchat_template = 'chat'\ngit_profile = 'work'\n\
default_agent = 'claude'\nmax_agents = 2\nfocus = false\nclose_when_done = false\ndefault_host = 'rex'\ncopy_ignored = false\nagent_dirs = ['/tmp', '~/cache']\nsetup_cmd = 'setup'\npre_setup_cmd = 'pre-setup'\n\
post_remove_cmd = 'post-remove'\npost_done_cmd = 'post-done'\npost_ready_cmd = 'post-ready'\npost_agent_exit_cmd = 'agent-exit'\npost_resource_acquire_cmd = 'acquire'\n\
pre_resource_release_cmd = 'release'\npost_setup_cmd = 'attach'\npre_remove_cmd = 'detach'\npresets = ['cargo']\n\
[env]\nSESSION = '{workspace}'\n[caches.go]\nscope = 'shared'\n[demos.web]\npreset = 'web'\ncmd = ['npm', 'run', 'dev']\n[secrets.TOKEN]\ncommand = ['token']\n[commands]\nreview = ['review']\n[agent_resume]\nreview = ['review', '--resume']\n[agent_auth]\nfj = '/fj'\nfj_home = '/fj-home'\ngh = '/gh'\nglab = '/glab'\ntea = '/tea'\ngit_profile = 'agent'\n[claude]\ndefault_mode = 'app'\n[codex]\ndefault_mode = 'app'\n[herdr]\nenabled = false\ntab_name = '{branch}'\nnew_tab = false\nworkspace_name = 'work'\nclose_when_done = false\n[rex]\nenabled = false\nnew_window = false\nplacement = 'split'\nwindow_name = '{branch}'\nsession_name = 'work'\nclose_when_done = false\n[happy]\nenabled = true\n\
[ports]\non_conflict = 'auto'\nstart = 3000\nend = 3100\n[ports.web]\nport = 3000\n\
[resources.lock]\ncapacity = 1\n[resource_pools.devices]\ncapacity = 2\n\
[resource_pools.devices.resources.phone]\ncapacity = 1\n\
[simulators]\nrequires_approval = true\napproval_lifetime = 'workspace'\npreferred = ['phone']\n\
[cleanup]\ngrace_minutes = 5\n[cleanup.auto]\nenabled = false\nidle_minutes = 30\n[cleanup.pr]\nenabled = false\n[cleanup.merged]\nenabled = false\nidle_minutes = 60\n[cleanup.restore]\nretention_days = 7\narchive_max_mb = 5\n[done]\ncleanup = false\n[idle_stop]\nenabled = true\n[review]\npost = false\n[ready]\nundraft = false\n[unready]\ndraft = true\n[pr]\ndraft = true\nrequired_labels = ['area/']\n[issue]\nrequired_labels = ['type/']\nother_repositories = 'direct'\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n[land]\npush = true\nrequires_approval = false\n\
[approvals]\nissue_open = 'propose'\npr_open = 'propose'\n[api]\nagent_writes = true\n[seed]\ncmd = 'scripts/seed'\npaths = ['target']\n";

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

fn stack(global: &str, worktree: &str, saved: &str) -> Stack {
    let global: Config = toml::from_str(global).unwrap();
    let layers = ConfigLayers {
        worktree_file: parse(worktree).unwrap(),
        saved_repository_config: parse(saved).unwrap(),
    };
    Stack::new(&global, &layers)
}

#[test]
fn a_former_template_name_still_layers_and_warns() {
    let stack = stack(
        "agent_template = 'global'",
        "agent_template = 'file'",
        "issue_template = 'Plan first.'",
    );
    assert_eq!(
        stack.clone().resolve().unwrap().worker_template.as_deref(),
        Some("global\n\nfile")
    );
    assert_eq!(
        stack.warnings(),
        [
            "global config: agent_template and agent-template.md are now named \
                 worker_template and worker-template.md",
            "worktree file: agent_template and agent-template.md are now named \
                 worker_template and worker-template.md",
            "saved repository config: issue_template and issue-template.md are no \
                 longer used; move their instructions to worker_template",
        ]
    );
    let both = "worker_template = 'new'\nagent_template = 'old'";
    assert!(parse(both).is_err());
    assert!(Config::parse(both, &crate::paths::Paths::for_test("/home/test")).is_err());

    let directory = tempfile::tempdir().unwrap();
    for name in [
        config::templates::WORKER_FILE,
        config::templates::RENAMED_AGENT_FILE,
    ] {
        std::fs::write(directory.path().join(name), name).unwrap();
    }
    assert!(config::repo::load(directory.path()).is_err());
}

#[test]
fn a_worker_template_repeating_the_guidance_warns() {
    let stack = stack(
        "worker_template = 'You are working in Shoal workspace {workspace} on branch {branch} at {path}.'",
        "worker_template = 'Babysit {branch}.'",
        "",
    );
    assert_eq!(
        stack.warnings(),
        [
            "global config: worker_template repeats Shoal's built-in guidance, which \
              every launch already receives; keep only your additions"
        ]
    );
}

#[test]
fn templates_append_one_repository_value_and_report_the_composed_text() {
    for key in ["worker_template", "chat_template"] {
        let default = match key {
            "chat_template" => Some(config::templates::CHAT_DEFAULT),
            _ => None,
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
                Layer::SavedRepositoryConfig,
            ),
            (
                Some("global"),
                Some("file"),
                Some(""),
                Some("global".into()),
                Layer::SavedRepositoryConfig,
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
                Some(default.map_or_else(|| "file".into(), |base| format!("{base}\n\nfile"))),
                Layer::WorktreeFile,
            ),
            (
                None,
                Some("file"),
                Some(""),
                default.map(str::to_owned).or(Some(String::new())),
                Layer::SavedRepositoryConfig,
            ),
        ] {
            let stack = stack(&config(global), &config(worktree), &config(saved));
            let effective = json(&stack.clone().resolve().unwrap());
            assert_eq!(
                effective[key],
                json(&expected),
                "{key}: {global:?}, {worktree:?}, {saved:?}"
            );
            let entry = stack
                .report()
                .unwrap()
                .into_iter()
                .find(|entry| entry.key == key)
                .unwrap();
            assert_eq!(entry.value, effective[key]);
            assert_eq!(entry.layer, layer);
        }
    }
}

#[test]
fn herdr_defaults_and_options_resolve_independently() {
    let defaults = stack("", "", "").resolve().unwrap().herdr;
    assert!(defaults.enabled && defaults.new_tab);
    assert_eq!(defaults.close_when_done, None);
    assert_eq!(defaults.tab_name, None);
    assert_eq!(defaults.workspace_name, "{repo}");
    let settings = stack(
        "[herdr]\nenabled = false\nnew_tab = false\nworkspace_name = 'work'\n",
        "[herdr]\nnew_tab = true\nclose_when_done = false\n",
        "",
    )
    .resolve()
    .unwrap()
    .herdr;
    assert!(!settings.enabled && settings.new_tab);
    assert_eq!(settings.close_when_done, Some(false));
    assert_eq!(settings.workspace_name, "work");
}

#[test]
fn rex_defaults_and_options_resolve_independently() {
    let defaults = stack("", "", "").resolve().unwrap();
    assert_eq!(defaults.default_host, crate::sessions::HostName::Zmx);
    let rex = defaults.rex;
    assert!(rex.enabled && rex.new_window);
    assert_eq!(rex.placement, config::repo::RexPlacement::Window);
    assert_eq!(rex.session_name, "{repo}");
    assert_eq!((rex.window_name, rex.close_when_done), (None, None));
    let settings = stack(
        "default_host = 'rex'\n[rex]\nenabled = false\nsession_name = 'work'\n",
        "[rex]\nenabled = true\nplacement = 'split'\nclose_when_done = false\n",
        "",
    )
    .resolve()
    .unwrap();
    assert_eq!(settings.default_host, crate::sessions::HostName::Rex);
    assert!(settings.rex.enabled);
    assert_eq!(settings.rex.session_name, "work");
    assert_eq!(settings.rex.placement, config::repo::RexPlacement::Split);
    assert_eq!(settings.rex.close_when_done, Some(false));
    let herdr = stack("default_host = 'herdr'\n", "", "").resolve().unwrap();
    assert_eq!(herdr.default_host, crate::sessions::HostName::Herdr);
    assert!(toml::from_str::<Config>("default_host = 'tmux'\n").is_err());
    assert!(toml::from_str::<Config>("default_host = 'terminal'\n").is_err());
    assert!(toml::from_str::<Config>("[rex]\nplacement = 'tab'\n").is_err());
}

#[test]
fn places_close_when_done_unless_a_layer_turns_it_off() {
    assert!(stack("", "", "").resolve().unwrap().close_when_done);
    assert!(
        !stack("close_when_done = false\n", "", "")
            .resolve()
            .unwrap()
            .close_when_done
    );
    assert!(
        stack("close_when_done = false\n", "", "close_when_done = true\n")
            .resolve()
            .unwrap()
            .close_when_done
    );
}

#[test]
fn presets_come_from_the_global_config_unless_the_repository_names_its_own() {
    use crate::presets::Preset;
    let presets = |global, worktree, saved| {
        stack(global, worktree, saved)
            .resolve()
            .unwrap()
            .presets
            .unwrap_or_default()
    };
    assert!(presets("", "", "").is_empty());
    assert_eq!(presets("presets = ['xcode']\n", "", ""), [Preset::Xcode]);
    assert_eq!(
        presets("presets = ['xcode']\n", "presets = ['cargo']\n", ""),
        [Preset::Cargo]
    );
    assert_eq!(
        presets(
            "presets = ['xcode']\n",
            "presets = ['cargo']\n",
            "presets = ['bun']\n"
        ),
        [Preset::Bun]
    );
    // An empty list turns the inherited presets off.
    assert!(presets("presets = ['xcode']\n", "presets = []\n", "").is_empty());
}

#[test]
fn focus_is_unset_by_default_and_layers_per_repository() {
    assert_eq!(stack("", "", "").resolve().unwrap().focus, None);
    assert_eq!(
        stack("focus = false\n", "", "").resolve().unwrap().focus,
        Some(false)
    );
    assert_eq!(
        stack("focus = false\n", "", "focus = true\n")
            .resolve()
            .unwrap()
            .focus,
        Some(true)
    );
}

#[test]
fn herdr_tab_names_resolve_and_report_the_winning_layer() {
    for (global, worktree, saved, expected, layer) in [
        ("", "", "", None, Layer::BuiltInDefault),
        (
            "[herdr]\ntab_name = '{branch}'",
            "",
            "",
            Some("{branch}"),
            Layer::GlobalConfig,
        ),
        (
            "[herdr]\ntab_name = '{branch}'",
            "[herdr]\ntab_name = '{repo}'",
            "[herdr]\nnew_tab = true",
            Some("{repo}"),
            Layer::WorktreeFile,
        ),
        (
            "[herdr]\ntab_name = '{branch}'",
            "[herdr]\ntab_name = '{repo}'",
            "[herdr]\ntab_name = 'Work'",
            Some("Work"),
            Layer::SavedRepositoryConfig,
        ),
    ] {
        let stack = stack(global, worktree, saved);
        assert_eq!(
            stack.clone().resolve().unwrap().herdr.tab_name.as_deref(),
            expected
        );
        let entry = stack
            .report()
            .unwrap()
            .into_iter()
            .find(|entry| entry.key == "herdr.tab_name")
            .unwrap();
        assert_eq!(entry.value, json(&expected));
        assert_eq!(entry.layer, layer);
    }
}

#[test]
fn every_option_survives_layering_in_both_directions() {
    let full = parse(FULL).unwrap();
    let RepoConfig {
        commands,
        agent_resume,
        // Retired, so no layer passes it on.
        retired_issue_template: _,
        // Moved to `worker_template` before layering.
        renamed_agent_template: _,
        worker_template,
        chat_template,
        agent_auth,
        git_profile,
        default_agent,
        max_agents,
        focus,
        close_when_done,
        default_host,
        claude,
        codex,
        herdr,
        rex,
        happy,
        copy_ignored,
        agent_dirs,
        setup_cmd,
        pre_setup_cmd,
        post_remove_cmd,
        post_done_cmd,
        post_ready_cmd,
        post_agent_exit_cmd,
        post_resource_acquire_cmd,
        pre_resource_release_cmd,
        post_setup_cmd,
        pre_remove_cmd,
        ports,
        env,
        caches,
        demos,
        presets,
        secrets,
        resources,
        resource_pools,
        simulators,
        cleanup,
        // Parsing folds the former names into `cleanup`.
        auto_cleanup: _,
        pr_cleanup: _,
        done,
        idle_stop,
        review,
        ready,
        unready,
        pr,
        issue,
        land,
        approvals,
        api,
        seed,
    } = full.clone();
    assert!(
        !commands.is_empty()
            && !env.is_empty()
            && !caches.is_empty()
            && !demos.is_empty()
            && !secrets.is_empty()
            && !agent_resume.is_empty()
            && !resources.is_empty()
            && !resource_pools.is_empty()
    );
    assert!(
        !ports.definitions.is_empty() && !simulators.preferred.is_empty() && !seed.paths.is_empty()
    );
    for present in [
        worker_template.is_some(),
        chat_template.is_some(),
        agent_auth.fj.is_some(),
        agent_auth.fj_home.is_some(),
        agent_auth.gh.is_some(),
        agent_auth.glab.is_some(),
        agent_auth.tea.is_some(),
        agent_auth.git_profile.is_some(),
        git_profile.is_some(),
        default_agent.is_some(),
        max_agents.is_some(),
        focus.is_some(),
        close_when_done.is_some(),
        claude.default_mode.is_some(),
        codex.default_mode.is_some(),
        herdr.enabled.is_some(),
        herdr.tab_name.is_some(),
        herdr.new_tab.is_some(),
        herdr.workspace_name.is_some(),
        herdr.close_when_done.is_some(),
        default_host.is_some(),
        rex.enabled.is_some(),
        rex.new_window.is_some(),
        rex.placement.is_some(),
        rex.window_name.is_some(),
        rex.session_name.is_some(),
        rex.close_when_done.is_some(),
        happy.enabled.is_some(),
        copy_ignored.is_some(),
        agent_dirs.is_some(),
        setup_cmd.is_some(),
        pre_setup_cmd.is_some(),
        post_remove_cmd.is_some(),
        post_done_cmd.is_some(),
        post_ready_cmd.is_some(),
        post_agent_exit_cmd.is_some(),
        post_resource_acquire_cmd.is_some(),
        pre_resource_release_cmd.is_some(),
        post_setup_cmd.is_some(),
        pre_remove_cmd.is_some(),
        ports.on_conflict.is_some(),
        ports.start.is_some(),
        ports.end.is_some(),
        simulators.requires_approval.is_some(),
        simulators.approval_lifetime.is_some(),
        cleanup.grace_minutes.is_some(),
        cleanup.auto.enabled.is_some(),
        cleanup.auto.idle_minutes.is_some(),
        cleanup.pr.enabled.is_some(),
        cleanup.merged.enabled.is_some(),
        cleanup.merged.idle_minutes.is_some(),
        cleanup.restore.retention_days.is_some(),
        cleanup.restore.archive_max_mb.is_some(),
        done.cleanup.is_some(),
        idle_stop.enabled.is_some(),
        review.post.is_some(),
        ready.undraft.is_some(),
        unready.draft.is_some(),
        pr.draft.is_some(),
        pr.required_labels.is_some(),
        issue.required_labels.is_some(),
        issue.other_repositories.is_some(),
        issue.plan_label.is_some(),
        issue.planned_label.is_some(),
        land.push.is_some(),
        land.requires_approval.is_some(),
        approvals.issue_open.is_some(),
        approvals.pr_open.is_some(),
        api.agent_writes.is_some(),
        seed.cmd.is_some(),
        presets.is_some(),
    ] {
        assert!(present, "the fixture must set every option");
    }
    assert_eq!(json(&full.clone().over(RepoConfig::default())), json(&full));
    assert_eq!(json(&RepoConfig::default().over(full.clone())), json(&full));
    // The same values reported from any single layer name that layer.
    for (index, layer) in [
        (1, Layer::GlobalConfig),
        (2, Layer::WorktreeFile),
        (3, Layer::SavedRepositoryConfig),
    ] {
        let mut stack = stack("", "", "");
        stack.layers[index].0 = full.clone();
        for entry in stack.report().unwrap() {
            let built_in_command = entry
                .key
                .strip_prefix("commands.")
                .is_some_and(|name| named_commands::defaults().contains_key(name));
            if !built_in_command {
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
            "[cleanup.auto]\nenabled = true\n[codex]\ndefault_mode = 'cli'\n",
            "[cleanup.auto]\nidle_minutes = 5\n",
            "",
        ),
        // Partial nested tables merge per option across all three layers.
        (
            "[ports]\nstart = 3000\nend = 4000\n[commands]\nreview = ['global']\ncheck = ['check']\n",
            "[ports]\nstart = 3500\n[ports.web]\nport = 3600\n[commands]\nreview = ['file']\n",
            "[ports]\nend = 3900\n[ports.web]\nenv = 'WEB'\n[commands]\nreview = ['saved']\n\
                 [simulators]\npreferred = ['tablet']\n[agent_auth]\nfj = '/saved/fj'\n",
        ),
        (
            "[agent_auth]\ngh = '/global/gh'\n[resources.lock]\ncapacity = 1\n",
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
    assert_eq!(entry("codex.default_mode").layer, Layer::BuiltInDefault);
    assert_eq!(entry("codex.default_mode").value, "cli");
    assert_eq!(entry("setup_cmd").value, Value::Null);
    assert_eq!(
        entry("simulators.preferred").value,
        json(&Vec::<String>::new())
    );
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
                Layer::SavedRepositoryConfig,
            ),
        ] {
            let stack = stack(global, worktree, saved);
            let settings = stack.clone().resolve().unwrap();
            assert_eq!(kind.command(&settings).map(String::as_str), expected);
            assert!(settings.setup_cmd.is_none());
            assert!(settings.pre_setup_cmd.is_none());
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
        "pre_setup_cmd = 'global/pre'\npost_remove_cmd = 'global/remove'\n",
        "post_setup_cmd = 'scripts/attach.sh'\npre_setup_cmd = 'file/pre'\n",
        "pre_remove_cmd = '/opt/detach'\n",
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
        ("SESSION", "{workspace}", Layer::SavedRepositoryConfig),
        ("EMPTY", "", Layer::SavedRepositoryConfig),
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
        (json(&["saved", "two words"]), Layer::SavedRepositoryConfig)
    );
    assert_eq!(command("check").1, Layer::GlobalConfig);
    assert_eq!(command("lint").1, Layer::WorktreeFile);
    assert_eq!(command("codex").1, Layer::BuiltInDefault);
    let commands = stack.named(|config| &config.commands);
    for (name, (value, layer)) in &commands {
        assert_eq!((json(value), *layer), command(name), "{name}");
    }
    // Three configured names plus every built-in provider's launcher.
    assert_eq!(commands.len(), 3 + crate::ai::PROVIDERS.len());
    let effective = stack.resolve().unwrap();
    assert_eq!(effective.commands["review"], ["saved", "two words"]);
    assert_eq!(effective.commands["check"], ["check"]);
    assert!(effective.commands.contains_key("claude"));
}
