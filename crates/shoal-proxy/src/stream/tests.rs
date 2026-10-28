use super::*;

#[test]
fn requests_name_their_protocol_by_path() {
    assert_eq!(
        Protocol::of("/v1/messages"),
        Some(Protocol::AnthropicMessages)
    );
    assert_eq!(
        Protocol::of("/backend-api/codex/responses"),
        Some(Protocol::OpenAiResponses)
    );
    assert_eq!(Protocol::of("/v1/messages/count_tokens"), None);
    assert_eq!(Protocol::of("/v1/models"), None);
}

#[test]
fn events_open_carry_or_fail_a_stream() {
    let anthropic = Protocol::AnthropicMessages;
    let openai = Protocol::OpenAiResponses;
    let error = |kind: &str| json!({"type": "error", "error": {"type": kind}});
    let failed = |code: &str| json!({"response": {"error": {"code": code}}});
    for (protocol, name, data, kind) in [
        (anthropic, "message_start", json!({}), Kind::Preamble),
        (anthropic, "ping", json!({}), Kind::Preamble),
        (anthropic, "content_block_start", json!({}), Kind::Content),
        (
            anthropic,
            "error",
            error("overloaded_error"),
            Kind::Failure(Failure::Overloaded),
        ),
        (
            anthropic,
            "error",
            error("rate_limit_error"),
            Kind::Failure(Failure::RateLimited),
        ),
        (
            anthropic,
            "error",
            error("api_error"),
            Kind::Failure(Failure::ServerError),
        ),
        (
            anthropic,
            "",
            error("invalid_request_error"),
            Kind::Failure(Failure::Other),
        ),
        (openai, "response.created", json!({}), Kind::Preamble),
        (
            openai,
            "response.output_item.added",
            json!({}),
            Kind::Content,
        ),
        (
            openai,
            "response.failed",
            failed("server_is_overloaded"),
            Kind::Failure(Failure::Overloaded),
        ),
        (
            openai,
            "response.failed",
            failed("rate_limit_exceeded"),
            Kind::Failure(Failure::RateLimited),
        ),
        (
            openai,
            "error",
            json!({"code": "server_error"}),
            Kind::Failure(Failure::ServerError),
        ),
        (
            openai,
            "response.failed",
            failed("insufficient_quota"),
            Kind::Failure(Failure::Exhausted),
        ),
    ] {
        assert_eq!(
            protocol.classify(name, &data),
            kind,
            "{protocol:?} {name} {data}"
        );
    }
}

#[test]
fn events_end_at_a_blank_line() {
    let stream = b"event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\nevent: error\ndata: {}";
    let (end, name, data) = next_event(stream).unwrap();
    assert_eq!((name.as_str(), data), ("ping", json!({"type": "ping"})));
    assert_eq!(next_event(&stream[end..]), None);
}

#[test]
fn openai_streams_report_the_failure_they_end_with() {
    let stream = Protocol::OpenAiResponses
        .failure_stream(Failure::Overloaded, None)
        .unwrap();
    let (_, name, data) = next_event(&stream).unwrap();
    assert_eq!(name, "response.failed");
    assert_eq!(
        Protocol::OpenAiResponses.classify(&name, &data),
        Kind::Failure(Failure::Overloaded)
    );
    assert_eq!(
        Protocol::AnthropicMessages.failure_stream(Failure::Overloaded, None),
        None
    );
    assert_eq!(
        Protocol::OpenAiResponses.failure_stream(Failure::Other, None),
        None
    );
}

#[test]
fn usage_limits_are_rate_limits_whose_own_error_reaches_codex() {
    let failed = json!({"response": {"error": {"code": "usage_limit_reached"}}});
    assert_eq!(
        Protocol::OpenAiResponses.classify("response.failed", &failed),
        Kind::Failure(Failure::RateLimited)
    );
    // The upstream's error, named by its type, keeps its message and reset.
    let error = json!({
        "type": "usage_limit_reached",
        "message": "You've hit your usage limit.",
        "resets_in_seconds": 7200,
    });
    let stream = Protocol::OpenAiResponses
        .failure_stream(Failure::RateLimited, Some(&error))
        .unwrap();
    let (_, _, data) = next_event(&stream).unwrap();
    let reported = &data["response"]["error"];
    assert_eq!(reported["code"], "usage_limit_reached");
    assert_eq!(reported["message"], "You've hit your usage limit.");
    assert_eq!(reported["resets_in_seconds"], 7200);
}
