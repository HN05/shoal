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
    assert_eq!(search.needs(), None);

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
    assert_eq!(search.needs(), Some(Capability::Labels));
}
