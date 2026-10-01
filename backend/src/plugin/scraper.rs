//! Search-facing business DTOs. Plugin wire DTOs are defined in ting-plugin-contract.

use serde::{Deserialize, Serialize};

/// Search result containing a list of books and pagination information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// List of book items in the search results
    pub items: Vec<BookItem>,

    /// Total number of results available
    pub total: Option<u64>,

    /// Whether the source knows another page exists.
    pub has_more: Option<bool>,

    /// Current page number (1-indexed)
    pub page: u32,

    /// Number of items per page
    pub page_size: u32,
}

/// Book item in search results
///
/// Contains basic information about a book, typically shown in search results
/// or book lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookItem {
    /// Unique identifier on the source platform
    pub id: Option<String>,

    /// Source URL when the source has no stable platform identifier.
    #[serde(default)]
    pub source_url: Option<String>,

    /// Book title
    #[serde(default)]
    pub title: String,

    /// Author name
    #[serde(default)]
    pub author: String,

    /// Cover image URL (optional)
    #[serde(default)]
    pub cover_url: Option<String>,

    /// Brief introduction or description (optional)
    #[serde(default)]
    pub intro: Option<String>,

    /// Narrator name (optional, for audiobooks)
    #[serde(default)]
    pub narrator: Option<String>,

    /// Subtitle (optional)
    #[serde(default)]
    pub subtitle: Option<String>,

    /// Published Year (optional)
    #[serde(default)]
    pub published_year: Option<String>,

    /// Published Date (optional)
    #[serde(default)]
    pub published_date: Option<String>,

    /// Publisher (optional)
    #[serde(default)]
    pub publisher: Option<String>,

    /// ISBN (optional)
    #[serde(default)]
    pub isbn: Option<String>,

    /// ASIN (optional)
    #[serde(default)]
    pub asin: Option<String>,

    /// Language (optional)
    #[serde(default)]
    pub language: Option<String>,

    /// Genre
    #[serde(default)]
    pub genre: Option<String>,

    /// Explicit content
    #[serde(default)]
    pub explicit: Option<bool>,

    /// Abridged version
    #[serde(default)]
    pub abridged: Option<bool>,

    /// Tags or categories
    #[serde(default)]
    pub tags: Vec<String>,

    /// Total duration in seconds (optional)
    #[serde(default)]
    pub duration: Option<u64>,

    /// Optional chapter title template selected by metadata provider.
    #[serde(default)]
    pub chapter_title_template: Option<String>,

    /// Optional cleaned chapter titles in the same order as scanned files.
    #[serde(default)]
    pub chapter_titles: Vec<String>,

    #[serde(default)]
    pub score: Option<f64>,
}

/// Detailed book information
///
/// Contains comprehensive metadata about a book, including all information
/// needed to display a book detail page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookDetail {
    /// Unique identifier on the source platform
    pub id: Option<String>,

    /// Book title
    pub title: String,

    /// Author name
    pub author: String,

    /// Narrator name (optional, for audiobooks)
    #[serde(default)]
    pub narrator: Option<String>,

    /// Cover image URL (optional)
    #[serde(default)]
    pub cover_url: Option<String>,

    /// Subtitle (optional)
    #[serde(default)]
    pub subtitle: Option<String>,

    /// Published Year (optional)
    #[serde(default)]
    pub published_year: Option<String>,

    /// Published Date (optional)
    #[serde(default)]
    pub published_date: Option<String>,

    /// Publisher (optional)
    #[serde(default)]
    pub publisher: Option<String>,

    /// ISBN (optional)
    #[serde(default)]
    pub isbn: Option<String>,

    /// ASIN (optional)
    #[serde(default)]
    pub asin: Option<String>,

    /// Language (optional)
    #[serde(default)]
    pub language: Option<String>,

    /// Explicit content
    #[serde(default)]
    pub explicit: bool,

    /// Abridged version
    #[serde(default)]
    pub abridged: bool,

    /// Full introduction or description
    pub intro: String,

    /// Tags or categories
    #[serde(default)]
    pub tags: Vec<String>,

    /// Genre
    #[serde(default)]
    pub genre: Option<String>,

    /// Total number of chapters
    pub chapter_count: u32,

    /// Total duration in seconds (optional)
    #[serde(default)]
    pub duration: Option<u64>,

    /// Optional chapter title template selected by metadata provider.
    #[serde(default)]
    pub chapter_title_template: Option<String>,

    /// Optional cleaned chapter titles in the same order as scanned files.
    #[serde(default)]
    pub chapter_titles: Vec<String>,
}

/// Chapter information
///
/// Represents a single chapter or episode in a book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    /// Unique identifier on the source platform
    pub id: String,

    /// Chapter title
    pub title: String,

    /// Chapter index (0-indexed)
    pub index: u32,

    /// Duration in seconds (optional)
    #[serde(default)]
    pub duration: Option<u64>,

    /// Whether the chapter is free to access
    #[serde(default = "default_true")]
    pub is_free: bool,
}

/// Default value for is_free field (true)
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_result_serialization() {
        let result = SearchResult {
            items: vec![BookItem {
                id: Some("123".to_string()),
                source_url: None,
                title: "Test Book".to_string(),
                author: "Test Author".to_string(),
                cover_url: Some("https://example.com/cover.jpg".to_string()),
                intro: Some("Test intro".to_string()),
                narrator: None,
                subtitle: None,
                published_year: None,
                published_date: None,
                publisher: None,
                isbn: None,
                asin: None,
                language: None,
                genre: None,
                explicit: None,
                abridged: None,
                tags: vec![],
                duration: None,
                chapter_title_template: None,
                chapter_titles: vec![],
                score: None,
            }],
            total: Some(100),
            has_more: Some(true),
            page: 1,
            page_size: 20,
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: SearchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.items.len(), 1);
        assert_eq!(deserialized.total, Some(100));
        assert_eq!(deserialized.page, 1);
    }

    #[test]
    fn test_book_detail_serialization() {
        let detail = BookDetail {
            id: Some("456".to_string()),
            title: "Test Book".to_string(),
            author: "Test Author".to_string(),
            narrator: Some("Test Narrator".to_string()),
            cover_url: Some("https://example.com/cover.jpg".to_string()),
            subtitle: None,
            published_year: None,
            published_date: None,
            publisher: None,
            isbn: None,
            asin: None,
            language: None,
            explicit: false,
            abridged: false,
            intro: "Full introduction".to_string(),
            tags: vec!["fiction".to_string(), "sci-fi".to_string()],
            genre: None,
            chapter_count: 50,
            duration: Some(36000),
            chapter_title_template: None,
            chapter_titles: vec!["Chapter 1".to_string(), "Chapter 2".to_string()],
        };

        let json = serde_json::to_string(&detail).unwrap();
        let deserialized: BookDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id.as_deref(), Some("456"));
        assert_eq!(deserialized.chapter_count, 50);
        assert_eq!(deserialized.tags.len(), 2);
        assert_eq!(deserialized.chapter_titles.len(), 2);
    }

    #[test]
    fn test_chapter_serialization() {
        let chapter = Chapter {
            id: "789".to_string(),
            title: "Chapter 1".to_string(),
            index: 0,
            duration: Some(1800),
            is_free: true,
        };

        let json = serde_json::to_string(&chapter).unwrap();
        let deserialized: Chapter = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, "789");
        assert_eq!(deserialized.index, 0);
        assert!(deserialized.is_free);
    }

    #[test]
    fn test_chapter_default_is_free() {
        let json = r#"{"id":"789","title":"Chapter 1","index":0}"#;
        let chapter: Chapter = serde_json::from_str(json).unwrap();

        assert!(chapter.is_free);
    }
}
