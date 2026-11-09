use super::*;
use crate::items::item::ItemState;

fn item(kind: ItemKind) -> Item {
    Item {
        kind,
        number: 12,
        url: "https://forge.example/team/repo/pulls/12".into(),
        title: "Fix".into(),
        state: ItemState::Open,
        labels: Vec::new(),
        assignees: Vec::new(),
        milestone: None,
        pr: None,
    }
}

fn label(name: &str, description: &str) -> Label {
    Label {
        name: name.into(),
        description: description.into(),
        exclusive: false,
    }
}

#[test]
fn required_entries_are_label_prefixes() {
    let required = vec!["area/".to_owned(), "type/".to_owned(), "bug".to_owned()];
    assert_eq!(
        missing(&required, &["area/cli".into(), "bugfix".into()]),
        ["type/"]
    );
    assert_eq!(missing(&required, &[]), ["area/", "type/", "bug"]);
    assert!(missing(&[], &[]).is_empty());
}

#[test]
fn refusals_offer_the_labels_that_satisfy_each_missing_entry() {
    let available = [
        label("area/cli", "Command line"),
        label("type/bug", ""),
        label("tracking", "Standing issue"),
    ];
    let refused = refusal(ItemKind::Pr, &["area/", "type/"], &available);
    assert!(refused.contains("area/, type/"), "{refused}");
    assert!(
        refused.ends_with(":\n  area/cli: Command line\n  type/bug"),
        "{refused}"
    );
    // Without matching labels the refusal lists none.
    assert_eq!(refusal(ItemKind::Issue, &["area/"], &[]).lines().count(), 1);
}

#[test]
fn headings_skip_front_matter_fences_and_hashtags() {
    let text = "---\nname: Bug\nlabels: type/bug\n---\n## Summary\n#hashtag\n```\n# not one\n```\n### Testing ###\n";
    assert_eq!(headings(without_front_matter(text)), ["Summary", "Testing"]);
    assert_eq!(without_front_matter("---\nunclosed"), "---\nunclosed");
}

#[test]
fn descriptions_are_checked_against_the_forge_templates() {
    let root = tempfile::tempdir().unwrap();
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let pr = item(ItemKind::Pr);
    // No template asks for nothing.
    assert_eq!(template_warning(&forgejo, root.path(), &pr, ""), None);

    std::fs::create_dir(root.path().join(".forgejo")).unwrap();
    std::fs::write(
        root.path().join(".forgejo/PULL_REQUEST_TEMPLATE.md"),
        "## Summary\n\n## Testing\n",
    )
    .unwrap();
    let warning = template_warning(&forgejo, root.path(), &pr, "Closes #3").unwrap();
    assert!(
        warning.contains(".forgejo/PULL_REQUEST_TEMPLATE.md: Summary, Testing;"),
        "{warning}"
    );
    let written = "## summary\nFix\n## Testing\ncargo test\n";
    assert_eq!(template_warning(&forgejo, root.path(), &pr, written), None);

    let issues = root.path().join(".github/ISSUE_TEMPLATE");
    std::fs::create_dir_all(&issues).unwrap();
    std::fs::write(issues.join("bug.md"), "---\nname: Bug\n---\n## Steps\n").unwrap();
    std::fs::write(issues.join("idea.md"), "## Motivation\n").unwrap();
    std::fs::write(issues.join("config.yml"), "blank_issues_enabled: false\n").unwrap();
    let issue = item(ItemKind::Issue);
    let warning = template_warning(&forgejo, root.path(), &issue, "Broken").unwrap();
    assert!(
        warning.contains(
            "none of the templates .github/ISSUE_TEMPLATE/bug.md, .github/ISSUE_TEMPLATE/idea.md;"
        ),
        "{warning}"
    );
    assert_eq!(
        template_warning(&forgejo, root.path(), &issue, "## Motivation\nWhy"),
        None
    );

    // A freeform choice among headed ones asks for nothing.
    std::fs::write(issues.join("blank.md"), "Describe it.\n").unwrap();
    assert_eq!(
        template_warning(&forgejo, root.path(), &issue, "Broken"),
        None
    );

    let bitbucket = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    assert_eq!(template_warning(&bitbucket, root.path(), &pr, ""), None);
}

#[test]
fn a_template_without_headings_still_takes_precedence() {
    let root = tempfile::tempdir().unwrap();
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    for (dir, text) in [
        (".forgejo", "Say what changed.\n"),
        (".github", "## Summary\n"),
    ] {
        std::fs::create_dir(root.path().join(dir)).unwrap();
        std::fs::write(root.path().join(dir).join("pull_request_template.md"), text).unwrap();
    }
    let pr = item(ItemKind::Pr);
    assert_eq!(template_warning(&forgejo, root.path(), &pr, "Fix"), None);
}

#[test]
fn linked_issue_closes_only_without_a_description() {
    assert_eq!(pull_description(None, None), "");
    assert_eq!(
        pull_description(None, Some(IssueNumber::new(7))),
        "Closes #7"
    );
    assert_eq!(
        pull_description(Some("".into()), Some(IssueNumber::new(7))),
        ""
    );
}
