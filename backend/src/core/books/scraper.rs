//! Book search, scraping and search-result caching.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::core::app::error::{Result, TingError};
use crate::plugin::manager::PluginManager;
use crate::plugin::types::scraper::{BookDetail, BookItem, SearchResult};
use ting_plugin_contract::capability::MetadataOperation;
use ting_plugin_contract::scraper::{
    ChapterCandidate, ScraperResult, SearchContext, SearchMode, SearchPage, SearchRequest,
};

const AGGREGATE_CANDIDATE_PAGE_SIZE: u32 = 20;
const MAX_SEARCH_CACHE_ENTRIES: usize = 100;

#[derive(Clone, PartialEq, Eq, Hash)]
struct SearchCacheKey {
    source_id: String,
    params: BTreeMap<String, String>,
    page: u32,
    page_size: u32,
}

struct SearchCacheEntry {
    result: SearchResult,
    expires_at: Instant,
}

/// Scraper service for coordinating scraper plugin operations
pub struct ScraperService {
    plugin_manager: Arc<PluginManager>,
    search_cache: RwLock<HashMap<SearchCacheKey, SearchCacheEntry>>,
    cache_ttl: Duration,
}

impl ScraperService {
    pub fn new(plugin_manager: Arc<PluginManager>) -> Self {
        Self::with_cache_ttl(plugin_manager, Duration::from_secs(300))
    }

    pub fn with_cache_ttl(plugin_manager: Arc<PluginManager>, cache_ttl: Duration) -> Self {
        Self {
            plugin_manager,
            search_cache: RwLock::new(HashMap::new()),
            cache_ttl,
        }
    }

    pub async fn get_sources(&self) -> Vec<crate::api::models::ScraperSourceInfo> {
        use crate::plugin::types::PluginState;
        self.plugin_manager
            .find_plugins_by_capability_kind("metadata_provider")
            .await
            .into_iter()
            .filter_map(|info| {
                let aggregate_auto_scrape = info.capabilities.iter().any(|capability| {
                    matches!(capability, crate::plugin::types::PluginCapability::MetadataProvider(cap)
                        if cap.aggregate_auto_scrape)
                });
                info.scraper
                    .clone()
                    .map(|capabilities| (info, capabilities, aggregate_auto_scrape))
            })
            .map(|info| {
                let (info, capabilities, aggregate_auto_scrape) = info;
                let search_fields = capabilities.search_fields.clone();
                let result_fields = capabilities.result_fields.clone();
                let result_field_labels = capabilities.result_field_labels.clone();
                crate::api::models::ScraperSourceInfo {
                    id: info.id,
                    name: info.name,
                    description: Some(info.description),
                    version: info.version,
                    enabled: matches!(info.state, PluginState::Active),
                    auto_scrape: capabilities.auto_scrape,
                    aggregate_auto_scrape,
                    search_fields,
                    result_fields,
                    result_field_labels,
                }
            })
            .collect()
    }

    async fn auto_source_ids(&self) -> HashSet<String> {
        self.get_sources()
            .await
            .into_iter()
            .filter(|source| source.enabled && source.auto_scrape)
            .map(|source| source.id)
            .collect()
    }

    async fn select_scraper(&self, source: Option<&str>) -> Result<String> {
        if let Some(source_id) = source {
            let sources = self.get_sources().await;
            if sources.iter().any(|s| s.id == source_id && s.enabled) {
                return Ok(source_id.to_string());
            }
            if let Some(s) = sources.iter().find(|s| s.name == source_id && s.enabled) {
                return Ok(s.id.clone());
            }
            return Err(TingError::PluginNotFound(format!(
                "Scraper source '{}' not found or not enabled",
                source_id
            )));
        }

        let sources = self.get_sources().await;
        let active_sources: Vec<_> = sources.into_iter().filter(|s| s.enabled).collect();
        if active_sources.is_empty() {
            return Err(TingError::PluginNotFound(
                "No active scraper plugins available".to_string(),
            ));
        }
        if let Some(source) = active_sources
            .iter()
            .find(|source| !source.aggregate_auto_scrape)
        {
            return Ok(source.id.clone());
        }
        Ok(active_sources[0].id.clone())
    }

    fn search_request(
        title: Option<&str>,
        author: Option<&str>,
        narrator: Option<&str>,
        page: u32,
        page_size: u32,
        filters: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        Self::search_request_with_context(
            title,
            author,
            narrator,
            (page, page_size),
            filters,
            Vec::new(),
            None,
        )
    }

    fn search_request_with_context(
        title: Option<&str>,
        author: Option<&str>,
        narrator: Option<&str>,
        pagination: (u32, u32),
        filters: serde_json::Map<String, serde_json::Value>,
        chapter_candidates: Vec<ChapterCandidate>,
        context: Option<SearchContext>,
    ) -> Result<serde_json::Value> {
        let request = SearchRequest {
            title: title.map(str::to_owned),
            author: author.map(str::to_owned),
            narrator: narrator.map(str::to_owned),
            page: pagination.0,
            page_size: pagination.1,
            filters,
            chapter_candidates,
            context,
        };
        request
            .validate()
            .map_err(|error| TingError::ValidationError(error.into()))?;
        serde_json::to_value(request)
            .map_err(|error| TingError::SerializationError(error.to_string()))
    }

    fn parse_search_page(value: serde_json::Value) -> Result<SearchResult> {
        let page: SearchPage = serde_json::from_value(value).map_err(|error| {
            TingError::DeserializationError(format!("Invalid scraper search page: {error}"))
        })?;
        page.validate()
            .map_err(|error| TingError::DeserializationError(error.into()))?;
        Ok(SearchResult {
            items: page
                .items
                .into_iter()
                .map(Self::book_item_from_result)
                .collect(),
            total: page.total,
            has_more: page.has_more,
            page: page.page,
            page_size: page.page_size,
        })
    }

    fn book_item_from_result(result: ScraperResult) -> BookItem {
        BookItem {
            id: result.id,
            source_url: result.source_url,
            title: result.title,
            author: result.author.unwrap_or_default(),
            narrator: result.narrator,
            cover_url: result.cover_url,
            intro: result.intro,
            subtitle: result.subtitle,
            publisher: result.publisher,
            language: result.language,
            genre: result.genre,
            published_year: result.published_year.map(|year| year.to_string()),
            published_date: result.published_date,
            isbn: result.isbn,
            asin: result.asin,
            explicit: result.explicit,
            abridged: result.abridged,
            tags: result.tags,
            duration: result.duration,
            score: result.score,
            chapter_title_template: result.chapter_title_template,
            chapter_titles: result.chapter_titles,
        }
    }

    fn take_chapter_candidates(
        scanner_context: &mut Option<serde_json::Value>,
    ) -> Vec<ChapterCandidate> {
        let chapters = scanner_context
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|object| {
                object.remove("directory");
                object.remove("chapters")
            });
        let Some(serde_json::Value::Array(chapters)) = chapters else {
            return Vec::new();
        };
        chapters
            .into_iter()
            .enumerate()
            .filter_map(|(index, chapter)| {
                let title = chapter
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| chapter.get("filename").and_then(serde_json::Value::as_str))?
                    .trim();
                if title.is_empty() {
                    return None;
                }
                Some(ChapterCandidate {
                    id: index.to_string(),
                    title: title.to_string(),
                })
            })
            .collect()
    }

    pub async fn search(
        &self,
        query: &str,
        author: Option<&str>,
        narrator: Option<&str>,
        source: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResult> {
        if query.trim().is_empty() {
            return Err(TingError::ValidationError(
                "Search query cannot be empty".to_string(),
            ));
        }

        let clean_query = query
            .split('丨')
            .next()
            .unwrap_or(query)
            .split('|')
            .next()
            .unwrap_or(query)
            .trim();
        let clean_query = if clean_query.is_empty() {
            query
        } else {
            clean_query
        };

        let source_id = self.select_scraper(source).await?;
        let author = author.map(str::trim).filter(|value| !value.is_empty());
        let narrator = narrator.map(str::trim).filter(|value| !value.is_empty());
        let mut normalized_params = BTreeMap::from([("title".into(), clean_query.to_owned())]);
        if let Some(author) = author {
            normalized_params.insert("author".into(), author.to_owned());
        }
        if let Some(narrator) = narrator {
            normalized_params.insert("narrator".into(), narrator.to_owned());
        }
        let cache_key = SearchCacheKey {
            source_id: source_id.clone(),
            params: normalized_params,
            page,
            page_size,
        };
        if let Some(result) = self.get_cached_search(&cache_key) {
            tracing::debug!(source_id, "Scraper search cache hit");
            return Ok(result);
        }

        if self.is_aggregate_source(&source_id).await {
            let result = self
                .search_aggregate_source(&source_id, &cache_key.params, page, page_size)
                .await?;
            self.cache_search_result(cache_key, result.clone());
            return Ok(result);
        }

        tracing::debug!(
            "Searching for {}, calling plugin {}",
            clean_query,
            source_id
        );
        let params = Self::search_request(
            Some(clean_query),
            author,
            narrator,
            page,
            page_size,
            Default::default(),
        )?;

        let result = match self
            .plugin_manager
            .invoke_metadata(&source_id, MetadataOperation::Search, params.clone())
            .await
        {
            Ok(result) => result,
            Err(e) => {
                if source.is_some() {
                    return Err(
                        e.with_plugin_execution_context(format!("Scraper '{}' failed", source_id))
                    );
                }
                tracing::debug!("Trying another scraper as fallback");
                let fallback_source = self.try_fallback_scraper(&source_id).await?;
                let value = self
                    .plugin_manager
                    .invoke_metadata(&fallback_source, MetadataOperation::Search, params)
                    .await
                    .map_err(|e| {
                        e.with_plugin_execution_context("All scrapers failed. Last error")
                    })?;
                // A fallback response must not be cached as the primary source.
                return Self::parse_search_page(value);
            }
        };

        let result = Self::parse_search_page(result)?;
        self.cache_search_result(cache_key, result.clone());
        Ok(result)
    }

    pub async fn search_with_params(
        &self,
        search_params: &HashMap<String, String>,
        source: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResult> {
        let mut normalized_params = BTreeMap::new();
        for (key, value) in search_params {
            if !value.trim().is_empty() {
                normalized_params.insert(key.clone(), value.trim().to_string());
            }
        }

        if normalized_params.is_empty() {
            return Err(TingError::ValidationError(
                "At least one search parameter is required".to_string(),
            ));
        }

        let query = normalized_params.get("title").cloned();
        let clean_query = query.map(|query| {
            let clean_query = query.split('|').next().unwrap_or(&query).trim().to_string();
            if clean_query.is_empty() {
                query
            } else {
                clean_query
            }
        });
        if let Some(clean_query) = &clean_query {
            normalized_params.insert("title".to_string(), clean_query.clone());
        }
        let author = normalized_params
            .get("author")
            .filter(|s| !s.trim().is_empty())
            .cloned();
        let narrator = normalized_params
            .get("narrator")
            .filter(|s| !s.trim().is_empty())
            .cloned();

        let source_id = self.select_scraper(source).await?;
        let cache_key = SearchCacheKey {
            source_id: source_id.clone(),
            params: normalized_params.clone(),
            page,
            page_size,
        };
        if let Some(result) = self.get_cached_search(&cache_key) {
            tracing::debug!(source_id, "Scraper search cache hit");
            return Ok(result);
        }

        if self.is_aggregate_source(&source_id).await {
            let search_result = self
                .search_aggregate_source(&source_id, &normalized_params, page, page_size)
                .await?;
            self.cache_search_result(cache_key, search_result.clone());
            return Ok(search_result);
        }

        let filters = normalized_params
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "title" | "author" | "narrator"))
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect();
        let params = Self::search_request(
            clean_query.as_deref(),
            author.as_deref(),
            narrator.as_deref(),
            page,
            page_size,
            filters,
        )?;

        let result = match self
            .plugin_manager
            .invoke_metadata(&source_id, MetadataOperation::Search, params.clone())
            .await
        {
            Ok(result) => result,
            Err(e) => {
                if source.is_some() {
                    return Err(
                        e.with_plugin_execution_context(format!("Scraper '{}' failed", source_id))
                    );
                }
                let fallback_source = self.try_fallback_scraper(&source_id).await?;
                let value = self
                    .plugin_manager
                    .invoke_metadata(&fallback_source, MetadataOperation::Search, params)
                    .await
                    .map_err(|e| {
                        e.with_plugin_execution_context("All scrapers failed. Last error")
                    })?;
                return Self::parse_search_page(value);
            }
        };

        let result = Self::parse_search_page(result)?;
        self.cache_search_result(cache_key, result.clone());
        Ok(result)
    }

    async fn try_fallback_scraper(&self, failed_source: &str) -> Result<String> {
        let sources = self.get_sources().await;
        let fallback = sources
            .into_iter()
            .find(|s| s.enabled && s.id != failed_source && !s.aggregate_auto_scrape);
        match fallback {
            Some(source) => {
                tracing::debug!("Using fallback scraper: {}", source.id);
                Ok(source.id)
            }
            None => Err(TingError::PluginNotFound(
                "No fallback scraper available".to_string(),
            )),
        }
    }

    async fn is_aggregate_source(&self, source_id: &str) -> bool {
        self.aggregate_source_ids().await.contains(source_id)
    }

    async fn clean_query_with_aggregate_sources(
        &self,
        query: &str,
        aggregate_sources: &[String],
        context: Option<&serde_json::Value>,
    ) -> String {
        let trimmed_query = query.trim();
        if trimmed_query.is_empty() || aggregate_sources.is_empty() {
            return query.to_string();
        }

        for aggregate_source in aggregate_sources {
            let Ok(cleanup_context) = Self::search_request_with_context(
                Some(trimmed_query),
                None,
                None,
                (1, 1),
                Default::default(),
                Vec::new(),
                Some(SearchContext {
                    mode: SearchMode::TitleCleanup,
                    candidates: Vec::new(),
                    merged_metadata: None,
                    scanner_context: context.cloned(),
                    scraper_query: None,
                }),
            ) else {
                break;
            };

            match self
                .plugin_manager
                .invoke_metadata(aggregate_source, MetadataOperation::Search, cleanup_context)
                .await
            {
                Ok(value) => {
                    if let Some(detail) = Self::detail_from_plugin_value(value, trimmed_query) {
                        let cleaned = detail.title.trim();
                        if !cleaned.is_empty() {
                            if cleaned != trimmed_query {
                                tracing::info!(
                                    source_id = %aggregate_source,
                                    original_query = %trimmed_query,
                                    cleaned_query = %cleaned,
                                    "AI title cleanup changed scraper query"
                                );
                            }
                            return cleaned.to_string();
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    source_id = %aggregate_source,
                    error = %e,
                    message_key = "scraper.title_cleanup.failed",
                    message_params = %serde_json::json!({
                        "source_id": aggregate_source,
                        "error": e.to_string(),
                    }),
                    "Aggregate scraper title cleanup failed"
                ),
            }
        }

        query.to_string()
    }

    fn source_id_scope_param(value: Option<&String>) -> Option<HashSet<String>> {
        value.map(|raw| {
            if let Ok(ids) = serde_json::from_str::<Vec<String>>(raw) {
                return ids
                    .into_iter()
                    .map(|id| id.trim().to_string())
                    .filter(|id| !id.is_empty())
                    .collect();
            }

            raw.split([',', ';', '\n'])
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect()
        })
    }

    async fn search_aggregate_source(
        &self,
        aggregate_source: &String,
        search_params: &BTreeMap<String, String>,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResult> {
        use serde_json::json;

        let query = search_params
            .get("title")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                TingError::ValidationError("Aggregate scraper search requires a title".to_string())
            })?;
        let author = search_params
            .get("author")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        let narrator = search_params
            .get("narrator")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());

        let aggregate_sources = self.aggregate_source_ids().await;
        let candidate_scope =
            Self::source_id_scope_param(search_params.get("candidate_sources")).unwrap_or_default();
        let candidate_source_ids: Vec<String> = self
            .get_sources()
            .await
            .into_iter()
            .filter(|source| {
                source.enabled
                    && source.auto_scrape
                    && source.id.as_str() != aggregate_source.as_str()
                    && !aggregate_sources.contains(&source.id)
                    && candidate_scope.contains(&source.id)
            })
            .map(|source| source.id)
            .collect();

        let mut source_results: HashMap<String, BookDetail> = HashMap::new();
        let mut candidate_items = Vec::new();

        for source_id in &candidate_source_ids {
            match self
                .search_source_direct(
                    source_id,
                    query,
                    author,
                    narrator,
                    1,
                    AGGREGATE_CANDIDATE_PAGE_SIZE,
                )
                .await
            {
                Ok(search_res) => {
                    if let Some(item) = search_res.items.first() {
                        let detail = Self::detail_from_item(item);
                        source_results.insert(source_id.clone(), detail);
                    }

                    for (rank, item) in search_res.items.iter().enumerate() {
                        let relevance_score = Self::candidate_title_relevance_score(query, item);
                        let mut item_json =
                            serde_json::to_value(item).unwrap_or_else(|_| serde_json::json!({}));
                        if let Some(object) = item_json.as_object_mut() {
                            object.insert(
                                "source_id".to_string(),
                                serde_json::Value::String(source_id.clone()),
                            );
                            object.insert(
                                "source_result_rank".to_string(),
                                serde_json::json!(rank + 1),
                            );
                            object.insert(
                                "title_relevance_score".to_string(),
                                serde_json::json!(relevance_score),
                            );
                        }
                        candidate_items.push(item_json);
                    }
                }
                Err(e) => tracing::warn!(
                    source_id = %source_id,
                    aggregate_source = %aggregate_source,
                    error = %e,
                    message_key = "scraper.search.failed",
                    message_params = %serde_json::json!({
                        "source_id": source_id,
                        "error": e.to_string(),
                    }),
                    "Scraper search failed while preparing aggregate search"
                ),
            }
        }

        candidate_items.sort_by(|left, right| {
            let right_score = Self::candidate_value_score(right);
            let left_score = Self::candidate_value_score(left);
            right_score
                .partial_cmp(&left_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    Self::candidate_value_rank(left).cmp(&Self::candidate_value_rank(right))
                })
                .then_with(|| {
                    Self::candidate_value_title(left).cmp(&Self::candidate_value_title(right))
                })
        });
        candidate_items.truncate(20);

        let merge_config = crate::db::models::ScraperConfig {
            default_sources: candidate_source_ids,
            ..Default::default()
        };
        let merged_metadata = self.merge_source_results(query, &merge_config, &source_results);

        let aggregate_context = Self::search_request_with_context(
            Some(query),
            author,
            narrator,
            (page, page_size),
            Default::default(),
            Vec::new(),
            Some(SearchContext {
                mode: SearchMode::Aggregate,
                candidates: candidate_items,
                merged_metadata: Some(json!(merged_metadata)),
                scanner_context: Some(json!({
                "current_metadata": {
                    "title": query,
                    "author": author,
                    "narrator": narrator,
                }
                })),
                scraper_query: None,
            }),
        )?;

        let value = self
            .plugin_manager
            .invoke_metadata(
                aggregate_source,
                MetadataOperation::Search,
                aggregate_context,
            )
            .await
            .map_err(|e| {
                e.with_plugin_execution_context(format!(
                    "Aggregate scraper '{}' failed",
                    aggregate_source
                ))
            })?;

        Self::parse_search_page(value)
    }

    async fn search_source_direct(
        &self,
        source_id: &String,
        query: &str,
        author: Option<&str>,
        narrator: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> Result<SearchResult> {
        let params = Self::search_request(
            Some(query),
            author,
            narrator,
            page,
            page_size,
            Default::default(),
        )?;

        let value = self
            .plugin_manager
            .invoke_metadata(source_id, MetadataOperation::Search, params)
            .await
            .map_err(|e| {
                e.with_plugin_execution_context(format!(
                    "Scraper '{}' failed while preparing aggregate candidates",
                    source_id
                ))
            })?;

        Self::parse_search_page(value)
    }

    fn get_cached_search(&self, key: &SearchCacheKey) -> Option<SearchResult> {
        let mut cache = self.search_cache.write().ok()?;
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);
        cache.get(key).map(|entry| entry.result.clone())
    }

    fn cache_search_result(&self, key: SearchCacheKey, result: SearchResult) {
        if self.cache_ttl.is_zero() {
            return;
        }
        let Some(expires_at) = Instant::now().checked_add(self.cache_ttl) else {
            return;
        };
        if let Ok(mut cache) = self.search_cache.write() {
            let now = Instant::now();
            cache.retain(|_, entry| entry.expires_at > now);
            if cache.len() >= MAX_SEARCH_CACHE_ENTRIES
                && !cache.contains_key(&key)
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, entry)| entry.expires_at)
                    .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
            cache.insert(key, SearchCacheEntry { result, expires_at });
        }
    }

    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.search_cache.write() {
            cache.clear();
        }
        tracing::info!("Scraper service cache cleared");
    }

    pub fn get_cache_stats(&self) -> usize {
        let Ok(mut cache) = self.search_cache.write() else {
            return 0;
        };
        let now = Instant::now();
        cache.retain(|_, entry| entry.expires_at > now);
        cache.len()
    }

    /// Scrape book metadata using the provided configuration strategy
    pub async fn scrape_book_metadata(
        &self,
        query: &str,
        config: &crate::db::models::ScraperConfig,
    ) -> Result<BookDetail> {
        self.scrape_book_metadata_with_context(query, config, None)
            .await
    }

    /// Scrape book metadata and optionally pass scanner context to aggregate providers.
    pub async fn scrape_book_metadata_with_context(
        &self,
        query: &str,
        config: &crate::db::models::ScraperConfig,
        context: Option<serde_json::Value>,
    ) -> Result<BookDetail> {
        let mut all_sources = HashSet::new();

        for s in &config.default_sources {
            all_sources.insert(s.clone());
        }
        if let Some(sources) = &config.author_sources {
            sources.iter().for_each(|s| {
                all_sources.insert(s.clone());
            });
        }
        if let Some(sources) = &config.narrator_sources {
            sources.iter().for_each(|s| {
                all_sources.insert(s.clone());
            });
        }
        if let Some(sources) = &config.cover_sources {
            sources.iter().for_each(|s| {
                all_sources.insert(s.clone());
            });
        }
        if let Some(sources) = &config.intro_sources {
            sources.iter().for_each(|s| {
                all_sources.insert(s.clone());
            });
        }
        if let Some(sources) = &config.tags_sources {
            sources.iter().for_each(|s| {
                all_sources.insert(s.clone());
            });
        }

        let auto_sources = self.auto_source_ids().await;
        let aggregate_sources = self.aggregate_source_ids().await;
        all_sources.retain(|source| auto_sources.contains(source));

        if all_sources.is_empty() {
            if let Some(source) = auto_sources
                .iter()
                .find(|source| !aggregate_sources.contains(*source))
            {
                all_sources.insert(source.clone());
            } else {
                return Err(TingError::NotFound(
                    "No active automatic scraper plugins available".to_string(),
                ));
            }
        }

        let aggregate_sources_to_run: Vec<String> = all_sources
            .iter()
            .filter(|source| aggregate_sources.contains(*source))
            .cloned()
            .collect();
        let scrape_sources: HashSet<String> = all_sources
            .iter()
            .filter(|source| !aggregate_sources.contains(*source))
            .cloned()
            .collect();
        let scraper_query = self
            .clean_query_with_aggregate_sources(query, &aggregate_sources_to_run, context.as_ref())
            .await;

        let mut source_results: HashMap<String, BookDetail> = HashMap::new();
        let mut candidate_items = Vec::new();
        for source_id in scrape_sources {
            match self
                .search(
                    &scraper_query,
                    None,
                    None,
                    Some(&source_id),
                    1,
                    AGGREGATE_CANDIDATE_PAGE_SIZE,
                )
                .await
            {
                Ok(search_res) => {
                    if let Some(item) = search_res.items.first() {
                        let detail = Self::detail_from_item(item);
                        source_results.insert(source_id.clone(), detail);
                    }

                    for (rank, item) in search_res.items.iter().enumerate() {
                        let relevance_score =
                            Self::candidate_title_relevance_score(&scraper_query, item);
                        let mut item_json =
                            serde_json::to_value(item).unwrap_or_else(|_| serde_json::json!({}));
                        if let Some(object) = item_json.as_object_mut() {
                            object.insert(
                                "source_id".to_string(),
                                serde_json::Value::String(source_id.clone()),
                            );
                            object.insert(
                                "source_result_rank".to_string(),
                                serde_json::json!(rank + 1),
                            );
                            object.insert(
                                "title_relevance_score".to_string(),
                                serde_json::json!(relevance_score),
                            );
                        }
                        candidate_items.push(item_json);
                    }
                }
                Err(e) => tracing::warn!(
                    source_id = %source_id,
                    error = %e,
                    message_key = "scraper.search.failed",
                    message_params = %serde_json::json!({
                        "source_id": source_id,
                        "error": e.to_string(),
                    }),
                    "Scraper search failed"
                ),
            }
        }

        candidate_items.sort_by(|left, right| {
            let right_score = Self::candidate_value_score(right);
            let left_score = Self::candidate_value_score(left);
            right_score
                .partial_cmp(&left_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    Self::candidate_value_rank(left).cmp(&Self::candidate_value_rank(right))
                })
                .then_with(|| {
                    Self::candidate_value_title(left).cmp(&Self::candidate_value_title(right))
                })
        });

        if source_results.is_empty() && aggregate_sources_to_run.is_empty() {
            return Err(TingError::NotFound(
                "No metadata found from any scraper".to_string(),
            ));
        }

        let final_detail = self.merge_source_results(query, config, &source_results);

        if !aggregate_sources_to_run.is_empty() {
            let mut scanner_context = context;
            let chapter_candidates = Self::take_chapter_candidates(&mut scanner_context);
            let batches: Vec<&[ChapterCandidate]> = if chapter_candidates.is_empty() {
                vec![&[]]
            } else {
                chapter_candidates.chunks(500).collect()
            };

            for aggregate_source in aggregate_sources_to_run {
                let mut selected = None;
                let mut cleaned_titles = Vec::new();
                for batch in &batches {
                    let aggregate_request = Self::search_request_with_context(
                        Some(query),
                        None,
                        None,
                        (1, 1),
                        Default::default(),
                        batch.to_vec(),
                        Some(SearchContext {
                            mode: SearchMode::Aggregate,
                            candidates: candidate_items.clone(),
                            merged_metadata: Some(serde_json::json!(final_detail)),
                            scanner_context: scanner_context.clone(),
                            scraper_query: Some(scraper_query.clone()),
                        }),
                    )?;
                    match self
                        .plugin_manager
                        .invoke_metadata(
                            &aggregate_source,
                            MetadataOperation::Search,
                            aggregate_request,
                        )
                        .await
                    {
                        Ok(value) => {
                            if let Some(mut detail) = Self::detail_from_plugin_value(value, query) {
                                if !batch.is_empty() {
                                    if detail.chapter_titles.len() != batch.len() {
                                        detail.chapter_titles =
                                            batch.iter().map(|c| c.title.clone()).collect();
                                    }
                                    cleaned_titles.extend(detail.chapter_titles.iter().cloned());
                                }
                                if selected.is_none() {
                                    selected = Some(detail);
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                source_id = %aggregate_source,
                                %error,
                                "Aggregate scraper failed for a chapter batch"
                            );
                            cleaned_titles.extend(batch.iter().map(|c| c.title.clone()));
                        }
                    }
                }
                if let Some(mut detail) = selected {
                    if !chapter_candidates.is_empty() {
                        detail.chapter_titles = cleaned_titles;
                    }
                    return Ok(detail);
                }
            }
        }

        if source_results.is_empty() {
            return Err(TingError::NotFound(
                "No metadata found from any scraper".to_string(),
            ));
        }

        Ok(final_detail)
    }

    fn candidate_title_relevance_score(query: &str, item: &BookItem) -> f64 {
        let query = Self::normalize_title_for_match(query);
        let title = Self::normalize_title_for_match(&item.title);
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

    async fn aggregate_source_ids(&self) -> HashSet<String> {
        self.plugin_manager
            .find_capabilities_by_kind("metadata_provider")
            .await
            .into_iter()
            .filter(|registration| {
                matches!(&registration.capability,
                    crate::plugin::types::PluginCapability::MetadataProvider(cap)
                    if cap.aggregate_auto_scrape)
            })
            .map(|registration| registration.plugin_id)
            .collect()
    }

    fn detail_from_item(item: &BookItem) -> BookDetail {
        BookDetail {
            id: item.id.clone(),
            title: item.title.clone(),
            author: item.author.clone(),
            narrator: item.narrator.clone(),
            cover_url: item.cover_url.clone(),
            intro: item.intro.clone().unwrap_or_default(),
            tags: item.tags.clone(),
            chapter_count: 0,
            duration: item.duration,
            subtitle: item.subtitle.clone(),
            published_year: item.published_year.clone(),
            published_date: item.published_date.clone(),
            publisher: item.publisher.clone(),
            isbn: item.isbn.clone(),
            asin: item.asin.clone(),
            language: item.language.clone(),
            explicit: item.explicit.unwrap_or(false),
            abridged: item.abridged.unwrap_or(false),
            genre: item.genre.clone(),
            chapter_title_template: item.chapter_title_template.clone(),
            chapter_titles: item.chapter_titles.clone(),
        }
    }

    fn detail_from_plugin_value(value: serde_json::Value, query: &str) -> Option<BookDetail> {
        let _ = query;
        Self::parse_search_page(value)
            .ok()?
            .items
            .first()
            .map(Self::detail_from_item)
    }

    fn merge_source_results(
        &self,
        query: &str,
        config: &crate::db::models::ScraperConfig,
        source_results: &HashMap<String, BookDetail>,
    ) -> BookDetail {
        let mut final_detail = BookDetail {
            id: None,
            title: query.to_string(),
            author: String::new(),
            narrator: None,
            cover_url: None,
            intro: String::new(),
            tags: vec![],
            chapter_count: 0,
            duration: None,
            subtitle: None,
            published_year: None,
            published_date: None,
            publisher: None,
            isbn: None,
            asin: None,
            language: None,
            explicit: false,
            abridged: false,
            genre: None,
            chapter_title_template: None,
            chapter_titles: Vec::new(),
        };

        macro_rules! get_effective_sources {
            ($specific:expr) => {{
                let mut sources = Vec::new();
                if let Some(s) = $specific {
                    sources.extend(s);
                }
                for ds in &config.default_sources {
                    if !sources.contains(&ds) {
                        sources.push(ds);
                    }
                }
                sources
            }};
        }

        // Title from default sources
        for source in &config.default_sources {
            if let Some(detail) = source_results.get(source)
                && !detail.title.is_empty()
            {
                final_detail.title = detail.title.clone();
                break;
            }
        }
        if final_detail.title == query {
            for detail in source_results.values() {
                if !detail.title.is_empty() {
                    final_detail.title = detail.title.clone();
                    break;
                }
            }
        }

        // Per-field merge from specific + default sources
        for source in get_effective_sources!(config.author_sources.as_ref()) {
            if let Some(detail) = source_results.get(source)
                && !detail.author.is_empty()
            {
                final_detail.author = detail.author.clone();
                break;
            }
        }
        for source in get_effective_sources!(config.narrator_sources.as_ref()) {
            if let Some(detail) = source_results.get(source)
                && detail.narrator.is_some()
            {
                final_detail.narrator = detail.narrator.clone();
                break;
            }
        }
        for source in get_effective_sources!(config.cover_sources.as_ref()) {
            if let Some(detail) = source_results.get(source)
                && detail.cover_url.is_some()
            {
                final_detail.cover_url = detail.cover_url.clone();
                break;
            }
        }
        for source in get_effective_sources!(config.intro_sources.as_ref()) {
            if let Some(detail) = source_results.get(source)
                && !detail.intro.is_empty()
            {
                final_detail.intro = detail.intro.clone();
                break;
            }
        }
        for source in get_effective_sources!(config.tags_sources.as_ref()) {
            if let Some(detail) = source_results.get(source)
                && !detail.tags.is_empty()
            {
                final_detail.tags = detail.tags.clone();
                break;
            }
        }

        // Remaining fields from default sources
        for source in &config.default_sources {
            if let Some(detail) = source_results.get(source) {
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
}

#[cfg(test)]
#[path = "../../../tests/unit/core/books/scraper.rs"]
mod tests;
