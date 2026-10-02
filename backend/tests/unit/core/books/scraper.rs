use super::*;
use crate::plugin::manager::{PluginConfig, PluginEntry};
use crate::plugin::types::scraper::BookItem;
use crate::plugin::types::{
    Plugin, PluginContext, PluginInvocationContext, PluginMetadata, PluginState,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct CountingScraper {
    metadata: PluginMetadata,
    calls: AtomicUsize,
    fail: AtomicBool,
}

#[async_trait::async_trait]
impl Plugin for CountingScraper {
    fn metadata(&self) -> &PluginMetadata {
        &self.metadata
    }

    async fn initialize(&self, _: &PluginContext) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn invoke(&self, _: &str, input: Value, _: &PluginInvocationContext) -> Result<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(TingError::PluginExecutionError(
                "test search failure".into(),
            ));
        }
        let mut result = item(&self.metadata.id);
        result.author = "Author".into();
        Ok(json!({
            "ok": true,
            "data": {
                "items": [result],
                "page": input["page"],
                "page_size": input["page_size"],
                "total": 1,
                "has_more": false,
            }
        }))
    }
}

fn cache_test_manager(directory: &std::path::Path) -> Arc<PluginManager> {
    Arc::new(
        PluginManager::new(PluginConfig {
            plugin_dir: directory.join("plugins"),
            enable_hot_reload: false,
            max_memory_per_plugin: 128 * 1024 * 1024,
            max_execution_time: Duration::from_secs(30),
        })
        .unwrap(),
    )
}

async fn add_scraper(manager: &PluginManager, id: &str, aggregate: bool) -> Arc<CountingScraper> {
    let metadata = crate::plugin::types::metadata::parse_plugin_metadata_value(json!({
        "id": id, "name": id, "version": "2.0.0", "min_core_version": "2.0.0",
        "author": "Test", "description": {"en": "Search cache fixture"},
        "runtime": "wasm", "entry_point": "test.wasm",
        "capabilities": [{
            "id": "metadata.search", "kind": "metadata_provider", "operations": ["search"],
            "auto_scrape": true, "aggregate_auto_scrape": aggregate,
            "search_fields": [
                {"key": "title", "label": {"en": "Title"}, "required": true, "type": "text"},
                {"key": "author", "label": {"en": "Author"}, "required": false, "type": "text"},
                {"key": "narrator", "label": {"en": "Narrator"}, "required": false, "type": "text"},
                {"key": "category", "label": {"en": "Category"}, "required": false, "type": "text"}
            ],
            "result_fields": [{"key": "title", "label": {"en": "Title"}}],
            "filters_schema": {
                "type": "object", "properties": {"category": {"type": "string"}},
                "additionalProperties": false
            }
        }]
    }), "cache fixture").unwrap();
    let plugin = Arc::new(CountingScraper {
        metadata,
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
    });
    let mut entry = PluginEntry::new(plugin.metadata.clone(), plugin.clone());
    entry.state = PluginState::Active;
    manager
        .registry
        .write()
        .await
        .insert(plugin.metadata.instance_id(), entry);
    plugin
}

#[tokio::test]
async fn search_entry_points_share_cache_and_expired_results_are_refetched() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let plugin = add_scraper(&manager, "cache-source", false).await;
    let service = ScraperService::new(manager);
    let source = plugin.metadata.instance_id();
    service
        .search(" Book ", Some(" Author "), None, Some(&source), 1, 20)
        .await
        .unwrap();
    let params = HashMap::from([
        ("author".into(), "Author".into()),
        ("title".into(), " Book ".into()),
    ]);
    service
        .search_with_params(&params, Some(&source), 1, 20)
        .await
        .unwrap();
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 1);
    assert_eq!(service.get_cache_stats(), 1);
    {
        let mut cache = service.search_cache.write().unwrap();
        for entry in cache.values_mut() {
            entry.expires_at = Instant::now();
        }
    }
    service
        .search_with_params(&params, Some(&source), 1, 20)
        .await
        .unwrap();
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 2);
    service.clear_cache();
    assert_eq!(service.get_cache_stats(), 0);
    service
        .search_with_params(&params, Some(&source), 1, 20)
        .await
        .unwrap();
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn source_fields_filters_and_pagination_do_not_share_results() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let first = add_scraper(&manager, "first-source", false).await;
    let second = add_scraper(&manager, "second-source", false).await;
    let service = ScraperService::new(manager.clone());
    let first_id = first.metadata.instance_id();
    for (author, narrator, page, page_size) in [
        (None, None, 1, 20),
        (Some("Author"), None, 1, 20),
        (None, Some("Narrator"), 1, 20),
        (None, None, 2, 20),
        (None, None, 1, 10),
    ] {
        service
            .search("Book", author, narrator, Some(&first_id), page, page_size)
            .await
            .unwrap();
    }
    let second_id = second.metadata.instance_id();
    let response = service
        .search("Book", None, None, Some(&second_id), 1, 20)
        .await
        .unwrap();
    assert_eq!(response.items[0].title, "second-source");
    for category in ["audio", "ebook"] {
        let params = HashMap::from([
            ("title".into(), "Book".into()),
            ("category".into(), category.into()),
        ]);
        service
            .search_with_params(&params, Some(&first_id), 1, 20)
            .await
            .unwrap();
        service
            .search_with_params(&params, Some(&first_id), 1, 20)
            .await
            .unwrap();
    }
    assert_eq!(first.calls.load(Ordering::SeqCst), 7);
    assert_eq!(second.calls.load(Ordering::SeqCst), 1);
    manager
        .registry
        .write()
        .await
        .get_mut(&first_id)
        .unwrap()
        .state = PluginState::Unloaded;
    assert!(
        service
            .search("Book", None, None, Some(&first_id), 1, 20)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_search_is_not_cached_and_aggregate_search_is_cached() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let plugin = add_scraper(&manager, "retry-source", false).await;
    let aggregate = add_scraper(&manager, "aggregate-source", true).await;
    let service = ScraperService::new(manager);
    let source = plugin.metadata.instance_id();
    let params = HashMap::from([("title".into(), "Book".into())]);
    plugin.fail.store(true, Ordering::SeqCst);
    assert!(
        service
            .search_with_params(&params, Some(&source), 1, 20)
            .await
            .is_err()
    );
    assert_eq!(service.get_cache_stats(), 0);
    plugin.fail.store(false, Ordering::SeqCst);
    service
        .search_with_params(&params, Some(&source), 1, 20)
        .await
        .unwrap();
    service
        .search_with_params(&params, Some(&source), 1, 20)
        .await
        .unwrap();
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 2);
    let aggregate_id = aggregate.metadata.instance_id();
    service
        .search_with_params(&params, Some(&aggregate_id), 1, 20)
        .await
        .unwrap();
    service
        .search("Book", None, None, Some(&aggregate_id), 1, 20)
        .await
        .unwrap();
    assert_eq!(aggregate.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn search_cache_has_a_hard_entry_limit_and_zero_ttl_disables_it() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let plugin = add_scraper(&manager, "bounded-source", false).await;
    let source = plugin.metadata.instance_id();
    let service = ScraperService::new(manager.clone());
    for index in 0..=MAX_SEARCH_CACHE_ENTRIES {
        service
            .search(&format!("Book {index}"), None, None, Some(&source), 1, 20)
            .await
            .unwrap();
    }
    assert_eq!(service.get_cache_stats(), MAX_SEARCH_CACHE_ENTRIES);
    let service = ScraperService::with_cache_ttl(manager, Duration::ZERO);
    let before = plugin.calls.load(Ordering::SeqCst);
    for _ in 0..2 {
        service
            .search("Book", None, None, Some(&source), 1, 20)
            .await
            .unwrap();
    }
    assert_eq!(plugin.calls.load(Ordering::SeqCst), before + 2);
    assert_eq!(service.get_cache_stats(), 0);
}

fn item(title: &str) -> BookItem {
    BookItem {
        id: Some(title.to_string()),
        source_url: None,
        title: title.to_string(),
        author: String::new(),
        cover_url: None,
        intro: None,
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
        tags: Vec::new(),
        duration: None,
        chapter_title_template: None,
        chapter_titles: Vec::new(),
        score: None,
    }
}

#[test]
fn scores_exact_title_above_partial_matches() {
    let exact = ScraperService::candidate_title_relevance_score("三体", &item("三体"));
    let contains = ScraperService::candidate_title_relevance_score("三体", &item("三体全集"));
    let overlap = ScraperService::candidate_title_relevance_score("三体", &item("三体广播剧"));
    let unrelated = ScraperService::candidate_title_relevance_score("三体", &item("活着"));

    assert!(exact > contains);
    assert!(contains >= overlap);
    assert!(overlap > unrelated);
}

#[test]
fn normalizes_title_punctuation_for_matching() {
    let formatted = ScraperService::candidate_title_relevance_score("三体", &item("《三 体》"));
    let exact = ScraperService::candidate_title_relevance_score("三体", &item("三体"));

    assert_eq!(formatted, exact);
}
