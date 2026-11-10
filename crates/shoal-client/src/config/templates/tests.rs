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
fn the_facts_tell_each_launch_how_it_receives_updates() {
    let settings = settings("");
    let repo = repository();
    for delivery in [Delivery::Pushed, Delivery::Watched] {
        let workspace = brief(&settings, &"worker".into(), worker(delivery), None).unwrap();
        let chat = brief(
            &settings,
            &"chat".into(),
            chat(ChatHome::Checkout(&repo), delivery),
            None,
        );
        for system in [workspace.system, chat.unwrap().system] {
            let watches = system.contains("`shoal watch pr`");
            assert_eq!(watches, delivery == Delivery::Watched, "{system}");
            assert!(!system.contains("{updates}"));
        }
    }
}

#[test]
fn rules_follow_the_guidance_and_instructions_follow_the_subject() {
    let plain = settings("");
    let place = worker(Delivery::Watched);
    let guidance = brief(&plain, &"worker".into(), place, None).unwrap();
    assert!(
        guidance
            .system
            .starts_with("You are working in Shoal workspace topic")
    );
    assert_eq!(guidance.first_message, None);
    let started = brief(&plain, &"worker".into(), place, Some(&issue())).unwrap();
    assert_eq!(started.system, guidance.system);
    // The subject is never rendered; the instructions are.
    let instructions = plain.modes["worker"].instructions.trim_end();
    let subject = issue().text.trim_end().to_owned();
    assert_eq!(
        started.first_message,
        Some(format!("{subject}\n\n{instructions}"))
    );
    let added = settings(
        "[rules.worker]\ntext = 'Babysit {branch}.'\n\
         [modes.worker]\ninstructions = 'Then tell #{issue.number}.'\n",
    );
    let started = brief(&added, &"worker".into(), place, Some(&issue())).unwrap();
    assert_eq!(
        started.system,
        format!("{}\n\nBabysit topic.", guidance.system)
    );
    assert_eq!(
        started.first_message,
        Some(format!("{subject}\n\n{instructions}\n\nThen tell #7."))
    );
    let error = brief(&plain, &"triage".into(), place, None).unwrap_err();
    assert!(error.to_string().contains("no mode triage"), "{error}");
}

/// Asserts that `system` holds the facts that name `place_text`, then both
/// space rules, then `rules`, the mode's rules, with every field filled in.
fn assert_layers(system: &str, place_text: &str, rules: &str) {
    let at = |text: &str| {
        system
            .find(text)
            .unwrap_or_else(|| panic!("no {text}: {system}"))
    };
    assert!(at(place_text) < at("`shoal done`"), "{system}");
    assert!(at(place_text) < at("`shoal work rename`"), "{system}");
    assert!(system.ends_with(rules), "{system}");
    assert!(!system.contains('{'), "{system}");
}

fn repository() -> crate::model::Repository {
    serde_json::from_value(serde_json::json!({
        "id": "r", "path": "/repo", "source": "/repo", "last_used": 0,
    }))
    .unwrap()
}

fn chat(home: ChatHome<'_>, delivery: Delivery) -> Place<'_> {
    Place::Chat {
        repo: Some("shoal"),
        home,
        delivery,
    }
}

#[test]
fn every_space_gets_its_facts_and_the_space_rules_before_its_modes_rules() {
    let settings = settings(
        "[rules.worker]\ntext = 'Babysit {branch}.'\n\
         [rules.chat]\ntext = 'Answer in {repo}.'\n",
    );
    let worker = brief(&settings, &"worker".into(), worker(Delivery::Pushed), None).unwrap();
    assert_layers(&worker.system, "workspace topic", "\n\nBabysit topic.");
    let repo = repository();
    let workspace = workspace();
    for (home, place_text) in [
        (ChatHome::Checkout(&repo), "checkout at /repo"),
        (ChatHome::Workspace(&workspace), "worktree at /work/topic"),
    ] {
        let place = chat(home, Delivery::Pushed);
        let chat = brief(&settings, &"chat".into(), place, None).unwrap();
        assert_layers(&chat.system, place_text, "\n\nAnswer in shoal.");
        let plan = brief(&settings, &"plan".into(), place, Some(&issue())).unwrap();
        assert_eq!(plan.system, chat.system);
        let first = plan.first_message.unwrap();
        assert!(first.starts_with("Issue #7: Decide {path}\n"), "{first}");
        assert!(first.ends_with("work the plan splits off."), "{first}");
    }
}

#[test]
fn built_in_modes_carry_the_work_rule_until_a_mode_leaves_it_out() {
    let place = worker(Delivery::Watched);
    let system = |config: &str| {
        brief(&settings(config), &"worker".into(), place, None)
            .unwrap()
            .system
    };
    let extended = system("[rules.work]\ntext = 'Push often.'\n");
    assert!(extended.contains("`shoal review resolve`"), "{extended}");
    assert!(extended.ends_with("Push often."), "{extended}");
    let left_out = system("[modes.worker]\nrules = ['worker']\n");
    assert!(!left_out.contains("`shoal review resolve`"), "{left_out}");
}

#[test]
fn a_chat_mode_without_the_chat_rule_still_gets_the_facts_and_space_rules() {
    let settings = settings(
        "[rules.triage]\ntext = 'Label it.'\n\
         [modes.triage]\nkind = 'chat'\nrules = ['triage']\n",
    );
    let repo = repository();
    let place = chat(ChatHome::Checkout(&repo), Delivery::Pushed);
    let triage = brief(&settings, &"triage".into(), place, None).unwrap();
    assert_layers(&triage.system, "checkout at /repo", "\n\nLabel it.");
}

#[test]
fn setup_takes_its_base_modes_rules_and_its_own_instructions() {
    let settings = settings(
        "[rules.worker]\ntext = 'Babysit {branch}.'\n\
         [rules.orchestrate]\ntext = 'Manage every space.'\n",
    );
    let subject = Subject::told("Write the config.".into());
    let root = Place::Chat {
        repo: None,
        home: ChatHome::Root(std::path::Path::new("/home/me/shoal")),
        delivery: Delivery::Pushed,
    };
    for (place, base) in [(worker(Delivery::Pushed), "worker"), (root, "orchestrate")] {
        let setup = brief(&settings, &modes::ModeName::SETUP, place, Some(&subject)).unwrap();
        let based = brief(&settings, &base.into(), place, None).unwrap();
        assert_eq!(setup.system, based.system);
        assert_eq!(
            system(&settings, &modes::ModeName::SETUP, place, None).unwrap(),
            based.system
        );
        let first = setup.first_message.unwrap();
        assert!(first.starts_with("Write the config.\n\n"), "{first}");
        assert!(first.contains("`shoal daemon reload`"), "{first}");
    }
    let repo = repository();
    let in_checkout = chat(ChatHome::Checkout(&repo), Delivery::Pushed);
    assert!(
        brief(
            &settings,
            &modes::ModeName::SETUP,
            in_checkout,
            Some(&subject)
        )
        .is_err()
    );
}

#[test]
fn a_chat_without_a_repository_is_told_it_runs_in_root_dir() {
    let place = Place::Chat {
        repo: None,
        home: ChatHome::Root(std::path::Path::new("/home/me/shoal")),
        delivery: Delivery::Pushed,
    };
    let chat = brief(&settings(""), &"chat".into(), place, None).unwrap();
    assert!(
        chat.system.contains("directory at /home/me/shoal"),
        "{}",
        chat.system
    );
}

#[test]
fn plan_instructions_swap_only_configured_labels() {
    let place = worker(Delivery::Watched);
    let first = |config: &str| {
        brief(&settings(config), &"plan".into(), place, Some(&issue()))
            .unwrap()
            .first_message
            .unwrap()
    };
    assert!(
        first("[issue]\nplan_label = 'needs-plan'\nplanned_label = 'planned'\n")
            .contains("replace its `needs-plan` label with `planned`")
    );
    assert!(
        first("[issue]\nplan_label = 'needs-plan'\n").contains("remove its `needs-plan` label")
    );
    assert!(first("[issue]\nplanned_label = 'planned'\n").contains("add the `planned` label"));
    assert!(!first("").contains("label"));
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
fn restored_rules_name_the_issue_the_space_links() {
    let settings =
        settings("[rules.worker]\ntext = 'On #{issue.number}, {issue.title}: {issue.url}'\n");
    let mut linked = workspace();
    let url = "https://forge.example/team/repo/issues/7";
    linked.links.issue = Some(url.into());
    linked.links.issue_title = Some("Decide".into());
    let issue = IssueFields::linked(&linked.links);
    let place = Place::Workspace {
        workspace: &linked,
        delivery: Delivery::Watched,
    };
    let system = system(&settings, &"worker".into(), place, issue.as_ref()).unwrap();
    assert_layers(
        &system,
        "workspace topic",
        &format!("\n\nOn #7, Decide: {url}"),
    );
    linked.links.issue = None;
    assert!(IssueFields::linked(&linked.links).is_none());
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
