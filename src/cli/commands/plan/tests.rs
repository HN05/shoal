use super::*;
use crate::config::options::Options;

fn labels(plan: Option<&str>, planned: Option<&str>) -> config::Issue {
    config::Issue {
        plan_label: plan.map(str::to_owned),
        planned_label: planned.map(str::to_owned),
        ..config::repo::IssueConfig::built_in()
            .resolve("issue")
            .unwrap()
    }
}

#[test]
fn the_oldest_issue_is_refused_beyond_the_listing_limit() {
    let item = |number: u64| -> Item {
        serde_json::from_value(serde_json::json!({
            "kind": "issue", "number": number, "url": "url", "title": "title",
            "state": "open", "labels": [],
        }))
        .unwrap()
    };
    let mut issues: Vec<_> = (0..MAX_LIMIT as u64 - 1).rev().map(item).collect();
    assert_eq!(oldest(&issues, "issues").unwrap().unwrap().number, 0);
    issues.insert(0, item(MAX_LIMIT as u64));
    let error = oldest(&issues, "issues").unwrap_err().to_string();
    assert!(
        error.contains("only the newest 1000 open issues"),
        "{error}"
    );
    assert!(oldest(&[], "issues").unwrap().is_none());
}

#[test]
fn the_prompt_carries_the_issue_and_swaps_only_configured_labels() {
    let issue = Issue {
        number: 7,
        title: "Decide {title}".into(),
        url: "https://forge.example/team/repo/issues/7".into(),
        details: "Which way?".into(),
    };
    let prompt = prompt(&issue, &labels(Some("needs-plan"), Some("planned")));
    assert!(
        prompt.starts_with(
            "Plan issue #7: Decide {title}\nhttps://forge.example/team/repo/issues/7\n\nWhich way?\n\n"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("`needs-plan`"), "{prompt}");
    assert!(prompt.contains("`planned`"), "{prompt}");
    assert!(prompt.contains("--issue 7 "), "{prompt}");
    let removes = relabel(&labels(Some("needs-plan"), None));
    assert!(removes.contains("remove its `needs-plan`"), "{removes}");
    let adds = relabel(&labels(None, Some("planned")));
    assert!(adds.contains("add the `planned`"), "{adds}");
    assert_eq!(relabel(&labels(None, None)), "");
}

#[test]
fn a_plan_chat_is_named_after_its_issue_without_asking() {
    let repo = Repository {
        id: "id".into(),
        path: "/repo".into(),
        source: "https://forge.example/team/shoal.git".into(),
        last_used: 0,
        name: None,
        workspaces_dir: None,
        contribution: false,
    };
    let issue = Issue {
        number: 1211,
        title: "Attach to chats".into(),
        url: "https://forge.example/team/shoal/issues/1211".into(),
        details: String::new(),
    };
    let topic = topic(&repo, &issue, &labels(None, None));
    assert_eq!(topic.name, "shoal-plan-1211");
    assert!(!topic.asks_name);
}
