//! Pure scraper candidate ranking and configured metadata selection.
//! Shared by interactive aggregate search and automatic library scraping.
use crate::core::app::error::{Result, TingError};
use crate::plugin::types::scraper::{BookDetail, BookItem, ScraperSourceInfo, SearchResult};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Map directory/user clues onto the fields declared by each source. A source
/// with only one contributor field can still use the other role for ranking.
pub(super) fn source_search_params(
    params: &BTreeMap<String, String>,
    source: &ScraperSourceInfo,
) -> BTreeMap<String, String> {
    let name = super::text_cleaner::BookName::parse(
        params.get("title").map(String::as_str).unwrap_or_default(),
    );
    let author = params.get("author").map(String::as_str).or(name.author);
    let narrator = params.get("narrator").map(String::as_str).or(name.narrator);
    let supports = |key| source.search_fields.iter().any(|field| field.key == key);
    let has_author = supports("author");
    let has_narrator = supports("narrator");
    let mut prepared = params.clone();
    if prepared.contains_key("title") {
        prepared.insert("title".into(), name.title.to_owned());
    }
    prepared.remove("author");
    prepared.remove("narrator");
    let (author, narrator) = match (has_author, has_narrator) {
        (true, true) => (author, narrator),
        (true, false) => (author.or(narrator), None),
        (false, true) => (None, narrator.or(author)),
        (false, false) => (None, None),
    };
    for (key, value) in [("author", author), ("narrator", narrator)] {
        if let Some(value) = value {
            prepared.insert(key.into(), value.to_owned());
        }
    }
    prepared
}

/// Directory suffixes can match either role when ranking editions.
/// No hint match leaves the provider's original order intact.
pub(super) fn prioritize_contributor_matches(
    name: &str,
    author: Option<&str>,
    narrator: Option<&str>,
    results: &mut SearchResult,
) {
    let name = super::text_cleaner::BookName::parse(name);
    let hints: Vec<_> = [author.or(name.author), narrator.or(name.narrator)]
        .into_iter()
        .flatten()
        .flat_map(|names| names.split([',', '，', '、', ';', '；']))
        .map(normalize_contributor)
        .filter(|hint| !hint.is_empty())
        .collect();
    if hints.is_empty() {
        return;
    }
    let score = |item: &BookItem| {
        if candidate_title_relevance_score(name.title, item) < 850.0 {
            return 0;
        }
        let author = normalize_contributor(&item.author);
        let narrator = normalize_contributor(item.narrator.as_deref().unwrap_or_default());
        hints
            .iter()
            .filter(|hint| author.contains(hint.as_str()) || narrator.contains(hint.as_str()))
            .count()
    };
    results
        .items
        .sort_by_cached_key(|item| std::cmp::Reverse(score(item)));
}

fn normalize_contributor(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(super) fn collect_candidates(
    query: &str,
    source_id: &str,
    results: SearchResult,
    details: &mut HashMap<String, BookDetail>,
    candidates: &mut Vec<serde_json::Value>,
) -> Result<()> {
    if let Some(item) = results.items.first() {
        details.insert(source_id.to_owned(), BookDetail::from(item));
    }
    #[derive(serde::Serialize)]
    struct Candidate<'a> {
        #[serde(flatten)]
        item: &'a BookItem,
        source_id: &'a str,
        source_result_rank: usize,
        title_relevance_score: f64,
    }
    for (rank, item) in results.items.iter().enumerate() {
        candidates.push(
            serde_json::to_value(Candidate {
                item,
                source_id,
                source_result_rank: rank + 1,
                title_relevance_score: candidate_title_relevance_score(query, item),
            })
            .map_err(|error| TingError::SerializationError(error.to_string()))?,
        );
    }
    Ok(())
}

pub(super) fn rank_candidates(candidates: &mut [serde_json::Value]) {
    candidates.sort_by(|left, right| {
        candidate_value_score(right)
            .total_cmp(&candidate_value_score(left))
            .then_with(|| candidate_value_rank(left).cmp(&candidate_value_rank(right)))
            .then_with(|| candidate_value_title(left).cmp(&candidate_value_title(right)))
    });
}

pub(super) fn candidate_title_relevance_score(query: &str, item: &BookItem) -> f64 {
    let query = normalize_title_for_match(query);
    let title = normalize_title_for_match(&item.title);
    if query.is_empty() || title.is_empty() {
        return 0.0;
    }

    if title == query {
        return 1000.0;
    }
    if title.contains(&query) {
        return 900.0 + (query.chars().count() as f64 / title.chars().count() as f64);
    }
    if query.contains(&title) {
        return 850.0 + (title.chars().count() as f64 / query.chars().count() as f64);
    }

    let query_chars: HashSet<char> = query.chars().collect();
    let title_chars: HashSet<char> = title.chars().collect();
    let overlap = query_chars.intersection(&title_chars).count() as f64;
    if overlap == 0.0 {
        return 0.0;
    }

    let query_coverage = overlap / query_chars.len().max(1) as f64;
    let title_coverage = overlap / title_chars.len().max(1) as f64;
    500.0 * ((query_coverage * 0.7) + (title_coverage * 0.3))
}

fn normalize_title_for_match(value: &str) -> String {
    value
        .split('丨')
        .next()
        .unwrap_or(value)
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '-' | '_'
                        | '.'
                        | '·'
                        | ':'
                        | '：'
                        | '|'
                        | '/'
                        | '\\'
                        | '('
                        | ')'
                        | '（'
                        | '）'
                        | '['
                        | ']'
                        | '【'
                        | '】'
                        | '{'
                        | '}'
                        | '《'
                        | '》'
                        | '<'
                        | '>'
                        | '，'
                        | ','
                        | '。'
                        | '！'
                        | '!'
                        | '?'
                        | '？'
                )
        })
        .collect()
}

fn candidate_value_score(value: &serde_json::Value) -> f64 {
    value
        .get("title_relevance_score")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

fn candidate_value_rank(value: &serde_json::Value) -> u64 {
    value
        .get("source_result_rank")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::MAX)
}

fn candidate_value_title(value: &serde_json::Value) -> String {
    value
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(super) fn merge_source_results(
    query: &str,
    config: &crate::db::models::ScraperConfig,
    source_results: &HashMap<String, BookDetail>,
) -> BookDetail {
    let mut final_detail = BookDetail::default();

    let select = |specific: Option<&Vec<String>>| {
        specific
            .into_iter()
            .flatten()
            .chain(config.default_sources.iter())
            .filter_map(|source| source_results.get(source.trim()))
            .collect::<Vec<_>>()
    };
    let defaults = select(None);
    final_detail.title = defaults
        .iter()
        .find(|detail| !detail.title.is_empty())
        .map(|detail| detail.title.clone())
        .or_else(|| {
            source_results
                .iter()
                .filter(|(_, detail)| !detail.title.is_empty())
                .min_by_key(|(source, _)| *source)
                .map(|(_, detail)| detail.title.clone())
        })
        .unwrap_or_else(|| query.to_owned());
    final_detail.author = select(config.author_sources.as_ref())
        .iter()
        .find(|detail| !detail.author.is_empty())
        .map(|detail| detail.author.clone())
        .unwrap_or_default();
    final_detail.narrator = select(config.narrator_sources.as_ref())
        .iter()
        .find_map(|detail| detail.narrator.clone());
    final_detail.cover_url = select(config.cover_sources.as_ref())
        .iter()
        .find_map(|detail| detail.cover_url.clone());
    final_detail.intro = select(config.intro_sources.as_ref())
        .iter()
        .find(|detail| !detail.intro.is_empty())
        .map(|detail| detail.intro.clone())
        .unwrap_or_default();
    final_detail.tags = select(config.tags_sources.as_ref())
        .iter()
        .find(|detail| !detail.tags.is_empty())
        .map(|detail| detail.tags.clone())
        .unwrap_or_default();

    // Remaining fields from default sources
    for source in &config.default_sources {
        if let Some(detail) = source_results.get(source.trim()) {
            if final_detail.subtitle.is_none() {
                final_detail.subtitle = detail.subtitle.clone();
            }
            if final_detail.published_year.is_none() {
                final_detail.published_year = detail.published_year.clone();
            }
            if final_detail.published_date.is_none() {
                final_detail.published_date = detail.published_date.clone();
            }
            if final_detail.publisher.is_none() {
                final_detail.publisher = detail.publisher.clone();
            }
            if final_detail.isbn.is_none() {
                final_detail.isbn = detail.isbn.clone();
            }
            if final_detail.asin.is_none() {
                final_detail.asin = detail.asin.clone();
            }
            if final_detail.language.is_none() {
                final_detail.language = detail.language.clone();
            }
            if detail.explicit {
                final_detail.explicit = true;
            }
            if detail.abridged {
                final_detail.abridged = true;
            }
            if final_detail.chapter_title_template.is_none() {
                final_detail.chapter_title_template = detail.chapter_title_template.clone();
            }
            if final_detail.chapter_titles.is_empty() && !detail.chapter_titles.is_empty() {
                final_detail.chapter_titles = detail.chapter_titles.clone();
            }
        }
    }

    final_detail
}
