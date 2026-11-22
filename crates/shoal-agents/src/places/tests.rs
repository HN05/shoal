use crate::places::*;
use crate::stand_in::Fixture;
use anyhow::Context as _;
use anyhow::Result;
use shoal_core::sessions::HostName;
use shoal_core::sessions::Origin;
use shoal_core::sessions::PlaceOwnership;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

#[tokio::test]
async fn a_later_instance_under_a_reused_id_takes_its_record() -> Result<()> {
    let fixture = Arc::new(Fixture::new().await);
    let mut workspaces = Vec::new();
    for name in ["first", "second", "third"] {
        workspaces.push(fixture.workspace(name).await.id);
    }
    let session = |instance: &str| Origin {
        instance: Some(instance.into()),
        ..Origin::created(HostName::Zmx, "shared".into())
    };
    let owner = async || -> Result<Option<SpaceId>> {
        Ok(fixture
            .agents
            .place_owner(&Origin::created(HostName::Zmx, "shared".into()))
            .await?
            .map(|(owner, _)| owner))
    };
    fixture
        .agents
        .record_owned_place(
            &*fixture.daemon,
            &SpaceSelector::id(&workspaces[0]),
            session("1:1"),
        )
        .await?;
    // The session ended and another under its name began, before any agent
    // registered in it.
    fixture
        .agents
        .record_owned_place(
            &*fixture.daemon,
            &SpaceSelector::id(&workspaces[1]),
            session("2:2"),
        )
        .await?;
    assert_eq!(owner().await?.as_ref(), Some(&workspaces[1]));
    // A late record of the same instance keeps the owner it has.
    fixture
        .agents
        .record_owned_place(
            &*fixture.daemon,
            &SpaceSelector::id(&workspaces[2]),
            session("2:2"),
        )
        .await?;
    assert_eq!(owner().await?.as_ref(), Some(&workspaces[1]));
    Ok(())
}

#[tokio::test]
async fn closing_an_earlier_instance_keeps_the_record_that_replaced_it() -> Result<()> {
    let fixture = Arc::new(Fixture::new().await);
    let mut workspaces = Vec::new();
    for name in ["first", "second"] {
        workspaces.push(fixture.workspace(name).await.id);
    }
    let session = |instance: &str| Origin {
        instance: Some(instance.into()),
        ..Origin::created(HostName::Zmx, "shared".into())
    };
    fixture
        .agents
        .record_owned_place(
            &*fixture.daemon,
            &SpaceSelector::id(&workspaces[0]),
            session("1:1"),
        )
        .await?;
    // A close read the first record, then a later session's record replaced
    // it before the close forgot what it read.
    let (_, closed) = fixture
        .agents
        .place_owner(&Origin::created(HostName::Zmx, "shared".into()))
        .await?
        .context("the first record")?;
    fixture
        .agents
        .record_owned_place(
            &*fixture.daemon,
            &SpaceSelector::id(&workspaces[1]),
            session("2:2"),
        )
        .await?;
    fixture.agents.forget_owned_place(closed).await?;
    let (owner, kept) = fixture
        .agents
        .place_owner(&Origin::created(HostName::Zmx, "shared".into()))
        .await?
        .context("the later record")?;
    assert_eq!(owner, workspaces[1]);
    assert_eq!(kept.instance.as_deref(), Some("2:2"));
    Ok(())
}

#[tokio::test]
async fn a_place_closes_with_the_workspace_its_record_names() -> Result<()> {
    let fixture = Arc::new(Fixture::new().await);
    let tabbed = fixture.workspace("tabbed").await;
    let other = fixture.workspace("other").await;
    let pane = Origin::created(HostName::Herdr, "w1:p9".into());
    fixture
        .agents
        .record_owned_place(
            &*fixture.daemon,
            &SpaceSelector::id(&tabbed.id),
            pane.clone(),
        )
        .await?;
    assert!(
        fixture
            .agents
            .place_ownership(
                &*fixture.daemon,
                &SpaceSelector::id(&tabbed.id),
                &Origin::created(HostName::Herdr, "w1:p9".into())
            )
            .await?
            == PlaceOwnership::Space
    );
    assert!(
        fixture
            .agents
            .place_ownership(
                &*fixture.daemon,
                &SpaceSelector::id(&other.id),
                &Origin::created(HostName::Herdr, "w1:p9".into())
            )
            .await?
            != PlaceOwnership::Space
    );
    assert!(
        fixture
            .agents
            .place_ownership(
                &*fixture.daemon,
                &SpaceSelector::id(&tabbed.id),
                &Origin::created(HostName::Rex, "w1:p9".into())
            )
            .await?
            != PlaceOwnership::Space
    );
    // A place the daemon does not know is no one's it knows of.
    assert_eq!(
        fixture
            .agents
            .place_ownership(
                &*fixture.daemon,
                &SpaceSelector::id(&tabbed.id),
                &Origin::created(HostName::Herdr, "w1:p8".into())
            )
            .await?,
        PlaceOwnership::Unknown
    );
    // A chat that takes the place over takes it from the workspace.
    let chat = SpaceId::generate();
    fixture
        .agents
        .store
        .run(move |db| record_chat_place(db, &chat, &pane))
        .await?;
    assert_eq!(
        fixture
            .agents
            .place_ownership(
                &*fixture.daemon,
                &SpaceSelector::id(&tabbed.id),
                &Origin::created(HostName::Herdr, "w1:p9".into())
            )
            .await?,
        PlaceOwnership::Elsewhere
    );
    Ok(())
}
