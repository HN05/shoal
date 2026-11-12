use super::*;

#[test]
fn an_item_url_reads_as_its_kind_number_and_canonical_url() -> Result<()> {
    for (url, number, canonical) in [
        (
            "https://codeberg.org/team/repo/issues/3#issuecomment-9",
            ItemNumber::Issue(IssueNumber::new(3)),
            "https://codeberg.org/team/repo/issues/3",
        ),
        (
            "https://github.com/team/repo/pull/12/",
            ItemNumber::Pr(PullNumber::new(12)),
            "https://github.com/team/repo/pull/12",
        ),
        (
            "https://code.corp.example/group/sub/project/-/merge_requests/3",
            ItemNumber::Pr(PullNumber::new(3)),
            "https://code.corp.example/group/sub/project/-/merge_requests/3",
        ),
        (
            "https://codeberg.org/team/repo/actions/runs/2765/jobs/1",
            ItemNumber::Run(RunNumber::new(2765)),
            "https://codeberg.org/team/repo/actions/runs/2765",
        ),
    ] {
        let item: ItemRef = url.parse()?;
        assert_eq!(item.number(), number, "{url}");
        assert_eq!(item.url(), canonical, "{url}");
        assert_eq!(serde_json::to_value(&item)?, canonical);
        assert_eq!(serde_json::from_value::<ItemRef>(canonical.into())?, item);
    }
    for url in [
        "https://github.com/team/repo/issues/0",
        "https://github.com/team/repo",
        "https://github.com/team/repo/tree/main",
        "42",
    ] {
        assert!(url.parse::<ItemRef>().is_err(), "{url}");
    }
    Ok(())
}

#[test]
fn an_item_url_of_another_kind_is_refused() {
    let pull = "https://codeberg.org/team/repo/pulls/12";
    assert!(ItemRef::parse_kind(LinkKind::Issue, pull).is_err());
    assert!(ItemRef::parse_kind(LinkKind::Ci, pull).is_err());
}

#[test]
fn a_number_names_an_item_in_the_repository_it_is_given() -> Result<()> {
    let own = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let item = ItemRef::in_repository(&own, LinkKind::Pr, &"#12".parse()?)?;
    assert_eq!(item.url(), "https://codeberg.org/team/repo/pulls/12");
    assert_eq!(item.number(), ItemNumber::Pr(PullNumber::new(12)));
    let pasted = "https://codeberg.org/team/repo/pulls/12".parse()?;
    assert_eq!(ItemRef::in_repository(&own, LinkKind::Pr, &pasted)?, item);
    assert!(ItemRef::in_repository(&own, LinkKind::Issue, &pasted).is_err());
    let other = "https://codeberg.org/team/other/pulls/12".parse()?;
    assert!(ItemRef::in_repository(&own, LinkKind::Pr, &other).is_err());
    Ok(())
}

#[test]
fn an_input_is_a_positive_number_or_an_item_url() -> Result<()> {
    assert_eq!("#42".parse::<ItemInput>()?, ItemInput::Number(42));
    assert_eq!(serde_json::to_value("42".parse::<ItemInput>()?)?, "42");
    let url = "https://github.com/team/repo/issues/3";
    assert_eq!(url.parse::<ItemInput>()?.kind(), Some(LinkKind::Issue));
    for input in ["0", "#", "#x", "fix-login", "99999999999999999999999"] {
        assert!(input.parse::<ItemInput>().is_err(), "{input}");
    }
    Ok(())
}

#[test]
fn a_route_in_a_repository_path_leaves_the_items_own_route_to_decide() -> Result<()> {
    let url = "https://gitlab.com/a/b/issues/project/-/merge_requests/3";
    let item: ItemRef = url.parse()?;
    assert_eq!(item.number(), ItemNumber::Pr(PullNumber::new(3)));
    assert_eq!(item.repository().path, "a/b/issues/project");
    let numbered = ItemRef::numbered(item.repository(), LinkKind::Pr, 3)?;
    assert_eq!(
        serde_json::from_value::<ItemRef>(serde_json::to_value(&numbered)?)?,
        item
    );
    Ok(())
}

#[test]
fn an_item_of_another_repository_does_not_belong_to_this_one() -> Result<()> {
    let item: ItemRef = "https://codeberg.org/team/old/issues/7".parse()?;
    assert!(item.belongs_to(item.repository()).is_ok());
    let moved = ForgeRepo::parse("https://codeberg.org/team/new")?;
    assert!(item.belongs_to(&moved).is_err());
    Ok(())
}
