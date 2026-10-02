use super::{
    http_response_body, initial_admin_language_enabled, language_for_system_language,
    parse_platform_language,
};

#[test]
fn enables_initial_language_only_for_the_explicit_fpk_flag() {
    assert!(initial_admin_language_enabled(Some("1")));
    assert!(initial_admin_language_enabled(Some(" true ")));
    assert!(!initial_admin_language_enabled(None));
    assert!(!initial_admin_language_enabled(Some("false")));
}

#[test]
fn chooses_chinese_only_for_chinese_system_languages() {
    assert_eq!(language_for_system_language(Some("zh-CN")), "zh-CN");
    assert_eq!(language_for_system_language(Some("ZH_hans")), "zh-CN");
    assert_eq!(language_for_system_language(Some("en-US")), "en-US");
    assert_eq!(language_for_system_language(None), "en-US");
}

#[test]
fn parses_platform_language_from_open_api_response() {
    let language = parse_platform_language(
        br#"{"code":0,"data":{"systemLanguage":"zh-CN","systemVersion":"1.2.0401"}}"#,
    )
    .unwrap();

    assert_eq!(language, "zh-CN");
}

#[test]
fn extracts_a_chunked_open_api_response() {
    let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";

    assert_eq!(
        http_response_body(response).unwrap(),
        b"hello world".to_vec()
    );
}
