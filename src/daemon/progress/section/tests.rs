use super::*;
use crate::client::progress::Step;

fn progress(status: Option<&str>, steps: &[(&str, bool)]) -> Progress {
    Progress {
        status: status.map(str::to_owned),
        steps: (steps.iter())
            .map(|(name, done)| Step {
                name: (*name).to_owned(),
                done: *done,
            })
            .collect(),
        current: None,
        updated_at: crate::time::UnixTime::EPOCH,
    }
}

#[test]
fn a_section_shows_the_status_line_and_the_steps() {
    let shown = progress(
        Some("Reproduced; the cause is the <!-- limit"),
        &[("Reproduce", true), ("Fix the timeout", false)],
    );
    assert_eq!(
        render(&shown),
        "<!-- shoal:progress -->\n\
         **Status:** Reproduced; the cause is the &lt;!-- limit\n\
         \n\
         - [x] Reproduce\n\
         - [ ] Fix the timeout\n\
         <!-- /shoal:progress -->"
    );
    assert_eq!(
        render(&progress(None, &[("Reproduce", false)])),
        "<!-- shoal:progress -->\n- [ ] Reproduce\n<!-- /shoal:progress -->"
    );
}

#[test]
fn sections_are_added_replaced_and_removed_leaving_the_body_as_it_was() {
    let first = render(&progress(Some("Reproduced"), &[]));
    let second = render(&progress(Some("Fixed"), &[("Fix", true)]));
    for body in ["Closes #12\n", "Closes #12", "", "Closes #12\n\n\n"] {
        let added = with_section(body, Some(&first));
        assert!(
            added.starts_with(body) && added.ends_with(&first),
            "{added:?}"
        );
        let replaced = with_section(&added, Some(&second));
        assert_eq!(replaced, with_section(body, Some(&second)));
        assert_eq!(with_section(&replaced, None), body);
    }
    // Manual edits inside the markers are replaced; text after them stays.
    let edited = "Intro\n\n<!-- shoal:progress -->\nmine\n<!-- /shoal:progress -->\nOutro\n";
    assert_eq!(
        with_section(edited, Some(&first)),
        format!("Intro\n\n{first}\nOutro\n")
    );
    assert_eq!(with_section(edited, None), "Intro\nOutro\n");
    assert_eq!(with_section("No section", None), "No section");
}

#[test]
fn markers_quoted_in_a_code_block_are_left_alone() {
    let section = render(&progress(Some("Reproduced"), &[]));
    let quoted = "Docs:\n\n```markdown\n<!-- shoal:progress -->\n**Status:** x\n<!-- /shoal:progress -->\n```\n";
    let added = with_section(quoted, Some(&section));
    assert_eq!(added, format!("{quoted}\n\n{section}"));
    let replaced = with_section(&added, Some(&render(&progress(Some("Fixed"), &[]))));
    assert!(replaced.starts_with(quoted), "{replaced}");
    assert_eq!(with_section(&replaced, None), quoted);
}
