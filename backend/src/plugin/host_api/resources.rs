//! Host resource management. A scope is bound to one registry generation and user.
//! No plugin path, URL or user supplied in JSON can grant resource access.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use ting_plugin_contract::format_calls::{
    ChunkRef, MAX_MEDIA_CHUNK_BYTES, MAX_SAFE_INTEGER, ResourceId, SessionId,
};
use ting_plugin_contract::protocol::PluginErrorCode;
use ting_plugin_contract::resources::{ResourceCall, ResourceRead, ResourceStat};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub type ResourceResult<T> = std::result::Result<T, ResourceError>;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ResourceError {
    pub code: PluginErrorCode,
    pub message: &'static str,
}

fn error(code: PluginErrorCode, message: &'static str) -> ResourceError {
    ResourceError { code, message }
}

fn io_error(_: io::Error) -> ResourceError {
    // File paths and signed URLs must not escape through plugin errors.
    error(PluginErrorCode::InternalError, "Resource I/O failed")
}

/// Sources are opened by trusted core code. An implementation must honor
/// the requested limit, deadline and cancellation, including remote ranges.
pub trait ResourceSource: Send + Sync {
    fn stat(&self) -> ResourceStat;
    fn read_at(
        &self,
        offset: u64,
        max_bytes: usize,
        cancel: &CancellationToken,
    ) -> ResourceResult<Vec<u8>>;
}

struct FileSource {
    file: File,
    stat: ResourceStat,
}

impl ResourceSource for FileSource {
    fn stat(&self) -> ResourceStat {
        self.stat.clone()
    }

    fn read_at(
        &self,
        offset: u64,
        max_bytes: usize,
        cancel: &CancellationToken,
    ) -> ResourceResult<Vec<u8>> {
        if cancel.is_cancelled() {
            return Err(error(
                PluginErrorCode::Cancelled,
                "Resource scope cancelled",
            ));
        }
        let mut bytes = vec![0; max_bytes];
        let count = positioned_read(&self.file, &mut bytes, offset).map_err(io_error)?;
        bytes.truncate(count);
        Ok(bytes)
    }
}

struct MemorySource(Arc<[u8]>, ResourceStat);

impl ResourceSource for MemorySource {
    fn stat(&self) -> ResourceStat {
        self.1.clone()
    }

    fn read_at(
        &self,
        offset: u64,
        max_bytes: usize,
        cancel: &CancellationToken,
    ) -> ResourceResult<Vec<u8>> {
        if cancel.is_cancelled() {
            return Err(error(
                PluginErrorCode::Cancelled,
                "Resource scope cancelled",
            ));
        }
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(self.0.len());
        let end = start.saturating_add(max_bytes).min(self.0.len());
        Ok(self.0[start..end].to_vec())
    }
}

#[cfg(windows)]
fn positioned_read(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, bytes, offset)
}

#[cfg(unix)]
fn positioned_read(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, bytes, offset)
}

#[cfg(windows)]
fn positioned_write(file: &File, bytes: &[u8], offset: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(file, bytes, offset)
}

#[cfg(unix)]
fn positioned_write(file: &File, bytes: &[u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(file, bytes, offset)
}

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub max_handles: usize,
    pub max_inflight_chunks: usize,
    pub max_chunk_bytes: usize,
    pub max_memory_input_bytes: usize,
    pub max_output_bytes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_handles: 128,
            max_inflight_chunks: 4,
            max_chunk_bytes: MAX_MEDIA_CHUNK_BYTES as usize,
            max_memory_input_bytes: 8 * 1024 * 1024,
            max_output_bytes: MAX_SAFE_INTEGER,
        }
    }
}

enum Resource {
    Input {
        source: Arc<dyn ResourceSource>,
        /// Maximum readable absolute end. A prefix grant cannot read beyond it.
        readable_end: u64,
        position: u64,
        resident_bytes: usize,
    },
    Output {
        file: File,
        path: PathBuf,
        stat: ResourceStat,
        position: u64,
    },
}

#[derive(Default)]
struct ScopeState {
    resources: HashMap<String, Resource>,
    issued_resources: HashSet<String>,
    chunks: HashMap<String, Arc<[u8]>>,
    sessions: HashSet<String>,
    issued_sessions: HashSet<String>,
    resident_inputs: usize,
    output_paths: Vec<PathBuf>,
}

impl Drop for ScopeState {
    fn drop(&mut self) {
        self.resources.clear();
        for path in &self.output_paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Never serialized. Possessing a JSON ResourceId without this scope grants
/// nothing. Core callers retain session scopes explicitly until close/cancel.
pub struct ResourceScope {
    plugin_id: String,
    generation: Uuid,
    principal: Option<String>,
    limits: ResourceLimits,
    staging_dir: PathBuf,
    cancel: CancellationToken,
    state: Mutex<ScopeState>,
    read_lock: Mutex<()>,
}

struct StageOnDrop(PathBuf);

impl Drop for StageOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl std::fmt::Debug for ResourceScope {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("ResourceScope")
            .field("plugin_id", &self.plugin_id)
            .field("generation", &self.generation)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl ResourceScope {
    pub(crate) fn new(
        plugin_id: String,
        generation: Uuid,
        principal: Option<String>,
        staging_dir: PathBuf,
        limits: ResourceLimits,
    ) -> Self {
        Self {
            plugin_id,
            generation,
            principal,
            limits,
            staging_dir,
            cancel: CancellationToken::new(),
            state: Mutex::new(ScopeState::default()),
            read_lock: Mutex::new(()),
        }
    }

    pub fn authorize(
        &self,
        plugin_id: &str,
        generation: Uuid,
        principal: Option<&str>,
    ) -> ResourceResult<()> {
        self.authorize_owner(plugin_id, generation, principal)?;
        self.check_active()
    }

    pub fn authorize_owner(
        &self,
        plugin_id: &str,
        generation: Uuid,
        principal: Option<&str>,
    ) -> ResourceResult<()> {
        if self.plugin_id != plugin_id
            || self.generation != generation
            || self.principal.as_deref() != principal
        {
            return Err(error(
                PluginErrorCode::PermissionDenied,
                "Resource scope owner mismatch",
            ));
        }
        Ok(())
    }

    fn check_active(&self) -> ResourceResult<()> {
        if self.cancel.is_cancelled() {
            return Err(error(
                PluginErrorCode::Cancelled,
                "Resource scope cancelled",
            ));
        }
        Ok(())
    }

    fn state(&self) -> ResourceResult<MutexGuard<'_, ScopeState>> {
        self.state
            .lock()
            .map_err(|_| error(PluginErrorCode::InternalError, "Resource state unavailable"))
    }

    fn check_handle_limit(&self, state: &ScopeState) -> ResourceResult<()> {
        // Bound tombstones as well: a plugin cannot grow them by open/close.
        if state.issued_resources.len() + state.issued_sessions.len() >= self.limits.max_handles {
            return Err(error(
                PluginErrorCode::ResourceLimit,
                "Resource handle limit reached",
            ));
        }
        Ok(())
    }

    pub fn grant_source(
        &self,
        source: Arc<dyn ResourceSource>,
        readable_end: u64,
        resident_bytes: usize,
    ) -> ResourceResult<ResourceId> {
        self.check_active()?;
        if readable_end > MAX_SAFE_INTEGER {
            return Err(error(
                PluginErrorCode::InvalidInput,
                "Invalid readable range",
            ));
        }
        let mut state = self.state()?;
        self.check_handle_limit(&state)?;
        let resident = state
            .resident_inputs
            .checked_add(resident_bytes)
            .ok_or_else(|| error(PluginErrorCode::ResourceLimit, "Input memory limit reached"))?;
        if resident > self.limits.max_memory_input_bytes {
            return Err(error(
                PluginErrorCode::ResourceLimit,
                "Input memory limit reached",
            ));
        }
        let id = Uuid::new_v4().to_string();
        state.resident_inputs = resident;
        state.issued_resources.insert(id.clone());
        state.resources.insert(
            id.clone(),
            Resource::Input {
                source,
                readable_end,
                position: 0,
                resident_bytes,
            },
        );
        Ok(ResourceId(id))
    }

    pub fn grant_bytes(
        &self,
        bytes: Arc<[u8]>,
        mime_type: Option<String>,
    ) -> ResourceResult<ResourceId> {
        let length = bytes.len();
        let source = MemorySource(
            bytes,
            ResourceStat {
                length: Some(length as u64),
                mime_type,
                readable: true,
                writable: false,
                seekable: true,
                revision: None,
                finished: true,
            },
        );
        self.grant_source(Arc::new(source), length as u64, length)
    }

    pub fn grant_file(
        &self,
        path: &Path,
        readable_end: u64,
        mime_type: Option<String>,
    ) -> ResourceResult<ResourceId> {
        let file = File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > MAX_SAFE_INTEGER {
            return Err(error(PluginErrorCode::InvalidInput, "Invalid source file"));
        }
        let revision = format!("{}:{:?}", metadata.len(), metadata.modified().ok());
        let length = metadata.len();
        self.grant_source(
            Arc::new(FileSource {
                file,
                stat: ResourceStat {
                    length: Some(length),
                    mime_type,
                    readable: true,
                    writable: false,
                    seekable: true,
                    revision: Some(revision),
                    finished: true,
                },
            }),
            readable_end.min(length),
            0,
        )
    }

    /// Only trusted core callers can widen the grant for a probe or metadata
    /// call. This cannot be reached through resources.invoke.
    pub(crate) fn extend_readable_end(&self, id: &ResourceId, end: u64) -> ResourceResult<()> {
        self.check_active()?;
        if end > MAX_SAFE_INTEGER {
            return Err(error(PluginErrorCode::InvalidInput, "Invalid source range"));
        }
        let mut state = self.state()?;
        match state.resources.get_mut(&id.0) {
            Some(Resource::Input {
                source,
                readable_end,
                ..
            }) => {
                if source.stat().length.is_some_and(|length| end > length) {
                    return Err(error(
                        PluginErrorCode::InvalidInput,
                        "Grant exceeds source length",
                    ));
                }
                *readable_end = end;
                Ok(())
            }
            _ => Err(error(
                PluginErrorCode::NotFound,
                "Readable resource unavailable",
            )),
        }
    }

    pub fn stat(&self, id: &ResourceId) -> ResourceResult<ResourceStat> {
        self.check_active()?;
        match self.state()?.resources.get(&id.0) {
            Some(Resource::Input { source, .. }) => Ok(source.stat()),
            Some(Resource::Output { stat, .. }) => Ok(stat.clone()),
            None => Err(error(PluginErrorCode::NotFound, "Resource unavailable")),
        }
    }

    fn validate_range(&self, offset: u64, max_bytes: usize) -> ResourceResult<()> {
        self.check_active()?;
        if max_bytes == 0
            || max_bytes > self.limits.max_chunk_bytes
            || offset > MAX_SAFE_INTEGER
            || offset
                .checked_add(max_bytes as u64)
                .is_none_or(|end| end > MAX_SAFE_INTEGER)
        {
            return Err(error(
                PluginErrorCode::ResourceLimit,
                "Resource range limit exceeded",
            ));
        }
        Ok(())
    }

    pub fn read_at(
        &self,
        id: &ResourceId,
        offset: u64,
        max_bytes: usize,
    ) -> ResourceResult<(Vec<u8>, bool)> {
        self.validate_range(offset, max_bytes)?;
        let (source, end) = {
            let state = self.state()?;
            match state.resources.get(&id.0) {
                Some(Resource::Input {
                    source,
                    readable_end,
                    ..
                }) => (Arc::clone(source), *readable_end),
                Some(Resource::Output { file, stat, .. }) if stat.finished => (
                    Arc::new(FileSource {
                        file: file.try_clone().map_err(io_error)?,
                        stat: stat.clone(),
                    }) as Arc<dyn ResourceSource>,
                    stat.length.unwrap_or(0),
                ),
                Some(Resource::Output { .. }) => {
                    return Err(error(
                        PluginErrorCode::PermissionDenied,
                        "Output is not finished",
                    ));
                }
                None => return Err(error(PluginErrorCode::NotFound, "Resource unavailable")),
            }
        };
        if offset > end {
            return Err(error(
                PluginErrorCode::PermissionDenied,
                "Read exceeds granted prefix",
            ));
        }
        let allowed = max_bytes.min((end - offset) as usize);
        if allowed == 0 {
            let eof = source.stat().length.is_some_and(|length| offset >= length);
            return if eof {
                Ok((Vec::new(), true))
            } else {
                Err(error(
                    PluginErrorCode::PermissionDenied,
                    "Read exceeds granted prefix",
                ))
            };
        }
        let bytes = source.read_at(offset, allowed, &self.cancel)?;
        self.check_active()?;
        if bytes.len() > allowed {
            return Err(error(
                PluginErrorCode::InvalidOutput,
                "Source exceeded requested range",
            ));
        }
        // Revalidate after unlocked I/O: close/cancel may have happened.
        if !self.state()?.resources.contains_key(&id.0) {
            return Err(error(
                PluginErrorCode::NotFound,
                "Resource closed during read",
            ));
        }
        let eof = source
            .stat()
            .length
            .is_some_and(|length| offset + bytes.len() as u64 >= length);
        if bytes.is_empty() && !eof {
            return Err(error(
                PluginErrorCode::NetworkError,
                "Source returned no bytes before EOF",
            ));
        }
        Ok((bytes, eof))
    }

    pub fn create_chunk(&self, bytes: &[u8]) -> ResourceResult<ChunkRef> {
        self.check_active()?;
        if bytes.len() > self.limits.max_chunk_bytes {
            return Err(error(
                PluginErrorCode::ResourceLimit,
                "Chunk byte limit exceeded",
            ));
        }
        let mut state = self.state()?;
        if state.chunks.len() >= self.limits.max_inflight_chunks {
            return Err(error(
                PluginErrorCode::ResourceLimit,
                "Release a chunk before reading more",
            ));
        }
        let id = Uuid::new_v4().to_string();
        state.chunks.insert(id.clone(), Arc::from(bytes));
        Ok(ChunkRef(id))
    }

    pub fn chunk(&self, id: &ChunkRef) -> ResourceResult<Arc<[u8]>> {
        self.check_active()?;
        self.state()?
            .chunks
            .get(&id.0)
            .cloned()
            .ok_or_else(|| error(PluginErrorCode::NotFound, "Chunk lease unavailable"))
    }

    pub fn release_chunk(&self, id: &ChunkRef) -> ResourceResult<()> {
        self.state()?
            .chunks
            .remove(&id.0)
            .ok_or_else(|| error(PluginErrorCode::NotFound, "Chunk lease unavailable"))?;
        Ok(())
    }

    pub fn create_output(&self, mime_type: Option<String>) -> ResourceResult<ResourceId> {
        self.check_active()?;
        let mut state = self.state()?;
        self.check_handle_limit(&state)?;
        std::fs::create_dir_all(&self.staging_dir).map_err(io_error)?;
        let id = Uuid::new_v4().to_string();
        let path = self.staging_dir.join(format!("{id}.staging"));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(io_error)?;
        state.output_paths.push(path.clone());
        state.issued_resources.insert(id.clone());
        state.resources.insert(
            id.clone(),
            Resource::Output {
                file,
                path,
                stat: ResourceStat {
                    length: Some(0),
                    mime_type,
                    readable: false,
                    writable: true,
                    seekable: true,
                    revision: None,
                    finished: false,
                },
                position: 0,
            },
        );
        Ok(ResourceId(id))
    }

    pub fn write_at(&self, id: &ResourceId, offset: u64, bytes: &[u8]) -> ResourceResult<usize> {
        self.validate_range(offset, bytes.len())?;
        if offset + bytes.len() as u64 > self.limits.max_output_bytes {
            return Err(error(
                PluginErrorCode::ResourceLimit,
                "Output byte limit exceeded",
            ));
        }
        let mut state = self.state()?;
        match state.resources.get_mut(&id.0) {
            Some(Resource::Output {
                file,
                stat,
                position,
                ..
            }) if !stat.finished => {
                let written = positioned_write(file, bytes, offset).map_err(io_error)?;
                *position = offset + written as u64;
                stat.length = Some(stat.length.unwrap_or(0).max(*position));
                Ok(written)
            }
            Some(_) => Err(error(
                PluginErrorCode::PermissionDenied,
                "Resource is not writable",
            )),
            None => Err(error(PluginErrorCode::NotFound, "Resource unavailable")),
        }
    }

    pub fn finish(&self, id: &ResourceId) -> ResourceResult<()> {
        self.check_active()?;
        let mut state = self.state()?;
        match state.resources.get_mut(&id.0) {
            Some(Resource::Output { file, stat, .. }) => {
                file.sync_all().map_err(io_error)?;
                stat.finished = true;
                stat.writable = false;
                stat.readable = true;
                Ok(())
            }
            _ => Err(error(
                PluginErrorCode::PermissionDenied,
                "Resource is not an output",
            )),
        }
    }

    pub fn close(&self, id: &ResourceId) -> ResourceResult<()> {
        let mut state = self.state()?;
        if !state.issued_resources.contains(&id.0) {
            return Err(error(PluginErrorCode::NotFound, "Unknown resource"));
        }
        if let Some(resource) = state.resources.remove(&id.0) {
            match resource {
                Resource::Input { resident_bytes, .. } => {
                    state.resident_inputs = state.resident_inputs.saturating_sub(resident_bytes);
                }
                Resource::Output { file, path, .. } => {
                    drop(file);
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        Ok(())
    }

    pub fn create_session(&self) -> ResourceResult<SessionId> {
        self.check_active()?;
        let mut state = self.state()?;
        self.check_handle_limit(&state)?;
        let id = Uuid::new_v4().to_string();
        state.sessions.insert(id.clone());
        state.issued_sessions.insert(id.clone());
        Ok(SessionId(id))
    }

    pub fn check_session(&self, id: &SessionId) -> ResourceResult<()> {
        self.check_active()?;
        if !self.state()?.sessions.contains(&id.0) {
            return Err(error(
                PluginErrorCode::PermissionDenied,
                "Session scope mismatch",
            ));
        }
        Ok(())
    }

    pub fn close_session(&self, id: &SessionId) -> ResourceResult<()> {
        let mut state = self.state()?;
        if !state.issued_sessions.contains(&id.0) {
            return Err(error(PluginErrorCode::NotFound, "Unknown session"));
        }
        state.sessions.remove(&id.0);
        Ok(())
    }

    /// Trusted core commit. A plugin can only write its private staging output;
    /// it cannot choose a destination path or commit into the media library.
    pub(crate) fn commit_local_output(
        &self,
        output: &ResourceId,
        source: &ResourceId,
        path: &Path,
        expected_revision: &str,
    ) -> ResourceResult<()> {
        self.check_active()?;
        let original_revision = self
            .stat(source)?
            .revision
            .ok_or_else(|| error(PluginErrorCode::InvalidInput, "Source has no file revision"))?;
        if original_revision != expected_revision {
            return Err(error(PluginErrorCode::Conflict, "Source revision changed"));
        }
        let stat = self.stat(output)?;
        if !stat.finished || !stat.readable || stat.length.is_none() {
            return Err(error(
                PluginErrorCode::InvalidOutput,
                "Output is not finished",
            ));
        }
        let length = stat.length.unwrap_or(0);
        let dir = path
            .parent()
            .ok_or_else(|| error(PluginErrorCode::InvalidInput, "Invalid destination"))?;
        let stage = StageOnDrop(dir.join(format!(".ting-metadata-{}.staging", Uuid::new_v4())));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage.0)
            .map_err(io_error)?;
        let mut offset = 0;
        while offset < length {
            let (bytes, _) = self.read_at(
                output,
                offset,
                (length - offset).min(self.limits.max_chunk_bytes as u64) as usize,
            )?;
            if bytes.is_empty() {
                return Err(error(
                    PluginErrorCode::InvalidOutput,
                    "Staging output ended early",
                ));
            }
            file.write_all(&bytes).map_err(io_error)?;
            offset += bytes.len() as u64;
        }
        file.sync_all().map_err(io_error)?;
        drop(file);
        let metadata = std::fs::metadata(path).map_err(io_error)?;
        let current_revision = format!("{}:{:?}", metadata.len(), metadata.modified().ok());
        if current_revision != expected_revision {
            return Err(error(
                PluginErrorCode::Conflict,
                "Source revision changed during write",
            ));
        }
        std::fs::rename(&stage.0, path).map_err(io_error)?;
        Ok(())
    }

    /// Host-only asset commit. The caller authorizes and resolves the target
    /// before invoking this method; the plugin can only name its staging ID.
    pub(crate) fn commit_asset_output(
        &self,
        output: &ResourceId,
        path: &Path,
        max_bytes: u64,
    ) -> ResourceResult<u64> {
        self.check_active()?;
        let stat = match self.state()?.resources.get(&output.0) {
            Some(Resource::Output { stat, .. }) => stat.clone(),
            _ => {
                return Err(error(
                    PluginErrorCode::InvalidInput,
                    "Asset resource is not an output",
                ));
            }
        };
        if !stat.finished || !stat.readable || stat.writable {
            return Err(error(
                PluginErrorCode::InvalidInput,
                "Asset output is not finished",
            ));
        }
        let length = stat
            .length
            .filter(|length| *length <= max_bytes)
            .ok_or_else(|| error(PluginErrorCode::ResourceLimit, "Asset exceeds byte limit"))?;
        let dir = path
            .parent()
            .ok_or_else(|| error(PluginErrorCode::InvalidInput, "Invalid asset destination"))?;
        let stage = StageOnDrop(dir.join(format!(".ting-asset-{}.staging", Uuid::new_v4())));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stage.0)
            .map_err(io_error)?;
        let mut offset = 0;
        while offset < length {
            let (bytes, _) = self.read_at(
                output,
                offset,
                (length - offset).min(self.limits.max_chunk_bytes as u64) as usize,
            )?;
            if bytes.is_empty() {
                return Err(error(
                    PluginErrorCode::InvalidOutput,
                    "Asset output ended early",
                ));
            }
            file.write_all(&bytes).map_err(io_error)?;
            offset += bytes.len() as u64;
        }
        file.sync_all().map_err(io_error)?;
        drop(file);
        self.check_active()?;
        std::fs::rename(&stage.0, path).map_err(io_error)?;
        Ok(length)
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
        if let Ok(mut state) = self.state.lock() {
            state.resources.clear();
            state.chunks.clear();
            state.sessions.clear();
            state.resident_inputs = 0;
            for path in &state.output_paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn managed_bytes(&self) -> usize {
        self.state()
            .map(|state| {
                state.resident_inputs
                    + state
                        .chunks
                        .values()
                        .map(|chunk| chunk.len())
                        .sum::<usize>()
            })
            .unwrap_or(0)
    }

    /// Runtime transports all dispatch to this control entry point.
    pub fn invoke(
        &self,
        operation: &str,
        input: serde_json::Value,
    ) -> ResourceResult<serde_json::Value> {
        let operation = operation.strip_prefix("resources.").ok_or_else(|| {
            error(
                PluginErrorCode::UnsupportedOperation,
                "Unknown resource operation",
            )
        })?;
        let call: ResourceCall = serde_json::from_value(serde_json::json!({
            "operation": operation, "input": input,
        }))
        .map_err(|_| {
            error(
                PluginErrorCode::InvalidInput,
                "Invalid resource control input",
            )
        })?;
        fn encode(value: impl serde::Serialize) -> ResourceResult<serde_json::Value> {
            serde_json::to_value(value)
                .map_err(|_| error(PluginErrorCode::InternalError, "Resource encoding failed"))
        }
        match call {
            ResourceCall::Stat(request) => encode(self.stat(&request.resource)?),
            ResourceCall::ReadAt(request) => {
                let (bytes, eof) = self.read_at(
                    &request.resource,
                    request.offset,
                    usize::try_from(request.max_bytes).unwrap_or(usize::MAX),
                )?;
                let chunk = self.create_chunk(&bytes)?;
                encode(ResourceRead {
                    chunk,
                    bytes: bytes.len() as u64,
                    eof,
                })
            }
            ResourceCall::Read(request) => {
                let _serial = self
                    .read_lock
                    .lock()
                    .map_err(|_| error(PluginErrorCode::InternalError, "Read state unavailable"))?;
                let position = match self.state()?.resources.get(&request.resource.0) {
                    Some(Resource::Input { position, .. }) => *position,
                    _ => {
                        return Err(error(
                            PluginErrorCode::NotFound,
                            "Readable resource unavailable",
                        ));
                    }
                };
                let (bytes, eof) = self.read_at(
                    &request.resource,
                    position,
                    usize::try_from(request.max_bytes).unwrap_or(usize::MAX),
                )?;
                let chunk = self.create_chunk(&bytes)?;
                if let Some(Resource::Input {
                    position: cursor, ..
                }) = self.state()?.resources.get_mut(&request.resource.0)
                {
                    *cursor = position + bytes.len() as u64;
                }
                encode(ResourceRead {
                    chunk,
                    bytes: bytes.len() as u64,
                    eof,
                })
            }
            ResourceCall::Seek(request) => {
                self.check_active()?;
                if request.offset > MAX_SAFE_INTEGER {
                    return Err(error(PluginErrorCode::InvalidInput, "Invalid seek offset"));
                }
                let mut state = self.state()?;
                match state.resources.get_mut(&request.resource.0) {
                    Some(Resource::Input {
                        position,
                        readable_end,
                        ..
                    }) if request.offset <= *readable_end => *position = request.offset,
                    Some(Resource::Output { position, .. })
                        if request.offset <= self.limits.max_output_bytes =>
                    {
                        *position = request.offset
                    }
                    _ => {
                        return Err(error(
                            PluginErrorCode::PermissionDenied,
                            "Seek exceeds resource grant",
                        ));
                    }
                }
                Ok(serde_json::json!({ "offset": request.offset }))
            }
            ResourceCall::Close(request) => {
                self.close(&request.resource)?;
                Ok(serde_json::json!({}))
            }
            ResourceCall::CreateOutput(request) => Ok(serde_json::json!({
                "resource": self.create_output(request.mime_type)?
            })),
            ResourceCall::Finish(request) => {
                self.finish(&request.resource)?;
                Ok(serde_json::json!({}))
            }
            ResourceCall::ReleaseChunk(request) => {
                self.release_chunk(&request.chunk)?;
                Ok(serde_json::json!({}))
            }
            ResourceCall::CreateSession(_) => {
                Ok(serde_json::json!({ "session_id": self.create_session()? }))
            }
            ResourceCall::CheckSession(request) => {
                self.check_session(&request.session_id)?;
                Ok(serde_json::json!({}))
            }
            ResourceCall::CloseSession(request) => {
                self.close_session(&request.session_id)?;
                Ok(serde_json::json!({}))
            }
        }
    }
}

impl Drop for ResourceScope {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/host_api/resources.rs"]
mod tests;
