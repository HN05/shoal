use serde_json::json;

use super::*;
use crate::{
    test_support::{Received, Reply, Server, agent_on, repo_on},
    transport::limits::rate_limited,
};
use shoal_forge_types::backends::ForgeKind;

#[test]
fn searches_name_the_repository_kind_state_and_filters() {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let search = Search {
        state: StateFilter::Closed,
        labels: vec!["good first issue".into()],
        author: Some("dev".into()),
        text: Some("crash in:title".into()),
        matching: Matching::Both,
        limit: 5,
    };
    assert_eq!(
        search_query(&repo, ItemKind::Pr, &search),
        "repo:team/repo is:pr is:closed label:\"good first issue\" author:dev crash in:title"
    );
    let all = Search {
        state: StateFilter::All,
        labels: Vec::new(),
        author: None,
        text: None,
        matching: Matching::Keyword,
        limit: 5,
    };
    assert_eq!(
        search_query(&repo, ItemKind::Issue, &all),
        "repo:team/repo is:issue"
    );
}

fn items(numbers: &[u64]) -> Reply {
    let items: Vec<_> = numbers.iter().map(|n| json!({"number": n})).collect();
    (200, Vec::new(), json!({"items": items}).to_string().into())
}

fn hybrid(request: &Received) -> bool {
    request.line.contains("&search_type=hybrid&")
}

/// The numbers `matching` finds for "proxy" among open issues, up to
/// `limit`, where keyword search finds #5 and #3 and the search by meaning
/// answers with `meaning`, and the lines of the requests sent.
async fn found(
    text: Option<&str>,
    matching: Matching,
    limit: usize,
    meaning: impl Fn() -> Reply + Send + Sync + 'static,
) -> (Result<Vec<u64>>, Vec<String>) {
    let server = Server::start(move |request| {
        if hybrid(request) {
            meaning()
        } else {
            items(&[5, 3])
        }
    });
    let repo = repo_on(&server, ForgeKind::GitHub, "team/repo");
    let account = agent_on(&server, "token");
    let search = Search {
        text: text.map(str::to_owned),
        matching,
        limit,
        ..Search::picker()
    };
    let found = searched(&repo, &account, ItemKind::Issue, &search).await;
    let numbers = found.and_then(|found| found.iter().map(item_number).collect());
    let lines = server.received().into_iter().map(|r| r.line).collect();
    (numbers, lines)
}

#[tokio::test]
async fn each_match_sends_its_own_search_and_a_search_without_words_only_keywords() {
    let meaning = || items(&[4]);
    let (keyword, sent) = found(Some("proxy"), Matching::Keyword, 30, meaning).await;
    assert_eq!(keyword.unwrap(), [5, 3]);
    assert_eq!(
        sent,
        [
            "GET /api/v3/search/issues?q=repo%3Ateam%2Frepo%20is%3Aissue%20is%3Aopen%20proxy&sort=created&order=desc&per_page=100&page=1"
        ]
    );
    let (by_meaning, sent) = found(Some("proxy"), Matching::Meaning, 30, meaning).await;
    assert_eq!(by_meaning.unwrap(), [4]);
    assert_eq!(
        sent,
        [
            "GET /api/v3/search/issues?q=repo%3Ateam%2Frepo%20is%3Aissue%20is%3Aopen%20proxy&search_type=hybrid&sort=created&order=desc&per_page=100&page=1"
        ]
    );
    let (all, sent) = found(None, Matching::Both, 30, meaning).await;
    assert_eq!(all.unwrap(), [5, 3]);
    assert!(
        !sent.iter().any(|line| line.contains("search_type")),
        "{sent:?}"
    );
}

#[tokio::test]
async fn matching_both_ways_adds_the_meaning_matches_keywords_miss_newest_first() {
    let (both, sent) = found(Some("proxy"), Matching::Both, 3, || items(&[4, 3, 1])).await;
    assert_eq!(both.unwrap(), [5, 4, 3]);
    assert_eq!(sent.len(), 2, "{sent:?}");
}

#[tokio::test]
async fn matching_both_ways_takes_the_keyword_matches_when_meaning_fails() {
    let limited = || {
        let headers = [
            "x-ratelimit-resource: semantic_search",
            "x-ratelimit-remaining: 0",
            "x-ratelimit-reset: 60",
        ];
        let headers = headers.map(str::to_owned).to_vec();
        (
            403,
            headers,
            br#"{"message": "API rate limit exceeded"}"#.to_vec(),
        )
    };
    let (both, _) = found(Some("proxy"), Matching::Both, 30, limited).await;
    assert_eq!(both.unwrap(), [5, 3]);
    let (by_meaning, _) = found(Some("proxy"), Matching::Meaning, 30, limited).await;
    let error = by_meaning.unwrap_err();
    assert_eq!(
        rate_limited(&error).map(|limited| limited.limit),
        Some(Some("semantic_search"))
    );
    // Such as on a server that takes no search by meaning, which fails a
    // search by meaning alone.
    let refused = || {
        (
            422,
            Vec::new(),
            br#"{"message": "Validation Failed"}"#.to_vec(),
        )
    };
    let (both, _) = found(Some("proxy"), Matching::Both, 30, refused).await;
    assert_eq!(both.unwrap(), [5, 3]);
    let (by_meaning, _) = found(Some("proxy"), Matching::Meaning, 30, refused).await;
    assert!(by_meaning.is_err());
}
