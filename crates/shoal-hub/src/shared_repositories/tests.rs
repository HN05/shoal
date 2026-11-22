use std::collections::BTreeMap;

use shoal_protocol::hub::sharing;

use super::*;

const URL: &str = "https://example.test/team/app.git";

fn repository(id: &str, source: &str, shared_url: Option<&str>) -> Repository {
    Repository {
        id: id.into(),
        path: format!("/repositories/{id}").into(),
        source: source.into(),
        last_used: 0,
        name: Some(id.into()),
        workspaces_dir: format!("/repositories/{id}").into(),
        contribution: false,
        shared_url: shared_url.map(Into::into),
    }
}

fn app(url: &str, name: &str) -> SharedRepository {
    SharedRepository {
        url: url.into(),
        name: name.into(),
    }
}

fn copy(value: Option<SharedRepository>, revision: i64) -> Copy<SharedRepositories> {
    sharing::Copy {
        key: Origin::of(URL),
        value,
        revision,
    }
}

#[test]
fn a_shared_registration_is_an_item_as_the_hub_last_agreed_on_it() {
    let ssh = "git@example.test:team/app.git";
    let repositories = [
        repository("app", ssh, Some(ssh)),
        repository("other", "/checkouts/other", None),
    ];
    // Shared here first, it is the registration's own.
    let items = shared(&repositories, &BTreeMap::new());
    assert_eq!(items, BTreeMap::from([(Origin::of(URL), app(ssh, "app"))]));
    // Agreed on under another URL and name, the hub's is no edit.
    let agreed = BTreeMap::from([(Origin::of(URL), copy(Some(app(URL, "desk-app")), 3))]);
    let items = shared(&repositories, &agreed);
    assert_eq!(
        items,
        BTreeMap::from([(Origin::of(URL), app(URL, "desk-app"))])
    );
}

#[tokio::test]
async fn a_copy_shares_the_registration_of_its_origin_or_clones_one_where_there_is_none() {
    let shared_copy = copy(Some(app(URL, "app")), 2);
    let registered = [repository("mine", URL, None)];
    assert_eq!(
        applying(&shared_copy, &registered).await,
        Applying::Share(vec!["mine".into()], Some(URL.into()))
    );
    let elsewhere = [repository(
        "other",
        "https://example.test/team/other.git",
        None,
    )];
    assert_eq!(applying(&shared_copy, &elsewhere).await, Applying::Clone);
    // A copy that unshares it leaves the registration, no longer shared.
    let sharing_here = [repository("mine", URL, Some(URL))];
    assert_eq!(
        applying(&copy(None, 3), &sharing_here).await,
        Applying::Share(vec!["mine".into()], None)
    );
}
