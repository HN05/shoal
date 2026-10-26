use super::*;
use serde_json::json;

#[test]
fn method_names_are_their_protocol_tags() {
    assert_eq!(Method::Status.name(), "status");
    let watch = Method::WatchNotifications {
        keep_unread: false,
        after: 0,
    };
    assert_eq!(watch.name(), "watch_notifications");
}

#[test]
fn frame_pages_split_long_listings_in_order() {
    assert_eq!(
        frame_pages(Vec::<String>::new()).unwrap(),
        [Vec::<String>::new()]
    );
    let items: Vec<String> = (0..300).map(|n| format!("{n:0>500}")).collect();
    let pages = frame_pages(items.clone()).unwrap();
    assert!(pages.len() > 1);
    for page in &pages {
        assert!(serde_json::to_vec(page).unwrap().len() < MAX_FRAME - 512);
    }
    assert_eq!(pages.concat(), items);
}

#[test]
fn text_pieces_fit_frames_and_keep_characters_whole() {
    assert!(text_pieces("").is_empty());
    // Quotes double when escaped; multibyte characters straddle piece ends.
    let text = format!("{}{}", "\"".repeat(MAX_FRAME), "é".repeat(MAX_FRAME));
    let pieces = text_pieces(&text);
    for page in frame_pages(pieces.clone()).unwrap() {
        assert!(serde_json::to_vec(&page).unwrap().len() < MAX_FRAME - 512);
    }
    assert_eq!(pieces.concat(), text);
}

#[tokio::test]
async fn buffered_execution_reads_preserve_adjacent_frames() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    write(&mut writer, &ExecutionEvent::Finished { exit_code: 7 })
        .await
        .unwrap();
    write(&mut writer, &ExecutionEvent::Finished { exit_code: 8 })
        .await
        .unwrap();
    let mut reader = BufReader::new(reader);
    for expected in [7, 8] {
        let event: ExecutionEvent = read_buffered(&mut reader).await.unwrap();
        assert!(matches!(event, ExecutionEvent::Finished { exit_code } if exit_code == expected));
    }
}

#[tokio::test]
async fn a_frame_cut_off_by_a_closed_connection_reads_as_end_of_stream() {
    for sent in [&b""[..], b"{\"type\":\"fin"] {
        let (mut writer, reader) = tokio::io::duplex(1024);
        writer.write_all(sent).await.unwrap();
        drop(writer);
        let error = read::<ExecutionEvent>(&mut BufReader::new(reader))
            .await
            .unwrap_err();
        let error = error.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}

#[test]
fn daemon_error_codes_preserve_wire_spellings() {
    for (code, spelling) in [
        (ErrorCode::InvalidRequest, "invalid_request"),
        (ErrorCode::ProtocolMismatch, "protocol_mismatch"),
        (ErrorCode::ScopeDenied, "scope_denied"),
        (ErrorCode::ManagedService, "managed_service"),
        (ErrorCode::OperationFailed, "operation_failed"),
        (ErrorCode::ExecutionFailed, "execution_failed"),
        (ErrorCode::UnprovenStop, "unproven_stop"),
        (ErrorCode::Unknown("future_code".into()), "future_code"),
    ] {
        let wire = json!({"protocol": VERSION, "id": 7, "type": "error",
                "data": {"code": spelling, "message": "failed"}});
        let response = Response::new(7, Body::error(code.clone(), "failed"));
        assert_eq!(serde_json::to_value(response).unwrap(), wire);
        let response: Response = serde_json::from_value(wire).unwrap();
        let error = response.body.into_result().unwrap_err();
        let remote = error.downcast_ref::<RemoteError>().unwrap();
        assert_eq!(remote.code, code);
        assert_eq!(remote.message, "failed");
        let display = match code {
            ErrorCode::OperationFailed => "failed".to_owned(),
            _ => format!("{spelling}: failed"),
        };
        assert_eq!(error.to_string(), display);
    }
    for code in [json!(null), json!(42), json!({"unknown": "future_code"})] {
        assert!(serde_json::from_value::<ErrorCode>(code).is_err());
    }
}

#[test]
fn workspace_hook_rejects_unknown_kinds() {
    assert!(
        serde_json::from_value::<Method>(serde_json::json!({
            "workspace_hook": {"workspace": "worker", "kind": "unknown"}
        }))
        .is_err()
    );
}

#[test]
fn pr_actions_preserve_existing_wire_fields() {
    use crate::daemon::links::pr::Action;
    for (action, url, clear) in [
        (
            Action::Watch {
                url: "https://forge.example/team/repo/pulls/7".into(),
            },
            Some("https://forge.example/team/repo/pulls/7"),
            false,
        ),
        (Action::Acknowledge, None, false),
        (Action::Clear, None, true),
    ] {
        let wire = json!({"set_pr": {"workspace": "worker", "url": url, "clear": clear}});
        let decoded: Method = serde_json::from_value(wire.clone()).unwrap();
        let Method::SetPr {
            workspace,
            action: decoded_action,
        } = &decoded
        else {
            panic!("wrong method")
        };
        assert_eq!(workspace, "worker");
        assert_eq!(decoded_action, &action);
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    }
    let decoded: Method =
        serde_json::from_value(json!({"set_pr": {"workspace": "worker", "clear": false}})).unwrap();
    assert!(matches!(
        decoded,
        Method::SetPr {
            action: Action::Acknowledge,
            ..
        }
    ));
}

#[test]
fn selective_unwatch_round_trips_and_rejects_conflicting_actions() {
    let wire =
        json!({"set_pr": {"workspace": "worker", "url": null, "clear": false, "unwatch": "7"}});
    let decoded: Method = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
    for action in [
        json!({"clear": true, "unwatch": "7"}),
        json!({"clear": false, "url": "8", "unwatch": "7"}),
    ] {
        assert!(serde_json::from_value::<crate::daemon::links::pr::Action>(action).is_err());
    }
}

#[test]
fn pr_actions_reject_conflicting_wire_fields() {
    let error = serde_json::from_value::<Method>(
        json!({"set_pr": {"workspace": "worker", "url": "7", "clear": true}}),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("clear cannot include a URL"),
        "{error}"
    );
    assert!(
        serde_json::from_value::<Method>(json!({"set_pr": {"workspace": "worker", "url": "7"}}))
            .is_err()
    );
}

#[test]
fn reattachment_keeps_its_wire_spelling() {
    let wire = json!({"reattach": {
        "execution": "execution", "wrapper": {"pid": 2, "birth": "wrapper"},
        "child": null, "group_id": 2, "scope_token": "token"
    }});
    let Method::Reattach(request) = serde_json::from_value(wire).unwrap() else {
        panic!("reattachment must keep its method name");
    };
    assert!(request.agent.is_none() && !request.recover);
    assert_eq!(
        serde_json::to_value(Control::Detach).unwrap(),
        json!({"type": "detach"})
    );
}

#[test]
fn pause_reasons_are_optional_on_the_wire() {
    let control: Control = serde_json::from_value(json!({"type": "pause"})).unwrap();
    assert!(matches!(
        control,
        Control::Pause {
            reason: None,
            removed: false
        }
    ));
    let control: Control = serde_json::from_value(json!({"type": "stop"})).unwrap();
    assert!(matches!(control, Control::Stop { reason: None }));
    let wire = serde_json::to_value(Control::Pause {
        reason: Some("shoal stop".into()),
        removed: false,
    })
    .unwrap();
    assert_eq!(wire, json!({"type": "pause", "reason": "shoal stop"}));
    // Wrappers from before the reason read it as their unit variant.
    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Legacy {
        Pause,
    }
    assert!(matches!(
        serde_json::from_value::<Legacy>(wire).unwrap(),
        Legacy::Pause
    ));
}

#[test]
fn older_overload_controls_use_a_generic_reason() {
    let control: Control =
        serde_json::from_value(json!({"type": "overload_stop", "recover": false})).unwrap();
    assert!(matches!(
        control,
        Control::OverloadStop { recover: false, reason, resumes_when }
            if reason == "system overload" && resumes_when == "system load is healthy"
    ));
}

#[test]
fn tracked_execution_requests_require_a_known_kind() {
    let wrapper = crate::process::identity::capture(std::process::id())
        .unwrap()
        .unwrap();
    for (kind, spelling) in [
        (ExecutionKind::Command, "command"),
        (ExecutionKind::Setup, "setup"),
        (ExecutionKind::Land, "land"),
    ] {
        let method = Method::Execute {
            workspace: "worker".into(),
            wrapper: wrapper.clone(),
            kind,
            agent: Some("test-agent".into()),
            recover: false,
            reattach: true,
            queue_entry: None,
            demo: None,
        };
        let encoded = serde_json::to_value(method).unwrap();
        assert_eq!(encoded["execute"]["kind"], spelling);
        let Method::Execute {
            kind: decoded,
            agent,
            ..
        } = serde_json::from_value(encoded).unwrap()
        else {
            panic!("execution must use the shared request");
        };
        assert_eq!(decoded, kind);
        assert_eq!(agent.as_deref(), Some("test-agent"));
    }
    for kind in [None, Some("unknown"), Some("Setup")] {
        let mut request = json!({"execute": {"workspace": "worker", "wrapper": wrapper}});
        if let Some(kind) = kind {
            request["execute"]["kind"] = kind.into();
        }
        assert!(serde_json::from_value::<Method>(request).is_err());
    }
    for method in ["prepare", "land_workspace"] {
        assert!(
            serde_json::from_value::<Method>(json!({
                method: {"workspace": "worker", "wrapper": wrapper}
            }))
            .is_err()
        );
    }
}

#[test]
fn extraction_preserves_wire_shapes() {
    for (wire, expected) in [
        (json!({"type": "ok"}), "Ok"),
        (json!({"type": "workspaces", "data": []}), "Workspaces"),
        (
            json!({"type": "layered_config", "data": {"worktree_file": {}, "saved_repository_config": {}}}),
            "LayeredConfig",
        ),
        (json!({"type": "busy", "data": {"message": "busy"}}), "Busy"),
        (
            json!({"type": "error", "data": {"code": "future_code", "message": "failed"}}),
            "Error",
        ),
    ] {
        let body: Body = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(body.variant_name(), expected);
        // Config layers serialize defaults; other bodies round-trip exactly.
        if expected != "LayeredConfig" {
            assert_eq!(serde_json::to_value(&body).unwrap(), wire);
        }
        match expected {
            "Ok" => <()>::try_from(body).unwrap(),
            "Workspaces" => assert!(Vec::<Workspace>::try_from(body).unwrap().is_empty()),
            "LayeredConfig" => {
                Box::<ConfigLayers>::try_from(body).unwrap();
            }
            "Error" => {
                let error = DaemonStatus::try_from(body).unwrap_err();
                let remote = error.downcast_ref::<RemoteError>().unwrap();
                assert_eq!(remote.code, ErrorCode::Unknown("future_code".into()));
                assert_eq!(remote.message, "failed");
            }
            _ => assert_eq!(
                DaemonStatus::try_from(body).unwrap_err().to_string(),
                format!("unexpected daemon response; expected Status, received {expected}")
            ),
        }
    }
}

#[test]
fn stream_payloads_and_acknowledgements_preserve_remote_errors() {
    for error in [
        ExecutionPlan::try_from(Body::error(ErrorCode::ScopeDenied, "denied")).unwrap_err(),
        Notification::try_from(Body::error(ErrorCode::ScopeDenied, "denied")).unwrap_err(),
        <()>::try_from(Body::error(ErrorCode::ScopeDenied, "denied")).unwrap_err(),
    ] {
        let remote = error.downcast_ref::<RemoteError>().unwrap();
        assert_eq!(remote.code, ErrorCode::ScopeDenied);
        assert_eq!(remote.message, "denied");
    }
    let error = <()>::try_from(Body::Workspaces(vec![])).unwrap_err();
    assert_eq!(
        error.to_string(),
        "unexpected daemon response; expected Ok, received Workspaces"
    );
}
