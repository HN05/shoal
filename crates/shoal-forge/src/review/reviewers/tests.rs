use super::*;

#[test]
fn team_names_have_an_owner() {
    assert_eq!(team_name("team/core"), Some(("team", "core")));
    assert_eq!(team_name("kim"), None);
    assert_eq!(Reviewer::team("team", "core", None).name, "team/core");
}
