use super::super::LibraryScanner;
use super::super::shared::{
    apply_chapter_title_template, chapter_title_template_preserves_raw,
    clean_or_preserve_chapter_title,
};
use crate::core::app::error::{Result, TingError};
use crate::db::models::Chapter;
use crate::db::repository::Repository;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use uuid::Uuid;

pub(super) struct ChapterProcessingOptions<'a> {
    pub(super) last_scanned: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) task_id: Option<&'a str>,
    pub(super) use_filename_as_title: bool,
    pub(super) extract_extra_chapters: bool,
    pub(super) cloud_mode: bool,
    pub(super) changed_file_paths: Option<&'a HashSet<PathBuf>>,
    pub(super) json_chapters:
        Option<Vec<crate::core::books::metadata_writer::AudiobookshelfChapter>>,
    pub(super) chapter_title_template: Option<&'a str>,
    pub(super) chapter_title_overrides: Option<&'a [String]>,
}

impl LibraryScanner {
    pub(super) async fn process_chapters(
        &self,
        book_id: &str,
        files: &[PathBuf],
        options: ChapterProcessingOptions<'_>,
    ) -> Result<bool> {
        let ChapterProcessingOptions {
            last_scanned,
            task_id,
            use_filename_as_title,
            extract_extra_chapters,
            cloud_mode,
            changed_file_paths,
            json_chapters,
            chapter_title_template,
            chapter_title_overrides,
        } = options;
        let mut has_changes = false;
        let total_files = files.len();
        let mut json_chapters = json_chapters;

        // Use JSON chapters if available and count matches
        let use_json_chapters = if let Some(ref chapters) = json_chapters {
            if chapters.len() == total_files {
                info!("Using metadata.json chapters for book_id: {}", book_id);
                true
            } else {
                if !chapters.is_empty() {
                    warn!(
                        "metadata.json chapter count ({}) does not match file count ({}) for book {}. Ignoring JSON chapters.",
                        chapters.len(),
                        total_files,
                        book_id
                    );
                }
                false
            }
        } else {
            false
        };
        let mut json_chapters_matched_by_title = false;
        if use_json_chapters {
            let file_stems: Vec<String> = files
                .iter()
                .map(|path| {
                    path.file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            if let Some(chapters) = json_chapters.take() {
                let (aligned, matched_by_title) =
                    crate::core::books::metadata_writer::align_chapters_to_file_stems(
                        chapters,
                        &file_stems,
                    );
                if matched_by_title {
                    json_chapters_matched_by_title = true;
                    info!(
                        "Aligned metadata.json chapters to local files by title for book_id: {}",
                        book_id
                    );
                }
                json_chapters = Some(aligned);
            }
        }
        let preserve_raw_chapter_titles =
            chapter_title_template_preserves_raw(chapter_title_template);

        // Fetch book to check for regex rule
        let book = self
            .book_repo
            .find_by_id(book_id)
            .await?
            .ok_or_else(|| TingError::NotFound("Book not found".to_string()))?;

        let chapter_regex = if let Some(pattern) = &book.chapter_regex {
            regex::Regex::new(pattern).ok()
        } else {
            None
        };

        // Pre-fetch existing chapters to support efficient incremental scanning.
        // Before 1.5.2 local files could be stored as relative paths. Resolve
        // existing files so the old relative path matches the new absolute path.
        let existing_chapters = self.chapter_repo.find_by_book(book_id).await?;
        let mut chapter_map: HashMap<PathBuf, Chapter> = HashMap::new();
        let mut duplicate_chapter_ids = Vec::new();
        for ch in existing_chapters {
            let path = canonical_existing_path(Path::new(&ch.path));
            if let Some(existing) = chapter_map.get(&path) {
                let chapter_is_relative = Path::new(&ch.path).is_relative();
                let existing_is_relative = Path::new(&existing.path).is_relative();
                if chapter_is_relative != existing_is_relative {
                    if chapter_is_relative {
                        duplicate_chapter_ids.push(existing.id.clone());
                        chapter_map.insert(path, ch);
                    } else {
                        duplicate_chapter_ids.push(ch.id);
                    }
                } else {
                    chapter_map.insert(PathBuf::from(&ch.path), ch);
                }
            } else {
                chapter_map.insert(path, ch);
            }
        }
        for chapter_id in duplicate_chapter_ids {
            self.chapter_repo.delete(&chapter_id).await?;
            has_changes = true;
        }

        let mut main_counter = 0;
        let mut extra_counter = 0;

        // Track processed chapter IDs to find deleted ones
        let mut processed_chapter_ids = HashSet::new();

        for (index, file_path) in files.iter().enumerate() {
            if index % 25 == 0 || index + 1 == total_files {
                // Progress/cancellation updates are database operations; keep
                // them periodic instead of writing once per handful of files.
                self.check_cancellation(task_id).await?;
                self.update_progress_key(
                    task_id,
                    "scan.chapter.processing",
                    serde_json::json!({
                        "current": index + 1,
                        "total": total_files,
                    }),
                )
                .await;
            }

            // Incremental Scan Logic
            // Check if file exists in DB
            let canonical_file_path = canonical_existing_path(file_path);
            let mut existing_chapter = chapter_map.get(&canonical_file_path).cloned();

            // Check if file has changed
            let is_modified = if let Some(changed_file_paths) = changed_file_paths {
                changed_file_paths.contains(file_path)
            } else if let Some(last_scan) = last_scanned {
                if let Ok(metadata) = std::fs::metadata(file_path) {
                    if let Ok(mtime) = metadata.modified() {
                        let mtime_utc: chrono::DateTime<chrono::Utc> = mtime.into();
                        mtime_utc > last_scan
                    } else {
                        true // Can't read mtime, force check
                    }
                } else {
                    true // Can't read metadata, force check
                }
            } else {
                true // No last scan, force check
            };

            // Common Logic: Calculate Regex/Filename properties
            let filename_str = file_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string();
            let ai_chapter_title = chapter_title_overrides
                .and_then(|titles| titles.get(index))
                .map(|title| title.trim())
                .filter(|title| !title.is_empty());
            let mut regex_idx = None;
            let mut regex_title = None;

            if let Some(re) = &chapter_regex
                && let Some(caps) = re.captures(&filename_str)
            {
                if let Some(m) = caps.get(1)
                    && let Ok(idx) = m.as_str().parse::<i32>()
                {
                    regex_idx = Some(idx);
                }
                if let Some(m) = caps.get(2) {
                    regex_title = Some(m.as_str().to_string());
                }
            }

            // Optimization: If chapter exists and file is not modified, skip processing!
            if let Some(ref ch) = existing_chapter
                && !is_modified
            {
                // Update index if needed (e.g. reordering files), but skip hashing/metadata
                // Also respect manual_corrected if we were to update anything else

                let title_override = if ch.manual_corrected == 0 {
                    if let Some(ai_title) = ai_chapter_title {
                        let (_, is_extra) = self
                            .text_cleaner
                            .clean_chapter_title(ai_title, book.title.as_deref());
                        Some((ai_title.to_string(), is_extra))
                    } else if let Some(rt) = regex_title.clone() {
                        let (cleaned, is_extra) = clean_or_preserve_chapter_title(
                            self.text_cleaner.as_ref(),
                            &rt,
                            book.title.as_deref(),
                            preserve_raw_chapter_titles,
                        );
                        Some((cleaned, is_extra))
                    } else if use_filename_as_title {
                        let (cleaned, is_extra) = clean_or_preserve_chapter_title(
                            self.text_cleaner.as_ref(),
                            &filename_str,
                            book.title.as_deref(),
                            preserve_raw_chapter_titles,
                        );
                        Some((cleaned, is_extra))
                    } else if use_json_chapters {
                        json_chapters.as_ref().and_then(|chapters| {
                            chapters.get(index).map(|chapter| {
                                let (_, is_extra) = self
                                    .text_cleaner
                                    .clean_chapter_title(&chapter.title, book.title.as_deref());
                                (chapter.title.clone(), is_extra)
                            })
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };

                let counter_is_extra = if ch.manual_corrected != 0 {
                    ch.is_extra == 1
                } else {
                    extract_extra_chapters
                        && title_override
                            .as_ref()
                            .map(|(_, is_extra)| *is_extra)
                            .unwrap_or(ch.is_extra == 1)
                };
                let idx_from_counter = if counter_is_extra {
                    extra_counter += 1;
                    extra_counter
                } else {
                    main_counter += 1;
                    main_counter
                };

                // Final Index (Regex overrides counter)
                let target_idx = regex_idx.unwrap_or(idx_from_counter);

                // Check if we need to update Title or Index
                // Cases to update even if not modified:
                // 1. Regex applied/changed and provides new title/index.
                // 2. use_filename_as_title is TRUE and current title != filename.
                // 3. Index changed due to reordering.

                let mut should_update = false;
                let mut new_title = ch.title.clone();
                let mut new_idx = ch.chapter_index;
                let json_duration = if json_chapters_matched_by_title {
                    json_chapters
                        .as_ref()
                        .and_then(|chapters| chapters.get(index))
                        .map(|chapter| ((chapter.end - chapter.start).round() as i32).max(0))
                } else {
                    None
                };
                let duration_changed = json_duration
                    .map(|duration| ch.duration != Some(duration))
                    .unwrap_or(false);
                let mut new_is_extra = if extract_extra_chapters {
                    ch.is_extra
                } else {
                    0
                };
                if new_is_extra != ch.is_extra {
                    should_update = true;
                }

                // Check Index
                if new_idx != Some(target_idx) {
                    new_idx = Some(target_idx);
                    should_update = true;
                }

                // Check title overrides. metadata.json is a fallback, but chapter regex
                // and forced filename titles must still apply to unchanged files.
                // JSON titles are preserved verbatim, while still detecting extras.
                if let Some((target_title, target_is_extra)) = title_override {
                    let target_title = apply_chapter_title_template(
                        chapter_title_template,
                        book.title.as_deref(),
                        target_idx,
                        &target_title,
                    );

                    if ch.title.as_deref() != Some(&target_title) {
                        new_title = Some(target_title);
                        should_update = true;
                    }

                    let target_is_extra = if extract_extra_chapters && target_is_extra {
                        1
                    } else {
                        0
                    };
                    if new_is_extra != target_is_extra {
                        new_is_extra = target_is_extra;
                        should_update = true;
                    }
                }

                let scanned_path = file_path.to_string_lossy().to_string();
                if ch.path != scanned_path
                    || duration_changed
                    || (should_update && ch.manual_corrected == 0)
                {
                    let mut updated_ch = ch.clone();
                    updated_ch.path = scanned_path;
                    if let Some(duration) = json_duration {
                        updated_ch.duration = Some(duration);
                    }
                    if ch.manual_corrected == 0 {
                        updated_ch.chapter_index = new_idx;
                        updated_ch.title = new_title;
                        updated_ch.is_extra = new_is_extra;
                    }
                    self.chapter_repo.update(&updated_ch).await?;
                    has_changes = true;
                }
                processed_chapter_ids.insert(ch.id.clone());
                continue;
            }

            // If we are here, either it's a new file OR it's modified.

            // Calculate content-based hash
            let file_hash = self.calculate_file_hash(file_path)?;

            // Check if chapter exists by Hash (Global Deduplication)
            // But we must be careful: if we already found it by Path, we know it's that chapter.
            // If we found by Path, but Hash changed, it's an update.
            // If we didn't find by Path, we check Hash to see if it's a move/rename.

            if existing_chapter.is_none()
                && let Ok(Some(ch)) = self.chapter_repo.find_by_hash(&file_hash).await
            {
                // Found by hash (Rename/Move case)
                // But we are processing a specific book_id here.
                // If the found chapter belongs to another book, we might be stealing it?
                // Or it's a duplicate file (e.g. same intro file in multiple books).
                // If it's the same book, we treat it as the "existing chapter".
                if ch.book_id == book_id {
                    existing_chapter = Some(ch);
                }
                // If different book, we create a new chapter record (duplicate content allowed across books)
            }

            // Extract metadata
            // If using JSON chapters, calculate duration from JSON (end - start)
            let duration = if use_json_chapters {
                if let Some(ref chapters) = json_chapters {
                    if index < chapters.len() {
                        let chapter = &chapters[index];
                        ((chapter.end - chapter.start).round() as i32).max(0)
                    } else {
                        // Fallback to file extraction
                        let (_, _, _, _, _, d) =
                            self.extract_chapter_metadata(file_path, cloud_mode).await;
                        d
                    }
                } else {
                    // Fallback to file extraction
                    let (_, _, _, _, _, d) =
                        self.extract_chapter_metadata(file_path, cloud_mode).await;
                    d
                }
            } else {
                // Extract from file
                let (_, _, _, _, _, d) = self.extract_chapter_metadata(file_path, cloud_mode).await;
                d
            };

            // Extract title (only if not using JSON chapters)
            let extracted_title = if !use_json_chapters {
                let (_, t, _, _, _, _) = self.extract_chapter_metadata(file_path, cloud_mode).await;
                t
            } else {
                String::new()
            };

            // metadata.json is the fallback title source. Explicit chapter regex
            // and filename-based titles still override it and go through cleaner.
            let (raw_title, should_clean_title) = if let Some(rt) = regex_title {
                (rt, true)
            } else if use_filename_as_title {
                (filename_str.clone(), true)
            } else if use_json_chapters {
                if let Some(ref chapters) = json_chapters {
                    if index < chapters.len() {
                        (chapters[index].title.clone(), false)
                    } else {
                        (filename_str.clone(), true)
                    }
                } else {
                    (filename_str.clone(), true)
                }
            } else if !extracted_title.is_empty() {
                (extracted_title, true)
            } else {
                (filename_str.clone(), true)
            };

            let (final_title, detected_as_extra) = if let Some(ai_title) = ai_chapter_title {
                let (_, is_extra) = self
                    .text_cleaner
                    .clean_chapter_title(ai_title, book.title.as_deref());
                (ai_title.to_string(), is_extra)
            } else if should_clean_title {
                clean_or_preserve_chapter_title(
                    self.text_cleaner.as_ref(),
                    &raw_title,
                    book.title.as_deref(),
                    preserve_raw_chapter_titles,
                )
            } else {
                let (_, is_extra) = self
                    .text_cleaner
                    .clean_chapter_title(&raw_title, book.title.as_deref());
                (raw_title, is_extra)
            };
            let is_extra = extract_extra_chapters && detected_as_extra;

            // Calculate Index using counters
            let counter_idx = if is_extra {
                extra_counter += 1;
                extra_counter
            } else {
                main_counter += 1;
                main_counter
            };

            // Final Index
            let chapter_idx = regex_idx.unwrap_or(counter_idx);
            let final_title = apply_chapter_title_template(
                chapter_title_template,
                book.title.as_deref(),
                chapter_idx,
                &final_title,
            );

            if let Some(mut ch) = existing_chapter {
                // Update Existing
                // Check Lock
                if ch.manual_corrected == 0 {
                    ch.title = Some(final_title);
                    ch.chapter_index = Some(chapter_idx);
                    ch.is_extra = if is_extra { 1 } else { 0 };
                }
                // Always update duration/path/hash if file changed
                ch.path = file_path.to_string_lossy().to_string();
                ch.duration = Some(duration);
                ch.hash = Some(file_hash);
                ch.book_id = book_id.to_string();

                self.chapter_repo.update(&ch).await?;
                has_changes = true;
                processed_chapter_ids.insert(ch.id.clone());
            } else {
                // Create New
                let chapter_id = Uuid::new_v4().to_string();
                let chapter = Chapter {
                    id: chapter_id.clone(),
                    book_id: book_id.to_string(),
                    title: Some(final_title),
                    path: file_path.to_string_lossy().to_string(),
                    duration: Some(duration),
                    chapter_index: Some(chapter_idx),
                    is_extra: if is_extra { 1 } else { 0 },
                    hash: Some(file_hash),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    manual_corrected: 0,
                };

                match self.chapter_repo.create(&chapter).await {
                    Ok(_) => {
                        has_changes = true;
                        processed_chapter_ids.insert(chapter_id);
                    }
                    Err(e) => warn!("Failed to create chapter: {}", e),
                }
            }
        }

        // Handle deleted chapters
        for (path, ch) in chapter_map {
            if !processed_chapter_ids.contains(&ch.id) {
                // The chapter file is missing, remove from DB
                if !path.exists() {
                    info!("Removing missing chapter from DB: {:?}", path);
                    if let Err(e) = self.chapter_repo.delete(&ch.id).await {
                        warn!("Failed to delete missing chapter {}: {}", ch.id, e);
                    } else {
                        has_changes = true;
                    }
                }
            }
        }

        Ok(has_changes)
    }

    fn calculate_file_hash(&self, path: &Path) -> Result<String> {
        let mut file = std::fs::File::open(path).map_err(TingError::IoError)?;
        let metadata = file.metadata().map_err(TingError::IoError)?;
        let len = metadata.len();

        let mut buffer = vec![0; 16384]; // 16KB
        let n = file.read(&mut buffer).map_err(TingError::IoError)?;

        let mut hasher = Sha256::new();
        hasher.update(&buffer[..n]);
        hasher.update(len.to_le_bytes());
        // Also include filename to distinguish different chapters with same content/size (unlikely but possible)
        if let Some(name) = path.file_name() {
            hasher.update(name.to_string_lossy().as_bytes());
        }

        Ok(format!("{:x}", hasher.finalize()))
    }
}
fn canonical_existing_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_legacy_relative_path_to_absolute_path() {
        let current_dir = std::env::current_dir().unwrap();
        let temp_dir = current_dir
            .join("target")
            .join(format!("chapter-path-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let absolute_path = temp_dir.join("chapter.mp3");
        std::fs::write(&absolute_path, b"audio").unwrap();
        let relative_path = absolute_path.strip_prefix(&current_dir).unwrap();

        assert_eq!(
            canonical_existing_path(relative_path),
            canonical_existing_path(&absolute_path)
        );

        std::fs::remove_dir_all(temp_dir).unwrap();
    }
}
