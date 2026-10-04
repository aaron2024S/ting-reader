use crate::core::app::error::{Result, TingError};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::process::Child;
use tokio::sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub(super) const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_GENERATIONS: usize = 3;

pub(super) struct HlsProcess {
    pub child: Child,
    pub input_task: Option<JoinHandle<()>>,
    pub stderr_task: Option<JoinHandle<()>>,
    pub input_error: Arc<AtomicBool>,
}

impl HlsProcess {
    pub fn check_input(&self) -> Result<()> {
        if self.input_error.load(Ordering::Relaxed) {
            return Err(TingError::ExternalError("HLS decoded input failed".into()));
        }
        Ok(())
    }

    pub async fn stop(mut self) {
        for task in [self.input_task.take(), self.stderr_task.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
            let _ = task.await;
        }
        // kill() also waits, reaping exited children before removing files.
        let _ = self.child.kill().await;
    }
}

impl Drop for HlsProcess {
    fn drop(&mut self) {
        for task in [&self.input_task, &self.stderr_task].into_iter().flatten() {
            task.abort();
        }
        // AudioService configures kill_on_drop for the child itself.
    }
}

#[derive(Clone)]
pub(super) struct HlsGeneration {
    pub seq: u32,
    pub start_offset: f64,
    pub temp_dir: PathBuf,
}

pub(super) struct HlsSession {
    pub chapter_id: String,
    pub library_id: String,
    pub is_strm: bool,
    pub temp_dir: PathBuf,
    pub last_accessed: Instant,
    pub generations: VecDeque<HlsGeneration>,
    pub process: Option<HlsProcess>,
    pub closed: bool,
    permit: Option<OwnedSemaphorePermit>,
}

impl HlsSession {
    pub async fn next_generation(&mut self, start_offset: f64) -> Result<HlsGeneration> {
        if let Some(process) = self.process.take() {
            process.stop().await;
        }
        let seq = self.generations.back().map_or(0, |g| g.seq + 1);
        let generation = HlsGeneration {
            seq,
            start_offset,
            temp_dir: self.temp_dir.join(seq.to_string()),
        };
        tokio::fs::create_dir_all(&generation.temp_dir).await?;
        self.generations.push_back(generation.clone());
        // Retain recent generations for in-flight playlist/segment requests.
        while self.generations.len() > MAX_GENERATIONS {
            if let Some(old) = self.generations.pop_front() {
                tokio::fs::remove_dir_all(old.temp_dir).await?;
            }
        }
        Ok(generation)
    }
}

/// Each session owns a permit from reservation until explicit or idle cleanup.
/// Per-session locks serialize startup/seek without blocking other sessions.
pub struct HlsSessionManager {
    sessions: RwLock<HashMap<String, Arc<Mutex<HlsSession>>>>,
    base_temp_dir: PathBuf,
    permits: Arc<Semaphore>,
}

/// Cancelled or failed initialization must release its reserved session too.
pub(super) struct PendingHlsSession {
    pub id: String,
    pub session: Arc<Mutex<HlsSession>>,
    manager: Arc<HlsSessionManager>,
    committed: bool,
}

impl PendingHlsSession {
    pub fn commit(mut self) -> String {
        self.committed = true;
        self.id.clone()
    }
}

impl Drop for PendingHlsSession {
    fn drop(&mut self) {
        if !self.committed {
            let manager = self.manager.clone();
            let id = self.id.clone();
            tokio::spawn(async move {
                manager.close_session(&id).await;
            });
        }
    }
}

impl HlsSessionManager {
    pub fn new(base_temp_dir: PathBuf) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            base_temp_dir,
            permits: Arc::new(Semaphore::new(3)),
        }
    }

    pub(super) async fn create_session(
        self: &Arc<Self>,
        chapter_id: String,
        library_id: String,
        is_strm: bool,
    ) -> Result<PendingHlsSession> {
        let permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            TingError::ResourceLimitExceeded("Too many concurrent HLS sessions".into())
        })?;
        let id = Uuid::new_v4().to_string();
        let session = Arc::new(Mutex::new(HlsSession {
            chapter_id,
            library_id,
            is_strm,
            temp_dir: self.base_temp_dir.join(&id),
            last_accessed: Instant::now(),
            generations: VecDeque::new(),
            process: None,
            closed: false,
            permit: Some(permit),
        }));
        self.sessions
            .write()
            .await
            .insert(id.clone(), session.clone());
        Ok(PendingHlsSession {
            id,
            session,
            manager: self.clone(),
            committed: false,
        })
    }

    pub(super) async fn get_session(&self, id: &str) -> Result<Arc<Mutex<HlsSession>>> {
        let session = self
            .sessions
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| TingError::NotFound("HLS session not found".into()))?;
        {
            let mut guard = session.lock().await;
            if guard.closed {
                return Err(TingError::NotFound("HLS session closed".into()));
            }
            guard.last_accessed = Instant::now();
        }
        Ok(session)
    }

    pub async fn close_session(&self, id: &str) {
        let session = self.sessions.write().await.remove(id);
        if let Some(session) = session {
            let mut session = session.lock().await;
            session.closed = true;
            if let Some(process) = session.process.take() {
                process.stop().await;
            }
            if let Err(error) = tokio::fs::remove_dir_all(&session.temp_dir).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(session_id = id, %error, "HLS temporary directory cleanup failed");
            }
            session.permit.take();
        }
    }

    pub async fn cleanup_expired(&self) {
        let sessions: Vec<_> = self
            .sessions
            .read()
            .await
            .iter()
            .map(|(id, session)| (id.clone(), session.clone()))
            .collect();
        for (id, session) in sessions {
            let expired = if let Ok(mut guard) = session.try_lock() {
                if guard.last_accessed.elapsed() >= SESSION_IDLE_TIMEOUT {
                    guard.closed = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if expired {
                self.close_session(&id).await;
            }
        }
    }
}
