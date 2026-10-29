use super::*;
use crate::{backends::ForgeKind, items::item::PrSummary};
use serde_json::{Value, json};

fn pr(forge: &ForgeRepo, draft: bool, labels: &[&str]) -> Current {
    Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 7,
            url: String::new(),
            title: if draft && forge.kind == ForgeKind::Forgejo {
                "WIP: Fix".into()
            } else {
                "Fix".into()
            },
            state: ItemState::Open,
            labels: labels.iter().map(|label| label.to_string()).collect(),
            pr: Some(PrSummary {
                head: "topic".into(),
                base: "main".into(),
                draft,
            }),
        },
        node_id: Some("PR_1".into()),
        head_repository: Some("team/repo".into()),
        body: String::new(),
        author: None,
        head_sha: None,
        base_sha: None,
        conflict: None,
        merges_now: false,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        replaced_labels: Vec::new(),
    }
}

fn requests(forge: &ForgeRepo, current: &Current, action: Action) -> Vec<(String, String, Value)> {
    forge
        .plan(current, &action)
        .unwrap()
        .into_iter()
        .map(|request| {
            (
                request.method.as_str().to_owned(),
                request.endpoint,
                request.body.unwrap_or_default(),
            )
        })
        .collect()
}

fn forges() -> [ForgeRepo; 2] {
    [
        ForgeRepo::parse("git@github.com:team/repo.git").unwrap(),
        ForgeRepo::parse("https://forge.example/team/repo").unwrap(),
    ]
}

#[test]
fn posts_are_keyed_where_each_forge_records_their_activity() {
    let [github, forgejo] = forges();
    let gitlab = ForgeRepo::parse("https://gitlab.com/team/repo.git").unwrap();
    let comment = Action::Comment {
        body: "hi".into(),
        attachments: Vec::new(),
    };
    let reply = |reply: Option<&str>| {
        Action::Thread(ThreadChange {
            comment: 5,
            reply: reply.map(str::to_owned),
            resolve: Some(true),
        })
    };
    let response = json!({"id": 9, "node_id": "IC_9", "pull_request_review_id": 4});
    let posted = |forge: &ForgeRepo, action: &Action| forge.posted(action, &response);
    assert_eq!(
        posted(&github, &comment).as_deref(),
        Some("discussion:IC_9")
    );
    assert_eq!(posted(&forgejo, &comment).as_deref(), Some("discussion:9"));
    assert_eq!(
        posted(&github, &reply(Some("Fixed"))).as_deref(),
        Some("inline:9")
    );
    assert_eq!(
        posted(&forgejo, &reply(Some("Fixed"))).as_deref(),
        Some("reviews:4")
    );
    assert_eq!(
        posted(&gitlab, &reply(Some("Fixed"))).as_deref(),
        Some("discussion:9")
    );
    assert_eq!(posted(&forgejo, &reply(None)), None);
    assert_eq!(posted(&forgejo, &Action::Close), None);
    assert_eq!(forgejo.posted(&comment, &json!({})), None);
}

#[test]
fn edits_patch_fields_replace_labels_and_change_reviewers() {
    for forge in forges() {
        let edit = Edit {
            title: Some("New".into()),
            base: Some("next".into()),
            add_labels: vec!["a".into(), "bug".into()],
            remove_labels: vec!["old".into()],
            add_reviewers: vec!["sam".into(), "team/core".into()],
            remove_reviewers: vec!["kim".into()],
            ..Edit::default()
        };
        let current = pr(&forge, false, &["bug", "old"]);
        assert_eq!(
            requests(&forge, &current, Action::Edit(edit)),
            [
                (
                    "PATCH".into(),
                    "repos/team/repo/pulls/7".into(),
                    json!({"title": "New", "base": "next"})
                ),
                (
                    "PUT".into(),
                    "repos/team/repo/issues/7/labels".into(),
                    json!({"labels": ["bug", "a"]})
                ),
                (
                    "POST".into(),
                    "repos/team/repo/pulls/7/requested_reviewers".into(),
                    json!({"reviewers": ["sam"], "team_reviewers": ["core"]})
                ),
                (
                    "DELETE".into(),
                    "repos/team/repo/pulls/7/requested_reviewers".into(),
                    json!({"reviewers": ["kim"], "team_reviewers": []})
                ),
            ]
        );
    }
}

#[test]
fn draft_state_is_a_mutation_on_github_and_a_title_on_forgejo() {
    let [github, forgejo] = forges();
    let ready = Action::Edit(Edit {
        draft: Some(false),
        ..Edit::default()
    });
    let planned = requests(&github, &pr(&github, true, &[]), ready.clone());
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].1, "graphql");
    assert_eq!(planned[0].2["variables"], json!({"id": "PR_1"}));
    assert!(
        planned[0].2["query"]
            .as_str()
            .unwrap()
            .contains("markPullRequestReadyForReview")
    );
    assert_eq!(
        requests(&forgejo, &pr(&forgejo, true, &[]), ready.clone()),
        [(
            "PATCH".into(),
            "repos/team/repo/pulls/7".into(),
            json!({"title": "Fix"})
        )]
    );
    // Already ready: nothing to send.
    assert!(requests(&forgejo, &pr(&forgejo, false, &[]), ready).is_empty());
    let draft_with_title = Action::Edit(Edit {
        title: Some("[WIP] Other".into()),
        draft: Some(true),
        ..Edit::default()
    });
    assert_eq!(
        requests(&forgejo, &pr(&forgejo, false, &[]), draft_with_title)[0].2,
        json!({"title": "WIP: Other"})
    );
    // A new title alone keeps a Forgejo draft a draft, and a ready PR ready.
    let retitle = Action::Edit(Edit {
        title: Some("New".into()),
        ..Edit::default()
    });
    assert_eq!(
        requests(&forgejo, &pr(&forgejo, true, &[]), retitle.clone())[0].2,
        json!({"title": "WIP: New"})
    );
    assert_eq!(
        requests(&forgejo, &pr(&forgejo, false, &[]), retitle)[0].2,
        json!({"title": "New"})
    );
}

#[test]
fn comments_state_changes_and_merges_use_each_forges_endpoints() {
    let [github, forgejo] = forges();
    for forge in [&github, &forgejo] {
        let open = pr(forge, false, &[]);
        assert_eq!(
            requests(
                forge,
                &open,
                Action::Comment {
                    body: "hi".into(),
                    attachments: Vec::new()
                }
            ),
            [(
                "POST".into(),
                "repos/team/repo/issues/7/comments".into(),
                json!({"body": "hi"})
            )]
        );
        assert_eq!(
            requests(forge, &open, Action::Close),
            [(
                "PATCH".into(),
                "repos/team/repo/pulls/7".into(),
                json!({"state": "closed"})
            )]
        );
        assert!(forge.plan(&open, &Action::Reopen).is_err());
    }
    let merge = Action::Merge {
        method: MergeMethod::Squash,
        delete_branch: true,
    };
    assert_eq!(
        requests(&github, &pr(&github, false, &[]), merge.clone()),
        [
            (
                "PUT".into(),
                "repos/team/repo/pulls/7/merge".into(),
                json!({"merge_method": "squash"})
            ),
            (
                "DELETE".into(),
                "repos/team/repo/git/refs/heads/topic".into(),
                Value::Null
            ),
        ]
    );
    assert_eq!(
        github.plan(&pr(&github, false, &[]), &merge).unwrap()[0].confirm,
        Some("merged")
    );
    let mut fork = pr(&github, false, &[]);
    fork.head_repository = Some("someone/repo".into());
    assert_eq!(requests(&github, &fork, merge.clone()).len(), 1);
    assert_eq!(
        requests(&forgejo, &pr(&forgejo, false, &[]), merge),
        [(
            "POST".into(),
            "repos/team/repo/pulls/7/merge".into(),
            json!({"Do": "squash", "delete_branch_after_merge": true})
        )]
    );
}

#[test]
fn gitlab_changes_merge_requests_in_one_update_by_user_id() {
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/sub/repo.git").unwrap();
    let mut current = pr(&gitlab, false, &["bug"]);
    current.reviewers = vec!["kim".into(), "lee".into()];
    current.user_ids = [("kim".into(), 1), ("lee".into(), 2), ("sam".into(), 3)].into();
    let edit = Action::Edit(Edit {
        title: Some("New".into()),
        body: Some("Why".into()),
        base: Some("next".into()),
        add_labels: vec!["a".into(), "b".into()],
        remove_labels: vec!["bug".into()],
        add_reviewers: vec!["sam".into()],
        remove_reviewers: vec!["kim".into()],
        draft: Some(true),
        ..Edit::default()
    });
    assert_eq!(
        requests(&gitlab, &current, edit),
        [(
            "PUT".into(),
            "projects/group%2Fsub%2Frepo/merge_requests/7".into(),
            json!({"title": "Draft: New", "description": "Why", "target_branch": "next",
                    "add_labels": "a,b", "remove_labels": "bug", "reviewer_ids": [2, 3]})
        )]
    );
    let unknown = Action::Edit(Edit {
        add_reviewers: vec!["ann".into()],
        ..Edit::default()
    });
    assert!(gitlab.plan(&current, &unknown).is_err());
    let ready = Action::Edit(Edit {
        draft: Some(false),
        ..Edit::default()
    });
    let mut draft = pr(&gitlab, true, &[]);
    draft.item.title = "[Draft] Fix".into();
    assert_eq!(
        requests(&gitlab, &draft, ready)[0].2,
        json!({"title": "Fix"})
    );

    let open = pr(&gitlab, false, &[]);
    assert_eq!(
        requests(
            &gitlab,
            &open,
            Action::Comment {
                body: "hi".into(),
                attachments: Vec::new()
            }
        ),
        [(
            "POST".into(),
            "projects/group%2Fsub%2Frepo/merge_requests/7/notes".into(),
            json!({"body": "hi"})
        )]
    );
    assert_eq!(
        requests(&gitlab, &open, Action::Close)[0].2,
        json!({"state_event": "close"})
    );
    let merge = |method| Action::Merge {
        method,
        delete_branch: true,
    };
    assert_eq!(
        requests(&gitlab, &open, merge(MergeMethod::Squash)),
        [(
            "PUT".into(),
            "projects/group%2Fsub%2Frepo/merge_requests/7/merge".into(),
            json!({"squash": true, "should_remove_source_branch": true})
        )]
    );
    assert!(gitlab.plan(&open, &merge(MergeMethod::Rebase)).is_err());
}

#[test]
fn automatic_merges_use_each_forges_requests() {
    let [github, forgejo] = forges();
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
    let auto = |delete_branch| Action::AutoMerge {
        method: MergeMethod::Squash,
        delete_branch,
    };
    let open = pr(&forgejo, false, &[]);
    assert_eq!(
        requests(&forgejo, &open, auto(true)),
        [(
            "POST".into(),
            "repos/team/repo/pulls/7/merge".into(),
            json!({"Do": "squash", "delete_branch_after_merge": true,
                    "merge_when_checks_succeed": true})
        )]
    );
    assert_eq!(
        requests(&forgejo, &open, Action::CancelAutoMerge),
        [(
            "DELETE".into(),
            "repos/team/repo/pulls/7/merge".into(),
            Value::Null
        )]
    );

    let open = pr(&gitlab, false, &[]);
    assert_eq!(
        requests(&gitlab, &open, auto(true)),
        [(
            "PUT".into(),
            "projects/group%2Frepo/merge_requests/7/merge".into(),
            json!({"squash": true, "should_remove_source_branch": true,
                    "merge_when_pipeline_succeeds": true})
        )]
    );
    assert_eq!(
        requests(&gitlab, &open, Action::CancelAutoMerge),
        [(
            "POST".into(),
            "projects/group%2Frepo/merge_requests/7/cancel_merge_when_pipeline_succeeds".into(),
            Value::Null
        )]
    );

    let mut open = pr(&github, false, &[]);
    let enable = requests(&github, &open, auto(false));
    assert_eq!((enable.len(), enable[0].1.as_str()), (1, "graphql"));
    assert_eq!(
        enable[0].2["variables"],
        json!({"id": "PR_1", "method": "SQUASH"})
    );
    assert!(
        enable[0].2["query"]
            .to_string()
            .contains("enablePullRequestAutoMerge")
    );
    let disable = requests(&github, &open, Action::CancelAutoMerge);
    assert_eq!(disable[0].2["variables"], json!({"id": "PR_1"}));
    assert!(
        disable[0].2["query"]
            .to_string()
            .contains("disablePullRequestAutoMerge")
    );
    // GitHub keeps its own setting for deleting the branch.
    assert!(github.plan(&open, &auto(true)).is_err());
    // GitHub refuses to merge later what it would merge now.
    open.merges_now = true;
    assert_eq!(
        requests(&github, &open, auto(false)),
        [(
            "PUT".into(),
            "repos/team/repo/pulls/7/merge".into(),
            json!({"merge_method": "squash"})
        )]
    );
    open.item.state = ItemState::Closed;
    assert!(github.plan(&open, &Action::CancelAutoMerge).is_err());
}

#[test]
fn actions_refuse_what_the_item_cannot_take() {
    assert!(
        Action::Edit(Edit::default())
            .validate(ItemKind::Pr)
            .is_err()
    );
    let base = Action::Edit(Edit {
        base: Some("main".into()),
        ..Edit::default()
    });
    assert!(base.validate(ItemKind::Issue).is_err());
    assert!(base.validate(ItemKind::Pr).is_ok());
    let merge = Action::Merge {
        method: MergeMethod::Merge,
        delete_branch: false,
    };
    assert!(merge.validate(ItemKind::Issue).is_err());
    assert!(
        Action::Comment {
            body: " ".into(),
            attachments: Vec::new()
        }
        .validate(ItemKind::Issue)
        .is_err()
    );
    let thread = |reply: Option<&str>, resolve| {
        Action::Thread(ThreadChange {
            comment: 5,
            reply: reply.map(str::to_owned),
            resolve,
        })
    };
    assert!(thread(Some("Why"), None).validate(ItemKind::Pr).is_ok());
    assert!(thread(None, Some(true)).validate(ItemKind::Pr).is_ok());
    assert!(thread(None, Some(true)).validate(ItemKind::Issue).is_err());
    assert!(thread(None, None).validate(ItemKind::Pr).is_err());
    assert!(
        thread(Some(" "), Some(true))
            .validate(ItemKind::Pr)
            .is_err()
    );
    assert_eq!(thread(None, Some(false)).name(), "unresolve");
}

#[test]
fn actions_need_the_capabilities_they_use() -> Result<()> {
    let rebase = Action::Merge {
        method: MergeMethod::Rebase,
        delete_branch: false,
    };
    assert_eq!(rebase.needs(ItemKind::Pr), [Capability::RebaseMerge]);
    let auto = Action::AutoMerge {
        method: MergeMethod::Rebase,
        delete_branch: false,
    };
    assert_eq!(
        auto.needs(ItemKind::Pr),
        [Capability::AutoMerge, Capability::RebaseMerge]
    );
    assert!(Action::CancelAutoMerge.validate(ItemKind::Issue).is_err());
    assert_eq!(Action::Reopen.needs(ItemKind::Pr), [Capability::ReopenPrs]);
    assert!(Action::Reopen.needs(ItemKind::Issue).is_empty());
    let labels = Action::Edit(Edit {
        remove_labels: vec!["bug".into()],
        draft: Some(true),
        ..Edit::default()
    });
    assert_eq!(
        labels.needs(ItemKind::Pr),
        [Capability::Labels, Capability::DraftPrs]
    );
    let gitlab = ForgeRepo::parse("https://gitlab.com/group/project")?;
    let refused = gitlab.require_all(rebase.needs(ItemKind::Pr)).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "Shoal cannot rebase and merge PRs on GitLab; GitLab merges with the project's merge method, so use merge or squash"
    );
    let bitbucket = ForgeRepo::parse("https://bitbucket.org/team/repo")?;
    assert!(bitbucket.require_all(labels.needs(ItemKind::Pr)).is_err());
    assert!(bitbucket.require_all(rebase.needs(ItemKind::Pr)).is_ok());
    assert!(bitbucket.require_all(auto.needs(ItemKind::Pr)).is_err());
    Ok(())
}

#[test]
fn edits_fail_when_the_forge_shows_their_labels_unchanged() {
    let [github, forgejo] = forges();
    let gitlab = ForgeRepo::parse("https://gitlab.com/team/repo.git").unwrap();
    assert!(github.kind.label_names_fold_case());
    assert!(!gitlab.kind.label_names_fold_case() && !forgejo.kind.label_names_fold_case());
    let item = pr(&forgejo, false, &["Bug", "complexity/small"]).item;
    let applied = |action: Action, fold_case: bool| action.ensure_applied(&item, fold_case);
    let edit = |add: &[&str], remove: &[&str]| {
        Action::Edit(Edit {
            add_labels: add.iter().map(|label| label.to_string()).collect(),
            remove_labels: remove.iter().map(|label| label.to_string()).collect(),
            ..Edit::default()
        })
    };
    // Only a forge that folds case keeps `Bug` for an added `bug`, and
    // elsewhere `Bug` stays when `bug` is removed.
    assert!(applied(edit(&["bug"], &["ready"]), true).is_ok());
    assert!(applied(edit(&["bug"], &[]), false).is_err());
    assert!(applied(edit(&[], &["bug"]), false).is_ok());
    assert!(applied(edit(&[], &["bug"]), true).is_err());
    assert!(applied(Action::Close, false).is_ok());
    assert_eq!(
        applied(
            edit(&["complexity/medium", "ready"], &["complexity/small"]),
            false
        )
        .unwrap_err()
        .to_string(),
        "the forge did not add complexity/medium, ready and did not remove \
             complexity/small on #7, which has the labels Bug, complexity/small"
    );
    let bare = pr(&forgejo, false, &[]).item;
    assert_eq!(
        edit(&["ready"], &[])
            .ensure_applied(&bare, false)
            .unwrap_err()
            .to_string(),
        "the forge did not add ready on #7, which has no labels"
    );
}

#[test]
fn reviews_need_line_comments_and_their_verdict_and_only_prs_take_them() {
    use crate::review::submit::{LineComment, NewReview, ReviewVerdict, Side};
    let review = |verdict, comments: usize| {
        Action::Review(NewReview {
            verdict,
            body: "Body".into(),
            comments: (0..comments)
                .map(|_| LineComment {
                    path: "a.rs".into(),
                    line: 1,
                    side: Side::New,
                    body: "Why?".into(),
                })
                .collect(),
        })
    };
    assert!(
        review(ReviewVerdict::Comment, 0)
            .needs(ItemKind::Pr)
            .is_empty()
    );
    assert_eq!(
        review(ReviewVerdict::Approve, 1).needs(ItemKind::Pr),
        [Capability::LineComments, Capability::ApprovePrs]
    );
    assert_eq!(
        review(ReviewVerdict::RequestChanges, 0).needs(ItemKind::Pr),
        [Capability::RequestChanges]
    );
    let issue = review(ReviewVerdict::Comment, 0).validate(ItemKind::Issue);
    assert_eq!(issue.unwrap_err().to_string(), "issues have no reviews");
}
