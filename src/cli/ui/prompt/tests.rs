use super::*;

#[test]
fn modified_skill_choice_defaults_to_keep_and_repeats_unknown_answers() {
    let path = Path::new("/skills/shoal-worker/SKILL.md");
    for (answers, expected) in [
        ("\n", ModifiedSkillChoice::Keep),
        ("", ModifiedSkillChoice::Keep),
        ("R\n", ModifiedSkillChoice::Replace),
        ("maybe\ndiff\n", ModifiedSkillChoice::Diff),
    ] {
        let mut output = Vec::new();
        let choice =
            modified_skill_with_io("shoal-worker", path, &mut answers.as_bytes(), &mut output)
                .unwrap();
        assert_eq!(choice, expected, "{answers:?}");
    }
    let mut output = Vec::new();
    modified_skill_with_io("shoal-worker", path, &mut "x\nk\n".as_bytes(), &mut output).unwrap();
    let prompt = "Modified skill shoal-worker at /skills/shoal-worker/SKILL.md: [k]eep, [r]eplace, or show [d]iff [k]: ";
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("{prompt}Please enter k, r, or d.\n{prompt}")
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
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "Register? [y/N] Please enter y or n.\nRegister? [y/N] "
    );
}

#[test]
fn blank_input_takes_the_default() {
    assert_eq!(or_default(String::new(), "repo-chat"), "repo-chat");
    assert_eq!(or_default("  ".into(), "repo-chat"), "repo-chat");
    assert_eq!(or_default(" talk ".into(), "repo-chat"), "talk");
}
