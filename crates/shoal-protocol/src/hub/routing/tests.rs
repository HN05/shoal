use super::*;

#[test]
fn an_event_names_the_files_that_stay_on_its_machine() {
    let files = ["report.md".to_owned(), "chart.png".to_owned()];
    assert_eq!(with_files("child: done", &[]), "child: done");
    let message = with_files("child: done", &files);
    assert!(message.starts_with("child: done\n"), "{message}");
    assert!(message.ends_with("report.md, chart.png"), "{message}");
}
