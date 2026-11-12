use super::*;

#[test]
fn searches_name_the_repository_kind_state_and_filters() {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let search = list::Search {
        state: list::StateFilter::Closed,
        labels: vec!["good first issue".into()],
        author: Some("dev".into()),
        text: Some("crash in:title".into()),
        limit: 5,
    };
    assert_eq!(
        search_query(&repo, ItemKind::Pr, &search),
        "repo:team/repo is:pr is:closed label:\"good first issue\" author:dev crash in:title"
    );
    let all = list::Search {
        state: list::StateFilter::All,
        labels: Vec::new(),
        author: None,
        text: None,
        limit: 5,
    };
    assert_eq!(
        search_query(&repo, ItemKind::Issue, &all),
        "repo:team/repo is:issue"
    );
}

#[test]
fn graphql_errors_fail_a_successful_response() {
    let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
    assert_eq!(
        graphql_errors(&failed).unwrap(),
        "not allowed; GraphQL error"
    );
    assert_eq!(
        graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
        None
    );
    assert_eq!(graphql_errors(&serde_json::json!([])), None);
}
