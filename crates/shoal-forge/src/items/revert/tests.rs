use super::*;
use crate::capability::Unsupported;
use serde_json::json;

#[tokio::test]
async fn forges_without_a_revert_refuse_before_sending_anything() {
    let account = Account::user(&Default::default());
    let revert = NewRevert {
        title: "Revert \"Fix\"".into(),
        ..NewRevert::default()
    };
    for remote in [
        "https://forge.example/team/repo",
        "git@gitlab.com:group/repo.git",
        "git@bitbucket.org:team/repo.git",
    ] {
        let forge = ForgeRepo::parse(remote).unwrap();
        let current = forge
            .current(ItemKind::Pr, 7, &merged_pull(forge.kind))
            .unwrap();
        let error = forge
            .revert_pull(&account, &current, &revert)
            .await
            .unwrap_err();
        let unsupported = error.downcast_ref::<Unsupported>().unwrap();
        assert_eq!(unsupported.capability, Capability::RevertPrs);
        assert!(error.to_string().contains("git revert"), "{error}");
    }
}

#[test]
fn commit_messages_come_from_where_each_forge_lists_them() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let listed = [json!({"sha": "a", "commit": {"message": "Fix\n\nWhy"}})];
    assert_eq!(
        messages(&listed, forgejo.kind.commit_message()).unwrap(),
        ["Fix\n\nWhy"]
    );
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
    let listed = [json!({"id": "a", "message": "Fix"})];
    assert_eq!(
        messages(&listed, gitlab.kind.commit_message()).unwrap(),
        ["Fix"]
    );
    assert!(messages(&listed, forgejo.kind.commit_message()).is_err());
}

/// A merged PR as `kind`'s REST API reports it.
fn merged_pull(kind: crate::backends::ForgeKind) -> serde_json::Value {
    use crate::backends::ForgeKind;
    match kind {
        ForgeKind::GitLab => json!({"iid": 7, "title": "Fix", "state": "merged",
                "source_branch": "topic", "target_branch": "main"}),
        ForgeKind::Bitbucket => json!({"id": 7, "title": "Fix", "state": "MERGED",
                "source": {"branch": {"name": "topic"}},
                "destination": {"branch": {"name": "main"}}}),
        _ => json!({"number": 7, "title": "Fix", "state": "closed", "merged": true,
                "head": {"ref": "topic"}, "base": {"ref": "main"}}),
    }
}

#[test]
fn reverts_default_to_the_reverted_pr_title_reference_and_labels() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let mut pull = merged_pull(forgejo.kind);
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
