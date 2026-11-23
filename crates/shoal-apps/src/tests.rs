use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::protocol::ConfigTarget;

use crate::stand_in::Fixture;

#[tokio::test]
async fn an_app_reads_its_table_resolved_for_a_repository_or_the_global_config() {
    let fixture = Fixture::new().await;
    fixture.configure("[apps.quay]\nboard = 'main'\ncolumns = 2\n");
    fixture
        .daemon
        .set_local_config("[apps.quay]\ncolumns = 3\n");
    let (apps, daemon) = (&fixture.apps, &fixture.daemon);
    let quay = "quay".parse().unwrap();
    let repository = ConfigTarget::Repository(RepositoryId::generate().into());
    let table = apps.app_config(daemon, &quay, repository).await;
    let table = table.unwrap().unwrap();
    assert_eq!(table["board"].as_str(), Some("main"));
    assert_eq!(table["columns"].as_integer(), Some(3));
    let table = apps.app_config(daemon, &quay, ConfigTarget::Global).await;
    assert_eq!(table.unwrap().unwrap()["columns"].as_integer(), Some(2));
    let other = "other".parse().unwrap();
    let none = apps.app_config(daemon, &other, ConfigTarget::Global).await;
    assert_eq!(none.unwrap(), None);
}
