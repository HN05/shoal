use super::*;

use crate::config::{Config, Effective, repo::ConfigLayers};

fn settings(repository: &str) -> Effective {
    let layers = ConfigLayers {
        worktree_file: crate::config::repo::parse(repository).unwrap(),
        ..Default::default()
    };
    Config::default().resolve(&layers).unwrap()
}

fn issue() -> Subject {
    Subject {
        text: "Issue #7: Decide {path}\nhttps://forge.example/r/issues/7\n".into(),
        issue: Some(IssueFields {
            number: 7,
            title: "Decide {path}".into(),
            url: "https://forge.example/r/issues/7".into(),
        }),
    }
}

fn worker(delivery: Delivery) -> Place<'static> {
    let workspace = Box::leak(Box::new(workspace()));
    Place::Workspace {
        workspace,
        delivery,
    }
}

#[test]
fn the_guidance_tells_each_launch_how_it_receives_updates() {
    let settings = settings("");
    let pushed = brief(&settings, "worker", worker(Delivery::Pushed), None).unwrap();
    assert!(
        pushed
            .system
            .contains("end your\nturn to wait for the next."),
        "{}",
        pushed.system
    );
    assert!(!pushed.system.contains("shoal watch pr"));
    let watched = brief(&settings, "worker", worker(Delivery::Watched), None).unwrap();
    assert!(watched.system.contains("open, run `shoal watch pr`."));
    assert!(!watched.system.contains("{updates}"));
}

#[test]
fn rules_follow_the_guidance_and_instructions_follow_the_subject() {
    let plain = settings("");
    let place = worker(Delivery::Watched);
    let guidance = brief(&plain, "worker", place, None).unwrap();
    assert!(
        guidance
            .system
            .starts_with("You are working in Shoal workspace topic")
    );
    assert_eq!(guidance.first_message, None);
    let started = brief(&plain, "worker", place, Some(&issue())).unwrap();
    assert_eq!(started.system, guidance.system);
    // The subject is never rendered; the instructions are.
    assert_eq!(
        started.first_message.as_deref(),
        Some(
            "Issue #7: Decide {path}\nhttps://forge.example/r/issues/7\n\n\
             Implement this, open a PR for it, and finish with `shoal done`."
        )
    );
    let added = settings(
        "[rules.worker]\ntext = 'Babysit {branch}.'\n\
         [modes.worker]\ninstructions = 'Then tell #{issue.number}.'\n",
    );
    let started = brief(&added, "worker", place, Some(&issue())).unwrap();
    assert_eq!(
        started.system,
        format!("{}\n\nBabysit topic.", guidance.system)
    );
    let first = started.first_message.unwrap();
    assert!(
        first.ends_with("finish with `shoal done`.\n\nThen tell #7."),
        "{first}"
    );
    let error = brief(&plain, "triage", place, None).unwrap_err();
    assert_eq!(
        error.to_string(),
        "no mode triage; [modes.triage] in config defines one"
    );
}

#[test]
fn the_chat_rule_describes_where_the_chat_runs_before_its_additions() {
    let settings = settings("[rules.chat]\ntext = 'Answer in {repo}.'\n");
    let repo: crate::model::Repository = serde_json::from_value(serde_json::json!({
        "id": "r", "path": "/repo", "source": "/repo", "last_used": 0,
    }))
    .unwrap();
    let workspace = workspace();
    for (home, opening) in [
        (
            ChatHome::Checkout(&repo),
            "You are chatting with the user about the shoal repository in its registered\ncheckout at /repo.",
        ),
        (
            ChatHome::Workspace(&workspace),
            "You are chatting with the user about Shoal workspace topic of the shoal\nrepository, on branch topic, in its worktree at /work/topic.",
        ),
    ] {
        let place = Place::Chat {
            repo: "shoal",
            home,
        };
        let chat = brief(&settings, "chat", place, None).unwrap();
        assert!(chat.system.starts_with(opening), "{}", chat.system);
        assert!(
            chat.system.ends_with("\n\nAnswer in shoal."),
            "{}",
            chat.system
        );
        // Only a workspace's agent gets Shoal's workspace guidance.
        assert!(!chat.system.contains("shoal done"));
        let plan = brief(&settings, "plan", place, Some(&issue())).unwrap();
        assert_eq!(plan.system, chat.system);
        let first = plan.first_message.unwrap();
        assert!(first.starts_with("Issue #7: Decide {path}\n"), "{first}");
        assert!(first.ends_with("`shoal start --issue 7`."), "{first}");
    }
}

#[test]
fn plan_instructions_swap_only_configured_labels() {
    let place = worker(Delivery::Watched);
    let first = |config: &str| {
        brief(&settings(config), "plan", place, Some(&issue()))
            .unwrap()
            .first_message
            .unwrap()
    };
    assert!(
        first("[issue]\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n")
            .contains("only the body. Then replace its `needs-plan` label with `planned`. Open")
    );
    assert!(
        first("[issue]\nplan_label = 'needs-plan'\n")
            .contains("only the body. Then remove its `needs-plan` label. Open")
    );
    assert!(
        first("[issue]\nplanned_label = 'planned'\n")
            .contains("only the body. Then add the `planned` label. Open")
    );
    assert!(first("").contains("only the body. Open"));
}

#[test]
fn a_copy_of_the_guidance_opening_repeats_it() {
    let earlier_default = "You are working in Shoal workspace {workspace} on branch {branch} at {path}.\n\
             Follow the repository's instructions.";
    assert!(repeats_guidance(earlier_default));
    assert!(!repeats_guidance("Babysit {branch}."));
}

fn workspace() -> crate::model::Workspace {
    serde_json::from_value(serde_json::json!({
        "id": "id", "repository_id": "repo", "name": "topic", "branch": "topic",
        "path": "/work/topic", "state": "ready", "created_at": 0, "updated_at": 0,
    }))
    .unwrap()
}

#[test]
fn review_instructions_keep_their_fixed_part_before_any_template() {
    let fields = ReviewFields {
        workspace: "fix-login",
        branch: "fix/login",
        path: "/state/reviews/7",
        base: "abc123",
    };
    let built_in = review_instructions(Some(review_base()), &fields);
    assert!(built_in.starts_with("You are reviewing the committed changes of Shoal workspace fix-login, on branch fix/login, checked out at /state/reviews/7."));
    assert!(built_in.contains("git diff abc123...HEAD --stat"));
    assert!(!built_in.contains("<!--"));
    let replaced = review_instructions(Some("Follow .review/review.md in {path}."), &fields);
    assert!(replaced.contains("report it in `checked`"));
    assert!(replaced.ends_with("Follow .review/review.md in /state/reviews/7."));
    assert!(!replaced.contains("careful senior colleague"));
    assert!(!review_instructions(None, &fields).contains("careful senior colleague"));
}
