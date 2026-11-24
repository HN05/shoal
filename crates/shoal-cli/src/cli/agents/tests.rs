use super::*;
use crate::agents::agent_off;

#[test]
fn enabled_turns_an_agent_off_and_happys_agents_on() {
    let settings = |text: &str| {
        let config = crate::config::Config::from_toml(text).unwrap();
        config.resolve(&Default::default()).unwrap()
    };
    let off = settings("[agents.claude]\nenabled = false\n");
    assert!(agent_off(&off, &"claude".into()).is_some());
    assert!(agent_off(&off, &"codex".into()).is_none());
    assert!(!names(&off).contains(&"claude".into()));
    // A turned-off agent does not review either.
    assert!(off.check_reviewer(&"claude".into()).is_err());
    assert!(off.check_reviewer(&"codex".into()).is_ok());
    assert!(agent_off(&off, &"happy-codex".into()).is_some());
    let on = settings("[agents.happy]\nenabled = true\n");
    assert!(agent_off(&on, &"happy-codex".into()).is_none());
}

#[test]
fn a_reviewer_needs_only_its_review_command_installed() {
    let config = crate::config::Config::from_toml(
        "[agents.present]\ncommand = ['sh']\nreview = ['{prompt}']\n\
         [agents.absent]\ncommand = ['no-such-reviewer']\nreview = ['{prompt}']\n",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    ensure_reviewer_available(&"present".into(), &settings).unwrap();
    let refused = ensure_reviewer_available(&"absent".into(), &settings).unwrap_err();
    assert!(
        refused.to_string().contains("no-such-reviewer"),
        "{refused}"
    );
    assert!(ensure_reviewer_available(&"unknown".into(), &settings).is_err());
}
