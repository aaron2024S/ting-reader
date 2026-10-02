use crate::core::app::error::{Result, TingError};
use crate::db::models::Chapter;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AudiobookshelfChapter {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudiobookshelfMetadata {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub chapters: Vec<AudiobookshelfChapter>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub series: Vec<String>,
    pub genres: Vec<String>,
    pub published_year: Option<String>,
    pub published_date: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub language: Option<String>,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub abridged: bool,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtendedMetadata {
    pub subtitle: Option<String>,
    pub published_year: Option<String>,
    pub published_date: Option<String>,
    pub publisher: Option<String>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub language: Option<String>,
    pub explicit: bool,
    pub abridged: bool,
    pub tags: Vec<String>, // Added tags here to preserve them
}

impl AudiobookshelfMetadata {
    pub fn update_book_fields(&mut self, book: &crate::db::models::Book) {
        self.title = book.title.clone();
        self.authors = book
            .author
            .clone()
            .map(|value| vec![value])
            .unwrap_or_default();
        self.narrators = book
            .narrator
            .clone()
            .map(|value| vec![value])
            .unwrap_or_default();
        self.description = book.description.clone();
        let split_values = |value: Option<&str>| -> Vec<String> {
            value
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        };
        self.tags = split_values(book.tags.as_deref());
        self.genres = split_values(book.genre.as_deref());
        self.published_year = book
            .year
            .map(|year| year.to_string())
            .or(self.published_year.take());
    }

    pub fn new(
        book: &crate::db::models::Book,
        chapters: Vec<AudiobookshelfChapter>,
        extended: ExtendedMetadata,
        series: Vec<String>,
    ) -> Self {
        let tags_vec: Vec<String> = book
            .tags
            .clone()
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Use book.year if available, otherwise fall back to extended.published_year
        let published_year = book.year.map(|y| y.to_string()).or(extended.published_year);

        Self {
            tags: tags_vec,
            chapters,
            title: book.title.clone(),
            subtitle: extended.subtitle,
            authors: book.author.clone().map(|s| vec![s]).unwrap_or_default(),
            narrators: book.narrator.clone().map(|s| vec![s]).unwrap_or_default(),
            series,
            genres: book
                .genre
                .clone()
                .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
                .unwrap_or_default(),
            published_year,
            published_date: extended.published_date,
            publisher: extended.publisher,
            description: book.description.clone(),
            isbn: extended.isbn,
            asin: extended.asin,
            language: extended.language,
            explicit: extended.explicit,
            abridged: extended.abridged,
            extra: Default::default(),
        }
    }
}

/// Align sidecar chapters with the scanned files when every file stem has an
/// exact chapter-title match. Older Ting Reader versions could write main and
/// extra chapters interleaved by their independent display indexes, so their
/// array positions are not reliable.
pub fn align_chapters_to_file_stems(
    chapters: Vec<AudiobookshelfChapter>,
    file_stems: &[String],
) -> (Vec<AudiobookshelfChapter>, bool) {
    if chapters.len() != file_stems.len() || chapters.is_empty() {
        return (chapters, false);
    }

    let mut indexes_by_title: HashMap<String, VecDeque<usize>> = HashMap::new();
    for (index, chapter) in chapters.iter().enumerate() {
        indexes_by_title
            .entry(chapter_match_key(&chapter.title))
            .or_default()
            .push_back(index);
    }

    let mut matched_indexes = Vec::with_capacity(file_stems.len());
    for file_stem in file_stems {
        let Some(index) = indexes_by_title
            .get_mut(&chapter_match_key(file_stem))
            .and_then(VecDeque::pop_front)
        else {
            return (chapters, false);
        };
        matched_indexes.push(index);
    }

    let aligned = matched_indexes
        .into_iter()
        .map(|index| chapters[index].clone())
        .collect();
    (aligned, true)
}

/// Audiobookshelf chapter offsets must follow media-file order. Main and extra
/// chapter indexes are separate UI sequences and cannot be used for this.
pub fn build_audiobookshelf_chapters(mut chapters: Vec<Chapter>) -> Vec<AudiobookshelfChapter> {
    chapters.sort_by(|a, b| natord::compare(&a.path, &b.path).then_with(|| a.id.cmp(&b.id)));

    let mut current_time = 0.0;
    chapters
        .into_iter()
        .enumerate()
        .map(|(index, chapter)| {
            let duration = chapter.duration.unwrap_or(0).max(0) as f64;
            let result = AudiobookshelfChapter {
                id: index as u32,
                start: current_time,
                end: current_time + duration,
                title: chapter.title.unwrap_or_default(),
            };
            current_time += duration;
            result
        })
        .collect()
}

fn chapter_match_key(value: &str) -> String {
    value.trim().to_lowercase()
}

pub fn remote_metadata_dir(book_path: &str) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    let hash = format!("{:x}", Sha256::digest(book_path.as_bytes()));
    Ok(std::env::current_dir()?.join("temp").join(hash))
}

pub fn write_metadata_json(dir: &Path, metadata: &AudiobookshelfMetadata) -> Result<()> {
    let path = dir.join("metadata.json");
    let file = std::fs::File::create(&path).map_err(TingError::IoError)?;
    serde_json::to_writer_pretty(file, metadata)
        .map_err(|e| TingError::SerializationError(e.to_string()))?;
    tracing::info!(
        target: "audit::metadata",
        message_key = "metadata.json.write_succeeded",
        message_params = %serde_json::json!({ "path": dir.display().to_string() }),
        path = %dir.display(),
        "Metadata JSON written"
    );
    Ok(())
}

pub fn read_metadata_json(dir: &Path) -> Result<Option<AudiobookshelfMetadata>> {
    let path = dir.join("metadata.json");
    if !path.exists() {
        return Ok(None);
    }
    let file = std::fs::File::open(&path).map_err(TingError::IoError)?;
    let metadata: AudiobookshelfMetadata = serde_json::from_reader(file)
        .map_err(|e| TingError::DeserializationError(e.to_string()))?;
    Ok(Some(metadata))
}

#[cfg(test)]
#[path = "../../../tests/unit/core/books/metadata_writer.rs"]
mod tests;
