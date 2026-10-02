use super::*;
use crate::plugin::types::{PluginInvocationContext, PluginMetadata};
use serde_json::json;

#[test]
fn blocks_bare_remote_and_symlink_escape_imports() {
    let package = tempfile::tempdir().unwrap();
    std::fs::write(
        package.path().join("plugin.js"),
        "export const ready = true",
    )
    .unwrap();
    std::fs::write(package.path().join("sdk.js"), "export const sdk = true").unwrap();
    std::fs::write(package.path().join("sdk.mjs"), "export const sdk = true").unwrap();
    let loader = PackageModuleLoader::new(package.path()).unwrap();
    let main = ModuleSpecifier::from_file_path(package.path().join("plugin.js")).unwrap();
    let resolved = loader
        .resolve("./sdk.js", main.as_str(), ResolutionKind::Import)
        .unwrap();
    assert_eq!(
        resolved.to_file_path().unwrap().canonicalize().unwrap(),
        package.path().join("sdk.js").canonicalize().unwrap()
    );
    assert!(
        loader
            .resolve("./sdk.mjs", main.as_str(), ResolutionKind::Import)
            .is_ok()
    );
    for request in [
        "axios",
        "https://example.org/sdk.js",
        "file:///etc/passwd.js",
        "../outside.js",
        "./sdk.json",
    ] {
        assert!(
            loader
                .resolve(request, main.as_str(), ResolutionKind::DynamicImport)
                .is_err(),
            "{request}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_mjs_import_executes_declared_operation() {
    let package = tempfile::tempdir().unwrap();
    std::fs::write(
        package.path().join("sdk.mjs"),
        include_str!("../../../../src/plugin/js/embedded_sdk.mjs"),
    )
    .unwrap();
    std::fs::write(
            package.path().join("plugin.js"),
            "import { success } from './sdk.mjs'; export function list_plugins() { return success({ items: [] }); }",
        )
        .unwrap();
    let mut metadata = PluginMetadata::new(
        "sdk-demo".into(),
        "SDK demo".into(),
        "2.0.0".into(),
        "Test".into(),
        "Module import".into(),
        "plugin.js".into(),
    );
    metadata.capabilities.push(
        serde_json::from_value(json!({
            "id": "store.demo",
            "kind": "plugin_store",
            "operations": ["list_plugins"]
        }))
        .unwrap(),
    );
    let mut runtime = super::super::runtime::JsRuntimeWrapper::new(
        package.path().join("plugin.js"),
        metadata,
        None,
    )
    .unwrap();
    runtime.load_module().await.unwrap();
    let output: serde_json::Value = runtime
        .call_function(
            "list_plugins",
            json!({}),
            &PluginInvocationContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(output, json!({"ok": true, "data": {"items": []}}));
}

#[tokio::test(flavor = "current_thread")]
async fn official_javascript_plugins_load_the_sdk() {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugin")
        .canonicalize()
        .unwrap();
    for name in [
        "ai-scraper-js",
        "ai-cover-generator",
        "ai-booklist-assistant",
        "rss-feed",
        "ting-reader-plugin-store",
    ] {
        let path = plugins.join(name);
        let metadata = crate::plugin::types::metadata::read_plugin_metadata(&path).unwrap();
        let mut runtime = super::super::runtime::JsRuntimeWrapper::new(
            path.join(&metadata.entry_point),
            metadata,
            None,
        )
        .unwrap();
        runtime.load_module().await.unwrap();
        if name == "ai-scraper-js" {
            let result: serde_json::Value = runtime
                .call_function(
                    "search",
                    json!({
                        "title": "SDK integration", "author": null, "narrator": null,
                        "page": 1, "page_size": 20, "filters": {},
                        "chapter_candidates": [], "context": null
                    }),
                    &PluginInvocationContext::default(),
                )
                .await
                .unwrap();
            assert_eq!(result["ok"], true);
            assert!(result["data"]["items"].is_array());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_javascript_writes_and_reads_host_asset_without_base64_control_messages() {
    let package = tempfile::tempdir().unwrap();
    std::fs::write(
        package.path().join("sdk.mjs"),
        include_str!("../../../../src/plugin/js/embedded_sdk.mjs"),
    )
    .unwrap();
    std::fs::write(
        package.path().join("plugin.js"),
        r#"import { success, createOutputFromBytes, readResource, encodeBase64 } from './sdk.mjs';
export async function list_plugins() {
  const id = await createOutputFromBytes(new Uint8Array([0, 1, 2, 3, 255]), 'image/png');
  const read = await readResource(id, 5);
  return success({ encoded: encodeBase64(read) });
}"#,
    )
    .unwrap();
    let mut metadata = PluginMetadata::new(
        "sdk-asset".into(),
        "SDK asset".into(),
        "2.0.0".into(),
        "Test".into(),
        "Asset resource".into(),
        "plugin.js".into(),
    );
    metadata.capabilities.push(
        serde_json::from_value(json!({
            "id": "store.asset", "kind": "plugin_store", "operations": ["list_plugins"]
        }))
        .unwrap(),
    );
    let scope = std::sync::Arc::new(crate::plugin::host_api::resources::ResourceScope::new(
        metadata.instance_id(),
        uuid::Uuid::new_v4(),
        None,
        package.path().join("stage"),
        crate::plugin::host_api::resources::ResourceLimits::default(),
    ));
    let mut runtime = super::super::runtime::JsRuntimeWrapper::new(
        package.path().join("plugin.js"),
        metadata,
        None,
    )
    .unwrap();
    runtime.load_module().await.unwrap();
    let result: serde_json::Value = runtime
        .call_function(
            "list_plugins",
            json!({}),
            &PluginInvocationContext {
                user: None,
                resources: Some(scope.clone()),
            },
        )
        .await
        .unwrap();
    assert_eq!(result, json!({"ok": true, "data": {"encoded": "AAECA/8="}}));
    assert_eq!(scope.managed_bytes(), 0);
}
