use super::*;
use crate::review::view::{Comment, Review, ReviewComment};

#[test]
fn comments_list_discussion_then_inline_comments_with_their_authors() {
    let comment = |id, author: &str| Comment {
        id,
        author: author.into(),
        created_at: String::new(),
        body: String::new(),
        reactions: Vec::new(),
    };
    let review_comment = |id, author: &str| ReviewComment {
        id,
        thread: id,
        author: author.into(),
        created_at: String::new(),
        path: String::new(),
        line: None,
        body: String::new(),
        resolved: None,
        reactions: Vec::new(),
    };
    let details = Details {
        number: 4,
        title: String::new(),
        state: String::new(),
        author: "ann".into(),
        created_at: String::new(),
        labels: Vec::new(),
        body: String::new(),
        pr: None,
        comments: Some(vec![comment(10, "bob")]),
        reviews: Some(vec![Review {
            author: "bot".into(),
            state: String::new(),
            submitted_at: String::new(),
            body: String::new(),
            comments: vec![review_comment(20, "bot"), review_comment(21, "me")],
        }]),
        reactions: Vec::new(),
    };
    assert_eq!(
        comments(&details).collect::<Vec<_>>(),
        [
            (Subject::Comment(10), "bob"),
            (Subject::ReviewComment(20), "bot"),
            (Subject::ReviewComment(21), "me"),
        ]
    );
}

#[test]
fn an_edit_needs_a_body() {
    let edit = |body: &str| CommentEdit {
        comment: 3,
        body: body.into(),
    };
    assert!(edit("Fixed").validate().is_ok());
    let error = edit(" \n").validate().unwrap_err();
    assert_eq!(error.to_string(), "the comment is empty");
}
