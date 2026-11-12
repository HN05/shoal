use clap::Parser;

use super::*;

#[derive(Parser)]
struct Command {
    #[command(flatten)]
    items: ItemArgs,
}

fn selection(args: &[&str]) -> Result<Selection> {
    let command = Command::try_parse_from(std::iter::once("view").chain(args.iter().copied()))?;
    command.items.selection()
}

#[test]
fn a_bare_number_leaves_its_kind_open() -> Result<()> {
    let selected = selection(&["#505"])?;
    assert_eq!(selected.kind, None);
    assert_eq!(selected.input, Some(ItemInput::Number(505)));
    assert!(selection(&["505", "506"]).is_err());
    assert!(selection(&["0"]).is_err());
    Ok(())
}

#[test]
fn a_kind_selects_its_items_or_the_one_named_after_it() -> Result<()> {
    let selected = selection(&["pr", "505"])?;
    assert_eq!(selected.kind, Some(LinkKind::Pr));
    assert_eq!(selected.input, Some(ItemInput::Number(505)));
    assert!(selection(&["issue"])?.input.is_none());
    let selected = selection(&["ci", "2765"])?;
    assert_eq!(selected.kind, Some(LinkKind::Ci));
    Ok(())
}

#[test]
fn a_url_gives_its_kind_and_must_match_the_one_named() -> Result<()> {
    let run = "https://forge.example/team/repo/actions/runs/2765/jobs/1";
    assert_eq!(selection(&[run])?.kind, Some(LinkKind::Ci));
    let pull = "https://code.corp.example/group/sub/project/-/merge_requests/3";
    assert_eq!(selection(&[pull])?.kind, Some(LinkKind::Pr));
    assert!(selection(&["pr", run]).is_err());
    assert!(selection(&["ci", "https://forge.example/team/repo/pulls/2"]).is_err());
    assert!(selection(&["pr", "https://github.com/team/repo/issues/1"]).is_err());
    Ok(())
}

#[test]
fn a_review_selection_names_no_item() {
    assert!(selection(&["review", "17"]).is_err());
}
