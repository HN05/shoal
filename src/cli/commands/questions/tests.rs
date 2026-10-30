use super::*;

fn question(text: &str, in_chat: bool) -> Question {
    Question {
        id: 3,
        workspace_id: "w1".into(),
        workspace: "cache".into(),
        created_at: 0,
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
        answer: None,
        answered_at: None,
        message_id: None,
        closed_at: None,
        withdrawn: false,
        in_chat,
    }
}

#[test]
fn a_question_asked_in_the_chat_shows_the_message_without_a_recommendation() {
    let asked = question("Which database?\n\nI recommend SQLite.", true);
    let plain = Palette::stdout(true);
    assert_eq!(
        details(&asked, plain),
        "Question 3, asked in the chat\n  Which database?\n\n  I recommend SQLite.\n  Workspace  cache"
    );
    assert_eq!(
        render(&asked, plain),
        "3 [open] cache: Which database?\n\n  I recommend SQLite."
    );
    assert_eq!(
        summary(&asked),
        "3  cache: Which database?  I recommend SQLite."
    );
}

#[test]
fn other_questions_show_their_recommendation() {
    let asked = question("Which database?", false);
    let plain = Palette::stdout(true);
    assert_eq!(
        details(&asked, plain),
        "Question 3: Which database?\n  Workspace    cache\n  Recommended  SQLite"
    );
    assert_eq!(
        summary(&asked),
        "3  cache: Which database?  Recommended: SQLite"
    );
}
