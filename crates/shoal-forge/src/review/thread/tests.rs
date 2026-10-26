use super::*;
use serde_json::{Value, json};

fn planned(
    forge: &str,
    thread: Thread,
    reply: Option<&str>,
    resolve: Option<bool>,
) -> Result<Vec<(String, String, Value)>> {
    let change = ThreadChange {
        comment: thread.first,
        reply: reply.map(str::to_owned),
        resolve,
    };
    Ok(ForgeRepo::parse(forge)?
        .plan_thread(7, &thread, &change)?
        .into_iter()
        .map(|request| {
            (
                request.method.as_str().to_owned(),
                request.endpoint,
                request.body.unwrap_or_default(),
            )
        })
        .collect())
}

fn thread(target: Target, resolved: Option<bool>) -> Thread {
    Thread {
        first: 5,
        replies: 0,
        resolved,
        target,
    }
}

#[test]
fn replies_come_before_resolution_on_each_forge() {
    let github = planned(
        "git@github.com:team/repo.git",
        thread(Target::GitHub { node: "T_1".into() }, Some(false)),
        Some("Fixed"),
        Some(true),
    )
    .unwrap();
    assert_eq!(
        github[0],
        (
            "POST".into(),
            "repos/team/repo/pulls/7/comments/5/replies".into(),
            json!({"body": "Fixed"})
        )
    );
    assert_eq!(github[1].1, "graphql");
    assert_eq!(github[1].2["variables"], json!({"id": "T_1"}));
    assert!(
        github[1].2["query"]
            .as_str()
            .unwrap()
            .contains("resolveReviewThread")
    );

    let gitlab = Target::GitLab {
        discussion: "abc".into(),
    };
    assert_eq!(
        planned(
            "git@gitlab.com:group/repo.git",
            thread(gitlab, Some(true)),
            Some("No"),
            Some(false)
        )
        .unwrap(),
        [
            (
                "POST".into(),
                "projects/group%2Frepo/merge_requests/7/discussions/abc/notes".into(),
                json!({"body": "No"})
            ),
            (
                "PUT".into(),
                "projects/group%2Frepo/merge_requests/7/discussions/abc".into(),
                json!({"resolved": false})
            ),
        ]
    );

    assert_eq!(
        planned(
            "git@bitbucket.org:team/repo.git",
            thread(Target::Bitbucket, Some(true)),
            Some("Why"),
            Some(false)
        )
        .unwrap(),
        [
            (
                "POST".into(),
                "repositories/team/repo/pullrequests/7/comments".into(),
                json!({"content": {"raw": "Why"}, "parent": {"id": 5}})
            ),
            (
                "DELETE".into(),
                "repositories/team/repo/pullrequests/7/comments/5/resolve".into(),
                Value::Null
            ),
        ]
    );
}

#[test]
fn forgejo_replies_on_the_threads_line_and_cannot_resolve() {
    let forgejo = "https://forge.example/team/repo";
    let old_side = Target::Forgejo {
        review: 3,
        path: "a.rs".into(),
        line: -9,
        extra_lines: 2,
    };
    assert_eq!(
        planned(
            forgejo,
            thread(old_side.clone(), Some(false)),
            Some("Kept"),
            None
        )
        .unwrap(),
        [(
            "POST".into(),
            "repos/team/repo/pulls/7/reviews/3/comments".into(),
            json!({"body": "Kept", "path": "a.rs", "old_position": 9, "extra_lines_count": 2})
        )]
    );
    // Refused before the reply is sent.
    assert!(
        planned(
            forgejo,
            thread(old_side, Some(false)),
            Some("Kept"),
            Some(true)
        )
        .is_err()
    );
}

#[test]
fn resolution_must_change() {
    let target = Target::GitHub { node: "T_1".into() };
    let github = "git@github.com:team/repo.git";
    assert!(planned(github, thread(target.clone(), Some(true)), None, Some(true)).is_err());
    assert_eq!(
        planned(github, thread(target, None), None, Some(false))
            .unwrap()
            .len(),
        1
    );
}
