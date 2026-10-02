use super::*;

#[tokio::test]
async fn download_policy_allows_http_and_private_literal_addresses() {
    for raw_url in [
        "http://127.0.0.1/plugin.tr",
        "http://192.168.1.20/plugin.tr",
        "https://198.18.1.89/plugin.tr",
    ] {
        assert!(
            validate_plugin_download_target(&Url::parse(raw_url).unwrap())
                .await
                .is_ok()
        );
    }
}

#[test]
fn download_url_logs_drop_credentials_and_query_values() {
    assert_eq!(
        redacted_download_url("https://user:secret@example.com/plugin.tr?token=secret#part"),
        "https://example.com/plugin.tr"
    );
    assert_eq!(
        redacted_download_url("https://[2606:4700:4700::1111]:8443/plugin.tr?token=secret"),
        "https://[2606:4700:4700::1111]:8443/plugin.tr"
    );
}
