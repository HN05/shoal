use shoal_protocol::protocol::ConfigTarget;

use crate::{config::Config, daemon::test_daemon::TestDaemon};

#[tokio::test]
async fn an_app_reads_its_table_resolved_for_a_repository_or_the_global_config() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let global = "[apps.quay]\nboard = 'main'\ncolumns = 2\n";
    manager
        .config_state
        .publish(Config::from_toml(global).unwrap());
    let local = manager
        .local_config_path(&daemon.repository.id)
        .await
        .unwrap();
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, "[apps.quay]\ncolumns = 3\n").unwrap();
    let quay = "quay".parse().unwrap();
    let repository = ConfigTarget::Repository(daemon.repository.id.clone().into());
    let table = manager
        .app_config(&quay, repository)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(table["board"].as_str(), Some("main"));
    assert_eq!(table["columns"].as_integer(), Some(3));
    let table = (manager.app_config(&quay, ConfigTarget::Global).await)
        .unwrap()
        .unwrap();
    assert_eq!(table["columns"].as_integer(), Some(2));
    let other = "other".parse().unwrap();
    let none = manager.app_config(&other, ConfigTarget::Global).await;
    assert_eq!(none.unwrap(), None);
}
