use super::*;

fn reaction(content: &str, count: u64, reacted: bool) -> Reaction {
    Reaction {
        content: content.into(),
        count,
        reacted,
    }
}

#[test]
fn reactions_show_emoji_where_the_terminal_allows_and_names_otherwise() {
    let reactions = [
        reaction("+1", 2, true),
        reaction("heart", 1, false),
        reaction("party_parrot", 1, false),
    ];
    assert_eq!(
        reactions_text(&reactions, true),
        "👍 2 (you), ❤️ 1, party_parrot 1"
    );
    assert_eq!(
        reactions_text(&reactions, false),
        "+1 2 (you), heart 1, party_parrot 1"
    );
}
