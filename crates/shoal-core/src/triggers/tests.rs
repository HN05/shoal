use serde_json::json;

use super::{Overlap, Source, Trigger};
use crate::config::{Config, repo};

const TRIAGE: &str = r#"
[triggers.triage]
on = "timer"
at = "09:00"
days = ["mon", "tue", "wed", "thu", "fri"]
timezone = "Europe/Oslo"
mode = "chat"
agent = "claude"
name = "Morning triage"
priority = "urgent"
permission = { push = "propose" }
task = "Triage the issues opened since {{ previous_at }}."
overlap = "queue"
"#;

fn trigger(text: &str) -> Trigger {
    let config = repo::parse(text).unwrap();
    config.triggers.into_values().next().unwrap()
}

#[test]
fn a_repository_trigger_launches_in_its_repository() {
    let trigger = trigger(TRIAGE);
    assert!(matches!(trigger.source, Source::Timer(_)));
    assert_eq!(trigger.overlap, Overlap::Queue);
    let input = json!({ "previous_at": "2026-11-13T09:00:00+01:00" });
    let launch = trigger
        .launch
        .launch(&input.into(), Some("repo-id".into()))
        .unwrap();
    assert_eq!(launch.repository.as_deref(), Some("repo-id"));
    assert_eq!(launch.mode.unwrap().as_str(), "chat");
    assert_eq!(launch.agent.unwrap().to_string(), "claude");
    assert_eq!(launch.name.unwrap().to_string(), "Morning triage");
    assert_eq!(launch.permissions.assignments(), ["push=propose"]);
    assert_eq!(
        launch.task,
        ["Triage the issues opened since 2026-11-13T09:00:00+01:00."]
    );
}

#[test]
fn a_firing_fails_on_a_missing_lookup_or_a_value_its_flag_refuses() {
    let trigger = trigger(
        "[triggers.fix]\non = 'timer'\nevery = '1h'\nissue = '{{ payload.issue }}'\npriority = '{{ payload.priority }}'",
    );
    let failure = |input: serde_json::Value| {
        format!(
            "{:#}",
            trigger.launch.launch(&input.into(), None).unwrap_err()
        )
    };
    assert!(failure(json!({})).contains("payload.priority"));
    let refused = failure(json!({ "payload": { "issue": "a title", "priority": "low" } }));
    assert!(refused.starts_with("issue a title"), "{refused}");
    let refused = failure(json!({ "payload": { "issue": 4, "priority": "soon" } }));
    assert!(refused.starts_with("priority soon"), "{refused}");
    let launch = trigger
        .launch
        .launch(
            &json!({ "payload": { "issue": 4, "priority": "low" } }).into(),
            None,
        )
        .unwrap();
    assert!(launch.issue.is_some());
}

#[test]
fn reading_refuses_what_no_firing_could_launch() {
    for text in [
        // A key no source or launch has.
        "[triggers.a]\non = 'timer'\nevery = '1h'\ncron = '* * * * *'",
        "[triggers.a]\non = 'cron'",
        "[triggers.a]\non = 'timer'\nevery = '1h'\nagent = 'Not An Agent'",
        "[triggers.a]\non = 'timer'\nevery = '1h'\nin = '3'",
        "[triggers.a]\non = 'timer'\nevery = '1h'\nissue = '4'\npr = 'https://github.com/o/r/pull/1'",
        "[triggers.a]\non = 'timer'\nevery = '1h'\ntask = '{{ unclosed'",
        "[triggers.a]\non = 'timer'\nevery = '1h'\nfilter = [{ path = 'a' }]",
        "[triggers.Bad]\non = 'timer'\nevery = '1h'",
        // A repository's trigger starts in that repository.
        "[triggers.a]\non = 'timer'\nevery = '1h'\nrepository = 'other'",
    ] {
        assert!(repo::parse(text).is_err(), "{text}");
    }
}

#[test]
fn global_triggers_stay_out_of_every_repository() {
    let config = Config::from_toml(
        "[triggers.nightly]\non = 'timer'\nat = '02:00'\nrepository = 'shoal'\ntask = 'Check'",
    )
    .unwrap();
    assert_eq!(
        config.triggers()["nightly"]
            .launch
            .repository
            .as_ref()
            .map(|r| r.as_str()),
        Some("shoal")
    );
    assert!(config.repository_layer().triggers.is_empty());
}

#[test]
fn a_trigger_writes_what_it_reads() {
    let trigger = trigger(TRIAGE);
    let written = toml::to_string(&trigger).unwrap();
    let read: Trigger = toml::from_str(&written).unwrap();
    assert_eq!(read, trigger);
}
