use super::*;

#[test]
fn closed_includes_merged_and_searches_refuse_bad_filters() {
    let mut search = Search::open(30);
    assert!(search.includes(ItemState::Open));
    assert!(!search.includes(ItemState::Merged));
    search.state = StateFilter::Closed;
    assert!(search.includes(ItemState::Merged));
    assert!(!search.includes(ItemState::Open));
    assert!(search.check().is_ok());
    assert_eq!(search.needs(), []);

    for change in [
        |search: &mut Search| search.limit = 0,
        |search: &mut Search| search.limit = MAX_LIMIT + 1,
        |search: &mut Search| search.text = Some(" ".into()),
        |search: &mut Search| search.author = Some(String::new()),
        |search: &mut Search| search.labels = vec![String::new()],
    ] {
        let mut search = Search::open(30);
        change(&mut search);
        assert!(search.check().is_err(), "{search:?}");
    }
    search.labels = vec!["bug".into()];
    assert!(search.check().is_ok());
    assert_eq!(search.needs(), [Capability::Labels]);
}

#[test]
fn only_a_search_for_words_by_meaning_alone_needs_a_search_by_meaning() {
    let mut search = Search::open(30);
    search.matching = Matching::Meaning;
    assert_eq!(search.matched_by(), Matching::Keyword);
    assert_eq!(search.needs(), []);
    search.text = Some("login fails behind a proxy".into());
    search.labels = vec!["bug".into()];
    assert_eq!(search.matched_by(), Matching::Meaning);
    assert_eq!(
        search.needs(),
        [Capability::Labels, Capability::MeaningSearch]
    );
    search.matching = Matching::Both;
    assert_eq!(search.matched_by(), Matching::Both);
    assert_eq!(search.needs(), [Capability::Labels]);
}
