//! The webhook listener over HTTP: which requests fire their trigger, and
//! that a daemon without `listen` opens no port; and `shoal trigger run`
//! with a sample request.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use shoal_protocol::triggers::{Fired, RunOrigin, RunOutcome, TriggerRun, WebhookRequest};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
};

use super::{MAX_BODY, MAX_DELIVERIES, accept};
use crate::{
    stand_in::Fixture,
    time::UnixTime,
    triggers::tests::{outcomes, runs},
};

const SECRET: &str = "s3cret";

/// A trigger that starts a chat on each issue a signed Forgejo request
/// says was labelled `agent`, with the secret its command prints.
const LABELLED: &str = "[secrets.HOOK]\nvalue = { cmd = ['printf', 's3cret'] }\n\
    [triggers.labelled]\non = 'webhook'\nsecret = 'HOOK'\nsignature = 'forgejo'\n\
    mode = 'chat'\nagent = 'claude'\nname = 'Labelled'\noverlap = 'parallel'\n\
    issue = '{{ payload.issue.number }}'\ntask = 'Handle {{ payload.issue.title }}'\n\
    filter = [{ path = 'headers.x-forgejo-event', equals = 'issues' },\n\
              { path = 'payload.issue.labels[].name', equals = 'agent' }]\n";

const ISSUE: &str = r#"{"action":"label_updated","issue":{"number":42,"title":"Crash","labels":[{"name":"agent"}]}}"#;

/// The repository's local config defines `triggers`, and its webhooks are
/// served on a port of their own.
async fn listening(triggers: &str) -> (Fixture, SocketAddr) {
    listening_with(triggers, MAX_DELIVERIES).await
}

/// [`listening`] with `permits` deliveries in flight at most.
async fn listening_with(triggers: &str, permits: usize) -> (Fixture, SocketAddr) {
    let fixture = Fixture::new(triggers).await;
    fixture.configure("[machine.webhooks]\nlisten = '127.0.0.1:0'\n");
    let (runner, daemon) = (fixture.runner.clone(), fixture.daemon());
    let address = runner.webhook_address(&*daemon);
    let listener = (runner.bind_webhooks(address).await).unwrap();
    let address = listener.local_addr().unwrap();
    let deliveries = Arc::new(Semaphore::new(permits));
    tokio::spawn(async move { accept(&runner, &daemon, &deliveries, Some(&listener)).await });
    (fixture, address)
}

/// The status of a POST of `body` with `headers` to `path`.
async fn post(address: SocketAddr, path: &str, headers: &[(&str, &str)], body: &[u8]) -> u16 {
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: shoal\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    // The listener may answer a body over its limit before taking all of it.
    let _ = stream.write_all(body).await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let response = String::from_utf8_lossy(&response);
    let status = response.split(' ').nth(1).unwrap_or_default();
    status.parse().unwrap_or_else(|_| panic!("{response}"))
}

/// The hex HMAC-SHA256 of `body` under `secret`.
fn sign(secret: &str, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A Forgejo "issues" delivery of `body` signed with `secret`.
async fn deliver(address: SocketAddr, delivery: &str, secret: &str, body: &str) -> u16 {
    let signature = sign(secret, body);
    let headers = [
        ("Content-Type", "application/json"),
        ("X-Forgejo-Event", "issues"),
        ("X-Forgejo-Delivery", delivery),
        ("X-Forgejo-Signature", signature.as_str()),
    ];
    post(
        address,
        "/webhooks/repo/labelled",
        &headers,
        body.as_bytes(),
    )
    .await
}

/// The trigger's runs once it has `count`, which an accepted request
/// records after its response.
async fn runs_once(fixture: &Fixture, count: usize) -> Vec<TriggerRun> {
    let mut changes = fixture.runner.store.watch_changes();
    let wait = async {
        loop {
            let runs = runs(fixture, "labelled").await;
            if runs.len() >= count {
                return runs;
            }
            changes.changed().await.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(60), wait)
        .await
        .expect("the trigger never recorded its runs")
}

#[tokio::test]
async fn a_signed_delivery_starts_its_run_once() {
    let (fixture, address) = listening(LABELLED).await;

    assert_eq!(deliver(address, "d-1", SECRET, ISSUE).await, 202);
    runs_once(&fixture, 1).await;
    assert_eq!(deliver(address, "d-1", SECRET, ISSUE).await, 202);

    let runs = runs_once(&fixture, 2).await;
    assert_eq!(
        outcomes(&runs),
        [RunOutcome::Started, RunOutcome::Duplicate]
    );
    let tasks = fixture.daemon.tasks();
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0][0].contains(">Crash</data-"), "{:?}", tasks[0]);
}

#[tokio::test]
async fn a_request_no_active_trigger_takes_is_refused_without_a_run() {
    let (fixture, address) = listening(LABELLED).await;
    let body = ISSUE.as_bytes();

    // A wrong signature, an unknown route and another method.
    assert_eq!(deliver(address, "d-1", "guess", ISSUE).await, 404);
    assert_eq!(post(address, "/webhooks/repo/other", &[], body).await, 404);
    assert_eq!(post(address, "/webhooks/labelled", &[], body).await, 404);
    assert_eq!(
        post(address, "/webhooks/other/labelled", &[], body).await,
        404
    );
    let mut stream = TcpStream::connect(address).await.unwrap();
    (stream.write_all(
        b"GET /webhooks/repo/labelled HTTP/1.1\r\nHost: shoal\r\nConnection: close\r\n\r\n",
    ))
    .await
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    // A body over the limit.
    let large = vec![b' '; MAX_BODY + 1];
    assert_eq!(
        post(address, "/webhooks/repo/labelled", &[], &large).await,
        413
    );
    // A paused trigger.
    let daemon = fixture.daemon();
    (fixture
        .runner
        .pause_trigger(&*daemon, fixture.context(), "labelled", true, None))
    .await
    .unwrap();
    assert_eq!(deliver(address, "d-2", SECRET, ISSUE).await, 404);

    // Each was refused before anything could fire.
    assert!(runs(&fixture, "labelled").await.is_empty());
}

#[tokio::test]
async fn a_repositorys_trigger_never_signs_with_a_global_secret() {
    // Another repository's agents receive the global secret.
    let local = LABELLED.replace(
        "[secrets.HOOK]\nvalue = { cmd = ['printf', 's3cret'] }\n",
        "",
    );
    let (fixture, address) = listening(&local).await;
    fixture.configure(
        "[machine.webhooks]\nlisten = '127.0.0.1:0'\n[secrets.HOOK]\nvalue = { cmd = ['printf', 's3cret'] }\n",
    );

    assert_eq!(deliver(address, "d-1", SECRET, ISSUE).await, 500);
    assert!(runs(&fixture, "labelled").await.is_empty());
}

#[tokio::test]
async fn a_delivery_accepted_before_a_pause_never_fires() {
    let fixture = Fixture::new(LABELLED).await;
    let daemon = fixture.daemon();
    let triggers = fixture.runner.configured_triggers(&*daemon).await;
    let trigger = (triggers.into_iter())
        .find(|trigger| trigger.name == "labelled")
        .unwrap();
    let request = WebhookRequest {
        headers: vec![("X-Forgejo-Event".into(), "issues".into())],
        query: Vec::new(),
        body: ISSUE.to_owned(),
    };
    (fixture
        .runner
        .pause_trigger(&*daemon, fixture.context(), "labelled", true, None))
    .await
    .unwrap();

    // Its firing waits for the lock, past the pause.
    (fixture
        .runner
        .fire_accepted(&daemon, &trigger, &request, UnixTime::now()))
    .await
    .unwrap();

    assert!(runs(&fixture, "labelled").await.is_empty());
    assert!(fixture.daemon.tasks().is_empty());
}

#[tokio::test]
async fn a_filtered_delivery_is_accepted_like_any_other() {
    let (fixture, address) = listening(LABELLED).await;
    let unlabelled = r#"{"issue":{"number":7,"title":"Typo","labels":[]}}"#;

    assert_eq!(deliver(address, "d-1", SECRET, unlabelled).await, 202);
    // The labelled delivery after it fires once the filter dropped the first.
    assert_eq!(deliver(address, "d-2", SECRET, ISSUE).await, 202);

    let runs = runs_once(&fixture, 1).await;
    assert_eq!(outcomes(&runs), [RunOutcome::Started]);
    assert_eq!(fixture.daemon.tasks().len(), 1);
}

#[tokio::test]
async fn a_request_past_the_deliveries_in_flight_is_turned_away() {
    let (fixture, address) = listening_with(LABELLED, 0).await;

    assert_eq!(deliver(address, "d-1", SECRET, ISSUE).await, 503);
    assert!(runs(&fixture, "labelled").await.is_empty());
}

#[tokio::test]
async fn without_listen_no_port_opens() {
    let fixture = Fixture::new(LABELLED).await;
    let address = fixture.runner.webhook_address(&*fixture.daemon());
    assert_eq!(address, None);
    assert!(fixture.runner.bind_webhooks(address).await.is_none());
}

#[tokio::test]
async fn a_sample_request_fires_unsigned_and_names_the_condition_that_drops_it() {
    let fixture = Fixture::new(LABELLED).await;
    let daemon = fixture.daemon();
    let run = |request| {
        fixture
            .runner
            .run_trigger(&daemon, fixture.context(), "labelled", Some(request), None)
    };
    let sample = |body: &str| WebhookRequest {
        headers: vec![("X-Forgejo-Event".into(), "issues".into())],
        query: Vec::new(),
        body: body.to_owned(),
    };

    let unlabelled = r#"{"issue":{"number":7,"labels":[]}}"#;
    let dropped = run(sample(unlabelled)).await.unwrap();
    let ran = run(sample(ISSUE)).await.unwrap();

    let Fired::Dropped { condition } = dropped else {
        panic!("{dropped:?}");
    };
    assert!(
        condition.starts_with("payload.issue.labels[].name"),
        "{condition}"
    );
    let Fired::Ran { run } = ran else {
        panic!("{ran:?}");
    };
    assert_eq!(
        (run.outcome, run.origin),
        (RunOutcome::Started, RunOrigin::Manual)
    );
}

#[tokio::test(start_paused = true)]
async fn a_secret_command_that_never_answers_fails_the_delivery() {
    let secret = shoal_core::secrets::Secret {
        value: shoal_protocol::config::value::Value::Cmd {
            cmd: vec!["sleep".into(), "3600".into()],
            on: None,
        },
        tools: Vec::new(),
        requires_approval: false,
        approval_lifetime: Default::default(),
    };

    let error = super::delivery::fetch_secret("HOOK", &secret, &Default::default())
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("secrets.HOOK"), "{error:#}");
}
