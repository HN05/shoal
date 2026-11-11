use super::*;
use crate::time::UnixTime;

fn question(text: &str, in_chat: bool) -> Question {
    Question {
        number: crate::client::item_numbers::ItemNumber {
            space: 2.into(),
            item: 3,
        },
        space_id: "w1".into(),
        space: "cache".into(),
        created_at: UnixTime::from_seconds(0),
        ask: Ask {
            question: text.into(),
            recommendation: if in_chat {
                String::new()
            } else {
                "SQLite".into()
            },
            reason: None,
            choices: Vec::new(),
        },
        status: crate::client::questions::QuestionStatus::Open,
        in_chat,
        files: Vec::new(),
    }
}

#[test]
fn a_question_asked_in_the_chat_shows_the_message_without_a_recommendation() {
    let asked = question("Which database?\n\nI recommend SQLite.", true);
    let plain = Palette::stdout(true);
    for shown in [
        details(&asked, plain),
        render(&asked, plain),
        summary(&asked),
    ] {
        assert!(shown.contains("I recommend SQLite."), "{shown}");
        assert!(!shown.contains("Recommended"), "{shown}");
    }
}

#[test]
fn questions_show_the_files_sent_with_them() {
    let mut asked = question("Which layout?", false);
    asked.files = ["a.png", "b.png"]
        .into_iter()
        .zip(1..)
        .map(|(name, id)| crate::client::files::SentFile {
            id,
            name: name.into(),
            title: None,
            path: format!("/files/{name}").into(),
        })
        .collect();
    let plain = Palette::stdout(true);
    assert_eq!(
        details(&asked, plain),
        "Question 2.3: Which layout?\n  Space        cache\n  Recommended  SQLite\n  \
            Files        /files/a.png\n               /files/b.png"
    );
    assert_eq!(
        render(&asked, plain),
        "2.3 [open] cache: Which layout?\n  Recommended: SQLite\n  \
            Attached file: /files/a.png\n  Attached file: /files/b.png"
    );
}
