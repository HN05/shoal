use clap::Parser;

use super::*;

#[derive(Parser)]
struct Command {
    #[command(flatten)]
    merge: MergeArgs,
}

fn parsed(args: &[&str]) -> Result<MergeArgs> {
    let command = Command::try_parse_from(std::iter::once("merge").chain(args.iter().copied()))?;
    Ok(command.merge)
}

fn merged(args: &[&str]) -> Result<Merged> {
    parsed(args)?.merged()
}

const PULL: &str = "https://forge.example/team/app/pulls/12";

#[test]
fn without_an_item_the_space_s_work_merges() -> Result<()> {
    assert_eq!(merged(&[])?, Merged::Work);
    assert_eq!(merged(&["-s", "3"])?, Merged::Work);
    // A repository names where a PR number is, so it needs one.
    assert!(merged(&["--repo", "team/app"]).is_err());
    Ok(())
}

#[test]
fn a_number_a_url_or_pr_names_a_pr() -> Result<()> {
    assert_eq!(merged(&["12"])?, Merged::Pr(Some(ItemInput::Number(12))));
    assert_eq!(
        merged(&["pr", "12"])?,
        Merged::Pr(Some(ItemInput::Number(12)))
    );
    assert_eq!(merged(&["pr"])?, Merged::Pr(None));
    let Merged::Pr(Some(ItemInput::Url(url))) = merged(&[PULL])? else {
        panic!("{PULL} names no PR");
    };
    assert_eq!(url.to_string(), PULL);
    assert_eq!(
        merged(&["12", "--repo", "team/other"])?,
        Merged::Pr(Some(ItemInput::Number(12)))
    );
    Ok(())
}

#[test]
fn only_prs_merge() {
    for args in [
        &["issue", "4"][..],
        &["ci", "2765"],
        &["review"],
        &["https://forge.example/team/app/issues/4"],
        &["https://forge.example/team/app/actions/runs/2765"],
    ] {
        assert!(merged(args).is_err(), "{args:?} merged");
    }
}

#[test]
fn a_merge_refuses_the_flags_of_the_path_it_does_not_take() -> Result<()> {
    assert!(parsed(&["--push"])?.ensure_pull_flags("PR 12").is_err());
    assert!(parsed(&["--no-push"])?.ensure_pull_flags("PR 12").is_err());
    assert!(
        parsed(&["-X", "squash", "--auto"])?
            .ensure_pull_flags("PR 12")
            .is_ok()
    );
    for flags in [&["--delete-branch"][..], &["--auto"], &["--cancel-auto"]] {
        let args = parsed(flags)?;
        assert!(args.ensure_local_flags("space 3").is_err(), "{flags:?}");
    }
    assert!(
        parsed(&["--push", "--yes"])?
            .ensure_local_flags("space 3")
            .is_ok()
    );
    Ok(())
}

#[test]
fn a_local_merge_takes_only_the_merge_method() -> Result<()> {
    assert!(
        parsed(&["-X", "merge"])?
            .ensure_local_flags("space 3")
            .is_ok()
    );
    assert!(
        parsed(&["-X", "squash"])?
            .ensure_local_flags("space 3")
            .is_err()
    );
    assert!(
        parsed(&["-X", "rebase"])?
            .ensure_local_flags("space 3")
            .is_err()
    );
    Ok(())
}

#[test]
fn push_flags_override_the_configured_push() -> Result<()> {
    assert_eq!(parsed(&[])?.push(), None);
    assert_eq!(parsed(&["--push"])?.push(), Some(true));
    assert_eq!(parsed(&["--push", "--no-push"])?.push(), Some(false));
    Ok(())
}
