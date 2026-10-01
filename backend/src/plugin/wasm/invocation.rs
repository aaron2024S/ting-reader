//! One locked Store frame covers allocation, identity, invoke and release.
//! Dropping an incomplete frame quarantines it and clears all Host context.

use super::plugin::{WasmPlugin, WasmPluginInner};
use crate::core::error::{Result, TingError};
use crate::plugin::types::PluginInvocationContext;
use std::ops::{Deref, DerefMut};
use wasmtime::Memory;

const MAX_CONTROL_BYTES: usize = 1024 * 1024;

struct InvocationFrame<'a> {
    inner: &'a mut WasmPluginInner,
    complete: bool,
}

impl Deref for InvocationFrame<'_> {
    type Target = WasmPluginInner;
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl DerefMut for InvocationFrame<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
    }
}

impl Drop for InvocationFrame<'_> {
    fn drop(&mut self) {
        let state = self.inner.store.data_mut();
        state.current_user = None;
        if !self.complete {
            state.unavailable = true;
            if let Some(scope) = state.resources.as_ref() {
                scope.cancel();
            }
        }
        state.resources = None;
        state.host_responses.clear();
    }
}

fn execution_error(message: &str, error: impl std::fmt::Display) -> TingError {
    TingError::PluginExecutionError(format!("{message}: {error:#}"))
}

async fn allocate(inner: &mut WasmPluginInner, memory: Memory, value: &[u8]) -> Result<i32> {
    let instance = inner.instance;
    let allocate = instance
        .get_typed_func::<i32, i32>(&mut inner.store, "alloc")
        .map_err(|error| execution_error("WASM alloc missing", error))?;
    let length = (value.len() + 1) as i32;
    let pointer = allocate
        .call_async(&mut inner.store, length)
        .await
        .map_err(|error| execution_error("WASM allocation failed", error))?;
    let offset = usize::try_from(pointer)
        .map_err(|error| execution_error("Invalid WASM allocation", error))?;
    if memory.write(&mut inner.store, offset, value).is_err()
        || memory
            .write(&mut inner.store, offset + value.len(), &[0])
            .is_err()
    {
        let _ = release(inner, pointer, length as usize).await;
        return Err(TingError::PluginExecutionError(
            "WASM allocation is outside memory".into(),
        ));
    }
    Ok(pointer)
}

async fn release(inner: &mut WasmPluginInner, pointer: i32, length: usize) -> Result<()> {
    let instance = inner.instance;
    let release = instance
        .get_typed_func::<(i32, i32), ()>(&mut inner.store, "dealloc")
        .map_err(|error| execution_error("WASM dealloc missing", error))?;
    release
        .call_async(&mut inner.store, (pointer, length as i32))
        .await
        .map_err(|error| execution_error("WASM deallocation failed", error))
}

impl WasmPlugin {
    pub(super) async fn call_lifecycle(&self, initializing: bool) -> Result<i32> {
        let mut inner = self.inner.lock().await;
        if inner.store.data().unavailable {
            return Err(TingError::PluginExecutionError(
                "WASM instance is unavailable".into(),
            ));
        }
        let function = if initializing {
            inner.exports.initialize.clone()
        } else {
            inner.exports.shutdown.clone()
        };
        let timeout = inner.execution_timeout;
        inner.store.set_epoch_deadline(1);
        let mut frame = InvocationFrame {
            inner: &mut inner,
            complete: false,
        };
        match tokio::time::timeout(timeout, function.call_async(&mut frame.store, ())).await {
            Ok(Ok(result)) => {
                frame.complete = true;
                Ok(result)
            }
            Ok(Err(error)) => Err(execution_error("WASM lifecycle failed", error)),
            Err(_) => Err(TingError::Timeout(
                "WASM lifecycle exceeded its deadline".into(),
            )),
        }
    }

    pub(super) async fn invoke_raw_json_with_context(
        &self,
        method: &str,
        params: serde_json::Value,
        context: &PluginInvocationContext,
    ) -> Result<String> {
        let input = serde_json::to_vec(&params)
            .map_err(|error| execution_error("WASM input encoding failed", error))?;
        if input.len() > MAX_CONTROL_BYTES || method.is_empty() || method.len() > 128 {
            return Err(TingError::PluginExecutionError(
                "WASM control message exceeds limit".into(),
            ));
        }
        let mut inner = self.inner.lock().await;
        if inner.store.data().unavailable {
            return Err(TingError::PluginExecutionError(
                "WASM instance is unavailable".into(),
            ));
        }
        let instance = inner.instance;
        let memory = instance
            .get_memory(&mut inner.store, "memory")
            .ok_or_else(|| TingError::PluginExecutionError("WASM memory missing".into()))?;
        inner.store.set_epoch_deadline(1);
        let timeout = inner.execution_timeout;
        inner.store.data_mut().current_user = context.user.clone();
        inner.store.data_mut().resources = context.resources.clone();
        let mut frame = InvocationFrame {
            inner: &mut inner,
            complete: false,
        };
        let outcome = tokio::time::timeout(timeout, async {
            let method_ptr = allocate(&mut frame, memory, method.as_bytes()).await?;
            let input_ptr = allocate(&mut frame, memory, &input).await?;
            let invoke = frame.exports.invoke.clone();
            let pointer = invoke
                .call_async(&mut frame.store, (method_ptr, input_ptr))
                .await
                .map_err(|error| execution_error("WASM invoke failed", error))?;
            let result = {
                let offset = usize::try_from(pointer)
                    .map_err(|error| execution_error("Invalid WASM output pointer", error))?;
                let data = memory.data(&frame.store);
                let end = data.len().min(offset.saturating_add(MAX_CONTROL_BYTES + 1));
                let bytes = data.get(offset..end).ok_or_else(|| {
                    TingError::PluginExecutionError("WASM output pointer outside memory".into())
                })?;
                let length = bytes.iter().position(|byte| *byte == 0).ok_or_else(|| {
                    TingError::PluginExecutionError(
                        "WASM output is unterminated or oversized".into(),
                    )
                })?;
                let allocation_end = offset + length + 1;
                let overlaps = |pointer: i32, size: usize| {
                    let begin = pointer as usize;
                    offset < begin + size && begin < allocation_end
                };
                if overlaps(method_ptr, method.len() + 1) || overlaps(input_ptr, input.len() + 1) {
                    return Err(TingError::PluginExecutionError(
                        "WASM output aliases borrowed input".into(),
                    ));
                }
                let text = String::from_utf8(bytes[..length].to_vec())
                    .map_err(|error| execution_error("Invalid WASM output UTF-8", error));
                release(&mut frame, pointer, length + 1).await?;
                text
            };
            release(&mut frame, method_ptr, method.len() + 1).await?;
            release(&mut frame, input_ptr, input.len() + 1).await?;
            result
        })
        .await;
        match outcome {
            Ok(Ok(result)) => {
                frame.complete = true;
                Ok(result)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(TingError::Timeout("WASM call exceeded its deadline".into())),
        }
    }
}
