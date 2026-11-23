use shoal_core::config::Config;

use super::*;
use crate::{
    identity::{Identity, KeyPair},
    stand_in::{self, StandIn},
    test_support::linked,
};

/// The hub, with a machine and a device joined, as it records them.
async fn hub() -> (tempfile::TempDir, Arc<Hub>, Member, Member) {
    let root = tempfile::tempdir().unwrap();
    let config = "[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n";
    let hub = stand_in::hub(&root, Config::from_toml(config).unwrap()).await;
    hub.own_member().await.unwrap();
    for (kind, name) in [
        (MemberKind::Machine, "laptop"),
        (MemberKind::Device, "phone"),
    ] {
        let key = Identity::new(&KeyPair::generate().unwrap()).unwrap().public;
        (hub.store)
            .write(move |tx| members::join(tx, kind, name, &key).map(drop))
            .await
            .unwrap();
    }
    let own = hub.hub_identity().await.unwrap().public.clone();
    let members = (hub.store)
        .read(move |tx| members::list(tx, &own))
        .await
        .unwrap();
    let named = |name: &str| (members.iter().find(|member| member.name == name)).cloned();
    let (laptop, phone) = (named("laptop").unwrap(), named("phone").unwrap());
    (root, hub, laptop, phone)
}

/// `member`'s call for what `printf sk-hub` prints on the hub's own
/// machine.
async fn ask(hub: &Arc<Hub>, member: &Member) -> Reply {
    let machine = hub.own_member().await.unwrap().to_string();
    let call = Call::Value {
        machine,
        setting: "machine.proxies.gateway.key".into(),
        command: vec!["printf".into(), "sk-hub".into()],
    };
    let daemon: Arc<dyn Daemon> = Arc::new(StandIn::default());
    let link = linked();
    hub.answer_member(&daemon, member, call, &link.opener)
        .await
        .0
}

/// The settings the hub records `requester` fetched.
async fn fetched(hub: &Hub, requester: &Member) -> Vec<String> {
    let requester = requester.id.clone();
    (hub.store)
        .read(move |tx| {
            let mut settings =
                tx.prepare("SELECT setting FROM hub_value_fetches WHERE requester=?1")?;
            let rows = settings.query_map([&requester], |row| row.get(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_machine_gets_what_another_prints_and_the_hub_records_the_setting() {
    let (_root, hub, laptop, _) = hub().await;

    let Reply::Value(Resolved(value)) = ask(&hub, &laptop).await else {
        panic!("expected a value");
    };

    assert_eq!(value, "sk-hub");
    assert_eq!(
        fetched(&hub, &laptop).await,
        ["machine.proxies.gateway.key"]
    );
}

#[tokio::test]
async fn a_device_has_no_machine_run_a_command() {
    let (_root, hub, _, phone) = hub().await;

    let reply = ask(&hub, &phone).await;

    let Reply::Failed { message } = reply else {
        panic!("expected a refusal");
    };
    assert!(message.contains("only a machine"), "{message}");
    assert!(fetched(&hub, &phone).await.is_empty());
}

/// A link whose other side answers every call with a failure, as a machine
/// whose command failed does, and the task that serves that side.
fn failing_link() -> (Arc<crate::session::Calls>, tokio::task::JoinHandle<()>) {
    use crate::{session::serve, transport::Channel};
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let side = |stream: tokio::io::DuplexStream| {
        let (read, write) = tokio::io::split(stream);
        Channel::lines(tokio::io::BufReader::new(read), write)
    };
    let (ours, theirs) = (side(ours), side(theirs));
    let calls = crate::session::Calls::new(ours.sender(), "desk");
    let served = calls.clone();
    tokio::spawn(async move {
        serve(
            ours,
            &served,
            |_| async { (Reply::Left, ()) },
            std::future::pending(),
        )
        .await;
    });
    let answering = crate::session::Calls::new(theirs.sender(), "laptop");
    let task = tokio::spawn(async move {
        let failed = |_| async {
            let message = "security exited with 44".to_owned();
            (Reply::Failed { message }, ())
        };
        serve(theirs, &answering, failed, std::future::pending()).await;
    });
    (calls, task)
}

fn run_call() -> Call {
    Call::RunValue {
        requester: "laptop".into(),
        setting: "machine.proxies.gateway.key".into(),
        command: vec!["security".into()],
    }
}

#[tokio::test]
async fn a_command_that_failed_there_is_no_unreachable_machine() {
    let (calls, _task) = failing_link();

    let error = calls.call(run_call()).await.unwrap_err();

    assert!(matches!(unanswered(error), Unanswered::Failed(_)));
}

#[tokio::test]
async fn a_link_that_ended_leaves_the_machine_unreachable() {
    let (calls, task) = failing_link();
    task.abort();
    let _ = task.await;

    let error = calls.call(run_call()).await.unwrap_err();

    assert!(matches!(unanswered(error), Unanswered::Unreachable(_)));
}
