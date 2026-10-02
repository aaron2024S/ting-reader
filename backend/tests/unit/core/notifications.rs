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
    assert_eq!(
        detect_business_error(r#"{"errcode":0,"errmsg":"ok"}"#),
        None
    );
    assert!(
        detect_business_error(r#"{"errcode":40058,"errmsg":"invalid parameter"}"#)
            .unwrap()
            .contains("40058")
    );
}
