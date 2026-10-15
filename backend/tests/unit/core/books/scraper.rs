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
    items: std::sync::RwLock<Option<Vec<BookItem>>>,
    requests: std::sync::Mutex<Vec<Value>>,
}

#[tokio::test]
async fn both_search_apis_normalize_title_separators_and_share_one_cache_entry() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let plugin = add_scraper(&manager, "separator-source", false).await;
    let source = plugin.metadata.instance_id();
    let service = ScraperService::new(manager);
    service
        .search(" Book丨Author ", None, None, Some(&source), 1, 20)
        .await
        .unwrap();
    service
        .search_with_params(
            &HashMap::from([("title".into(), " Book | Author ".into())]),
            Some(&source),
            1,
            20,
        )
        .await
        .unwrap();
    for title in ["Book-Author", " Book - Narrator ", "Book｜Description"] {
        service
            .search(title, None, None, Some(&source), 1, 20)
            .await
            .unwrap();
    }
    service
        .search("Book", Some("Author"), None, Some(&source), 1, 20)
        .await
        .unwrap();
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 3);
    assert_eq!(service.get_cache_stats(), 3);
}

#[tokio::test]
async fn explicit_source_failures_do_not_invoke_a_different_source() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let failed = add_scraper(&manager, "failed-source", false).await;
    let other = add_scraper(&manager, "other-source", false).await;
    failed.fail.store(true, Ordering::SeqCst);
    let source = failed.metadata.instance_id();
    let service = ScraperService::new(manager);
    assert!(
        service
            .search("Book", None, None, Some(&source), 1, 20)
            .await
            .is_err()
    );
    assert!(
        service
            .search_with_params(
                &HashMap::from([("title".into(), "Book".into())]),
                Some(&source),
                1,
                20,
            )
            .await
            .is_err()
    );
    assert_eq!(other.calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.get_cache_stats(), 0);
}

#[tokio::test]
async fn configured_sources_are_trimmed_deduplicated_and_used_for_field_merge() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let plugin = add_scraper(&manager, "configured-source", false).await;
    let source = plugin.metadata.instance_id();
    let service = ScraperService::new(manager);
    let config = crate::db::models::ScraperConfig {
        default_sources: vec![format!(" {source} "), source.clone()],
        author_sources: Some(vec![format!(" {source} ")]),
        ..Default::default()
    };
    let detail = service.scrape_book_metadata("Book", &config).await.unwrap();
    assert_eq!(detail.author, "Author");
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 1);
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
        self.requests.lock().unwrap().push(input.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(TingError::PluginExecutionError(
                "test search failure".into(),
            ));
        }
        let results = self.items.read().unwrap().clone().unwrap_or_else(|| {
            let mut result = item(&self.metadata.id);
            result.author = "Author".into();
            vec![result]
        });
        let total = results.len();
        Ok(json!({
            "ok": true,
            "data": {
                "items": results,
                "page": input["page"],
                "page_size": input["page_size"],
                "total": total,
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
    add_scraper_with_fields(
        manager,
        id,
        aggregate,
        &["title", "author", "narrator", "category"],
    )
    .await
}

async fn add_scraper_with_fields(
    manager: &PluginManager,
    id: &str,
    aggregate: bool,
    fields: &[&str],
) -> Arc<CountingScraper> {
    let mut manifest = json!({
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
    });
    manifest["capabilities"][0]["search_fields"]
        .as_array_mut()
        .unwrap()
        .retain(|field| fields.contains(&field["key"].as_str().unwrap()));
    manifest["capabilities"][0]["filters_schema"]["properties"]
        .as_object_mut()
        .unwrap()
        .retain(|key, _| fields.contains(&key.as_str()));
    let metadata =
        crate::plugin::types::metadata::parse_plugin_metadata_value(manifest, "cache fixture")
            .unwrap();
    let plugin = Arc::new(CountingScraper {
        metadata,
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
        items: std::sync::RwLock::new(None),
        requests: std::sync::Mutex::new(Vec::new()),
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
async fn folder_contributors_select_the_right_edition_without_polluting_cached_order() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let plugin = add_scraper(&manager, "editions", false).await;
    let mut first = item("Book");
    first.id = Some("first".into());
    first.author = "Other Author".into();
    first.narrator = Some("Other Narrator".into());
    let mut selected = item("Book");
    selected.id = Some("selected".into());
    selected.author = "Target Author".into();
    selected.narrator = Some("Target Narrator".into());
    let mut unrelated = item("Other Story");
    unrelated.id = Some("unrelated".into());
    unrelated.narrator = selected.narrator.clone();
    *plugin.items.write().unwrap() = Some(vec![first, unrelated, selected]);
    let source = plugin.metadata.instance_id();
    let service = ScraperService::new(manager);
    for query in [
        "Book-Target Author",
        "Book-Target Narrator",
        "Book-Target Author-Target Narrator",
        "Book-作者：Target Author-演播：Target Narrator",
    ] {
        let result = service
            .search(query, None, None, Some(&source), 1, 20)
            .await
            .unwrap();
        assert_eq!(result.items[0].id.as_deref(), Some("selected"));
    }
    for query in ["Book", "Book-Nobody", "Book | Target Narrator"] {
        let result = service
            .search(query, None, None, Some(&source), 1, 20)
            .await
            .unwrap();
        assert_eq!(result.items[0].id.as_deref(), Some("first"));
    }
    let result = service
        .search(
            "Book-Other Author/Other Narrator",
            Some("Target Author"),
            Some("Target Narrator"),
            Some(&source),
            1,
            20,
        )
        .await
        .unwrap();
    assert_eq!(result.items[0].id.as_deref(), Some("selected"));
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 5);
    let config = crate::db::models::ScraperConfig {
        default_sources: vec![source],
        ..Default::default()
    };
    let detail = service
        .scrape_book_metadata_with_context(
            "Book",
            &config,
            Some(json!({
                "directory_name": "Book-Target Narrator",
            })),
        )
        .await
        .unwrap();
    assert_eq!(detail.author, "Target Author");
    assert_eq!(detail.narrator.as_deref(), Some("Target Narrator"));
    // An unlabeled suffix is a matching clue, never assigned blindly as author.
    let detail = service
        .scrape_book_metadata("Book-Nobody", &config)
        .await
        .unwrap();
    assert_eq!(detail.author, "Other Author");
    assert_eq!(plugin.calls.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn name_hints_follow_each_sources_declared_fields_and_explicit_values() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let narrator_only =
        add_scraper_with_fields(&manager, "narrator-only", false, &["title", "narrator"]).await;
    let author_only =
        add_scraper_with_fields(&manager, "author-only", false, &["title", "author"]).await;
    let both = add_scraper(&manager, "both", false).await;
    let title_only = add_scraper_with_fields(&manager, "title-only", false, &["title"]).await;
    let service = ScraperService::new(manager);
    for (plugin, expected_author, expected_narrator) in [
        (&narrator_only, None, Some("示例演播")),
        (&author_only, Some("示例作者"), None),
        (&both, Some("示例作者"), Some("示例演播")),
        (&title_only, None, None),
    ] {
        let source = plugin.metadata.instance_id();
        service
            .search(
                "示例书名-示例作者/示例演播",
                None,
                None,
                Some(&source),
                1,
                20,
            )
            .await
            .unwrap();
        let request = plugin.requests.lock().unwrap().last().unwrap().clone();
        assert_eq!(request["title"], "示例书名");
        assert_eq!(request["author"].as_str(), expected_author);
        assert_eq!(request["narrator"].as_str(), expected_narrator);
        service
            .search("示例书名-示例作者", None, None, Some(&source), 1, 20)
            .await
            .unwrap();
        let request = plugin.requests.lock().unwrap().last().unwrap().clone();
        if expected_narrator.is_some() {
            assert_eq!(
                request["narrator"].as_str(),
                if expected_author.is_none() {
                    Some("示例作者")
                } else {
                    None
                }
            );
        }
        let result = service
            .search(
                "示例书名-示例作者/示例演播",
                Some("指定作者"),
                Some("指定演播"),
                Some(&source),
                1,
                20,
            )
            .await
            .unwrap();
        assert_eq!(result.items.len(), 1); // A nonmatch is retained, never filtered out.
        let request = plugin.requests.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            request["author"].as_str(),
            expected_author.map(|_| "指定作者")
        );
        assert_eq!(
            request["narrator"].as_str(),
            expected_narrator.map(|_| "指定演播")
        );
    }
}

#[tokio::test]
async fn automatic_and_aggregate_search_keep_folder_hints_in_provider_requests() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let provider =
        add_scraper_with_fields(&manager, "narrator-provider", false, &["title", "narrator"]).await;
    let aggregate = add_scraper(&manager, "aggregate", true).await;
    let source = provider.metadata.instance_id();
    let aggregate_source = aggregate.metadata.instance_id();
    let service = ScraperService::with_cache_ttl(manager, Duration::ZERO);
    let config = crate::db::models::ScraperConfig {
        default_sources: vec![source.clone()],
        ..Default::default()
    };
    for (name, expected) in [
        ("示例书名-示例作者／示例演播", "示例演播"),
        ("示例书名-示例作者", "示例作者"),
        ("示例书名-示例演播", "示例演播"),
    ] {
        service
            .scrape_book_metadata_with_context(
                "示例书名",
                &config,
                Some(json!({
                    "directory_name": name,
                })),
            )
            .await
            .unwrap();
        let request = provider.requests.lock().unwrap().last().unwrap().clone();
        assert_eq!(request["title"], "示例书名");
        assert_eq!(request["narrator"], expected);
        assert!(request["author"].is_null());
    }
    service
        .scrape_book_metadata_with_context(
            "示例书名第一部",
            &config,
            Some(json!({
                "directory_name": "示例书名第一部",
                "current_metadata": { "author": "继承作者", "narrator": "继承演播" },
            })),
        )
        .await
        .unwrap();
    let request = provider.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(request["title"], "示例书名第一部");
    assert_eq!(request["narrator"], "继承演播");
    assert!(request["author"].is_null());
    service
        .search_with_params(
            &HashMap::from([
                ("title".into(), "示例书名-示例作者／示例演播".into()),
                (
                    "candidate_sources".into(),
                    serde_json::to_string(&vec![source]).unwrap(),
                ),
            ]),
            Some(&aggregate_source),
            1,
            20,
        )
        .await
        .unwrap();
    let request = provider.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(request["narrator"], "示例演播");
    assert!(request["author"].is_null());
}

#[tokio::test]
async fn fallback_rebuilds_name_hints_for_its_own_supported_fields() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let author_only =
        add_scraper_with_fields(&manager, "author-source", false, &["title", "author"]).await;
    let narrator_only =
        add_scraper_with_fields(&manager, "narrator-source", false, &["title", "narrator"]).await;
    let service = ScraperService::new(manager);
    let primary = service.select_scraper(None).await.unwrap();
    if primary.id == author_only.metadata.instance_id() {
        author_only.fail.store(true, Ordering::SeqCst);
    } else {
        narrator_only.fail.store(true, Ordering::SeqCst);
    }
    service
        .search("示例书名-示例作者／示例演播", None, None, None, 1, 20)
        .await
        .unwrap();
    let author_request = author_only.requests.lock().unwrap().last().unwrap().clone();
    let narrator_request = narrator_only
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(author_request["author"], "示例作者");
    assert!(author_request["narrator"].is_null());
    assert_eq!(narrator_request["narrator"], "示例演播");
    assert!(narrator_request["author"].is_null());
    assert_eq!(service.get_cache_stats(), 0);
}

#[tokio::test]
async fn automatic_scraping_without_library_sources_never_invokes_active_plugins() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let regular = add_scraper(&manager, "unselected-source", false).await;
    let aggregate = add_scraper(&manager, "unselected-aggregate", true).await;
    let service = ScraperService::new(manager);
    for config in [
        crate::db::models::ScraperConfig::default(),
        crate::db::models::ScraperConfig {
            default_sources: vec![" ".into()],
            author_sources: Some(vec![]),
            narrator_sources: Some(vec!["".into()]),
            cover_sources: Some(vec![]),
            intro_sources: Some(vec![]),
            tags_sources: Some(vec![]),
            ..Default::default()
        },
    ] {
        assert!(!config.has_scraper_sources());
        assert!(matches!(
            service.scrape_book_metadata("Book", &config).await,
            Err(TingError::NotFound(_))
        ));
        assert!(matches!(
            service
                .scrape_book_metadata_with_context("Book", &config, Some(json!({})))
                .await,
            Err(TingError::NotFound(_))
        ));
    }
    assert_eq!(regular.calls.load(Ordering::SeqCst), 0);
    assert_eq!(aggregate.calls.load(Ordering::SeqCst), 0);
    assert_eq!(service.get_cache_stats(), 0);
}

#[tokio::test]
async fn unavailable_library_sources_never_fall_back_to_unselected_plugins() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let regular = add_scraper(&manager, "unselected-source", false).await;
    let aggregate = add_scraper(&manager, "unselected-aggregate", true).await;
    let inactive = add_scraper(&manager, "inactive-source", false).await;
    let inactive_id = inactive.metadata.instance_id();
    manager
        .registry
        .write()
        .await
        .get_mut(&inactive_id)
        .unwrap()
        .state = PluginState::Unloaded;
    let service = ScraperService::new(manager);
    for source in ["removed-source@2.0.0".into(), inactive_id] {
        let config = crate::db::models::ScraperConfig {
            default_sources: vec![source],
            ..Default::default()
        };
        assert!(config.has_scraper_sources());
        assert!(matches!(
            service.scrape_book_metadata("Book", &config).await,
            Err(TingError::NotFound(_))
        ));
    }
    assert_eq!(regular.calls.load(Ordering::SeqCst), 0);
    assert_eq!(aggregate.calls.load(Ordering::SeqCst), 0);
    assert_eq!(inactive.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn automatic_scraping_uses_only_explicit_default_or_field_sources() {
    let directory = tempfile::tempdir().unwrap();
    let manager = cache_test_manager(directory.path());
    let selected = add_scraper(&manager, "selected-source", false).await;
    let unselected = add_scraper(&manager, "unselected-source", false).await;
    let aggregate = add_scraper(&manager, "unselected-aggregate", true).await;
    let service = ScraperService::with_cache_ttl(manager, Duration::ZERO);
    let source = selected.metadata.instance_id();
    for config in [
        crate::db::models::ScraperConfig {
            default_sources: vec![source.clone()],
            ..Default::default()
        },
        crate::db::models::ScraperConfig {
            author_sources: Some(vec![source]),
            ..Default::default()
        },
    ] {
        assert!(config.has_scraper_sources());
        let detail = service.scrape_book_metadata("Book", &config).await.unwrap();
        assert_eq!(detail.author, "Author");
    }
    assert_eq!(selected.calls.load(Ordering::SeqCst), 2);
    assert_eq!(unselected.calls.load(Ordering::SeqCst), 0);
    assert_eq!(aggregate.calls.load(Ordering::SeqCst), 0);
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
    let exact = crate::core::books::scrape_results::candidate_title_relevance_score(
        "示例书名",
        &item("示例书名"),
    );
    let contains = crate::core::books::scrape_results::candidate_title_relevance_score(
        "示例书名",
        &item("示例书名全集"),
    );
    let overlap = crate::core::books::scrape_results::candidate_title_relevance_score(
        "示例书名",
        &item("示例书名广播剧"),
    );
    let unrelated = crate::core::books::scrape_results::candidate_title_relevance_score(
        "示例书名",
        &item("其他作品"),
    );

    assert!(exact > contains);
    assert!(contains >= overlap);
    assert!(overlap > unrelated);
}

#[test]
fn normalizes_title_punctuation_for_matching() {
    let formatted = crate::core::books::scrape_results::candidate_title_relevance_score(
        "示例书名",
        &item("《示 例 书 名》"),
    );
    let exact = crate::core::books::scrape_results::candidate_title_relevance_score(
        "示例书名",
        &item("示例书名"),
    );

    assert_eq!(formatted, exact);
}

#[test]
fn candidate_policy_preserves_source_identity_fields_and_deterministic_order() {
    use crate::core::books::scrape_results::{collect_candidates, rank_candidates};
    let mut details = HashMap::new();
    let mut candidates = Vec::new();
    let mut exact = item("示例书名");
    exact.author = "Author".into();
    exact.narrator = Some("Narrator".into());
    exact.tags = vec!["science fiction".into()];
    let results = SearchResult {
        items: vec![item("示例书名全集"), exact],
        total: Some(2),
        has_more: Some(false),
        page: 1,
        page_size: 20,
    };
    collect_candidates("示例书名", "source", results, &mut details, &mut candidates).unwrap();
    rank_candidates(&mut candidates);
    assert_eq!(candidates[0]["title"], "示例书名");
    assert_eq!(candidates[0]["source_id"], "source");
    assert_eq!(candidates[0]["source_result_rank"], 2);
    assert_eq!(candidates[0]["tags"], json!(["science fiction"]));
    assert_eq!(candidates[0]["narrator"], "Narrator");
    // Source precedence remains its original first result, independently of
    // the sorted candidates sent to aggregate providers.
    assert_eq!(details["source"].title, "示例书名全集");
    let converted: BookDetail =
        serde_json::from_value(serde_json::to_value(BookDetail::from(&item("示例书名"))).unwrap())
            .unwrap();
    assert_eq!(converted.title, "示例书名");
}
