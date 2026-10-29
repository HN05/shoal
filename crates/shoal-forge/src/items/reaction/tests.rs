use super::*;
use crate::review::view::{Comment, Review, ReviewComment};

fn listed(content: &str, login: &str) -> Listed {
    Listed {
        id: Value::Null,
        content: content.into(),
        login: login.into(),
    }
}

fn reaction(content: &str, count: u64, reacted: bool) -> Reaction {
    Reaction {
        content: content.into(),
        count,
        reacted,
    }
}

#[test]
fn reactions_count_by_content_in_first_use_order_and_mark_the_login() {
    let listed = [
        listed("heart", "ann"),
        listed("+1", "Bob"),
        listed("heart", "cat"),
        listed("+1", "ann"),
    ];
    assert_eq!(
        tally(&listed, "bob"),
        [reaction("heart", 2, false), reaction("+1", 2, true)]
    );
    assert!(tally(&[], "bob").is_empty());
}

#[test]
fn reactions_go_to_the_item_its_comments_and_its_inline_comments() {
    let comment = |id| Comment {
        id,
        author: String::new(),
        created_at: String::new(),
        body: String::new(),
        reactions: Vec::new(),
    };
    let review_comment = |id| ReviewComment {
        id,
        thread: id,
        author: String::new(),
        created_at: String::new(),
        path: String::new(),
        line: None,
        body: String::new(),
        resolved: None,
        reactions: Vec::new(),
    };
    let mut details = Details {
        number: 4,
        title: String::new(),
        state: String::new(),
        author: String::new(),
        created_at: String::new(),
        labels: Vec::new(),
        body: String::new(),
        pr: None,
        comments: Some(vec![comment(10), comment(11)]),
        reviews: Some(vec![Review {
            author: String::new(),
            state: String::new(),
            submitted_at: String::new(),
            body: String::new(),
            comments: vec![review_comment(20)],
        }]),
        reactions: Vec::new(),
    };
    assert_eq!(
        subjects(&details),
        [
            Subject::Item,
            Subject::Comment(10),
            Subject::Comment(11),
            Subject::ReviewComment(20)
        ]
    );
    let one = |content: &str| vec![reaction(content, 1, false)];
    place(
        &mut details,
        vec![one("+1"), Vec::new(), one("eyes"), one("heart")],
    );
    assert_eq!(details.reactions, one("+1"));
    let comments = details.comments.as_ref().unwrap();
    assert!(comments[0].reactions.is_empty());
    assert_eq!(comments[1].reactions, one("eyes"));
    let reviews = details.reviews.as_ref().unwrap();
    assert_eq!(reviews[0].comments[0].reactions, one("heart"));
}
