use super::*;

fn reference(state: CodeState, snippet: Option<&str>) -> CodeRef {
    let at = |lines: &str| CodeAt {
        path: "src/parse.rs".into(),
        lines: Some(lines.parse().unwrap()),
        blob: None,
    };
    CodeRef {
        id: 1,
        space: ThreadSpace {
            id: "w1".into(),
            name: None,
            number: None,
        },
        commit: None,
        original: at("40-52"),
        snippet: snippet.map(str::to_owned),
        latest: at("43-55"),
        state,
    }
}

#[test]
fn a_reference_reads_as_its_place_now_with_its_snippet() {
    let unchanged = reference(CodeState::Unchanged, Some("fn parse() {\n\n}"));
    assert_eq!(unchanged.place(), "src/parse.rs:43-55");
    assert_eq!(unchanged.snippet_lines("  "), "\n  fn parse() {\n\n  }");
    let outdated = reference(CodeState::Outdated, None);
    assert_eq!(outdated.place(), "src/parse.rs:43-55 (outdated)");
    assert_eq!(outdated.snippet_lines("  "), "");
}

#[test]
fn a_snippet_keeps_whole_lines_of_its_range_up_to_its_limit() {
    let text = "a\nb\nc\nd\n";
    assert_eq!(snippet(text, "2-3".parse().unwrap()), "b\nc");
    let long = format!("{}\n", "x".repeat(MAX_SNIPPET_LEN - 10)).repeat(3);
    let cut = snippet(&long, "1-3".parse().unwrap());
    assert_eq!(cut.lines().count(), 2);
    assert!(cut.ends_with('…'));
}

#[test]
fn a_changed_or_outdated_reference_stays_so() {
    use CodeState::*;
    assert_eq!(Unchanged.then(Changed), Changed);
    assert_eq!(Changed.then(Unchanged), Changed);
    assert_eq!(Outdated.then(Changed), Outdated);
}
