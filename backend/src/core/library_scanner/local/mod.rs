mod chapters;
mod metadata;

use super::{LibraryScanner, LocalLibraryScanContext, MetadataSource, ScanResult, ScanStatus};
use crate::core::app::error::Result;
use crate::core::books::nfo_manager::BookMetadata;
use crate::core::library_scanner::shared::{
    ChapterRangeDir, CoalescedRangeDirs, SeriesDirectoryCandidate, infer_series_directories,
    parse_chapter_range_dir_name, select_mergeable_range_groups,
};
use crate::db::repository::Repository;
use chapters::ChapterProcessingOptions;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

struct LocalBookProcessingContext<'a> {
    library_id: &'a str,
    dir: &'a Path,
    files: &'a [PathBuf],
    last_scanned: Option<chrono::DateTime<chrono::Utc>>,
    task_id: Option<&'a str>,
    scraper_config: &'a crate::db::models::ScraperConfig,
    manual_corrected_patterns: &'a [(String, String)],
    existing_info: Option<(String, i32, Option<String>)>,
    chapter_counts: &'a HashMap<String, crate::db::repository::chapter::ChapterCounts>,
    changed_file_paths: &'a HashSet<PathBuf>,
    fallback_title_override: Option<&'a str>,
    mode: super::ScanMode,
}

#[derive(Clone)]
struct LocalFileSnapshot {
    fingerprint: String,
    modified_at: Option<String>,
    size: Option<i64>,
}

impl LibraryScanner {
    /// Scan a local library
    pub(crate) async fn scan_local_library(
        &self,
        context: LocalLibraryScanContext<'_>,
    ) -> Result<ScanResult> {
        let LocalLibraryScanContext {
            library_id,
            path,
            task_id,
            last_scanned,
            scraper_config,
            mode,
            scan_paths,
        } = context;
        let mut scan_result = ScanResult {
            start_time: Some(std::time::Instant::now()),
            ..Default::default()
        };

        self.update_progress_key(task_id, "scan.local.scanning", serde_json::json!({}))
            .await;

        // Get all supported extensions dynamically
        let supported_extensions = self.get_supported_extensions().await;
        let cached_file_states = self
            .scan_state_repo
            .find_by_library_kind(library_id, "local_file")
            .await
            .unwrap_or_default();

        // 1. Recursively find all audio files and group them by directory
        let mut dir_groups: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut file_snapshots: HashMap<PathBuf, LocalFileSnapshot> = HashMap::new();
        let mut sidecar_fingerprints: HashMap<PathBuf, Vec<String>> = HashMap::new();
        let mut seen_file_paths = HashSet::new();
        let mut changed_file_paths = HashSet::new();
        let mut changed_file_dirs = HashSet::new();
        let mut changed_parent_dirs = HashSet::new();
        let mut deleted_file_paths = Vec::new();

        let mut walk_errors = 0usize;
        let walk_roots: Vec<PathBuf> = collapse_local_scan_roots(match scan_paths {
            Some(paths) if !paths.is_empty() && !mode.is_full() => paths
                .iter()
                .filter(|scan_path| scan_path.starts_with(path))
                .cloned()
                .collect(),
            _ => vec![path.to_path_buf()],
        });

        for walk_root in &walk_roots {
            // A scoped watcher path can legitimately disappear after a delete
            // or rename. Treat that as an empty subtree so the snapshot diff
            // removes its stale files/books; the main library root still uses
            // the ordinary error path to avoid mass deletion on an unavailable
            // mount.
            if !mode.is_full() && scan_paths.is_some() && !walk_root.exists() {
                continue;
            }
            for entry in WalkDir::new(walk_root).follow_links(true).into_iter() {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(e) => {
                        walk_errors += 1;
                        let error_path = e
                            .path()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        warn!(
                            path = %error_path,
                            error = %e,
                            "Failed to read local library path during scan"
                        );
                        if scan_result.errors.len() < 20 {
                            scan_result
                                .errors
                                .push(format!("Failed to read {}: {}", error_path, e));
                        }
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }

                let entry_path = entry.path();
                let ext = entry_path
                    .extension()
                    .map(|ext| ext.to_string_lossy().to_lowercase());
                let is_audio = ext
                    .as_ref()
                    .is_some_and(|ext| supported_extensions.contains(ext));
                let is_sidecar = is_local_scan_sidecar(entry_path);
                if !is_audio && !is_sidecar {
                    continue;
                }

                let path_buf = entry_path.to_path_buf();
                let snapshot = local_file_snapshot(&entry);
                let path_string = path_buf.to_string_lossy().to_string();
                seen_file_paths.insert(path_string.clone());
                if cached_file_states
                    .get(&path_string)
                    .map(|state| state.fingerprint != snapshot.fingerprint)
                    .unwrap_or(true)
                {
                    changed_file_paths.insert(path_buf.clone());
                    mark_changed_local_directory(
                        entry_path.parent(),
                        &mut changed_file_dirs,
                        &mut changed_parent_dirs,
                    );
                }
                if is_audio && let Some(parent) = entry_path.parent() {
                    dir_groups
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(path_buf.clone());
                }
                if is_sidecar && let Some(parent) = entry_path.parent() {
                    sidecar_fingerprints
                        .entry(parent.to_path_buf())
                        .or_default()
                        .push(snapshot.fingerprint.clone());
                }
                file_snapshots.insert(path_buf, snapshot);
            }
        }
        for state in cached_file_states.values() {
            let state_path = Path::new(&state.entry_path);
            let state_in_scope = mode.is_full()
                || walk_roots
                    .iter()
                    .any(|walk_root| state_path.starts_with(walk_root));
            if state_in_scope && !seen_file_paths.contains(&state.entry_path) {
                deleted_file_paths.push(state.entry_path.clone());
                let parent = state
                    .parent_path
                    .as_deref()
                    .map(Path::new)
                    .or_else(|| state_path.parent());
                mark_changed_local_directory(
                    parent,
                    &mut changed_file_dirs,
                    &mut changed_parent_dirs,
                );
            }
        }
        scan_result.failed_count += walk_errors;

        self.update_progress_key(
            task_id,
            "scan.audio_dirs.found",
            serde_json::json!({ "count": dir_groups.len() }),
        )
        .await;

        // 2. Process each directory group as a book
        let (dir_groups, coalesced_range_dirs) =
            coalesce_local_range_directory_groups(path, dir_groups);
        let live_book_state_paths: HashSet<String> = dir_groups
            .keys()
            .map(|dir| dir.to_string_lossy().to_string())
            .collect();
        let inferred_series = infer_local_series_directories(path, dir_groups.keys());
        let changed_series_keys: HashSet<(PathBuf, String)> = inferred_series
            .iter()
            .filter(|(dir, _)| {
                local_group_is_changed(
                    dir,
                    coalesced_range_dirs.get(*dir),
                    &changed_file_dirs,
                    &changed_parent_dirs,
                )
            })
            .filter_map(|(dir, series)| Some((dir.parent()?.to_path_buf(), series.title.clone())))
            .collect();
        let affected_series_dirs: HashSet<PathBuf> = inferred_series
            .iter()
            .filter(|(dir, series)| {
                dir.parent().is_some_and(|parent| {
                    changed_series_keys.contains(&(parent.to_path_buf(), series.title.clone()))
                })
            })
            .map(|(dir, _)| dir.clone())
            .collect();
        let total_groups = dir_groups.len();
        let mut processed_count = 0;

        // Pre-fetch all books (minimal) for the library to handle deletions and fast lookup
        // Returns: (id, path, hash, manual_corrected, match_pattern)
        let all_books_minimal = self
            .book_repo
            .find_all_minimal_by_library(library_id)
            .await
            .unwrap_or_default();
        let chapter_counts = if mode.is_full() {
            HashMap::new()
        } else {
            self.chapter_repo
                .count_by_library(library_id)
                .await
                .unwrap_or_default()
        };
        let scan_config_fingerprint = scraper_config_fingerprint(scraper_config);
        let cached_states = self
            .scan_state_repo
            .find_by_library_kind(library_id, "book")
            .await
            .unwrap_or_default();

        // Build lookup maps
        // Map: Path -> (id, manual_corrected, match_pattern)
        let mut book_path_map: HashMap<PathBuf, (String, i32, Option<String>)> = HashMap::new();
        let mut book_hash_map: HashMap<String, (String, i32, Option<String>)> = HashMap::new();

        for (id, path, hash, manual_corrected, match_pattern) in &all_books_minimal {
            book_path_map.insert(
                PathBuf::from(path),
                (id.clone(), *manual_corrected, match_pattern.clone()),
            );
            book_hash_map.insert(
                hash.clone(),
                (id.clone(), *manual_corrected, match_pattern.clone()),
            );
        }

        let manual_corrected_patterns: Vec<(String, String)> = all_books_minimal
            .iter()
            .filter(|(_, _, _, mc, mp)| *mc == 1 && mp.is_some())
            .map(|(id, _, _, _, mp)| (id.clone(), mp.clone().unwrap()))
            .collect();

        let mut found_book_ids: HashSet<String> = HashSet::new();
        let mut absorbed_range_book_ids: HashMap<String, String> = HashMap::new();
        let mut pending_states = Vec::new();
        let pending_file_states: Vec<crate::db::repository::LibraryScanState> = file_snapshots
            .iter()
            .filter(|(file_path, _)| changed_file_paths.contains(*file_path))
            .map(|(file_path, snapshot)| {
                let mut state = crate::db::repository::LibraryScanState::new(
                    library_id,
                    file_path.to_string_lossy(),
                    "local_file",
                    &snapshot.fingerprint,
                );
                state.modified_at = snapshot.modified_at.clone();
                state.size = snapshot.size;
                state.parent_path = file_path
                    .parent()
                    .map(|parent| parent.to_string_lossy().to_string());
                state
            })
            .collect();

        for (dir, mut files) in dir_groups {
            processed_count += 1;
            // Progress/cancellation updates are database operations. Keep them
            // periodic so a large scan does not turn into one DB round-trip per
            // directory while still remaining responsive to cancellation.
            let report_progress = processed_count == 1
                || processed_count % 16 == 0
                || processed_count == total_groups;
            if report_progress {
                self.check_cancellation(task_id).await?;
            }
            let dir_name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown");

            if report_progress {
                self.update_progress_key(
                    task_id,
                    "scan.item.processing",
                    serde_json::json!({
                        "current": processed_count,
                        "total": total_groups,
                        "name": dir_name,
                    }),
                )
                .await;
            }

            // Optimization: Find existing book to avoid DB lookup
            let mut existing_info = book_path_map.get(&dir).cloned();

            // If not found by path, try hash (for moved books)
            if existing_info.is_none() {
                let book_hash = self.generate_book_hash(&dir);
                existing_info = book_hash_map.get(&book_hash).cloned();
            }

            if existing_info.is_none()
                && let Some(child_dirs) = coalesced_range_dirs.get(&dir)
            {
                for child_dir in &child_dirs.child_dirs {
                    if let Some(info) = book_path_map.get(child_dir).cloned() {
                        existing_info = Some(info);
                        break;
                    }
                }
            }

            let dir_path_string = dir.to_string_lossy().to_string();
            let files_changed = local_group_is_changed(
                &dir,
                coalesced_range_dirs.get(&dir),
                &changed_file_dirs,
                &changed_parent_dirs,
            );
            let existing_lock_state = existing_info
                .as_ref()
                .map(|(_, manual_corrected, _)| *manual_corrected)
                .unwrap_or(0);
            let state_config_fingerprint =
                format!("{}:lock={}", scan_config_fingerprint, existing_lock_state);
            if let Some((book_id, _, _)) = existing_info.as_ref()
                && chapter_counts
                    .get(book_id)
                    .is_some_and(|counts| counts.missing_duration == 0)
                && cached_states.get(&dir_path_string).is_some_and(|state| {
                    !files_changed
                        && state.config_fingerprint.as_deref()
                            == Some(state_config_fingerprint.as_str())
                })
            {
                scan_result.total_books += 1;
                scan_result.books_skipped += 1;
                found_book_ids.insert(book_id.clone());
                if affected_series_dirs.contains(&dir)
                    && let Some(series_info) = inferred_series.get(&dir)
                    && let Err(e) = self
                        .link_book_to_inferred_series(library_id, book_id, series_info)
                        .await
                {
                    warn!(
                        path = ?dir,
                        book_id = %book_id,
                        error = %e,
                        "Failed to refresh affected inferred series member"
                    );
                }
                continue;
            }

            // Only directories in the changed set pay the natural-sort and
            // aggregate-fingerprint cost. The discovery pass itself stays O(N).
            files.sort_by(|a, b| {
                natord::compare(a.to_string_lossy().as_ref(), b.to_string_lossy().as_ref())
            });
            let dir_fingerprint = cached_states
                .get(&dir_path_string)
                .filter(|_| !files_changed)
                .map(|state| state.fingerprint.clone())
                .unwrap_or_else(|| {
                    local_directory_fingerprint(
                        &dir,
                        &files,
                        &file_snapshots,
                        &sidecar_fingerprints,
                    )
                });

            match self
                .process_book_directory(LocalBookProcessingContext {
                    library_id,
                    dir: &dir,
                    files: &files,
                    last_scanned,
                    task_id,
                    scraper_config,
                    manual_corrected_patterns: &manual_corrected_patterns,
                    existing_info,
                    chapter_counts: &chapter_counts,
                    changed_file_paths: &changed_file_paths,
                    fallback_title_override: coalesced_range_dirs
                        .get(&dir)
                        .and_then(|range_dirs| range_dirs.title_override.as_deref()),
                    mode,
                })
                .await
            {
                Ok((book_id, status)) => {
                    scan_result.total_books += 1;
                    match status {
                        ScanStatus::Created => scan_result.books_created += 1,
                        ScanStatus::Updated => scan_result.books_updated += 1,
                        ScanStatus::Skipped => scan_result.books_skipped += 1,
                    }
                    if status != ScanStatus::Skipped {
                        scan_result.changed_book_ids.insert(book_id.clone());
                    }
                    found_book_ids.insert(book_id.clone());
                    let mut state = crate::db::repository::LibraryScanState::new(
                        library_id,
                        dir_path_string,
                        "book",
                        dir_fingerprint,
                    );
                    state.config_fingerprint = Some(state_config_fingerprint);
                    pending_states.push(state);
                    if status != ScanStatus::Skipped
                        && let Some(series_info) = inferred_series.get(&dir)
                        && let Err(e) = self
                            .link_book_to_inferred_series(library_id, &book_id, series_info)
                            .await
                    {
                        warn!(
                            path = ?dir,
                            book_id = %book_id,
                            error = %e,
                            "Failed to link book to inferred series"
                        );
                    }
                    if status != ScanStatus::Skipped
                        && let Some(child_dirs) = coalesced_range_dirs.get(&dir)
                    {
                        for child_dir in &child_dirs.child_dirs {
                            if let Some((child_book_id, manual_corrected, _)) =
                                book_path_map.get(child_dir)
                                && child_book_id != &book_id
                                && *manual_corrected == 0
                            {
                                absorbed_range_book_ids
                                    .insert(child_book_id.clone(), book_id.clone());
                            }
                        }
                    }
                    debug!(book_id = %book_id, path = ?dir, status = ?status, "Processed book directory");
                }
                Err(e) => {
                    scan_result.failed_count += 1;
                    warn!(path = ?dir, error = %e, "Failed to process book directory");
                    scan_result
                        .errors
                        .push(format!("Failed to process {}: {}", dir.display(), e));
                }
            }

            if processed_count % 25 == 0 {
                self.plugin_manager.garbage_collect_all().await;
            }
        }

        let mut deleted_book_state_paths = Vec::new();
        for (source_id, target_id) in absorbed_range_book_ids {
            if found_book_ids.contains(&source_id) {
                continue;
            }

            if let Some(merge_service) = &self.merge_service {
                if let Err(e) = merge_service
                    .absorb_scanned_book(&target_id, &source_id)
                    .await
                {
                    warn!(
                        "Failed to absorb range-segment book {} into {}: {}",
                        source_id, target_id, e
                    );
                } else {
                    if let Some((_, source_path, _, _, _)) = all_books_minimal
                        .iter()
                        .find(|(id, _, _, _, _)| id == &source_id)
                    {
                        deleted_book_state_paths.push(source_path.clone());
                    }
                    scan_result.books_deleted += 1;
                }
            } else {
                info!(
                    "Deleting range-segment book record after merging into parent book: {}",
                    source_id
                );
                if let Err(e) = self.book_repo.delete(&source_id).await {
                    warn!(
                        "Failed to delete absorbed range-segment book {}: {}",
                        source_id, e
                    );
                } else {
                    if let Some((_, source_path, _, _, _)) = all_books_minimal
                        .iter()
                        .find(|(id, _, _, _, _)| id == &source_id)
                    {
                        deleted_book_state_paths.push(source_path.clone());
                    }
                    scan_result.books_deleted += 1;
                    if let Err(e) = self.chapter_repo.delete_by_book(&source_id).await {
                        warn!(
                            "Failed to delete chapters for absorbed range-segment book {}: {}",
                            source_id, e
                        );
                    }
                }
            }
        }

        // 3. Handle deletions. A scoped watcher scan is allowed to remove
        // missing books only inside the affected directories; an ordinary
        // incremental scan still avoids deletion because it did not inspect
        // the complete library.
        let complete_library_listing =
            mode.is_full() || scan_paths.is_none_or(|paths| paths.is_empty());
        if walk_errors == 0
            && (complete_library_listing || scan_paths.is_some_and(|paths| !paths.is_empty()))
        {
            for (id, path_str, _, _, _) in all_books_minimal {
                if !found_book_ids.contains(&id) {
                    let path = Path::new(&path_str);
                    let in_scope = complete_library_listing
                        || scan_paths
                            .map(|paths| paths.iter().any(|root| path.starts_with(root)))
                            .unwrap_or(false);
                    if in_scope {
                        deleted_book_state_paths.push(path_str.clone());
                        info!("Book path missing, deleting record: {}", path_str);
                        if let Err(e) = self.book_repo.delete(&id).await {
                            warn!("Failed to delete missing book {}: {}", id, e);
                        } else {
                            scan_result.books_deleted += 1;
                            if let Err(e) = self.chapter_repo.delete_by_book(&id).await {
                                warn!("Failed to delete chapters for missing book {}: {}", id, e);
                            }
                        }
                    }
                }
            }
        }

        if mode.is_full() && walk_errors == 0 {
            deleted_book_state_paths.extend(
                cached_states
                    .keys()
                    .filter(|entry_path| !live_book_state_paths.contains(*entry_path))
                    .cloned(),
            );
        }
        if walk_errors == 0 {
            self.scan_state_repo
                .delete_many(library_id, "book", deleted_book_state_paths)
                .await?;
            self.scan_state_repo
                .delete_many(library_id, "local_file", deleted_file_paths)
                .await?;
        }
        self.scan_state_repo.upsert_many(pending_states).await?;
        self.scan_state_repo
            .upsert_many(pending_file_states)
            .await?;
        if complete_library_listing && walk_errors == 0 {
            let baseline = crate::db::repository::LibraryScanState::new(
                library_id,
                path.to_string_lossy(),
                "local_baseline",
                "complete",
            );
            self.scan_state_repo.upsert_many(vec![baseline]).await?;
        }

        // Final garbage collection after scan
        self.plugin_manager.garbage_collect_all().await;

        Ok(scan_result)
    }

    /// Process a directory containing audio files as a book
    async fn process_book_directory(
        &self,
        context: LocalBookProcessingContext<'_>,
    ) -> Result<(String, ScanStatus)> {
        let LocalBookProcessingContext {
            library_id,
            dir,
            files,
            last_scanned,
            task_id,
            scraper_config,
            manual_corrected_patterns,
            existing_info,
            chapter_counts,
            changed_file_paths,
            fallback_title_override,
            mode,
        } = context;
        // Log scraper config for debugging
        debug!(
            "Processing book dir: {:?}, nfo_enabled: {}, json_enabled: {}",
            dir, scraper_config.nfo_writing_enabled, scraper_config.metadata_writing_enabled
        );

        // 0. Check New Chapter Protection only for a directory that is not
        // already mapped to a known book. This avoids testing every lock regex
        // against every existing directory during an incremental scan.
        if existing_info.is_none() {
            for (book_id, pattern) in manual_corrected_patterns {
                if !pattern.is_empty() {
                    let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if let Ok(re) = regex::Regex::new(pattern)
                        && re.is_match(dir_name)
                    {
                        info!(
                            "New Chapter Protection: Merging {} into existing book {}",
                            dir_name, book_id
                        );
                        let has_changes = self
                            .process_chapters(
                                book_id,
                                files,
                                ChapterProcessingOptions {
                                    last_scanned,
                                    task_id,
                                    use_filename_as_title: scraper_config.use_filename_as_title,
                                    extract_extra_chapters: scraper_config.extract_extra_chapters,
                                    cloud_mode: scraper_config.cloud_mode,
                                    changed_file_paths: Some(changed_file_paths),
                                    json_chapters: None,
                                    chapter_title_template: None,
                                    chapter_title_overrides: None,
                                },
                            )
                            .await?;
                        return Ok((
                            book_id.clone(),
                            if has_changes {
                                ScanStatus::Updated
                            } else {
                                ScanStatus::Skipped
                            },
                        ));
                    }
                }
            }
        }

        // 1. Check if Book Exists
        let mut existing_book_id = None;
        let mut is_manual_corrected = false;

        let book_hash = self.generate_book_hash(dir);

        if let Some((id, mc, _)) = existing_info {
            existing_book_id = Some(id);
            is_manual_corrected = mc == 1;
        } else if let Ok(Some(book)) = self.book_repo.find_by_hash(&book_hash).await {
            existing_book_id = Some(book.id.clone());
            is_manual_corrected = book.manual_corrected == 1;
        }

        // 2. Incremental fast path. Audio and sidecar mtimes are checked once,
        // and an unchanged directory returns before chapter reads or scraper I/O.
        let max_mtime = files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()))
            .chain(
                [dir.join("metadata.json"), dir.join("book.nfo")]
                    .iter()
                    .filter_map(|path| {
                        std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
                    }),
            )
            .max();
        let max_mtime_utc = max_mtime.map(chrono::DateTime::<chrono::Utc>::from);

        // A locked book is intentionally never auto-repaired by writing sidecars.
        // Do not defeat the incremental fast path just because a locked book
        // lacks a file that the current write setting would otherwise create.
        let required_sidecar_missing = !is_manual_corrected
            && ((scraper_config.metadata_writing_enabled && !dir.join("metadata.json").exists())
                || (scraper_config.nfo_writing_enabled && !dir.join("book.nfo").exists()));
        let mut skip_metadata_update = false;
        if !mode.is_full() {
            if let (Some(last_scan), Some(max_mt)) = (last_scanned, max_mtime_utc)
                && max_mt <= last_scan
                && !required_sidecar_missing
                && let Some(book_id) = existing_book_id.as_deref()
            {
                skip_metadata_update = chapter_counts
                    .get(book_id)
                    .map(|counts| counts.total == files.len() && counts.missing_duration == 0)
                    .unwrap_or(false);
            }

            if let Some(book_id) = existing_book_id.clone().filter(|_| skip_metadata_update) {
                return Ok((book_id, ScanStatus::Skipped));
            }
        }

        // 3. Extract Metadata
        let (scanned_meta, source) = self
            .extract_final_metadata(dir, files, scraper_config, fallback_title_override)
            .await;

        let mut title = scanned_meta.title.unwrap_or_else(|| {
            fallback_title_override
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("Unknown Book")
                .to_string()
        });
        if let Some(fallback_title) = fallback_title_override
            && !fallback_title.trim().is_empty()
            && source == MetadataSource::Fallback
        {
            title = fallback_title.to_string();
        }
        let mut author = scanned_meta.author;
        let mut narrator = scanned_meta.narrator;
        let mut description = scanned_meta.description;
        let mut tags = scanned_meta.tags;
        let mut genre = scanned_meta.genre;
        let mut cover_url = scanned_meta.cover_url;

        // Extended fields
        let subtitle = scanned_meta.subtitle;
        let mut published_year = scanned_meta.published_year;
        let published_date = scanned_meta.published_date;
        let publisher = scanned_meta.publisher;
        let isbn = scanned_meta.isbn;
        let asin = scanned_meta.asin;
        let language = scanned_meta.language;
        let explicit = scanned_meta.explicit;
        let abridged = scanned_meta.abridged;
        let json_tags = scanned_meta.json_tags;
        let json_series = scanned_meta.json_series;
        let json_chapters = scanned_meta.json_chapters;
        let chapter_title_template = scanned_meta.chapter_title_template;
        let chapter_titles = scanned_meta.chapter_titles;

        if author.is_none() {
            author = Some("Unknown".to_string());
        }

        // 3. Apply Manual Correction or Existing Data
        if is_manual_corrected
            && let Some(id) = &existing_book_id
            && let Ok(Some(book)) = self.book_repo.find_by_id(id).await
        {
            // A metadata lock owns the complete editable metadata shape,
            // including intentionally empty fields. Preserve those values
            // instead of filling only missing fields from a later scan.
            title = book.title.unwrap_or(title);
            author = book.author;
            narrator = book.narrator;
            description = book.description;
            tags = book.tags;
            genre = book.genre;
            cover_url = book.cover_url;
            // `year` is the persisted form of published_year in the
            // current book model, so preserve an explicit empty value.
            published_year = book.year.map(|year| year.to_string());
            // theme_color will be recalculated if cover_url changed later
        }

        // Theme Color
        let mut theme_color = None;
        if let Some(ref url) = cover_url {
            let cover_path = if url.starts_with("http") || url.starts_with("//") {
                url.clone()
            } else {
                let p = Path::new(url);
                if p.exists() {
                    url.clone()
                } else {
                    dir.join(url).to_string_lossy().to_string()
                }
            };

            // For local paths, we need to handle Windows UNC paths carefully
            let normalized_path =
                if !cover_path.starts_with("http") && !cover_path.starts_with("//") {
                    let p = Path::new(&cover_path);
                    // First try to canonicalize to resolve relative paths
                    let mut path_str = p
                        .canonicalize()
                        .unwrap_or_else(|_| p.to_path_buf())
                        .to_string_lossy()
                        .to_string();

                    // Then strip Windows UNC prefix if present, and normalize slashes
                    if path_str.starts_with("\\\\?\\") || path_str.starts_with("//?/") {
                        path_str = path_str[4..].to_string();
                    }
                    path_str.replace('\\', "/")
                } else {
                    cover_path
                };

            if let Ok(Some(color)) = crate::core::books::color::calculate_theme_color_with_client(
                &normalized_path,
                &self.http_client,
            )
            .await
            {
                theme_color = Some(color);
            }
        }

        // 4. Create/Update Book
        let book_id = existing_book_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        let mut book = crate::db::models::Book {
            id: book_id.clone(),
            library_id: library_id.to_string(),
            title: Some(title.clone()),
            author: author.clone(),
            narrator: narrator.clone(),
            cover_url: cover_url.clone(),
            description: description.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            path: dir.to_string_lossy().to_string(),
            hash: book_hash.clone(),
            theme_color: theme_color.clone(),
            skip_intro: 0,
            skip_outro: 0,
            tags: tags.clone(),
            genre: genre.clone(),
            year: published_year.as_ref().and_then(|y| y.parse::<i32>().ok()),
            manual_corrected: if is_manual_corrected { 1 } else { 0 },
            match_pattern: None,
            chapter_regex: None,
        };

        let status = if let Ok(Some(existing)) = self.book_repo.find_by_id(&book_id).await {
            // Chapter parsing rules are user-owned too; never drop a saved rule
            // merely because a scan source did not provide one.
            if book.chapter_regex.is_none() && existing.chapter_regex.is_some() {
                book.chapter_regex = existing.chapter_regex.clone();
            }
            if existing.manual_corrected == 0 {
                self.book_repo.update(&book).await?;
                ScanStatus::Updated
            } else {
                ScanStatus::Skipped
            }
        } else {
            self.book_repo.create(&book).await?;
            ScanStatus::Created
        };

        // 5. Process Chapters
        let chapters_changed = self
            .process_chapters(
                &book_id,
                files,
                ChapterProcessingOptions {
                    last_scanned,
                    task_id,
                    use_filename_as_title: scraper_config.use_filename_as_title,
                    extract_extra_chapters: scraper_config.extract_extra_chapters,
                    cloud_mode: scraper_config.cloud_mode,
                    changed_file_paths: Some(changed_file_paths),
                    json_chapters,
                    chapter_title_template: chapter_title_template.as_deref(),
                    chapter_title_overrides: if chapter_titles.is_empty() {
                        None
                    } else {
                        Some(chapter_titles.as_slice())
                    },
                },
            )
            .await?;

        // 5.1 Process series declared by metadata.json. A single-book series is
        // valid metadata; directory-based inference is handled separately and
        // only activates when multiple matching volume directories exist.
        // A locked book owns its manually selected series metadata as well.
        // New chapter discovery is still allowed, but automatic scans must not
        // create or relink series for that book.
        if !is_manual_corrected && !json_series.is_empty() {
            for series_title_raw in json_series {
                let series_title_raw = series_title_raw.trim();
                if series_title_raw.is_empty() {
                    continue;
                }

                // Parse series title and optional sequence number
                let mut series_title = series_title_raw.to_string();
                let mut explicit_order = None;

                if let Some(idx) = series_title_raw.rfind(" #") {
                    let (name_part, num_part) = series_title_raw.split_at(idx);
                    let num_str = num_part[2..].trim();
                    if let Ok(order) = num_str.parse::<i32>() {
                        series_title = name_part.trim().to_string();
                        explicit_order = Some(order);
                    }
                }

                tracing::debug!(
                    book_id = %book_id,
                    library_id = %library_id,
                    series_title = %series_title,
                    "Linking book to series declared in metadata.json"
                );

                // Find or create the series within this library.
                let new_series = crate::db::models::Series {
                    id: Uuid::new_v4().to_string(),
                    library_id: library_id.to_string(),
                    title: series_title.clone(),
                    author: author.clone(), // Initial author from first found book
                    narrator: narrator.clone(),
                    cover_url: cover_url.clone(),
                    description: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                };
                let series = self.series_repo.find_or_create_by_title(new_series).await?;

                // Link book to series if not already linked
                let books = self.series_repo.find_books_by_series(&series.id).await?;
                if let Some((_, current_order)) = books.iter().find(|(b, _)| b.id == book_id) {
                    // Already linked, update order if explicit order changed
                    if let Some(o) = explicit_order
                        && *current_order != o
                    {
                        self.series_repo
                            .add_book(crate::db::models::SeriesBook {
                                series_id: series.id.clone(),
                                book_id: book_id.clone(),
                                book_order: o,
                            })
                            .await?;
                    }
                } else {
                    // Not linked, insert it
                    let order = if let Some(o) = explicit_order {
                        o
                    } else {
                        books.len() as i32 + 1
                    };

                    self.series_repo
                        .add_book(crate::db::models::SeriesBook {
                            series_id: series.id.clone(),
                            book_id: book_id.clone(),
                            book_order: order,
                        })
                        .await?;

                    // If no explicit order, resort all books in series by natural order of title
                    if explicit_order.is_none() {
                        let mut all_books =
                            self.series_repo.find_books_by_series(&series.id).await?;
                        all_books.sort_by(|a, b| {
                            let t1 = a.0.title.as_deref().unwrap_or("");
                            let t2 = b.0.title.as_deref().unwrap_or("");
                            natord::compare(t1, t2)
                        });

                        let new_orders: Vec<(String, i32)> = all_books
                            .into_iter()
                            .enumerate()
                            .map(|(i, (b, _))| (b.id, (i + 1) as i32))
                            .collect();

                        self.series_repo
                            .update_book_orders(&series.id, new_orders)
                            .await?;
                    }

                    // DO NOT update series metadata based on subsequent books to avoid instability
                    // Series metadata should only be set on creation or manual update
                }
            }
        }

        // 6. Write NFO/Metadata
        // A manually locked book is protected from automatic sidecar rewrites.
        // Chapter discovery can still run, but metadata files must remain owned
        // by the user until the lock is released.
        if !is_manual_corrected && scraper_config.nfo_writing_enabled {
            debug!("Writing NFO for book: {}", book_id);
            if let Ok(Some(book)) = self.book_repo.find_by_id(&book_id).await {
                let mut metadata = BookMetadata::new(
                    book.title.clone().unwrap_or_default(),
                    "ting-reader".to_string(),
                    book.id.clone(),
                    0,
                );
                metadata.author = book.author.clone();
                metadata.narrator = book.narrator.clone();
                metadata.intro = book.description.clone();
                metadata.cover_url = book.cover_url.clone();
                if let Some(tags_str) = &book.tags {
                    metadata.tags.items = tags_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                if let Err(e) = self
                    .nfo_manager
                    .write_book_nfo_to_dir(Path::new(&book.path), &metadata)
                {
                    warn!("Failed to write NFO: {}", e);
                } else {
                    info!("Successfully wrote NFO to: {}", book.path);
                }
            }
        }

        if !is_manual_corrected && scraper_config.metadata_writing_enabled {
            debug!("Writing metadata.json for book: {}", book_id);
            let chapters = self.chapter_repo.find_by_book(&book_id).await?;
            let abs_chapters =
                crate::core::books::metadata_writer::build_audiobookshelf_chapters(chapters);
            let extended_meta = crate::core::books::metadata_writer::ExtendedMetadata {
                subtitle,
                published_year,
                published_date,
                publisher,
                isbn,
                asin,
                language,
                explicit,
                abridged,
                tags: json_tags,
            };

            // Get series for this book
            let series_list = self
                .series_repo
                .find_series_by_book(&book_id)
                .await
                .unwrap_or_default();
            let mut series_titles = Vec::new();
            for series in series_list {
                let formatted_title =
                    if let Ok(books) = self.series_repo.find_books_by_series(&series.id).await {
                        if let Some((_, order)) = books.iter().find(|(b, _)| b.id == book_id) {
                            format!("{} #{}", series.title, order)
                        } else {
                            series.title.clone()
                        }
                    } else {
                        series.title.clone()
                    };

                // Prevent duplicates
                if !series_titles.contains(&formatted_title) {
                    series_titles.push(formatted_title);
                }
            }

            let metadata_json = crate::core::books::metadata_writer::AudiobookshelfMetadata::new(
                &book,
                abs_chapters,
                extended_meta,
                series_titles,
            );
            if let Err(e) =
                crate::core::books::metadata_writer::write_metadata_json(dir, &metadata_json)
            {
                warn!(
                    target: "audit::metadata",
                    path = %dir.display(),
                    error = %e,
                    message_key = "metadata.json.write_failed",
                    message_params = %serde_json::json!({
                        "path": dir.display().to_string(),
                        "error": e.to_string(),
                    }),
                    "Failed to write metadata.json"
                );
            } else {
                debug!("Successfully wrote metadata.json to: {:?}", dir);
            }
        }

        let final_status = match status {
            ScanStatus::Created => ScanStatus::Created,
            _ => {
                if chapters_changed {
                    ScanStatus::Updated
                } else {
                    status
                }
            }
        };

        Ok((book_id, final_status))
    }

    pub(super) fn find_cover_image(&self, dir: &Path) -> Option<String> {
        let cover_names = [
            "cover.jpg",
            "cover.png",
            "cover.jpeg",
            "folder.jpg",
            "folder.png",
        ];
        for name in cover_names {
            let path = dir.join(name);
            if path.exists() {
                // Return path with forward slashes for better JSON/URL compatibility
                return Some(path.to_string_lossy().replace('\\', "/"));
            }
        }
        if let Ok(mut entries) = std::fs::read_dir(dir) {
            while let Some(Ok(entry)) = entries.next() {
                let path = entry.path();
                if path.is_file()
                    && let Some(ext) = path.extension()
                {
                    let ext_str = ext.to_string_lossy().to_lowercase();
                    if ["jpg", "jpeg", "png", "webp"].contains(&ext_str.as_str()) {
                        // Return path with forward slashes for better JSON/URL compatibility
                        return Some(path.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        None
    }

    fn generate_book_hash(&self, audiobook_dir: &Path) -> String {
        let path_str = audiobook_dir.to_string_lossy();
        let mut hasher = Sha256::new();
        hasher.update(path_str.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

fn local_directory_fingerprint(
    dir: &Path,
    files: &[PathBuf],
    file_snapshots: &HashMap<PathBuf, LocalFileSnapshot>,
    sidecar_fingerprints: &HashMap<PathBuf, Vec<String>>,
) -> String {
    let mut hasher = Sha256::new();
    let mut entries: Vec<String> = files
        .iter()
        .filter_map(|path| file_snapshots.get(path))
        .map(|snapshot| snapshot.fingerprint.clone())
        .collect();
    entries.extend(sidecar_fingerprints.get(dir).into_iter().flatten().cloned());
    entries.sort();
    for entry in entries {
        hasher.update(entry.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

fn collapse_local_scan_roots(mut roots: Vec<PathBuf>) -> Vec<PathBuf> {
    roots.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });

    let mut collapsed = Vec::with_capacity(roots.len());
    for root in roots {
        if collapsed
            .iter()
            .any(|existing: &PathBuf| root.starts_with(existing))
        {
            continue;
        }
        collapsed.push(root);
    }
    collapsed
}

fn local_file_snapshot(entry: &walkdir::DirEntry) -> LocalFileSnapshot {
    let metadata = entry.metadata().ok();
    let size = metadata
        .as_ref()
        .map(|metadata| metadata.len() as i64)
        .unwrap_or_default();
    let modified_nanos = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    LocalFileSnapshot {
        fingerprint: format!(
            "{}:{}:{}",
            entry.path().to_string_lossy(),
            size,
            modified_nanos
        ),
        modified_at: if modified_nanos == 0 {
            None
        } else {
            Some(modified_nanos.to_string())
        },
        size: Some(size),
    }
}

fn is_local_scan_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name == "metadata.json"
        || name == "book.nfo"
        || name.starts_with("cover.")
        || name.starts_with("folder.")
}

/// Mark the directory containing a changed file and all of its ancestors.
///
/// The containing directory is the unit that needs book reconciliation. The
/// ancestor set is kept separately so a coalesced range/series directory can
/// cheaply detect that one of its children changed without marking siblings.
fn mark_changed_local_directory(
    parent: Option<&Path>,
    changed_dirs: &mut HashSet<PathBuf>,
    changed_ancestors: &mut HashSet<PathBuf>,
) {
    let Some(parent) = parent else {
        return;
    };

    changed_dirs.insert(parent.to_path_buf());
    let mut current = parent;
    while let Some(ancestor) = current.parent() {
        if !changed_ancestors.insert(ancestor.to_path_buf()) {
            break;
        }
        current = ancestor;
    }
}

fn local_group_is_changed(
    dir: &Path,
    coalesced: Option<&CoalescedRangeDirs<PathBuf>>,
    changed_dirs: &HashSet<PathBuf>,
    changed_ancestors: &HashSet<PathBuf>,
) -> bool {
    if changed_dirs.contains(dir) || changed_ancestors.contains(dir) {
        return true;
    }

    coalesced.is_some_and(|range_dirs| {
        range_dirs.child_dirs.iter().any(|child_dir| {
            changed_dirs.contains(child_dir) || changed_ancestors.contains(child_dir)
        })
    })
}

fn scraper_config_fingerprint(config: &crate::db::models::ScraperConfig) -> String {
    let serialized = serde_json::to_vec(config).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(serialized);
    format!("{:x}", hasher.finalize())
}

fn coalesce_local_range_directory_groups(
    root: &Path,
    mut dir_groups: HashMap<PathBuf, Vec<PathBuf>>,
) -> (
    HashMap<PathBuf, Vec<PathBuf>>,
    HashMap<PathBuf, CoalescedRangeDirs<PathBuf>>,
) {
    let mut candidates: HashMap<PathBuf, Vec<(PathBuf, ChapterRangeDir)>> = HashMap::new();

    for dir in dir_groups.keys() {
        let Some(parent) = dir.parent() else {
            continue;
        };
        let Some(dir_name) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(range_dir) = parse_chapter_range_dir_name(dir_name) else {
            continue;
        };

        candidates
            .entry(parent.to_path_buf())
            .or_default()
            .push((dir.clone(), range_dir));
    }

    let mut coalesced_range_dirs = HashMap::new();

    for (parent, entries) in candidates {
        let parent_name = parent
            .file_name()
            .and_then(|name| name.to_str())
            .or_else(|| root.file_name().and_then(|name| name.to_str()))
            .unwrap_or("");

        let ranges: Vec<ChapterRangeDir> = entries.iter().map(|(_, range)| range.clone()).collect();

        for group in select_mergeable_range_groups(parent_name, &ranges) {
            let mut selected: Vec<(PathBuf, ChapterRangeDir)> = group
                .indices
                .into_iter()
                .map(|index| entries[index].clone())
                .collect();
            selected.sort_by_key(|(_, range)| (range.start, range.end));

            let Some(first_child_dir) = selected.first().map(|(child_dir, _)| child_dir.clone())
            else {
                continue;
            };

            let target_dir = if group.merge_into_parent {
                parent.clone()
            } else {
                first_child_dir
            };

            let child_dirs: Vec<PathBuf> = selected
                .iter()
                .map(|(child_dir, _)| child_dir.clone())
                .collect();

            for child_dir in &child_dirs {
                if let Some(mut child_files) = dir_groups.remove(child_dir) {
                    dir_groups
                        .entry(target_dir.clone())
                        .or_default()
                        .append(&mut child_files);
                }
            }

            if !child_dirs.is_empty() {
                coalesced_range_dirs.insert(
                    target_dir,
                    CoalescedRangeDirs {
                        child_dirs,
                        title_override: group.title,
                    },
                );
            }
        }
    }

    (dir_groups, coalesced_range_dirs)
}

fn infer_local_series_directories<'a>(
    root: &Path,
    dirs: impl Iterator<Item = &'a PathBuf>,
) -> HashMap<PathBuf, crate::core::library_scanner::shared::InferredSeriesInfo> {
    let candidates: Vec<SeriesDirectoryCandidate<PathBuf>> = dirs
        .filter_map(|dir| {
            let parent = dir.parent()?;
            let name = dir.file_name()?.to_str()?;
            let parent_name = parent
                .file_name()
                .and_then(|value| value.to_str())
                .or_else(|| root.file_name().and_then(|value| value.to_str()))
                .unwrap_or("");

            Some(SeriesDirectoryCandidate {
                key: dir.clone(),
                parent_key: parent.to_string_lossy().to_string(),
                parent_name: parent_name.to_string(),
                name: name.to_string(),
            })
        })
        .collect();

    infer_series_directories(&candidates)
}

#[cfg(test)]
#[path = "../../../../tests/unit/core/library_scanner/local.rs"]
mod tests;
