use super::*;
use crate::{
    errors::ErrorCode,
    methods::{GetWorkspace, Topic},
    records::RecordKind,
};

#[test]
fn a_request_names_its_method_and_params() {
    let request = Request {
        id: 3,
        call: Call::GetWorkspace(GetWorkspace {
            workspace: "api".into(),
        }),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"id": 3, "method": "workspace.get", "params": {"workspace": "api"}})
    );
    let read: Request = serde_json::from_value(value).unwrap();
    assert_eq!(read.call.name(), "workspace.get");
}

#[test]
fn unknown_values_and_fields_decode() {
    let response: Response = serde_json::from_value(serde_json::json!({
        "id": 1, "error": {"code": "too_tired", "message": "later", "extra": true}
    }))
    .unwrap();
    let Reply::Error(error) = response.reply else {
        panic!("expected an error: {response:?}");
    };
    assert_eq!(error.code, ErrorCode::Unknown("too_tired".into()));
    let event: Event = serde_json::from_value(serde_json::json!({"type": "rewound"})).unwrap();
    assert_eq!(event, Event::Unknown);
    let record: Record =
        serde_json::from_value(serde_json::json!({"type": "tab", "id": "x"})).unwrap();
    assert_eq!(record.key(), None);
    let key: RecordKey =
        serde_json::from_value(serde_json::json!({"type": "tab", "id": "x"})).unwrap();
    assert_eq!(key.kind, RecordKind::Unknown("tab".into()));
    assert_eq!(Topic::from("windows"), Topic::Unknown("windows".into()));
}

#[test]
fn pages_stay_within_their_size_and_keep_order() {
    let items: Vec<String> = (0..100).map(|n| format!("{n:0>2000}")).collect();
    let paged = pages(items.clone()).unwrap();
    assert!(paged.len() > 1);
    for page in &paged {
        assert!(serde_json::to_vec(page).unwrap().len() <= PAGE_BYTES + 2);
    }
    assert_eq!(paged.concat(), items);
    assert_eq!(pages(Vec::<String>::new()).unwrap(), [Vec::<String>::new()]);
}

#[tokio::test]
async fn a_frame_round_trips_and_a_hangup_reads_as_none() {
    let (client, server) = tokio::io::duplex(MAX_FRAME * 2);
    let (mut reader, mut writer) = (FrameReader::new(server), client);
    let opening = Opening {
        client_api: Hello {
            version: "1.0.0".into(),
            scope: None,
            client: Some("test".into()),
        },
    };
    write(&mut writer, &opening).await.unwrap();
    drop(writer);
    let opened: Opening = reader.read().await.unwrap().unwrap();
    assert_eq!(opened.client_api.client.as_deref(), Some("test"));
    assert!(reader.read::<Opening>().await.unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_read_cancelled_partway_through_a_frame_keeps_its_start() {
    use tokio::io::AsyncWriteExt as _;
    let (mut writer, server) = tokio::io::duplex(1024);
    let mut reader = FrameReader::new(server);
    writer.write_all(br#"{"version": "1."#).await.unwrap();
    let cancelled =
        tokio::time::timeout(std::time::Duration::from_secs(1), reader.read::<Hello>()).await;
    assert!(cancelled.is_err(), "the frame is unfinished: {cancelled:?}");
    writer.write_all(b"0.0\"}\n").await.unwrap();
    let hello: Hello = reader.read().await.unwrap().unwrap();
    assert_eq!(hello.version, "1.0.0");
}

#[test]
fn a_major_version_is_its_first_number() {
    assert_eq!(major("1.4.2"), Some(1));
    assert_eq!(major("x"), None);
}

#[test]
fn operation_progress_and_results_decode_including_unknown_ones() {
    use crate::operations::{OperationResult, OutputStream, Progress, Step};

    let response: Response = serde_json::from_value(serde_json::json!({
        "id": 2, "progress": {"index": 4, "type": "output", "stream": "stderr", "line": "ok"}
    }))
    .unwrap();
    let Reply::Progress(event) = response.reply else {
        panic!("expected progress: {response:?}");
    };
    assert_eq!(event.index, 4);
    assert_eq!(
        event.progress,
        Progress::Output {
            stream: OutputStream::Stderr,
            line: "ok".into()
        }
    );
    let step: Progress =
        serde_json::from_value(serde_json::json!({"type": "step", "step": "warming_up"})).unwrap();
    assert_eq!(
        step,
        Progress::Step {
            step: Step::Unknown("warming_up".into())
        }
    );
    let unknown: Progress =
        serde_json::from_value(serde_json::json!({"type": "percent", "value": 40})).unwrap();
    assert_eq!(unknown, Progress::Unknown);
    let result: OperationResult =
        serde_json::from_value(serde_json::json!({"type": "deployed"})).unwrap();
    assert_eq!(result, OperationResult::Unknown);
}
