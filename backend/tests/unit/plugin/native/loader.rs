use super::*;
use serde_json::json;
use std::process::Command;
use std::time::Duration;

fn fixture_library(bad_revision: bool) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let name = format!("native_fixture.{}", std::env::consts::DLL_EXTENSION);
    let library = temp.path().join(name);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_abi.rs");
    let mut compiler = Command::new("rustc");
    compiler
        .args(["--edition=2024", "--crate-type=cdylib", "--cap-lints=allow"])
        .arg(source)
        .arg("-o")
        .arg(&library);
    if bad_revision {
        compiler.args(["--cfg", "bad_revision"]);
    }
    let result = compiler.output().unwrap();
    assert!(
        result.status.success(),
        "Native test fixture compilation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    (temp, library)
}

fn create_test_metadata() -> PluginMetadata {
    PluginMetadata::new(
        "test-plugin".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "libtest.so".to_string(),
    )
}

#[test]
fn test_native_loader_creation() {
    let loader = NativeLoader::new();
    assert_eq!(loader.library_count(), 0);
}

/// Set TING_TEST_XM_LIBRARY to a freshly built official XM DLL / SO. CI's
/// source-plugin gate builds it before running this real ABI integration.
#[tokio::test]
async fn official_xm_native_abi_extracts_and_writes_without_file_paths() {
    let Ok(library) = std::env::var("TING_TEST_XM_LIBRARY") else {
        return;
    };
    let temporary = tempfile::tempdir().unwrap();
    let mut tag = id3::Tag::new();
    use id3::TagLike;
    tag.set_title("Chapter one");
    tag.set_album("Book | extra");
    tag.set_artist("Narrator");
    tag.add_frame(id3::Frame::text("TSIZ", "16"));
    tag.add_frame(id3::Frame::text("TENC", "00000000000000000000000000000000"));
    tag.add_frame(id3::Frame::text("TSSE", "prefix"));
    let mut bytes = Vec::new();
    tag.write_to(&mut bytes, id3::Version::Id3v23).unwrap();
    let header_size = bytes.len();
    bytes.extend([0u8; 16]);
    bytes.extend(b"plain-tail");
    let path = temporary.path().join("sample.xm");
    std::fs::write(&path, bytes).unwrap();

    let mut metadata = PluginMetadata::new(
        "xm-format".into(),
        "xm format".into(),
        "2.0.0".into(),
        "Ting Reader".into(),
        "XM format".into(),
        library.clone(),
    );
    metadata.capabilities = vec![
        serde_json::from_value(json!({
            "id":"format.handler","kind":"format_handler","extensions":["xm"],
            "operations":["probe","get_metadata_read_size","extract_metadata",
                "write_metadata","open_decrypt","read_chunk","seek","close","cancel"]
        }))
        .unwrap(),
    ];
    let id = metadata.instance_id();
    let loader = NativeLoader::new();
    loader
        .load_library(id.clone(), Path::new(&library), metadata)
        .unwrap();
    let scope = Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
        id.clone(),
        uuid::Uuid::new_v4(),
        None,
        temporary.path().join("staging"),
        crate::plugin::host_api::resources::ResourceLimits::default(),
    ));
    let input = scope.grant_file(&path, 8 * 1024 * 1024, None).unwrap();
    let host = || host_api::NativeHostInvocationContext {
        plugin_id: id.clone(),
        permissions: Vec::new(),
        user: None,
        host_gateway: None,
        resources: Some(Arc::clone(&scope)),
        runtime_handle: tokio::runtime::Handle::current(),
    };
    let initialized = loader.call_function(&id, "initialize", json!({})).unwrap();
    assert_eq!(initialized["ok"], true);
    let probed = loader
        .call_function_with_context(
            &id,
            "probe",
            json!({
                "input":input, "extension_hint":"xm", "mime_hint":null,
                "prefix_bytes":header_size,
            }),
            Some(host()),
        )
        .unwrap();
    assert_eq!(probed["data"]["kind"], "match");
    let extracted = loader
        .call_function_with_context(
            &id,
            "extract_metadata",
            json!({
                "input":input, "extract_cover":false,
            }),
            Some(host()),
        )
        .unwrap();
    assert_eq!(extracted["data"]["title"], "Chapter one");
    assert_eq!(extracted["data"]["album"], "Book");
    assert_eq!(extracted["data"]["narrator"], "Narrator");
    let output = scope.create_output(None).unwrap();
    let revision = scope.stat(&input).unwrap().revision.unwrap();
    let written = loader
        .call_function_with_context(
            &id,
            "write_metadata",
            json!({
                "input":input,"output":output,"source_revision":revision,
                "patch":{"title":"Chapter two"}
            }),
            Some(host()),
        )
        .unwrap();
    assert_eq!(written["ok"], true, "{written}");
    scope
        .commit_local_output(&output, &input, &path, &revision)
        .unwrap();
    let tag = id3::Tag::read_from_path(&path).unwrap();
    assert_eq!(tag.title(), Some("Chapter two"));
    assert_eq!(
        tag.get("TSIZ").and_then(|frame| frame.content().text()),
        Some("16")
    );
    assert_eq!(
        tag.get("TSSE").and_then(|frame| frame.content().text()),
        Some("prefix")
    );
    loader.call_function(&id, "shutdown", json!({})).unwrap();
    loader.unload_library(&id).unwrap();
}

#[test]
fn native_preflight_requires_all_declared_symbols_before_registration() {
    let mut metadata = create_test_metadata();
    metadata.capabilities = vec![
        serde_json::from_value(json!({
            "id": "format.xm", "kind": "format_handler",
            "extensions": ["xm"],
            "operations": ["probe", "get_metadata_read_size", "extract_metadata"]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "id": "format.special", "kind": "format_handler",
            "extensions": ["special"],
            "operations": ["probe", "extract_metadata"]
        }))
        .unwrap(),
    ];
    assert_eq!(
        missing_native_exports(&metadata, |name| name == "probe"),
        ["extract_metadata", "get_metadata_read_size"]
    );
    assert!(missing_native_exports(&metadata, |_| true).is_empty());
}

#[test]
fn native_v2_fixture_invokes_and_enforces_host_output_limit() {
    let (_temp, library) = fixture_library(false);
    let mut metadata = create_test_metadata();
    metadata.capabilities = vec![
        serde_json::from_value(json!({
            "id": "format.test", "kind": "format_handler",
            "extensions": ["test"], "operations": ["probe", "extract_metadata"]
        }))
        .unwrap(),
    ];
    let id = metadata.instance_id();
    let loader = NativeLoader::new();
    loader.load_library(id.clone(), &library, metadata).unwrap();
    assert_eq!(
        loader
            .call_function(&id, "probe", json!({"input": "opaque"}))
            .unwrap(),
        json!({"kind":"no_match"})
    );
    assert!(
        loader
            .call_function(&id, "probe", json!({"case": "oversize"}))
            .is_err()
    );
    assert!(loader.call_function(&id, "oversize", json!({})).is_err());
    let stats = loader.get_stats(&id).unwrap();
    assert_eq!(
        (
            stats.total_calls,
            stats.successful_calls,
            stats.failed_calls
        ),
        (2, 1, 1)
    );
    assert_eq!(stats.peak_memory_bytes, br#"{"kind":"no_match"}"#.len());
    loader.unload_library(&id).unwrap();
}

#[test]
fn native_v2_rejects_wrong_revision_before_reading_function_table() {
    let (_temp, library) = fixture_library(true);
    let loader = NativeLoader::new();
    let error = loader
        .load_library("fixture".into(), &library, create_test_metadata())
        .unwrap_err();
    assert!(error.to_string().contains("unsupported ABI revision"));
    assert_eq!(loader.library_count(), 0);
}

#[test]
fn timed_out_native_instance_never_accepts_another_call() {
    let (_temp, library) = fixture_library(false);
    let mut metadata = create_test_metadata();
    metadata.capabilities = vec![
        serde_json::from_value(json!({
            "id": "format.test", "kind": "format_handler",
            "extensions": ["test"], "operations": ["probe", "extract_metadata"]
        }))
        .unwrap(),
    ];
    let id = metadata.instance_id();
    let limits = ResourceLimits::custom(
        1024 * 1024,
        Duration::from_millis(25),
        1024 * 1024,
        1024 * 1024,
    );
    let loader = NativeLoader::with_limits(limits);
    loader.load_library(id.clone(), &library, metadata).unwrap();
    let start = Instant::now();
    assert!(matches!(
        loader.call_function(&id, "probe", json!({"case": "slow"})),
        Err(TingError::Timeout(_))
    ));
    assert!(start.elapsed() < Duration::from_millis(150));
    assert!(loader.call_function(&id, "probe", json!({})).is_err());
    let stats = loader.get_stats(&id).unwrap();
    assert_eq!(stats.timeout_errors, 1);
    // Unloading removes the instance immediately; the running worker
    // retains its own library reference until it completes.
    let unload_start = Instant::now();
    loader.unload_library(&id).unwrap();
    assert!(unload_start.elapsed() < Duration::from_millis(120));
    std::thread::sleep(Duration::from_millis(180));
}

#[test]
fn test_native_loader_with_custom_limits() {
    let limits = ResourceLimits::custom(
        256 * 1024 * 1024, // 256 MB
        Duration::from_secs(60),
        50 * 1024 * 1024, // 50 MB
        5 * 1024 * 1024,  // 5 MB/s
    );
    let loader = NativeLoader::with_limits(limits);
    assert_eq!(loader.library_count(), 0);
}

#[test]
fn test_is_loaded_returns_false_for_nonexistent() {
    let loader = NativeLoader::new();
    assert!(!loader.is_loaded(&"nonexistent".to_string()));
}

#[test]
fn test_list_loaded_libraries_empty() {
    let loader = NativeLoader::new();
    assert_eq!(loader.list_loaded_libraries().len(), 0);
}

#[test]
fn test_load_nonexistent_library_fails() {
    let loader = NativeLoader::new();
    let metadata = create_test_metadata();
    let result = loader.load_library(
        "test".to_string(),
        Path::new("/nonexistent/library.so"),
        metadata,
    );
    assert!(result.is_err());
}

#[test]
fn test_load_library_with_custom_limits() {
    let loader = NativeLoader::new();
    let metadata = create_test_metadata();
    let limits = ResourceLimits::restrictive();
    let result = loader.load_library_with_limits(
        "test".to_string(),
        Path::new("/nonexistent/library.so"),
        metadata,
        limits,
    );
    assert!(result.is_err());
}

#[test]
fn test_unload_nonexistent_library_fails() {
    let loader = NativeLoader::new();
    let result = loader.unload_library(&"nonexistent".to_string());
    assert!(result.is_err());
}

#[test]
fn test_get_metadata_nonexistent_fails() {
    let loader = NativeLoader::new();
    let result = loader.get_metadata(&"nonexistent".to_string());
    assert!(result.is_err());
}

#[test]
fn test_get_stats_nonexistent_fails() {
    let loader = NativeLoader::new();
    let result = loader.get_stats(&"nonexistent".to_string());
    assert!(result.is_err());
}

#[test]
fn test_resource_stats_default() {
    let stats = ResourceStats::default();
    assert_eq!(stats.total_calls, 0);
    assert_eq!(stats.successful_calls, 0);
    assert_eq!(stats.failed_calls, 0);
    assert_eq!(stats.timeout_errors, 0);
    assert_eq!(stats.peak_memory_bytes, 0);
    assert_eq!(stats.total_cpu_time, Duration::from_secs(0));
    assert!(stats.last_execution_time.is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn test_valid_library_extension_linux() {
    let loader = NativeLoader::new();
    assert!(loader.is_valid_library_extension(Path::new("test.so")));
    assert!(!loader.is_valid_library_extension(Path::new("test.dll")));
    assert!(!loader.is_valid_library_extension(Path::new("test.dylib")));
}

#[cfg(target_os = "windows")]
#[test]
fn test_valid_library_extension_windows() {
    let loader = NativeLoader::new();
    assert!(loader.is_valid_library_extension(Path::new("test.dll")));
    assert!(!loader.is_valid_library_extension(Path::new("test.so")));
    assert!(!loader.is_valid_library_extension(Path::new("test.dylib")));
}

#[cfg(target_os = "macos")]
#[test]
fn test_valid_library_extension_macos() {
    let loader = NativeLoader::new();
    assert!(loader.is_valid_library_extension(Path::new("test.dylib")));
    assert!(!loader.is_valid_library_extension(Path::new("test.dll")));
    assert!(!loader.is_valid_library_extension(Path::new("test.so")));
}
