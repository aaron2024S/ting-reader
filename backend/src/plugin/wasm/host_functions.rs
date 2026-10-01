//! WASM v2 Host control adapter. Binary resources use resource_bindings.

use super::plugin::PluginState;
use futures::FutureExt;
use ting_plugin_contract::native_abi::NativeStatus;
use wasmtime::*;

const ERR_INVALID_MEMORY: i32 = -1;
const ERR_INVALID_UTF8: i32 = -1;
const ERR_INVALID_JSON: i32 = -1;
const ERR_MISSING_GATEWAY: i32 = -7;
const ERR_HOST_PANIC: i32 = -7;
const ERR_SERIALIZE_RESPONSE: i32 = -7;

pub fn add_host_functions(linker: &mut Linker<PluginState>) -> anyhow::Result<()> {
    super::resource_bindings::add(linker)?;
    // ting_host_invoke(method_ptr, method_len, params_ptr, params_len) -> handle (>0) or error (<0)
    linker
        .func_wrap_async(
            "ting_env",
            "host_invoke",
            |mut caller: Caller<'_, PluginState>,
             (method_ptr, method_len, params_ptr, params_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    std::panic::AssertUnwindSafe(invoke_host(
                        &mut caller,
                        method_ptr,
                        method_len,
                        params_ptr,
                        params_len,
                    ))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        tracing::error!(operation = "host_invoke", "WASM Host callback panicked");
                        ERR_HOST_PANIC
                    })
                })
            },
        )
        .map_err(|e| anyhow::anyhow!("Failed to define host_invoke: {}", e))?;

    // ting_host_response_size(handle) -> size
    linker
        .func_wrap(
            "ting_env",
            "host_response_size",
            |caller: Caller<'_, PluginState>, handle: i32| -> i32 {
                guarded_host_callback("host_response_size", || {
                    caller
                        .data()
                        .host_responses
                        .get(&(handle as u32))
                        .map_or(-1, |body| body.len() as i32)
                })
            },
        )
        .map_err(|e| anyhow::anyhow!("Failed to define host_response_size: {}", e))?;

    // ting_host_read_body(handle, ptr, len) -> bytes_read
    linker
        .func_wrap(
            "ting_env",
            "host_read_body",
            |mut caller: Caller<'_, PluginState>, handle: i32, ptr: i32, len: i32| -> i32 {
                guarded_host_callback("host_read_body", || {
                    if ptr < 0 || len < 0 {
                        return -1;
                    }
                    let body =
                        if let Some(body) = caller.data().host_responses.get(&(handle as u32)) {
                            body.clone()
                        } else {
                            return -1;
                        };
                    let copy_len = std::cmp::min(body.len(), len as usize);
                    let mem = match caller.get_export("memory") {
                        Some(Extern::Memory(mem)) => mem,
                        _ => return -2,
                    };
                    if mem
                        .write(&mut caller, ptr as usize, &body[..copy_len])
                        .is_err()
                    {
                        return -3;
                    }
                    caller.data_mut().host_responses.remove(&(handle as u32));
                    copy_len as i32
                })
            },
        )
        .map_err(|e| anyhow::anyhow!("Failed to define host_read_body: {}", e))?;

    Ok(())
}

fn guarded_host_callback(operation: &'static str, callback: impl FnOnce() -> i32) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)) {
        Ok(value) => value,
        Err(_) => {
            tracing::error!(operation, "WASM Host callback panicked");
            ERR_HOST_PANIC
        }
    }
}

async fn invoke_host(
    caller: &mut Caller<'_, PluginState>,
    method_ptr: i32,
    method_len: i32,
    params_ptr: i32,
    params_len: i32,
) -> i32 {
    let method = match read_wasm_string(caller, method_ptr, method_len) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let params = match read_wasm_json(caller, params_ptr, params_len) {
        Ok(value) => value,
        Err(code) => return code,
    };

    if method.starts_with("resources.") {
        let Some(scope) = caller.data().resources.clone() else {
            return NativeStatus::NoScope as i32;
        };
        let response = match scope.invoke(&method, params) {
            Ok(response) => response,
            Err(error) => return resource_error_status(error.code),
        };
        let body = match serde_json::to_vec(&response) {
            Ok(body) => body,
            Err(_) => return ERR_SERIALIZE_RESPONSE,
        };
        return store_host_response(caller, body);
    }

    let plugin_id = caller.data().plugin_id.clone();
    let permissions = caller.data().permissions.clone();
    let gateway = match caller
        .data()
        .host_gateway
        .as_ref()
        .and_then(|handle| handle.get())
    {
        Some(gateway) => gateway,
        None => return ERR_MISSING_GATEWAY,
    };
    let user = caller.data().current_user.clone();
    let resources = caller.data().resources.clone();
    let response = match invoke_host_request(
        plugin_id,
        permissions,
        user,
        resources,
        gateway,
        method,
        params,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => return host_error_status(error),
    };
    let body = match serde_json::to_vec(&response) {
        Ok(body) => body,
        Err(_) => return ERR_SERIALIZE_RESPONSE,
    };
    store_host_response(caller, body)
}

async fn invoke_host_request(
    plugin_id: String,
    permissions: Vec<ting_plugin_contract::manifest::Permission>,
    user: Option<crate::plugin::PluginHostUser>,
    resources: Option<std::sync::Arc<crate::plugin::resources::ResourceScope>>,
    gateway: std::sync::Arc<crate::plugin::PluginHostGateway>,
    method: String,
    params: serde_json::Value,
) -> crate::core::error::Result<serde_json::Value> {
    crate::plugin::host_api::invoke(
        &plugin_id,
        Some(&permissions),
        user.as_ref(),
        resources.as_ref(),
        Some(&gateway),
        &method,
        params,
    )
    .await
}

fn resource_error_status(code: ting_plugin_contract::protocol::PluginErrorCode) -> i32 {
    match code {
        ting_plugin_contract::protocol::PluginErrorCode::Cancelled => -6,
        ting_plugin_contract::protocol::PluginErrorCode::ResourceLimit => -5,
        ting_plugin_contract::protocol::PluginErrorCode::PermissionDenied => -4,
        _ => -1,
    }
}

fn host_error_status(error: crate::core::error::TingError) -> i32 {
    match error {
        crate::core::error::TingError::PermissionDenied(_)
        | crate::core::error::TingError::SecurityViolation(_) => {
            NativeStatus::PermissionDenied as i32
        }
        crate::core::error::TingError::ResourceLimitExceeded(_) => {
            NativeStatus::ResourceLimit as i32
        }
        crate::core::error::TingError::NotFound(_)
        | crate::core::error::TingError::PluginNotFound(_) => NativeStatus::NotFound as i32,
        crate::core::error::TingError::InvalidRequest(_)
        | crate::core::error::TingError::ValidationError(_) => NativeStatus::InvalidInput as i32,
        _ => NativeStatus::InternalError as i32,
    }
}

fn read_wasm_bytes(
    caller: &mut Caller<'_, PluginState>,
    ptr: i32,
    len: i32,
) -> std::result::Result<Vec<u8>, i32> {
    if ptr < 0 || !(0..=1024 * 1024).contains(&len) {
        return Err(ERR_INVALID_MEMORY);
    }

    let mem = match caller.get_export("memory") {
        Some(Extern::Memory(mem)) => mem,
        _ => return Err(ERR_INVALID_MEMORY),
    };
    let start = ptr as usize;
    let len = len as usize;
    let end = start.checked_add(len).ok_or(ERR_INVALID_MEMORY)?;
    let ctx = caller.as_context();
    let data = mem.data(&ctx);
    if end > data.len() {
        return Err(ERR_INVALID_MEMORY);
    }

    Ok(data[start..end].to_vec())
}

fn read_wasm_string(
    caller: &mut Caller<'_, PluginState>,
    ptr: i32,
    len: i32,
) -> std::result::Result<String, i32> {
    String::from_utf8(read_wasm_bytes(caller, ptr, len)?).map_err(|_| ERR_INVALID_UTF8)
}

fn read_wasm_json(
    caller: &mut Caller<'_, PluginState>,
    ptr: i32,
    len: i32,
) -> std::result::Result<serde_json::Value, i32> {
    let bytes = read_wasm_bytes(caller, ptr, len)?;
    if bytes.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_slice(&bytes).map_err(|_| ERR_INVALID_JSON)
}

fn store_host_response(caller: &mut Caller<'_, PluginState>, body: Vec<u8>) -> i32 {
    if body.len() > 1024 * 1024
        || caller.data().host_responses.len() >= 128
        || caller
            .data()
            .host_responses
            .values()
            .map(Vec::len)
            .sum::<usize>()
            + body.len()
            > 8 * 1024 * 1024
    {
        return ting_plugin_contract::native_abi::NativeStatus::ResourceLimit as i32;
    }
    let handle = caller
        .data()
        .host_responses
        .keys()
        .copied()
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    caller.data_mut().host_responses.insert(handle, body);
    handle as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_callback_panics_become_internal_status() {
        let status = guarded_host_callback("test", || panic!("test panic"));
        assert_eq!(status, ERR_HOST_PANIC);
    }
}
