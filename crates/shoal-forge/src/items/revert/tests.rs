use super::*;
use crate::capability::Unsupported;
use serde_json::json;

#[tokio::test]
async fn forges_without_a_revert_refuse_before_sending_anything() {
    let account = Account::user(Default::default());
    let revert = NewRevert {
        title: "Revert \"Fix\"".into(),
        ..NewRevert::default()
    };
    let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let current = forge.current(ItemKind::Pr, 7, &merged_pull()).unwrap();
    let error = forge
        .revert_pull(&account, &current, &revert)
        .await
        .unwrap_err();
    let unsupported = error.downcast_ref::<Unsupported>().unwrap();
    assert_eq!(unsupported.capability, Capability::RevertPrs);
}

#[test]
fn pr_heads_stay_fetchable_where_the_forge_keeps_a_ref() {
    let kind = |remote: &str| ForgeRepo::parse(remote).unwrap().kind;
    assert_eq!(
        kind("https://forge.example/team/repo")
            .pull_head_ref(7)
            .as_deref(),
        Some("refs/pull/7/head")
    );
    assert_eq!(
        kind("git@gitlab.com:group/repo.git")
            .pull_head_ref(7)
            .as_deref(),
        Some("refs/merge-requests/7/head")
    );
    assert_eq!(
        kind("git@bitbucket.org:team/repo.git").pull_head_ref(7),
        None
    );
}

/// A merged PR as Forgejo's REST API reports it.
fn merged_pull() -> serde_json::Value {
    json!({"number": 7, "title": "Fix", "state": "closed", "merged": true,
        "head": {"ref": "topic"}, "base": {"ref": "main"}})
}

#[test]
fn reverts_default_to_the_reverted_pr_title_reference_and_labels() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let mut pull = merged_pull();
    pull["labels"] = json!([{"name": "type/bug"}]);
    pull["base"]["ref"] = json!("release");
    let item = forgejo.current(ItemKind::Pr, 7, &pull).unwrap().item;
    let filled = revert_options(&item, PullOptions::default());
    assert_eq!(filled.base.as_deref(), Some("release"));
    assert_eq!(filled.title.as_deref(), Some("Revert \"Fix\""));
    assert_eq!(filled.body.as_deref(), Some("Reverts #7"));
    assert_eq!(filled.labels, ["type/bug"]);
    let chosen = PullOptions {
        title: Some("Back out".into()),
        body: Some("Broke CI".into()),
        labels: vec!["type/chore".into()],
        ..PullOptions::default()
    };
    let filled = revert_options(&item, chosen.clone());
    assert_eq!(
        filled,
        PullOptions {
            base: Some("release".into()),
            ..chosen
        }
    );
    assert!(NewRevert::new(filled.clone(), true).draft);
    let ready = PullOptions {
        draft: Some(false),
        ..filled
    };
    assert!(!NewRevert::new(ready, true).draft);
}
