use super::*;

#[test]
fn issue_names_are_portable_bounded_and_stable() {
    let branch = |title: &str| issue_branch(34, title);
    assert_eq!(
        branch("Fix `API`: $(touch /tmp/no); 日本語"),
        "34-fix-api-touch"
    );
    assert_eq!(
        branch("Remove a merged workspace whose HEAD is a rebased copy"),
        "34-remove-merged-workspace"
    );
    let long = format!("{} done", "x".repeat(200));
    assert_eq!(branch(&long), format!("34-{}", "x".repeat(29)));
    let fits = format!("{} done", "x".repeat(29));
    assert_eq!(branch(&fits), format!("34-{}", "x".repeat(29)));
    assert_eq!(branch(&format!("Fix {}", "x".repeat(200))), "34-fix");
    assert_eq!(branch("日本語 to the"), "issue-34");
}
