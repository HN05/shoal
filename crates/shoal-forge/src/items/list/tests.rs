use super::*;
use shoal_forge_types::{
    backends::ForgeKind,
    capability::{Capability, Unsupported},
    items::list::Matching,
};

#[tokio::test]
async fn a_search_by_meaning_alone_is_refused_where_the_forge_has_none() {
    let repo = ForgeRepo::host("forge.example", "https", ForgeKind::Forgejo);
    let search = Search {
        text: Some("login fails behind a proxy".into()),
        matching: Matching::Meaning,
        ..Search::picker()
    };
    let account = Account::agent(Default::default(), Default::default());
    let error = (repo.list(&account, ItemKind::Issue, &search).await).unwrap_err();
    let refusal = error.downcast_ref::<Unsupported>();
    assert_eq!(
        refusal.map(|refusal| refusal.capability),
        Some(Capability::MeaningSearch)
    );
}
