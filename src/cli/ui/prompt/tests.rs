use super::*;

#[test]
fn modified_skill_choice_defaults_to_keep_and_repeats_unknown_answers() {
    let path = Path::new("/skills/shoal/SKILL.md");
    for (answers, expected) in [
        ("\n", ModifiedSkillChoice::Keep),
        ("", ModifiedSkillChoice::Keep),
        ("R\n", ModifiedSkillChoice::Replace),
        ("maybe\ndiff\n", ModifiedSkillChoice::Diff),
    ] {
        let mut output = Vec::new();
        let choice =
            modified_skill_with_io("shoal", path, &mut answers.as_bytes(), &mut output).unwrap();
        assert_eq!(choice, expected, "{answers:?}");
    }
    let mut output = Vec::new();
    modified_skill_with_io("shoal", path, &mut "x\nk\n".as_bytes(), &mut output).unwrap();
    // The unknown answer asks again.
    let output = String::from_utf8(output).unwrap();
    assert_eq!(
        output.matches("shoal at /skills/shoal/SKILL.md").count(),
        2,
        "{output}"
    );
}

#[test]
fn yes_no_questions_default_to_no_and_repeat_unknown_answers() {
    for (answers, expected) in [("\n", false), ("", false), ("maybe\nYes\n", true)] {
        let mut output = Vec::new();
        let answer = ask_yes_no("Register?", &mut answers.as_bytes(), &mut output).unwrap();
        assert_eq!(answer, expected, "{answers:?}");
    }
    let mut output = Vec::new();
    ask_yes_no("Register?", &mut "maybe\nn\n".as_bytes(), &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert_eq!(output.matches("Register?").count(), 2, "{output}");
}

#[test]
fn blank_input_takes_the_default() {
    assert_eq!(or_default(String::new(), "repo-chat"), "repo-chat");
    assert_eq!(or_default("  ".into(), "repo-chat"), "repo-chat");
    assert_eq!(or_default(" talk ".into(), "repo-chat"), "talk");
}

#[test]
fn refusals_name_the_argument_a_prompt_asks_for() {
    for (prompt, argument) in [
        ("Repository> ", "repository"),
        ("Removed workspace> ", "removed workspace"),
        ("Repository path or URL", "repository path or URL"),
        ("", ""),
    ] {
        assert_eq!(prompted_argument(prompt), argument, "{prompt:?}");
    }
}
