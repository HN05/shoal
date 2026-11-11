use super::*;

/// A chat copy started as `chat` in the background there.
fn chat_copy(chat: &str) -> Imported {
    Imported::Chat(crate::client::output::chat::BackgroundChat {
        chat: chat.into(),
        agent: "claude".into(),
        repository: Some("app".into()),
        place: "zellij".into(),
    })
}

#[test]
fn a_copy_under_another_name_is_named() {
    let renamed = copied_message("talk", "other", &chat_copy("talk-2"));
    assert!(renamed.contains("as talk-2"), "{renamed}");
    let same = copied_message("talk", "other", &chat_copy("talk"));
    assert!(!same.contains(" as "), "{same}");
}
