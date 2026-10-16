use super::*;

#[test]
fn websocket_failures_share_the_rest_classification_and_keep_the_message() {
    let failure = TingError::deserialization(
        CodecErrorKind::InvalidData,
        "websocket.receive",
        "Invalid message".into(),
    );
    let payload = serde_json::to_value(ServerMessage::Error {
        error: ErrorResponse::from_error_with_trace_id(&failure, "ws-trace".into()),
    })
    .unwrap();
    assert_eq!(payload["type"], "error");
    assert_eq!(payload["error"], "DeserializationError");
    assert_eq!(payload["message"], failure.to_string());
    assert_eq!(payload["details"]["reason"], "invalid_data");
    assert_eq!(payload["details"]["operation"], "websocket.receive");
    assert_eq!(payload["details"]["retryable"], false);
    assert_eq!(payload["trace_id"], "ws-trace");
}
