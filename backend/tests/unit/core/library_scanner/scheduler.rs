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
