use crate::core::library_scanner::ScanMode;
use crate::core::task_queue::TaskQueue;
use crate::db::models::{Library, ScraperConfig};
use crate::db::repository::LibraryRepository;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tracing::{info, warn};

pub struct LibrarySyncScheduler {
    library_repo: Arc<LibraryRepository>,
    task_queue: Arc<TaskQueue>,
}

impl LibrarySyncScheduler {
    pub fn new(library_repo: Arc<LibraryRepository>, task_queue: Arc<TaskQueue>) -> Self {
        Self {
            library_repo,
            task_queue,
        }
    }

    pub async fn start(self: Arc<Self>) {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(error) = self.enqueue_due_libraries().await {
                warn!(error = %error, "Scheduled library synchronization check failed");
            }
        }
    }

    async fn enqueue_due_libraries(&self) -> crate::core::app::error::Result<()> {
        let now = Utc::now();
        for library in self.library_repo.find_all().await? {
            if !matches!(library.library_type.as_str(), "rss" | "webdav") {
                continue;
            }

            let config = library
                .scraper_config
                .as_deref()
                .and_then(|json| serde_json::from_str::<ScraperConfig>(json).ok())
                .unwrap_or_default();
            if !config.scheduled_sync_enabled
                || !is_due(&library, &config.scheduled_sync_interval, now)
                || self.task_queue.has_active_library_scan(&library.id).await
            {
                continue;
            }

            match self
                .task_queue
                .enqueue_scan_library(&library.id, &library.url, ScanMode::Incremental)
                .await
            {
                Ok(task_id) => info!(
                    library_id = %library.id,
                    task_id = %task_id,
                    interval = %config.scheduled_sync_interval,
                    "Scheduled incremental library synchronization queued"
                ),
                Err(error) => warn!(
                    library_id = %library.id,
                    error = %error,
                    "Failed to queue scheduled library synchronization"
                ),
            }
        }
        Ok(())
    }
}

fn is_due(library: &Library, interval: &str, now: DateTime<Utc>) -> bool {
    let base = library
        .last_scanned_at
        .as_deref()
        .and_then(parse_datetime)
        .or_else(|| parse_datetime(&library.created_at));
    let Some(base) = base else {
        return false;
    };

    let required = match interval.trim().to_ascii_lowercase().as_str() {
        "hourly" => Duration::hours(1),
        "weekly" => Duration::weeks(1),
        "monthly" => Duration::days(30),
        _ => Duration::days(1),
    };
    now.signed_duration_since(base) >= required
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|value| value.and_utc())
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::is_due;
    use crate::db::models::Library;
    use chrono::{Duration, Utc};

    fn library(last_scanned_at: String) -> Library {
        Library {
            id: "library".to_string(),
            name: "Library".to_string(),
            library_type: "rss".to_string(),
            url: "https://example.com/feed.xml".to_string(),
            username: None,
            password: None,
            root_path: "/".to_string(),
            last_scanned_at: Some(last_scanned_at),
            created_at: Utc::now().to_rfc3339(),
            scraper_config: None,
        }
    }

    #[test]
    fn respects_hourly_and_daily_intervals() {
        let now = Utc::now();
        let library = library((now - Duration::hours(2)).to_rfc3339());
        assert!(is_due(&library, "hourly", now));
        assert!(!is_due(&library, "daily", now));
    }
}
