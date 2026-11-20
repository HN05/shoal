use super::*;

fn code(text: &str) -> CodeArg {
    text.parse().unwrap()
}

#[test]
fn code_names_a_file_and_its_lines_after_the_last_colon() {
    let lines = |text: &str| Some(text.parse::<Lines>().unwrap());
    assert_eq!(code("src/parse.rs:40-52").lines, lines("40-52"));
    assert_eq!(code("src/parse.rs:40").path, PathBuf::from("src/parse.rs"));
    assert_eq!(code("a:b/parse.rs:7").path, PathBuf::from("a:b/parse.rs"));
    let whole = code("notes:draft.md");
    assert_eq!((whole.path, whole.lines), ("notes:draft.md".into(), None));
    assert!(":3".parse::<CodeArg>().is_err());
}
