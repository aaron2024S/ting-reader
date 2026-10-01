//! Native ABI v2 transport. Context is an explicit per-call borrowed pointer.

use crate::plugin::host_api::resources::{ResourceError, ResourceScope};
use crate::plugin::wasm::sandbox::Permission;
use crate::plugin::{PluginHostGateway, PluginHostUser};
use std::ffi::c_void;
use std::sync::Arc;
use ting_plugin_contract::format_calls::{ChunkRef, MAX_MEDIA_CHUNK_BYTES, ResourceId};
use ting_plugin_contract::native_abi::{
    MAX_NATIVE_CONTROL_BYTES, NATIVE_ABI_REVISION, NativeAbiHeader, NativeHostApiV2, NativeStatus,
    NativeTargetInfo,
};
use ting_plugin_contract::protocol::PluginErrorCode;

#[derive(Clone)]
pub(crate) struct NativeHostInvocationContext {
    pub plugin_id: String,
    pub permissions: Vec<Permission>,
    pub user: Option<PluginHostUser>,
    pub host_gateway: Option<Arc<PluginHostGateway>>,
    pub resources: Option<Arc<ResourceScope>>,
    pub runtime_handle: tokio::runtime::Handle,
}

pub(crate) fn table(context: Option<&mut NativeHostInvocationContext>) -> NativeHostApiV2 {
    NativeHostApiV2 {
        header: NativeAbiHeader {
            abi_revision: NATIVE_ABI_REVISION,
            struct_size: std::mem::size_of::<NativeHostApiV2>() as u32,
        },
        target: NativeTargetInfo::CURRENT,
        user_data: context
            .map(|context| (context as *mut NativeHostInvocationContext).cast())
            .unwrap_or(std::ptr::null_mut()),
        invoke,
        read_at,
        write_at,
        chunk_create,
        chunk_copy,
    }
}

fn guarded(call: impl FnOnce() -> std::result::Result<(), NativeStatus>) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(Ok(())) => NativeStatus::Ok as i32,
        Ok(Err(status)) => status as i32,
        Err(_) => NativeStatus::InternalError as i32,
    }
}

fn resource_status(error: ResourceError) -> NativeStatus {
    match error.code {
        PluginErrorCode::InvalidInput => NativeStatus::InvalidInput,
        PluginErrorCode::NotFound => NativeStatus::NotFound,
        PluginErrorCode::PermissionDenied => NativeStatus::PermissionDenied,
        PluginErrorCode::ResourceLimit => NativeStatus::ResourceLimit,
        PluginErrorCode::Cancelled => NativeStatus::Cancelled,
        PluginErrorCode::UnsupportedOperation => NativeStatus::UnsupportedOperation,
        _ => NativeStatus::InternalError,
    }
}

unsafe fn context<'a>(
    pointer: *mut c_void,
) -> std::result::Result<&'a NativeHostInvocationContext, NativeStatus> {
    if pointer.is_null() {
        return Err(NativeStatus::NoScope);
    }
    Ok(unsafe { &*pointer.cast::<NativeHostInvocationContext>() })
}

unsafe fn bytes<'a>(
    pointer: *const u8,
    len: usize,
    limit: usize,
) -> std::result::Result<&'a [u8], NativeStatus> {
    if pointer.is_null() || len > limit {
        return Err(NativeStatus::InvalidInput);
    }
    Ok(unsafe { std::slice::from_raw_parts(pointer, len) })
}

unsafe fn text<'a>(pointer: *const u8, len: usize) -> std::result::Result<&'a str, NativeStatus> {
    std::str::from_utf8(unsafe { bytes(pointer, len, 128)? })
        .map_err(|_| NativeStatus::InvalidInput)
}

unsafe fn copy_result(
    bytes: &[u8],
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
    limit: usize,
) -> std::result::Result<(), NativeStatus> {
    if output.is_null() || length.is_null() || capacity > limit {
        return Err(NativeStatus::InvalidInput);
    }
    if capacity < bytes.len() {
        return Err(NativeStatus::ResourceLimit);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
        *length = bytes.len();
    }
    Ok(())
}

unsafe extern "C" fn invoke(
    pointer: *mut c_void,
    method: *const u8,
    method_len: usize,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    guarded(|| {
        let context = unsafe { context(pointer)? };
        let method = unsafe { text(method, method_len)? };
        let input: serde_json::Value =
            serde_json::from_slice(unsafe { bytes(input, input_len, MAX_NATIVE_CONTROL_BYTES)? })
                .map_err(|_| NativeStatus::InvalidInput)?;
        // Reject invalid output buffers before any Host mutation occurs.
        if output.is_null() || length.is_null() || capacity > MAX_NATIVE_CONTROL_BYTES {
            return Err(NativeStatus::InvalidInput);
        }
        let value = if method.starts_with("resources.") {
            context
                .resources
                .as_ref()
                .ok_or(NativeStatus::NoScope)?
                .invoke(method, input)
                .map_err(resource_status)?
        } else {
            let gateway = context.host_gateway.as_ref().ok_or(NativeStatus::NoScope)?;
            context
                .runtime_handle
                .block_on(crate::plugin::host_api::invoke(
                    &context.plugin_id,
                    Some(&context.permissions),
                    context.user.as_ref(),
                    context.resources.as_ref(),
                    Some(gateway),
                    method,
                    input,
                ))
                .map_err(|_| NativeStatus::InternalError)?
        };
        let encoded = serde_json::to_vec(&value).map_err(|_| NativeStatus::InternalError)?;
        unsafe { copy_result(&encoded, output, capacity, length, MAX_NATIVE_CONTROL_BYTES) }
    })
}

unsafe extern "C" fn read_at(
    pointer: *mut c_void,
    id: *const u8,
    id_len: usize,
    offset: u64,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
    eof: *mut bool,
) -> i32 {
    guarded(|| {
        let context = unsafe { context(pointer)? };
        let id = ResourceId(unsafe { text(id, id_len)? }.into());
        if output.is_null()
            || length.is_null()
            || eof.is_null()
            || capacity == 0
            || capacity > MAX_MEDIA_CHUNK_BYTES as usize
        {
            return Err(NativeStatus::InvalidInput);
        }
        let scope = context.resources.as_ref().ok_or(NativeStatus::NoScope)?;
        let (bytes, finished) = scope
            .read_at(&id, offset, capacity)
            .map_err(resource_status)?;
        unsafe {
            copy_result(
                &bytes,
                output,
                capacity,
                length,
                MAX_MEDIA_CHUNK_BYTES as usize,
            )?;
            *eof = finished;
        }
        Ok(())
    })
}

unsafe extern "C" fn write_at(
    pointer: *mut c_void,
    id: *const u8,
    id_len: usize,
    offset: u64,
    input: *const u8,
    input_len: usize,
    written: *mut usize,
) -> i32 {
    guarded(|| {
        let context = unsafe { context(pointer)? };
        let id = ResourceId(unsafe { text(id, id_len)? }.into());
        let input = unsafe { bytes(input, input_len, MAX_MEDIA_CHUNK_BYTES as usize)? };
        if written.is_null() {
            return Err(NativeStatus::InvalidInput);
        }
        let count = context
            .resources
            .as_ref()
            .ok_or(NativeStatus::NoScope)?
            .write_at(&id, offset, input)
            .map_err(resource_status)?;
        unsafe {
            *written = count;
        }
        Ok(())
    })
}

unsafe extern "C" fn chunk_create(
    pointer: *mut c_void,
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    guarded(|| {
        let context = unsafe { context(pointer)? };
        let input = unsafe { bytes(input, input_len, MAX_MEDIA_CHUNK_BYTES as usize)? };
        if output.is_null() || length.is_null() || !(36..=128).contains(&capacity) {
            return Err(NativeStatus::InvalidInput);
        }
        let id = context
            .resources
            .as_ref()
            .ok_or(NativeStatus::NoScope)?
            .create_chunk(input)
            .map_err(resource_status)?;
        unsafe { copy_result(id.0.as_bytes(), output, capacity, length, 128) }
    })
}

unsafe extern "C" fn chunk_copy(
    pointer: *mut c_void,
    id: *const u8,
    id_len: usize,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    guarded(|| {
        let context = unsafe { context(pointer)? };
        let id = ChunkRef(unsafe { text(id, id_len)? }.into());
        let bytes = context
            .resources
            .as_ref()
            .ok_or(NativeStatus::NoScope)?
            .chunk(&id)
            .map_err(resource_status)?;
        unsafe {
            copy_result(
                &bytes,
                output,
                capacity,
                length,
                MAX_MEDIA_CHUNK_BYTES as usize,
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
