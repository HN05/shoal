use super::*;
use crate::config::{
    Config,
    repo::{ConfigLayers, parse},
};

fn gate(global: &str, repository: &str) -> Gate {
    let layers = ConfigLayers {
        local_config: parse(repository).unwrap(),
        ..Default::default()
    };
    let settings = Config::from_toml(global).unwrap().resolve(&layers).unwrap();
    settings.gate.clone()
}

#[test]
fn a_requests_rules_add_each_governing_keys_to_the_gates() {
    let gate = gate(
        "[gate]\ninstructions = 'Follow AGENTS.md.'\nescalate = ['a release branch']\n",
        "[gate.merge]\ninstructions = 'Merge only green.'\nescalate = ['a migration']\n\
         [gate.push]\nescalate = ['a release branch', 'a tag']\n",
    );
    let rules = gate.rules(&[PermissionKey::Merge, PermissionKey::Push]);
    assert_eq!(
        rules.instructions,
        ["Follow AGENTS.md.", "Merge only green."]
    );
    assert_eq!(rules.escalate, ["a release branch", "a migration", "a tag"]);
    let issue = gate.rules(&[PermissionKey::IssueOpen]);
    assert_eq!(issue.escalate, ["a release branch"]);
}

#[test]
fn without_a_gate_table_it_escalates_nothing() {
    let rules = gate("", "").rules(&[PermissionKey::Push]);
    assert_eq!(rules, GateRules::default());
}

#[test]
fn an_escalation_case_needs_text() {
    let error = parse("[gate.push]\nescalate = ['  ']\n").unwrap_err();
    assert!(
        format!("{error:#}").contains("gate.push.escalate"),
        "{error:#}"
    );
}
