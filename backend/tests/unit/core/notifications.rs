use super::*;

#[test]
fn renders_raw_and_json_variables() {
    let payload = NotificationEventPayload::new(
        "book.created",
        "A \"quoted\" title",
        "line one\nline two",
        serde_json::json!({"book": {"title": "Example"}}),
    );
    let template =
        r#"{"title":{{json:title}},"message":{{json:message}},"book":{{json:data.book.title}}}"#;

    let rendered = render_template(template, &payload).unwrap();
    let json: Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(json["title"], "A \"quoted\" title");
    assert_eq!(json["message"], "line one\nline two");
    assert_eq!(json["book"], "Example");
}

#[test]
fn renders_complete_payload_as_json() {
    let payload = NotificationEventPayload::test_payload();
    let rendered = render_template(DEFAULT_BODY_TEMPLATE, &payload).unwrap();
    let json: Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(json["event"], "webhook.test");
    assert_eq!(json["data"]["username"], "admin");
}

#[test]
fn rejects_unknown_template_variables() {
    let error = render_template(
        "{{json:missing}}",
        &NotificationEventPayload::test_payload(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("Unknown webhook template variable")
    );
}

#[test]
fn detects_wecom_business_errors() {
    assert!(detect_business_error(r#"{"errcode":0,"errmsg":"ok"}"#).is_none());
    let error = detect_business_error(r#"{"errcode":40058,"errmsg":"invalid parameter"}"#).unwrap();
    assert_eq!(error.code, 40058);
    assert_eq!(error.message, "invalid parameter");
}

#[test]
fn webhook_test_failures_keep_upstream_codes_without_parsing_messages() {
    let error = webhook_failure(
        reqwest::StatusCode::OK,
        r#"{"errcode":40058,"errmsg":"invalid parameter"}"#,
    )
    .unwrap();
    assert_eq!(error.error, "ExternalServiceError");
    let details = error.details.unwrap();
    assert_eq!(details["reason"], "response");
    assert_eq!(details["upstream_code"], 40058);
    assert_eq!(details["upstream_status"], 200);
    let error = webhook_failure(reqwest::StatusCode::BAD_GATEWAY, "upstream offline").unwrap();
    assert_eq!(error.error, "NetworkError");
    assert_eq!(error.details.unwrap()["upstream_status"], 502);
    assert!(webhook_failure(reqwest::StatusCode::OK, "{}").is_none());
}
