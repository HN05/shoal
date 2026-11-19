use serde_json::json;

use super::*;

#[test]
fn ref_name_patterns_match_within_and_across_path_components() {
    for (pattern, reference, matches) in [
        ("refs/heads/main", "refs/heads/main", true),
        ("refs/heads/release/*", "refs/heads/release/1.0", true),
        (
            "refs/heads/release/*",
            "refs/heads/release/1.0/hotfix",
            false,
        ),
        (
            "refs/heads/release/**",
            "refs/heads/release/1.0/hotfix",
            true,
        ),
        ("refs/heads/v?", "refs/heads/v2", true),
        ("refs/heads/v?", "refs/heads/v/", false),
        ("refs/heads/*", "refs/heads/topic", true),
        ("refs/heads/*", "refs/tags/topic", false),
        ("refs/heads/release/**/*", "refs/heads/release/v1", true),
        ("refs/heads/release/**/*", "refs/heads/release/1/2/v1", true),
        ("refs/heads/v[0-9]", "refs/heads/v7", true),
        ("refs/heads/v[0-9]", "refs/heads/vx", false),
        ("refs/heads/v[!0-9]", "refs/heads/vx", true),
        ("refs/heads/v[!0-9]", "refs/heads/v/", false),
        ("refs/heads/[ab]-fix", "refs/heads/b-fix", true),
        ("refs/heads/[ab", "refs/heads/[ab", true),
    ] {
        assert_eq!(
            fnmatch(pattern.as_bytes(), reference.as_bytes()),
            matches,
            "{pattern} {reference}"
        );
    }
}

#[test]
fn a_ruleset_forbidding_deletion_protects_what_it_includes_but_excludes() {
    let ruleset = |rule: &str, include: &[&str], exclude: &[&str]| {
        json!({"rules": [{"type": rule}],
            "conditions": {"ref_name": {"include": include, "exclude": exclude}}})
    };
    assert_eq!(
        deleting_ref_names(&ruleset("non_fast_forward", &["~ALL"], &[])),
        None
    );
    let rules = DeletionRules {
        conditions: [
            ruleset("deletion", &["~DEFAULT_BRANCH"], &[]),
            ruleset(
                "deletion",
                &["refs/heads/release/*"],
                &["refs/heads/release/old"],
            ),
        ]
        .iter()
        .filter_map(deleting_ref_names)
        .collect(),
        default: "trunk".into(),
    };
    let protected: Vec<_> = ["trunk", "main", "release/2.0", "release/old", "topic"]
        .into_iter()
        .filter(|branch| rules.protects(branch))
        .collect();
    assert_eq!(protected, ["trunk", "release/2.0"]);
}
