use super::*;

fn review(verdict: ReviewVerdict, body: &str, comments: &[(&str, u64, &str)]) -> NewReview {
    NewReview {
        verdict,
        body: body.into(),
        comments: comments
            .iter()
            .map(|&(path, line, body)| LineComment {
                path: path.into(),
                line,
                side: Side::New,
                body: body.into(),
            })
            .collect(),
        commit: None,
    }
}

#[test]
fn reviews_need_a_body_or_line_comments_and_change_requests_a_body() {
    assert!(
        review(ReviewVerdict::Comment, "", &[("a.rs", 1, "x")])
            .validate()
            .is_ok()
    );
    assert!(
        review(ReviewVerdict::Approve, "Fine", &[])
            .validate()
            .is_ok()
    );
    for refused in [
        review(ReviewVerdict::Approve, " ", &[]),
        review(ReviewVerdict::RequestChanges, "", &[("a.rs", 1, "x")]),
        review(ReviewVerdict::Comment, "", &[("a.rs", 0, "x")]),
        review(ReviewVerdict::Comment, "", &[("a.rs", 3, "\n")]),
    ] {
        assert!(refused.validate().is_err(), "{refused:?}");
    }
}

#[test]
fn line_comments_read_from_json_default_to_the_new_side_and_refuse_unknowns() {
    let comments: Vec<LineComment> = serde_json::from_str(
        r#"[{"path": "a.rs", "line": 4, "body": "x"},
            {"path": "b.rs", "line": 7, "side": "old", "body": "y"}]"#,
    )
    .unwrap();
    assert_eq!(comments[0].side, Side::New);
    assert_eq!(comments[1].side, Side::Old);
    let unknown_field = r#"[{"path": "a.rs", "line": 4, "body": "x", "start_line": 2}]"#;
    assert!(serde_json::from_str::<Vec<LineComment>>(unknown_field).is_err());
    let unknown_side = r#"[{"path": "a.rs", "line": 4, "body": "x", "side": "left"}]"#;
    assert!(serde_json::from_str::<Vec<LineComment>>(unknown_side).is_err());
}
