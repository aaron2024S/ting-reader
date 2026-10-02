use super::{NativeLoader, NativePlugin};
use crate::plugin::native::host_api::table;
use crate::plugin::types::{Plugin, PluginMetadata};
use std::sync::Arc;
use ting_plugin_contract::native_abi::{NativeStatus, NativeTargetInfo};

#[test]
fn test_native_plugin_creation() {
    let metadata = PluginMetadata::new(
        "test-plugin@1.0.0".to_string(),
        "test-plugin".to_string(),
        "1.0.0".to_string(),
        "Test Author".to_string(),
        "Test plugin".to_string(),
        "plugin.dll".to_string(),
    );

    let loader = Arc::new(NativeLoader::new());
    let plugin = NativePlugin::new(
        "test-plugin@1.0.0".to_string(),
        metadata,
        loader,
        std::path::PathBuf::from("/tmp/test-plugin"),
        None,
    );

    assert_eq!(plugin.metadata().name, "test-plugin");
}

#[test]
fn native_host_callbacks_reject_without_borrowed_context() {
    let api = table(None);
    let mut output = [0u8; 128];
    let mut length = 0;
    let method = b"resources.stat";
    let status = unsafe {
        (api.invoke)(
            api.user_data,
            method.as_ptr(),
            method.len(),
            b"{}".as_ptr(),
            2,
            output.as_mut_ptr(),
            output.len(),
            &mut length,
        )
    };
    assert_eq!(status, NativeStatus::NoScope as i32);
    assert_eq!(length, 0);
    assert_eq!(api.header.abi_revision, 2);
    assert_eq!(api.target, NativeTargetInfo::CURRENT);
}
