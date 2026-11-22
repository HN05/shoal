use super::*;
use crate::time::UnixTime;
use crate::{
    config::repo::ConfigLayers,
    model::Chat,
    protocol::{Operation, RemovalMethod},
};

#[tokio::test(start_paused = true)]
async fn live_lookups_keep_the_interactive_budget() {
    let started = tokio::time::Instant::now();
    let error = live(std::future::pending::<Result<()>>())
        .await
        .unwrap_err();
    assert_eq!(started.elapsed(), Duration::from_millis(500));
    assert!(error.to_string().contains("timed out"), "{error}");
    assert_eq!(live(async { Ok("candidate") }).await.unwrap(), "candidate");
    assert_eq!(
        live::<()>(async { anyhow::bail!("discovery failed") })
            .await
            .unwrap_err()
            .to_string(),
        "discovery failed"
    );
}

#[test]
fn a_repositorys_own_agents_complete_as_agents() {
    let layers = ConfigLayers {
        worktree_file: crate::config::repo::parse("[agents.repo-only]\ncommand = ['repo-agent']\n")
            .unwrap(),
        ..Default::default()
    };
    let settings = layered_settings(&Config::default(), &layers);
    assert!(settings.agents.iter().any(|agent| agent == "repo-only"));
    assert_eq!(settings.launchers["repo-only"], ["repo-agent"]);
    // Agents are not commands.
    assert!(!settings.commands.contains_key("repo-only"));
    let global = layered_settings(&Config::default(), &ConfigLayers::default());
    assert!(!global.agents.iter().any(|agent| agent == "repo-only"));
}

#[test]
fn resume_completes_resumable_spaces_and_restore_removed_ones() {
    assert_eq!(target_of("space", "", "resume"), Some(Target::ResumeSpaces));
    assert_eq!(
        target_of("space", "", "restore"),
        Some(Target::RemovedSpaces)
    );
    let chat = |name: &str, running| Chat {
        narrowing: Default::default(),
        mode: Some("chat".into()),
        number: 1.into(),
        id: name.into(),
        repository_id: Some("repo".into()),
        name: name.into(),
        running,
        links: Default::default(),
        review: Vec::new(),
        holds: Vec::new(),
        agent_state: None,
        progress: None,
        sleep: None,
        wake_at: None,
        asleep: None,
        restart: None,
        parent: None,
        children: Vec::new(),
        workdir: shoal_protocol::model::Workdir::Checkout {
            path: "/repo".into(),
        },
        part: shoal_protocol::model::ChatPart {
            agent: Some("claude".into()),
            started_at: UnixTime::from_seconds(0),
            tmp_dir: None,
            completion: None,
        },
    };
    let candidates = stopped_chat_candidates(vec![chat("talk", false), chat("running", true)]);
    let offered: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.get_value().to_owned(),
                candidate.get_help().map(ToString::to_string),
            )
        })
        .collect();
    let help = format!("talk ({STOPPED_CHAT})");
    assert_eq!(offered, [("1".into(), Some(help))]);
}

#[test]
fn rm_completes_spaces() {
    let command = <crate::cli::Cli as clap::CommandFactory>::command();
    // Its arguments are deferred until the subcommand builds.
    let mut rm = command.find_subcommand("rm").unwrap().clone();
    rm.build();
    let positional = rm.get_positionals().next().unwrap();
    assert_eq!(
        target_of(positional.get_id().as_str(), "", "rm"),
        Some(Target::Spaces)
    );
}

#[test]
fn restore_completes_the_spaces_its_flags_act_on() {
    let target = |args: &[&str]| {
        let mut command = crate::cli::Cli::command();
        command.build();
        let matches = command
            .ignore_errors(true)
            .try_get_matches_from([&["shoal", "restore"][..], args].concat())
            .unwrap();
        let (_, restore) = matches.subcommand().unwrap();
        RestoreAction::of(restore).target()
    };
    assert_eq!(target(&[]), Target::RemovedSpaces);
    assert_eq!(target(&["--checkpoint"]), Target::Workspaces);
    assert_eq!(target(&["--discard-agent"]), Target::ResumeSpaces);
}

#[test]
fn a_line_names_the_request_its_command_sends_once_it_parses() {
    let acting = |line: &str| {
        let words: Vec<OsString> = line.split(' ').map(OsString::from).collect();
        typed_acting(&words).map(|acting| acting.operation())
    };
    // The word being completed is left out, so a partial number is no
    // space yet.
    assert!(matches!(
        acting("shoal stop --processes 1"),
        Some(Operation::Removal(RemovalMethod::StopProcesses { .. }))
    ));
    assert!(matches!(
        acting("shoal stop "),
        Some(Operation::Removal(RemovalMethod::StopSpace { .. }))
    ));
    // A hold names its required name before it parses.
    assert!(acting("shoal acquire hold ").is_none());
    assert!(acting("shoal repo ls ").is_none());
}

#[test]
fn a_refused_space_completes_with_its_reason_and_a_lacking_one_not_at_all() {
    let shown = |offer: Option<Offer>| {
        let candidate = CompletionCandidate::new("3");
        let shown = offered(candidate, "fix".into(), offer.as_ref());
        shown.map(|candidate| candidate.get_help().map(ToString::to_string))
    };
    assert_eq!(shown(None), Some(None));
    assert_eq!(shown(Some(Offer::Accepted)), Some(None));
    let reason = "workspace 3 is preparing".to_owned();
    let refused = shown(Some(Offer::Refused { reason }));
    assert_eq!(refused, Some(Some("fix: workspace 3 is preparing".into())));
    assert_eq!(shown(Some(Offer::Lacking)), None);
}
