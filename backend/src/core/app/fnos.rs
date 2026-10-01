//! fnOS-specific integration used by FPK installations.

#[cfg(any(unix, test))]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(any(unix, test))]
use serde::Deserialize;

const FPK_INITIAL_ADMIN_LANGUAGE_ENV: &str = "FNNAS_FPK_INITIAL_ADMIN_LANGUAGE";
#[cfg(unix)]
const OPEN_GATEWAY_SOCKET: &str = "/var/run/trim_open_gateway_apiscope.socket";

/// Returns a language for the initial default admin only when the FPK launcher
/// explicitly enables fnOS integration.
pub async fn initial_admin_language() -> Option<&'static str> {
    if !initial_admin_language_enabled(
        std::env::var(FPK_INITIAL_ADMIN_LANGUAGE_ENV)
            .ok()
            .as_deref(),
    ) {
        return None;
    }

    let system_language = match platform_system_language().await {
        Ok(language) => Some(language),
        Err(error) => {
            tracing::warn!(
                error = %error,
                message_key = "system.default_admin.fnos_language_unavailable",
                "Could not read fnOS system language while creating the default admin; using the available system-language fallback"
            );
            std::env::var("TRIM_SYS_LANGUAGE").ok()
        }
    };
    let language = language_for_system_language(system_language.as_deref());

    tracing::info!(
        system_language = ?system_language,
        language,
        message_key = "system.default_admin.fnos_language_selected",
        "Selected language for the initial default admin"
    );

    Some(language)
}

fn initial_admin_language_enabled(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some(value) if value.eq_ignore_ascii_case("true") || value == "1"
    )
}

fn language_for_system_language(system_language: Option<&str>) -> &'static str {
    let is_chinese = system_language
        .map(str::trim)
        .filter(|language| !language.is_empty())
        .is_some_and(|language| language.to_ascii_lowercase().starts_with("zh"));

    if is_chinese { "zh-CN" } else { "en-US" }
}

#[cfg(unix)]
async fn platform_system_language() -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::time::{Duration, timeout};

    let token = std::env::var("TRIM_API_TOKEN")
        .context("TRIM_API_TOKEN is not available")?
        .trim()
        .to_string();
    if token.is_empty() {
        bail!("TRIM_API_TOKEN is empty");
    }
    if token.contains('\r') || token.contains('\n') {
        bail!("TRIM_API_TOKEN contains an invalid header character");
    }

    let app_name = std::env::var("TRIM_APPNAME").unwrap_or_else(|_| "ting-reader".to_string());
    let body = serde_json::json!({
        "reqId": "ting-reader-initial-admin-language",
        "req": "trim.system.getPlatformConfig",
        "appName": app_name,
        "data": {},
    })
    .to_string();
    let request = format!(
        "POST /api/v1/trimapp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let response = timeout(Duration::from_secs(3), async {
        let mut stream = UnixStream::connect(OPEN_GATEWAY_SOCKET).await?;
        stream.write_all(request.as_bytes()).await?;
        stream.shutdown().await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, std::io::Error>(response)
    })
    .await
    .context("fnOS platform-config request timed out")??;

    let body = http_response_body(&response)?;
    parse_platform_language(&body)
}

#[cfg(not(unix))]
async fn platform_system_language() -> Result<String> {
    bail!("fnOS platform configuration is only available on Unix")
}

#[cfg(any(unix, test))]
fn http_response_body(response: &[u8]) -> Result<Vec<u8>> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("fnOS platform-config response is missing HTTP headers")?;
    let headers = std::str::from_utf8(&response[..header_end])
        .context("fnOS platform-config response headers are not valid UTF-8")?;
    let status = headers
        .lines()
        .next()
        .context("fnOS platform-config response is missing an HTTP status")?;
    let status_code = status
        .split_whitespace()
        .nth(1)
        .context("fnOS platform-config response has an invalid HTTP status")?;
    if status_code != "200" {
        bail!("fnOS platform-config request returned HTTP status {status_code}");
    }

    let body = &response[header_end + 4..];
    if headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case("Transfer-Encoding: chunked"))
    {
        decode_chunked_body(body)
    } else {
        Ok(body.to_vec())
    }
}

#[cfg(any(unix, test))]
fn decode_chunked_body(mut input: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();

    loop {
        let line_end = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("fnOS platform-config response has an invalid chunk header")?;
        let chunk_size = std::str::from_utf8(&input[..line_end])
            .context("fnOS platform-config response has a non-UTF-8 chunk header")?
            .split(';')
            .next()
            .context("fnOS platform-config response has an empty chunk header")?;
        let chunk_size = usize::from_str_radix(chunk_size.trim(), 16)
            .context("fnOS platform-config response has an invalid chunk size")?;
        input = &input[line_end + 2..];

        if chunk_size == 0 {
            return Ok(output);
        }
        if input.len() < chunk_size + 2 || &input[chunk_size..chunk_size + 2] != b"\r\n" {
            bail!("fnOS platform-config response ended in the middle of a chunk");
        }

        output.extend_from_slice(&input[..chunk_size]);
        input = &input[chunk_size + 2..];
    }
}

#[cfg(any(unix, test))]
#[derive(Deserialize)]
struct PlatformConfigResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    data: Option<PlatformConfig>,
}

#[cfg(any(unix, test))]
#[derive(Deserialize)]
struct PlatformConfig {
    #[serde(rename = "systemLanguage")]
    system_language: Option<String>,
}

#[cfg(any(unix, test))]
fn parse_platform_language(body: &[u8]) -> Result<String> {
    let response: PlatformConfigResponse =
        serde_json::from_slice(body).context("fnOS platform-config response is not valid JSON")?;
    if response.code != 0 {
        bail!(
            "fnOS platform-config request failed with code {}: {}",
            response.code,
            response.msg
        );
    }

    response
        .data
        .and_then(|data| data.system_language)
        .filter(|language| !language.trim().is_empty())
        .context("fnOS platform-config response does not include systemLanguage")
}

#[cfg(test)]
mod tests {
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
}
