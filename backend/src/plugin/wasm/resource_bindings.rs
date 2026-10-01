//! Bounded binary imports. Resource semantics remain in the shared Host scope.

use super::plugin::PluginState;
use crate::plugin::host_api::resources::{ResourceError, ResourceScope};
use std::sync::Arc;
use ting_plugin_contract::format_calls::{ChunkRef, MAX_MEDIA_CHUNK_BYTES, ResourceId};
use ting_plugin_contract::native_abi::NativeStatus;
use ting_plugin_contract::protocol::PluginErrorCode;
use wasmtime::{Caller, Extern, Linker, Memory};

fn status(error: ResourceError) -> i32 {
    (match error.code {
        PluginErrorCode::NotFound => NativeStatus::NotFound,
        PluginErrorCode::PermissionDenied => NativeStatus::PermissionDenied,
        PluginErrorCode::ResourceLimit => NativeStatus::ResourceLimit,
        PluginErrorCode::Cancelled => NativeStatus::Cancelled,
        PluginErrorCode::UnsupportedOperation => NativeStatus::UnsupportedOperation,
        _ => NativeStatus::InvalidInput,
    }) as i32
}

fn scope(caller: &Caller<'_, PluginState>) -> std::result::Result<Arc<ResourceScope>, i32> {
    caller
        .data()
        .resources
        .clone()
        .ok_or(NativeStatus::NoScope as i32)
}

fn memory(caller: &mut Caller<'_, PluginState>) -> std::result::Result<Memory, i32> {
    match caller.get_export("memory") {
        Some(Extern::Memory(memory)) => Ok(memory),
        _ => Err(NativeStatus::InvalidInput as i32),
    }
}

fn bounds(
    caller: &Caller<'_, PluginState>,
    memory: Memory,
    pointer: i32,
    length: i32,
    limit: usize,
) -> std::result::Result<std::ops::Range<usize>, i32> {
    if pointer < 0 || length < 0 || length as usize > limit {
        return Err(NativeStatus::InvalidInput as i32);
    }
    let start = pointer as usize;
    let end = start
        .checked_add(length as usize)
        .ok_or(NativeStatus::InvalidInput as i32)?;
    if end > memory.data_size(caller) {
        return Err(NativeStatus::InvalidInput as i32);
    }
    Ok(start..end)
}

fn id(
    caller: &mut Caller<'_, PluginState>,
    pointer: i32,
    length: i32,
) -> std::result::Result<String, i32> {
    let memory = memory(caller)?;
    let range = bounds(caller, memory, pointer, length, 128)?;
    std::str::from_utf8(&memory.data(&*caller)[range])
        .map(str::to_owned)
        .map_err(|_| NativeStatus::InvalidInput as i32)
}

pub(super) fn add(linker: &mut Linker<PluginState>) -> anyhow::Result<()> {
    linker.func_wrap(
        "ting_env",
        "resource_read_at",
        |mut caller: Caller<'_, PluginState>,
         pointer: i32,
         length: i32,
         offset: i64,
         output: i32,
         capacity: i32,
         eof_pointer: i32|
         -> i32 {
            guarded_resource_callback("resource_read_at", || {
                (|| {
                    let scope = scope(&caller)?;
                    let resource = ResourceId(id(&mut caller, pointer, length)?);
                    let memory = memory(&mut caller)?;
                    let range = bounds(
                        &caller,
                        memory,
                        output,
                        capacity,
                        MAX_MEDIA_CHUNK_BYTES as usize,
                    )?;
                    let eof_range = bounds(&caller, memory, eof_pointer, 1, 1)?;
                    let offset =
                        u64::try_from(offset).map_err(|_| NativeStatus::InvalidInput as i32)?;
                    let (bytes, eof) = scope
                        .read_at(&resource, offset, range.len())
                        .map_err(status)?;
                    memory
                        .write(&mut caller, range.start, &bytes)
                        .map_err(|_| NativeStatus::InvalidInput as i32)?;
                    memory
                        .write(&mut caller, eof_range.start, &[u8::from(eof)])
                        .map_err(|_| NativeStatus::InvalidInput as i32)?;
                    Ok(bytes.len() as i32)
                })()
            })
        },
    )?;
    linker.func_wrap(
        "ting_env",
        "resource_write_at",
        |mut caller: Caller<'_, PluginState>,
         pointer: i32,
         length: i32,
         offset: i64,
         input: i32,
         input_len: i32|
         -> i32 {
            guarded_resource_callback("resource_write_at", || {
                (|| {
                    let scope = scope(&caller)?;
                    let resource = ResourceId(id(&mut caller, pointer, length)?);
                    let memory = memory(&mut caller)?;
                    let range = bounds(
                        &caller,
                        memory,
                        input,
                        input_len,
                        MAX_MEDIA_CHUNK_BYTES as usize,
                    )?;
                    let offset =
                        u64::try_from(offset).map_err(|_| NativeStatus::InvalidInput as i32)?;
                    let written = scope
                        .write_at(&resource, offset, &memory.data(&caller)[range])
                        .map_err(status)?;
                    Ok(written as i32)
                })()
            })
        },
    )?;
    linker.func_wrap(
        "ting_env",
        "chunk_create",
        |mut caller: Caller<'_, PluginState>,
         input: i32,
         input_len: i32,
         output: i32,
         capacity: i32|
         -> i32 {
            guarded_resource_callback("chunk_create", || {
                (|| {
                    let scope = scope(&caller)?;
                    let memory = memory(&mut caller)?;
                    let input = bounds(
                        &caller,
                        memory,
                        input,
                        input_len,
                        MAX_MEDIA_CHUNK_BYTES as usize,
                    )?;
                    let output = bounds(&caller, memory, output, capacity, 128)?;
                    if output.len() < 36 {
                        return Err(NativeStatus::ResourceLimit as i32);
                    }
                    let id = scope
                        .create_chunk(&memory.data(&caller)[input])
                        .map_err(status)?;
                    memory
                        .write(&mut caller, output.start, id.0.as_bytes())
                        .map_err(|_| NativeStatus::InvalidInput as i32)?;
                    Ok(id.0.len() as i32)
                })()
            })
        },
    )?;
    linker.func_wrap(
        "ting_env",
        "chunk_copy",
        |mut caller: Caller<'_, PluginState>,
         pointer: i32,
         length: i32,
         output: i32,
         capacity: i32|
         -> i32 {
            guarded_resource_callback("chunk_copy", || {
                (|| {
                    let scope = scope(&caller)?;
                    let chunk = ChunkRef(id(&mut caller, pointer, length)?);
                    let memory = memory(&mut caller)?;
                    let output = bounds(
                        &caller,
                        memory,
                        output,
                        capacity,
                        MAX_MEDIA_CHUNK_BYTES as usize,
                    )?;
                    let bytes = scope.chunk(&chunk).map_err(status)?;
                    if output.len() < bytes.len() {
                        return Err(NativeStatus::ResourceLimit as i32);
                    }
                    memory
                        .write(&mut caller, output.start, &bytes)
                        .map_err(|_| NativeStatus::InvalidInput as i32)?;
                    Ok(bytes.len() as i32)
                })()
            })
        },
    )?;
    Ok(())
}

fn guarded_resource_callback(
    operation: &'static str,
    callback: impl FnOnce() -> std::result::Result<i32, i32>,
) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)) {
        Ok(result) => result.unwrap_or_else(|status| status),
        Err(_) => {
            tracing::error!(operation, "WASM resource callback panicked");
            NativeStatus::InternalError as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_callback_panics_become_internal_status() {
        let status = guarded_resource_callback("test", || panic!("test panic"));
        assert_eq!(status, NativeStatus::InternalError as i32);
    }
}
