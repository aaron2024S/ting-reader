use super::{Task, TaskPayload, TaskQueue};
use crate::core::error::{Result, TingError};
use crate::db::repository::Repository;
use id3::frame::{Picture, PictureType as Id3PictureType};
use id3::{Tag, TagLike, Version};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

impl TaskQueue {
    /// Run the actual task logic
    pub(super) async fn run_task(&self, task: &Task) -> Result<()> {
        debug!(
            task_id = %task.id,
            payload = ?task.payload,
            "Running task"
        );

        match &task.payload {
            TaskPayload::ScraperSearch { plugin_id, query } => {
                let scraper_service = self.scraper_service.as_ref().ok_or_else(|| {
                    crate::core::error::TingError::TaskError(
                        "Scraper service not configured".to_string(),
                    )
                })?;

                info!(plugin_id = %plugin_id, query = %query, "Executing scraper search task");
                let result = scraper_service
                    .search(query, None, None, Some(plugin_id), 1, 20)
                    .await?;
                info!(items = result.items.len(), "Scraper search completed");
            }
            TaskPayload::PluginInvoke {
                plugin_id,
                capability_id,
                operation,
                params,
            } => {
                let plugin_manager = self.plugin_manager.as_ref().ok_or_else(|| {
                    crate::core::error::TingError::TaskError(
                        "Plugin manager not configured".to_string(),
                    )
                })?;

                info!(
                    plugin_id = %plugin_id,
                    capability_id = %capability_id,
                    operation = %operation,
                    "Executing plugin invoke task"
                );
                plugin_manager
                    .invoke_capability(
                        plugin_id,
                        capability_id,
                        operation,
                        params.clone(),
                        &crate::plugin::types::PluginInvocationContext::default(),
                    )
                    .await?;
            }
            TaskPayload::Custom { task_type, data } => match task_type.as_str() {
                "library_scan" => {
                    self.handle_library_scan(data, &task.id).await?;
                }
                "write_metadata" => {
                    self.handle_write_metadata(data, &task.id).await?;
                }
                _ => {
                    self.handle_plugin_task(task_type, data, &task.id).await?;
                }
            },
            _ => {
                // Other task types can be handled here
                debug!("Task payload type not yet implemented");
            }
        }

        Ok(())
    }

    async fn handle_plugin_task(
        &self,
        task_type: &str,
        data: &serde_json::Value,
        task_id: &str,
    ) -> Result<()> {
        let plugin_manager = self.plugin_manager.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Plugin manager not configured".to_string())
        })?;
        let owner = data
            .get("_ting_task_owner")
            .and_then(|value| value.as_str());
        let handlers = plugin_manager.find_task_handlers(Some(task_type)).await;
        let Some(handler) = handlers
            .into_iter()
            .find(|handler| owner.is_none_or(|owner| handler.registration.plugin_id == owner))
        else {
            warn!(task_type = %task_type, "Unknown task type");
            return Err(crate::core::error::TingError::TaskError(format!(
                "Unknown task type: {}",
                task_type
            )));
        };

        info!(
            task_id = %task_id,
            task_type = %task_type,
            plugin_id = %handler.registration.plugin_id,
            capability_id = %handler.registration.capability.id(),
            "Dispatching custom task to plugin task_handler"
        );

        let handler_data = data
            .get("_ting_task_input")
            .cloned()
            .unwrap_or_else(|| data.clone());
        plugin_manager
            .invoke_capability(
                &handler.registration.plugin_id,
                handler.registration.capability.id(),
                "run",
                serde_json::json!({
                    "task_id": task_id,
                    "task_type": task_type,
                    "data": handler_data,
                    "capability_id": handler.registration.capability.id(),
                }),
                &crate::plugin::types::PluginInvocationContext::default(),
            )
            .await?;

        Ok(())
    }

    /// Handle library scan task
    async fn handle_library_scan(&self, data: &serde_json::Value, task_id: &str) -> Result<()> {
        let library_id = data["library_id"].as_str().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Missing library_id".to_string())
        })?;
        let library_path = data["library_path"].as_str().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Missing library_path".to_string())
        })?;
        let scan_mode = data["mode"]
            .as_str()
            .map(crate::core::library_scanner::ScanMode::from)
            .unwrap_or(crate::core::library_scanner::ScanMode::Incremental);
        let scan_paths = data["scan_paths"].as_array().map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(std::path::PathBuf::from))
                .collect::<Vec<_>>()
        });

        info!(
            library_id = %library_id,
            path = %library_path,
            scan_mode = %scan_mode.as_str(),
            scoped_paths = scan_paths.as_ref().map(|paths| paths.len()).unwrap_or(0),
            "Handling library scan task"
        );

        // Get repositories
        let book_repo = self.book_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Book repository not configured".to_string())
        })?;
        let chapter_repo = self.chapter_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError(
                "Chapter repository not configured".to_string(),
            )
        })?;
        let series_repo = self.series_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Series repository not configured".to_string())
        })?;
        let library_repo = self.library_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError(
                "Library repository not configured".to_string(),
            )
        })?;
        let library = library_repo.find_by_id(library_id).await?.ok_or_else(|| {
            crate::core::error::TingError::NotFound(format!("Library {} not found", library_id))
        })?;

        // Get services
        let text_cleaner = self.text_cleaner.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Text cleaner not configured".to_string())
        })?;
        let nfo_manager = self.nfo_manager.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("NFO manager not configured".to_string())
        })?;
        let audio_streamer = self.audio_streamer.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Audio streamer not configured".to_string())
        })?;
        let plugin_manager = self.plugin_manager.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Plugin manager not configured".to_string())
        })?;

        // Create library scanner
        let mut scanner = crate::core::library_scanner::LibraryScanner::new(
            crate::core::library_scanner::LibraryScannerDependencies {
                book_repo: book_repo.clone(),
                chapter_repo: chapter_repo.clone(),
                library_repo: library_repo.clone(),
                series_repo: series_repo.clone(),
                text_cleaner: text_cleaner.clone(),
                nfo_manager: nfo_manager.clone(),
                audio_streamer: audio_streamer.clone(),
                plugin_manager: plugin_manager.clone(),
            },
        )
        .with_task_repo(Arc::new(self.task_repo.clone()))
        .with_scraper_service(self.scraper_service.as_ref().unwrap().clone());

        if let Some(storage) = &self.storage_service {
            scanner = scanner.with_storage_service(storage.clone());
        }
        if let Some(merge_service) = &self.merge_service {
            scanner = scanner.with_merge_service(merge_service.clone());
        }
        if let Some(key) = &self.encryption_key {
            scanner = scanner.with_encryption_key(key.clone());
        }

        // Scan the library
        let result = scanner
            .scan_library_scoped(
                library_id,
                library_path,
                scan_mode,
                Some(task_id),
                scan_paths,
            )
            .await?;

        info!(
            message_key = "scan.library.completed",
            message_params = %serde_json::json!({
                "library_name": library.name,
                "created": result.books_created,
                "updated": result.books_updated,
                "deleted": result.books_deleted,
                "errors": result.errors.len(),
            }),
            library_id = %library_id,
            library_name = %library.name,
            library_type = %library.library_type,
            path = %library_path,
            books_created = result.books_created,
            books_updated = result.books_updated,
            books_deleted = result.books_deleted,
            errors = result.errors.len(),
            scan_mode = %scan_mode.as_str(),
            "Library scan completed"
        );

        // Update task message with result
        if let Err(e) = self
            .task_repo
            .update_progress_key(
                task_id,
                "scan.library.completed",
                serde_json::json!({
                    "library_name": library.name,
                    "created": result.books_created,
                    "updated": result.books_updated,
                    "deleted": result.books_deleted,
                    "errors": result.errors.len(),
                }),
            )
            .await
        {
            warn!(task_id = %task_id, error = %e, "Failed to update task progress message");
        }

        if let Some(notification_repo) = &self.notification_repo {
            let payload = crate::core::notifications::NotificationEventPayload::new(
                "library.scan_completed",
                "Library scan completed",
                format!(
                    "Library {} scan completed: created {}, updated {}, deleted {}",
                    library.name, result.books_created, result.books_updated, result.books_deleted
                ),
                serde_json::json!({
                    "library_id": library.id,
                    "library_name": library.name,
                    "library_type": library.library_type,
                    "path": library_path,
                    "mode": scan_mode.as_str(),
                    "task_id": task_id,
                    "books_created": result.books_created,
                    "books_updated": result.books_updated,
                    "books_deleted": result.books_deleted,
                    "errors": result.errors.len(),
                }),
            );
            if let Some(plugin_manager) = &self.plugin_manager {
                crate::core::notifications::dispatch_application_event(
                    notification_repo.clone(),
                    plugin_manager.clone(),
                    payload,
                );
            } else {
                crate::core::notifications::dispatch_notification_event(
                    notification_repo.clone(),
                    payload,
                );
            }
        }

        if !result.errors.is_empty() {
            warn!(errors = ?result.errors, "Library scan completed with errors");
        }

        Ok(())
    }

    /// Handle write metadata task
    async fn handle_write_metadata(&self, data: &serde_json::Value, task_id: &str) -> Result<()> {
        let book_id = data["book_id"].as_str().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Missing book_id".to_string())
        })?;

        info!(book_id = %book_id, "Handling write metadata task");

        // Get repositories
        let book_repo = self.book_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Book repository not configured".to_string())
        })?;
        let library_repo = self.library_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError(
                "Library repository not configured".to_string(),
            )
        })?;
        let chapter_repo = self.chapter_repo.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError(
                "Chapter repository not configured".to_string(),
            )
        })?;
        let plugin_manager = self.plugin_manager.as_ref().ok_or_else(|| {
            crate::core::error::TingError::TaskError("Plugin manager not configured".to_string())
        })?;

        let book = book_repo.find_by_id(book_id).await?.ok_or_else(|| {
            crate::core::error::TingError::NotFound(format!("Book with id {} not found", book_id))
        })?;

        // Recheck the setting when executing a previously queued task.
        let library = library_repo
            .find_by_id(&book.library_id)
            .await?
            .ok_or_else(|| {
                crate::core::error::TingError::NotFound(format!(
                    "Library with id {} not found",
                    book.library_id
                ))
            })?;

        if !library.can_write_metadata_files() {
            return Err(TingError::InvalidRequest(
                "Metadata file writing is disabled for this library".to_string(),
            ));
        }
        let remote = library.library_type == "webdav";
        let storage = if remote {
            Some(self.storage_service.as_ref().ok_or_else(|| {
                TingError::TaskError("WebDAV storage service is not configured".into())
            })?)
        } else {
            None
        };
        let key = self.encryption_key.as_deref().unwrap_or(&[0; 32]);

        // Resolve cover path
        let mut cover_path_str = None;
        let mut temp_cover_path = None;

        if let Some(ref url) = book.cover_url {
            if crate::core::webdav_metadata::is_cover_url(url) {
                if remote {
                    // URL covers remain references; the explicit audio write
                    // only embeds file covers already held in the book cache.
                    cover_path_str = None;
                } else {
                    // Download to temp
                    let temp_dir = self.temp_dir.join("ting-reader-covers");
                    if !temp_dir.exists() {
                        tokio::fs::create_dir_all(&temp_dir)
                            .await
                            .map_err(crate::core::error::TingError::IoError)?;
                    }

                    let mut fetch_url = url.clone();
                    let mut referer = "".to_string();
                    if let Some(idx) = fetch_url.find("#referer=") {
                        referer = fetch_url[idx + 9..].to_string();
                        fetch_url = fetch_url[..idx].to_string();
                    }

                    let ext = std::path::Path::new(&fetch_url)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("jpg");
                    let file_name = format!("{}.{}", Uuid::new_v4(), ext);
                    let path = temp_dir.join(file_name);

                    // Download
                    let client = reqwest::Client::new();
                    let mut req = client.get(&fetch_url).header(
                        reqwest::header::USER_AGENT,
                        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
                    );
                    if !referer.is_empty() {
                        req = req.header(reqwest::header::REFERER, referer);
                    }

                    match req.send().await {
                        Ok(resp) => {
                            if let Ok(bytes) = resp.bytes().await
                                && tokio::fs::write(&path, bytes).await.is_ok()
                            {
                                temp_cover_path = Some(path.clone());
                                cover_path_str = Some(path.to_string_lossy().to_string());
                            }
                        }
                        Err(e) => warn!("Failed to download cover for metadata writing: {}", e),
                    }
                }
            } else {
                // Local path
                let path = std::path::Path::new(url);
                if path.is_absolute() || path.exists() {
                    cover_path_str = Some(url.clone());
                } else {
                    let book_path = if remote {
                        crate::core::metadata_writer::remote_metadata_dir(&book.path)?
                    } else {
                        PathBuf::from(&book.path)
                    };
                    let joined = book_path.join(url);
                    // If joined path exists, use it. Otherwise fallback to original URL
                    // to avoid double-pathing (e.g. ./storage/./storage/...)
                    if joined.exists() {
                        cover_path_str = Some(joined.to_string_lossy().to_string());
                    } else {
                        cover_path_str = Some(url.clone());
                    }
                }
            }
        }

        // Get chapters
        let chapters = chapter_repo.find_by_book(book_id).await?;

        let mut success_count = 0;
        let mut error_count = 0;
        let mut skipped_count = 0;
        let total_chapters = chapters.len();

        for (index, chapter) in chapters.iter().enumerate() {
            if self
                .task_repo
                .find_by_id(task_id)
                .await?
                .is_some_and(|task| task.status == "cancelled")
            {
                return Err(TingError::TaskError(
                    "Metadata writing was cancelled".into(),
                ));
            }
            // Update progress
            let _ = self
                .task_repo
                .update_progress_key(
                    task_id,
                    "metadata.chapter.writing",
                    serde_json::json!({
                        "current": index + 1,
                        "total": total_chapters,
                        "chapter_title": chapter.title.as_deref().unwrap_or(""),
                    }),
                )
                .await;

            let source_path = std::path::Path::new(&chapter.path);
            let ext = source_path
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            // A book may contain mixed local audio and STRM chapters. STRM is
            // skipped per chapter, while other local audio files continue.
            if ext == "strm" {
                skipped_count += 1;
                continue;
            }

            let mut remote_file = None;
            if let Some(storage) = storage {
                let temporary = TemporaryMetadataFile(self.temp_dir.join(format!(
                    "metadata-{}.{}",
                    Uuid::new_v4(),
                    ext
                )));
                let download = async {
                    tokio::fs::create_dir_all(&self.temp_dir).await?;
                    let source = storage
                        .get_webdav_write_source(&library, &book.path, &chapter.path, key)
                        .await?;
                    let mut reader = source.reader;
                    let mut file = tokio::fs::File::create(&temporary.0).await?;
                    let copied = tokio::io::copy(&mut reader, &mut file).await?;
                    if source.length > 0 && copied != source.length {
                        return Err(TingError::NetworkError(
                            "Incomplete WebDAV audio download".into(),
                        ));
                    }
                    Ok::<_, TingError>(source.revision)
                }
                .await;
                match download {
                    Ok(revision) => remote_file = Some((temporary, revision)),
                    Err(error) => {
                        warn!(chapter_id = %chapter.id, error = %error, "Failed to download chapter for metadata writing");
                        error_count += 1;
                        continue;
                    }
                }
            }
            let path = remote_file
                .as_ref()
                .map(|(file, _)| file.0.as_path())
                .unwrap_or(source_path);
            if !path.exists() {
                error_count += 1;
                continue;
            }
            let successful_before = success_count;

            // A special format can have an ID3 header even when its payload
            // is encrypted. Select its declared extension before generic
            // container sniffing; the format registry verifies it with probe.
            let special_format = plugin_manager
                .find_capabilities_by_kind("format_handler")
                .await
                .into_iter()
                .any(|registration| registration.capability.extensions().contains(&ext));

            if special_format {
                let artist = if let Some(narrator) = &book.narrator {
                    if !narrator.trim().is_empty() {
                        narrator.as_str()
                    } else {
                        book.author.as_deref().unwrap_or("")
                    }
                } else {
                    book.author.as_deref().unwrap_or("")
                };

                let patch = ting_plugin_contract::format_calls::MetadataPatch {
                    title: ting_plugin_contract::format_calls::PatchField::Set(
                        chapter.title.clone().unwrap_or_default(),
                    ),
                    artist: ting_plugin_contract::format_calls::PatchField::Set(artist.into()),
                    album: ting_plugin_contract::format_calls::PatchField::Set(
                        book.title.clone().unwrap_or_default(),
                    ),
                    genre: ting_plugin_contract::format_calls::PatchField::Set(
                        book.genre.clone().unwrap_or_default(),
                    ),
                    description: ting_plugin_contract::format_calls::PatchField::Set(
                        book.description.clone().unwrap_or_default(),
                    ),
                    ..Default::default()
                };
                match plugin_manager
                    .write_local_format_metadata(
                        path,
                        patch,
                        cover_path_str.as_deref().map(std::path::Path::new),
                    )
                    .await
                {
                    Ok(true) => success_count += 1,
                    Ok(false) => {
                        warn!(
                            "Declared format plugin does not support metadata writing for {}",
                            ext
                        );
                        error_count += 1;
                    }
                    Err(e) => {
                        warn!("Failed to write metadata for {}: {}", chapter.path, e);
                        error_count += 1;
                    }
                }
            } else {
                // Generic formats may have a misleading filename extension.
                // Sniff only after ruling out special format declarations.
                let detected_format = detect_audio_format(path).unwrap_or_else(|| ext.to_string());
                if detected_format != ext {
                    warn!(
                        "Audio extension mismatch for {:?}: extension={}, detected={}",
                        path, ext, detected_format
                    );
                }
                // No plugin found, try native/builtin support
                if detected_format == "mp3" {
                    let path_clone = path.to_path_buf();
                    let title_clone = chapter.title.clone().unwrap_or_default();
                    let artist_clone = if let Some(narrator) = &book.narrator {
                        if !narrator.trim().is_empty() {
                            narrator.clone()
                        } else {
                            book.author.clone().unwrap_or_default()
                        }
                    } else {
                        book.author.clone().unwrap_or_default()
                    };
                    let album_clone = book.title.clone().unwrap_or_default();
                    let genre_clone = book.genre.clone().unwrap_or_default();
                    let desc_clone = book.description.clone().unwrap_or_default();
                    let cover_path_str_clone = cover_path_str.clone();

                    // Spawn blocking task for native ID3 write
                    let native_write_result = tokio::task::spawn_blocking(move || -> Result<()> {
                        let mut tag = match Tag::read_from_path(&path_clone) {
                            Ok(t) => t,
                            Err(_) => Tag::new(),
                        };

                        tag.set_title(&title_clone);
                        tag.set_artist(&artist_clone);
                        tag.set_album(&album_clone);
                        tag.set_genre(&genre_clone);

                        tag.remove_comment(Some("eng"), None);
                        tag.add_frame(id3::frame::Comment {
                            lang: "eng".to_string(),
                            description: "".to_string(),
                            text: desc_clone,
                        });

                        if let Some(cp) = cover_path_str_clone
                            && let Ok(data) = std::fs::read(&cp)
                        {
                            let mime_type = if cp.to_lowercase().ends_with("png") {
                                "image/png".to_string()
                            } else {
                                "image/jpeg".to_string()
                            };

                            tag.remove_all_pictures();
                            tag.add_frame(Picture {
                                mime_type,
                                picture_type: Id3PictureType::CoverFront,
                                description: "Cover".to_string(),
                                data,
                            });
                        }

                        tag.write_to_path(&path_clone, Version::Id3v23)
                            .map_err(|e| {
                                crate::core::error::TingError::IoError(std::io::Error::other(
                                    e.to_string(),
                                ))
                            })?;

                        Ok(())
                    })
                    .await;

                    match native_write_result {
                        Ok(Ok(_)) => {
                            info!(
                                "Successfully wrote metadata natively for MP3 (fallback): {:?}",
                                path
                            );
                            success_count += 1;
                        }
                        Ok(Err(e)) => {
                            warn!("Native ID3 write failed for {:?}: {}", path, e);
                            error_count += 1;
                        }
                        Err(e) => {
                            warn!("Native ID3 task panic for {:?}: {}", path, e);
                            error_count += 1;
                        }
                    }
                } else {
                    error_count += 1;
                }
            }
            if success_count > successful_before
                && let Some((temporary, revision)) = &remote_file
                && let Some(storage) = storage
            {
                // A library setting changed while downloading/writing must
                // take effect before committing anything to the drive.
                let commit = async {
                    let current = library_repo.find_by_id(&library.id).await?.ok_or_else(|| {
                        TingError::NotFound("WebDAV library no longer exists".into())
                    })?;
                    if !current.can_write_metadata_files() {
                        return Err(TingError::PermissionDenied(
                            "WebDAV metadata writing has been disabled".into(),
                        ));
                    }
                    if self
                        .task_repo
                        .find_by_id(task_id)
                        .await?
                        .is_some_and(|task| task.status == "cancelled")
                    {
                        return Err(TingError::TaskError(
                            "Metadata writing was cancelled".into(),
                        ));
                    }
                    storage
                        .put_webdav_audio_file(
                            &current,
                            &book.path,
                            &chapter.path,
                            &temporary.0,
                            revision,
                            key,
                        )
                        .await
                }
                .await;
                if let Err(error) = commit {
                    warn!(chapter_id = %chapter.id, error = %error, "Failed to commit WebDAV chapter metadata");
                    success_count -= 1;
                    error_count += 1;
                }
            }
        }

        // Cleanup temp cover
        if let Some(path) = temp_cover_path {
            let _ = tokio::fs::remove_file(path).await;
        }

        let _ = self
            .task_repo
            .update_progress_key(
                task_id,
                "metadata.write.completed",
                serde_json::json!({
                    "success": success_count,
                    "failed": error_count,
                    "skipped": skipped_count,
                }),
            )
            .await;

        tracing::info!(
            target: "audit::metadata",
            message_key = "metadata.write.completed_for_book",
            message_params = %serde_json::json!({
                "book_title": book.title.as_deref().unwrap_or(""),
                "book_id": book.id,
                "success": success_count,
                "failed": error_count,
                "skipped": skipped_count,
            }),
            book_id = %book.id,
            book_title = %book.title.as_deref().unwrap_or(""),
            success = success_count,
            failed = error_count,
            skipped = skipped_count,
            "Book metadata write completed"
        );

        if remote && error_count > 0 {
            return Err(TingError::TaskError(format!(
                "WebDAV metadata writing: {success_count} succeeded, {error_count} failed, {skipped_count} skipped"
            )));
        }
        Ok(())
    }
}

struct TemporaryMetadataFile(PathBuf);

impl Drop for TemporaryMetadataFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn detect_audio_format(path: &Path) -> Option<String> {
    const ASF_HEADER_GUID: [u8; 12] = [
        0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa,
    ];

    let mut file = File::open(path).ok()?;
    let mut header = [0_u8; 12];
    let read = file.read(&mut header).ok()?;
    if read < 4 {
        return None;
    }

    let mut media_offset = 0_u64;
    if read >= 10 && &header[..3] == b"ID3" {
        media_offset = 10
            + ((u64::from(header[6]) & 0x7f) << 21)
            + ((u64::from(header[7]) & 0x7f) << 14)
            + ((u64::from(header[8]) & 0x7f) << 7)
            + (u64::from(header[9]) & 0x7f);
        file.seek(SeekFrom::Start(media_offset)).ok()?;
        header.fill(0);
        if file.read(&mut header).ok()? < 4 {
            return None;
        }
    }

    if &header[4..8] == b"ftyp" {
        return Some("m4a".to_string());
    }
    if media_offset > 0 && &header[1..5] == b"ftyp" {
        return Some("m4a".to_string());
    }
    if header.starts_with(b"fLaC") {
        return Some("flac".to_string());
    }
    if header.starts_with(b"OggS") {
        return Some("ogg".to_string());
    }
    if header.starts_with(b"RIFF") && &header[8..12] == b"WAVE" {
        return Some("wav".to_string());
    }
    if header == ASF_HEADER_GUID {
        return Some("wma".to_string());
    }
    if header[0] == 0xff && header[1] & 0xe0 == 0xe0 {
        return Some("mp3".to_string());
    }

    None
}

#[cfg(test)]
mod metadata_format_tests {
    use super::detect_audio_format;
    use std::fs;

    fn id3_header(payload_size: u32) -> [u8; 10] {
        [
            b'I',
            b'D',
            b'3',
            3,
            0,
            0,
            ((payload_size >> 21) & 0x7f) as u8,
            ((payload_size >> 14) & 0x7f) as u8,
            ((payload_size >> 7) & 0x7f) as u8,
            (payload_size & 0x7f) as u8,
        ]
    }

    #[test]
    fn detects_mp4_hidden_behind_mp3_extension_and_id3() {
        let path = std::env::temp_dir().join(format!("ting-format-{}.mp3", uuid::Uuid::new_v4()));
        let mut bytes = id3_header(4).to_vec();
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&[0, 0, 0, 28, b'f', b't', b'y', b'p', b'M', b'4', b'A', b' ']);
        fs::write(&path, bytes).unwrap();

        assert_eq!(detect_audio_format(&path).as_deref(), Some("m4a"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn detects_mp4_with_box_size_damaged_by_id3_rewrite() {
        let path = std::env::temp_dir().join(format!("ting-format-{}.mp3", uuid::Uuid::new_v4()));
        let mut bytes = id3_header(0).to_vec();
        bytes.extend_from_slice(&[28, b'f', b't', b'y', b'p', b'M', b'4', b'A', b' ']);
        fs::write(&path, bytes).unwrap();

        assert_eq!(detect_audio_format(&path).as_deref(), Some("m4a"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn detects_standard_mp3_after_id3() {
        let path = std::env::temp_dir().join(format!("ting-format-{}.mp3", uuid::Uuid::new_v4()));
        let mut bytes = id3_header(0).to_vec();
        bytes.extend_from_slice(&[0xff, 0xfb, 0x50, 0x00]);
        fs::write(&path, bytes).unwrap();

        assert_eq!(detect_audio_format(&path).as_deref(), Some("mp3"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn detects_wma_asf_header() {
        let path = std::env::temp_dir().join(format!("ting-format-{}.bin", uuid::Uuid::new_v4()));
        fs::write(
            &path,
            [
                0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa,
            ],
        )
        .unwrap();

        assert_eq!(detect_audio_format(&path).as_deref(), Some("wma"));
        fs::remove_file(path).unwrap();
    }
}
