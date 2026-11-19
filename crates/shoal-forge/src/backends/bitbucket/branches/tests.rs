use serde_json::json;

use super::*;
use shoal_forge_types::backends::ForgeKind;

#[test]
fn a_delete_restriction_protects_the_branches_it_matches() {
    let listed = [
        json!({"name": "main", "target": {"hash": "a1", "date": "t1"}}),
        json!({"name": "topic", "target": {"hash": "b2", "date": "t2"}}),
        json!({"name": "release/1.0", "target": {"hash": "c3", "date": "t3"}}),
        json!({"name": "live", "target": {"hash": "d4", "date": "t4"}}),
    ];
    let restricted = [
        json!({"kind": "delete", "branch_match_kind": "glob", "pattern": "ma*"}),
        json!({"kind": "delete", "branch_match_kind": "branching_model", "branch_type": "release"}),
        json!({"kind": "delete", "branch_match_kind": "branching_model",
            "branch_type": "production"}),
    ];
    let model = json!({
        "development": {"name": "main", "branch": {"name": "main"}},
        "production": {"name": "live", "branch": {"name": "live"}},
        "branch_types": [{"kind": "release", "prefix": "release/"},
            {"kind": "feature", "prefix": "feature/"}],
    });
    let branches = restricted_branches(&listed, &restricted, &model).unwrap();
    let read: Vec<_> = (branches.iter())
        .map(|branch| (branch.name.as_str(), branch.head.as_str(), branch.protected))
        .collect();
    assert_eq!(
        read,
        [
            ("main", "a1", true),
            ("topic", "b2", false),
            ("release/1.0", "c3", true),
            ("live", "d4", true),
        ]
    );
    assert_eq!(branches[0].committed_at.as_deref(), Some("t1"));
}

#[test]
fn a_prs_head_in_a_fork_is_not_the_repositorys() {
    let mut repo = ForgeRepo::host("bitbucket.org", "https", ForgeKind::Bitbucket);
    repo.path = "team/project".into();
    let pull = |owner: &str| {
        json!({"id": 3, "source": {"branch": {"name": "topic"},
            "repository": {"full_name": format!("{owner}/project")}},
            "destination": {"branch": {"name": "main"}}})
    };
    let own = Bitbucket.open_pull(&repo, &pull("team")).unwrap();
    assert_eq!(
        (own.number.get(), own.fork, own.base.as_str()),
        (3, false, "main")
    );
    assert!(Bitbucket.open_pull(&repo, &pull("someone")).unwrap().fork);
}

#[test]
fn restriction_patterns_match_as_globs() {
    for (pattern, name, matches) in [
        ("main", "main", true),
        ("main", "main2", false),
        ("release/*", "release/1.0", true),
        ("release/*", "hotfix/1.0", false),
        ("*", "anything/at/all", true),
        ("*-stable", "v2-stable", true),
        ("*-stable", "v2-stable-old", false),
        ("a*b*c", "axxbyyc", true),
        ("a*b*c", "acb", false),
        ("ab*ba", "aba", false),
    ] {
        assert_eq!(glob_matches(pattern, name), matches, "{pattern} {name}");
    }
}
