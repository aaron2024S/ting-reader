use crate::core::app::config::Config;
use crate::core::app::error::Result;
use crate::core::storage::local_paths::{
    path_to_display_string, resolve_existing_local_library_root,
};
use crate::core::task_queue::TaskQueue;
use crate::db::repository::{LibraryRepository, LibraryScanState, LibraryScanStateRepository};
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, mpsc};
use tracing::{error, info, warn};

pub struct LibraryWatcher {
    library_repo: Arc<LibraryRepository>,
    scan_state_repo: Arc<LibraryScanStateRepository>,
    task_queue: Arc<TaskQueue>,
    config: Config,
    // Map of library_id -> notify::RecommendedWatcher
    watchers: RwLock<HashMap<String, notify::RecommendedWatcher>>,
    // Map of library_id -> mpsc::Sender for debounce
    debounce_senders: RwLock<HashMap<String, mpsc::Sender<Vec<String>>>>,
}

impl LibraryWatcher {
    pub fn new(
        library_repo: Arc<LibraryRepository>,
        scan_state_repo: Arc<LibraryScanStateRepository>,
        task_queue: Arc<TaskQueue>,
        config: Config,
    ) -> Self {
        Self {
            library_repo,
            scan_state_repo,
            task_queue,
            config,
            watchers: RwLock::new(HashMap::new()),
            debounce_senders: RwLock::new(HashMap::new()),
        }
    }

    pub async fn start_all(&self) -> Result<()> {
        let libraries = self.library_repo.find_all().await?;
        for library in libraries {
            if library.library_type == "local" {
                // Check if watcher is disabled in config
                let scraper_config: crate::db::models::ScraperConfig = library
                    .scraper_config
                    .as_ref()
                    .and_then(|json| serde_json::from_str(json).ok())
                    .unwrap_or_default();

                if !scraper_config.disable_watcher {
                    let full_path =
                        match resolve_existing_local_library_root(&library, &self.config) {
                            Ok(path) => path,
                            Err(e) => {
                                warn!(
                                    "Failed to resolve watcher path for library {}: {}",
                                    library.id, e
                                );
                                continue;
                            }
                        };
                    if let Err(e) = self
                        .watch_library(&library.id, &path_to_display_string(&full_path))
                        .await
                    {
                        warn!("Failed to start watcher for library {}: {}", library.id, e);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn watch_library(&self, library_id: &str, path: &str) -> Result<()> {
        let path_buf = PathBuf::from(path);
        if !path_buf.exists() || !path_buf.is_dir() {
            return Err(crate::core::app::error::TingError::NotFound(format!(
                "Directory not found: {}",
                path
            )));
        }

        let lib_id = library_id.to_string();
        let path_clone = path.to_string();

        let (tx, mut rx) = mpsc::channel::<Vec<String>>(100);

        // Debounce logic task
        let task_queue = self.task_queue.clone();
        let scan_state_repo = self.scan_state_repo.clone();
        let lib_id_clone = lib_id.clone();

        tokio::spawn(async move {
            loop {
                // Wait for an event and collect all affected directories during
                // the debounce window.
                let Some(initial_paths) = rx.recv().await else {
                    break;
                };
                let mut pending_paths = std::collections::HashSet::new();
                if let Err(e) =
                    persist_dirty_paths(&scan_state_repo, &lib_id_clone, &initial_paths).await
                {
                    warn!(
                        "Failed to persist watcher paths for library {}: {}",
                        lib_id_clone, e
                    );
                }
                pending_paths.extend(initial_paths);

                // Debounce: Wait 10 seconds. If more events come, reset timer.
                let timeout = tokio::time::sleep(Duration::from_secs(10));
                tokio::pin!(timeout);
                loop {
                    tokio::select! {
                        _ = &mut timeout => {
                            // Timeout expired, enqueue scan task
                            info!("Library watcher triggered scan for library {}", lib_id_clone);

                            let scan_paths = pending_paths.into_iter().collect();
                            if let Err(e) = task_queue
                                .enqueue_scan_library_scoped(
                                    &lib_id_clone,
                                    &path_clone,
                                    crate::core::library_scanner::ScanMode::Incremental,
                                    Some(scan_paths),
                                )
                                .await
                            {
                                warn!("Failed to enqueue auto-scan task: {}", e);
                            }
                            break;
                        }
                        opt = rx.recv() => {
                            let Some(paths) = opt else {
                                return; // Channel closed
                            };
                            if let Err(e) = persist_dirty_paths(
                                &scan_state_repo,
                                &lib_id_clone,
                                &paths,
                            )
                            .await
                            {
                                warn!(
                                    "Failed to persist watcher paths for library {}: {}",
                                    lib_id_clone, e
                                );
                            }
                            pending_paths.extend(paths);
                            // Reset timer
                            timeout.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
                        }
                    }
                }
            }
        });

        let tx_clone = tx.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        // Only trigger on creations, modifications or deletions
                        match event.kind {
                            EventKind::Create(_)
                            | EventKind::Modify(ModifyKind::Data(_))
                            | EventKind::Modify(ModifyKind::Name(_))
                            | EventKind::Remove(_) => {
                                // Filter out events caused by our own metadata generation
                                let should_ignore = event.paths.iter().any(|p| {
                                    if let Some(file_name) = p.file_name().and_then(|n| n.to_str())
                                    {
                                        file_name == "metadata.json"
                                            || file_name.ends_with(".nfo")
                                            || file_name.starts_with("cover.")
                                            || file_name.starts_with("folder.")
                                    } else {
                                        false
                                    }
                                });

                                if !should_ignore {
                                    let scan_paths: Vec<String> = event
                                        .paths
                                        .iter()
                                        .filter_map(|event_path| {
                                            if event_path.is_dir() {
                                                Some(event_path.to_string_lossy().to_string())
                                            } else {
                                                event_path.parent().map(|parent| {
                                                    parent.to_string_lossy().to_string()
                                                })
                                            }
                                        })
                                        .collect();
                                    if !scan_paths.is_empty() {
                                        let _ = tx_clone.blocking_send(scan_paths);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(e) => error!("Watch error: {:?}", e),
                }
            })
            .map_err(|e| {
                crate::core::app::error::TingError::IoError(std::io::Error::other(e.to_string()))
            })?;

        watcher
            .watch(&path_buf, RecursiveMode::Recursive)
            .map_err(|e| {
                crate::core::app::error::TingError::IoError(std::io::Error::other(e.to_string()))
            })?;

        self.watchers.write().await.insert(lib_id.clone(), watcher);
        self.debounce_senders.write().await.insert(lib_id, tx);

        let mut pending_paths: Vec<String> = self
            .scan_state_repo
            .find_by_library_kind(library_id, "local_dirty")
            .await?
            .into_keys()
            .collect();
        let has_baseline = self
            .scan_state_repo
            .find(library_id, path, "local_baseline")
            .await?
            .is_some();
        if has_baseline {
            // File notification backends cannot report changes made while the
            // process was stopped. Reconcile the root once after watcher
            // startup, then subsequent scans consume only persisted events.
            persist_dirty_paths(&self.scan_state_repo, library_id, &[path.to_string()]).await?;
            pending_paths.push(path.to_string());
        }
        pending_paths.sort();
        pending_paths.dedup();
        if !pending_paths.is_empty() {
            info!(
                library_id = %library_id,
                pending_paths = pending_paths.len(),
                "Recovering persisted local library changes"
            );
            self.task_queue
                .enqueue_scan_library_scoped(
                    library_id,
                    path,
                    crate::core::library_scanner::ScanMode::Incremental,
                    Some(pending_paths),
                )
                .await?;
        }

        info!("Started watching library {} at {:?}", library_id, path_buf);

        Ok(())
    }

    pub async fn stop_watching(&self, library_id: &str) {
        let mut watchers = self.watchers.write().await;
        if watchers.remove(library_id).is_some() {
            info!("Stopped watching library {}", library_id);
        }
        let mut senders = self.debounce_senders.write().await;
        senders.remove(library_id);
    }
}

async fn persist_dirty_paths(
    scan_state_repo: &LibraryScanStateRepository,
    library_id: &str,
    paths: &[String],
) -> Result<()> {
    let generation = uuid::Uuid::new_v4().to_string();
    let states = paths
        .iter()
        .filter(|path| !path.trim().is_empty())
        .map(|path| {
            let mut state = LibraryScanState::new(library_id, path, "local_dirty", &generation);
            state.parent_path = PathBuf::from(path)
                .parent()
                .map(|parent| parent.to_string_lossy().to_string());
            state
        })
        .collect();
    scan_state_repo.upsert_many(states).await
}
