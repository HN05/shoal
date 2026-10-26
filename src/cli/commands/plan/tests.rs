use super::*;

fn labels(plan: Option<&str>, planned: Option<&str>) -> config::Issue {
    config::Issue {
        plan_label: plan.map(str::to_owned),
        planned_label: planned.map(str::to_owned),
        ..Default::default()
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
    assert!(
        prompt.contains("only the body. Then replace its `needs-plan` label with `planned`. Open"),
        "{prompt}"
    );
    assert!(prompt.ends_with("`shoal add --issue 7 --agent <name>`."));
    assert_eq!(
        relabel(&labels(Some("needs-plan"), None)),
        " Then remove its `needs-plan` label."
    );
    assert_eq!(
        relabel(&labels(None, Some("planned"))),
        " Then add the `planned` label."
    );
    assert_eq!(relabel(&labels(None, None)), "");
}
