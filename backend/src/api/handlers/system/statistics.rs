//! Administrative library, user and listening statistics.

use crate::api::models::{
    AdminStatisticsOverview, AdminStatisticsResponse, BookActivityStatistics, LibraryStatistics,
    RecentActivityPoint, UserActivityStatistics,
};
use crate::api::state::AppState;
use crate::core::app::error::{Result, TingError};
use crate::db::repository::progress::LISTENING_EVENTS_RETENTION_DAYS;
use axum::{Json, extract::State, response::IntoResponse};
use chrono::Utc;

pub async fn get_admin_statistics(
    State(state): State<AppState>,
    user: crate::auth::middleware::AuthUser,
) -> Result<impl IntoResponse> {
    crate::api::require_admin(&user)?;

    let generated_at = Utc::now().to_rfc3339();
    let report = state
        .book_repo
        .db()
        .execute(move |conn| {
            let (total_books, total_chapters, total_duration): (i64, i64, i64) = conn
                .query_row(
                    "SELECT \
                        (SELECT COUNT(*) FROM books), \
                        (SELECT COUNT(*) FROM chapters), \
                        COALESCE((SELECT SUM(duration) FROM chapters), 0)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(TingError::DatabaseError)?;

            let (total_libraries, local_libraries, webdav_libraries, rss_libraries): (i64, i64, i64, i64) = conn
                .query_row(
                    "SELECT \
                        COUNT(*), \
                        COALESCE(SUM(CASE WHEN LOWER(type) = 'local' THEN 1 ELSE 0 END), 0), \
                        COALESCE(SUM(CASE WHEN LOWER(type) = 'webdav' THEN 1 ELSE 0 END), 0), \
                        COALESCE(SUM(CASE WHEN LOWER(type) = 'rss' THEN 1 ELSE 0 END), 0) \
                     FROM libraries",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .map_err(TingError::DatabaseError)?;

            let (total_users, admin_users): (i64, i64) = conn
                .query_row(
                    "SELECT \
                        COUNT(*), \
                        COALESCE(SUM(CASE WHEN role = 'admin' THEN 1 ELSE 0 END), 0) \
                     FROM users",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(TingError::DatabaseError)?;

            let (active_users, total_progress_records, total_listen_seconds): (i64, i64, f64) =
                conn.query_row(
                    "SELECT \
                        (SELECT COUNT(DISTINCT user_id) FROM listening_events \
                         WHERE activity_date >= DATE('now', ?1)), \
                        (SELECT COUNT(*) FROM progress WHERE chapter_id IS NOT NULL), \
                        COALESCE((SELECT SUM(listen_seconds) FROM listening_totals), 0.0)",
                    [format!("-{} days", LISTENING_EVENTS_RETENTION_DAYS)],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(TingError::DatabaseError)?;

            let mut library_stmt = conn
                .prepare(
                    "SELECT \
                        l.id, l.name, l.type, \
                        COUNT(DISTINCT b.id) AS total_books, \
                        COUNT(c.id) AS total_chapters, \
                        COALESCE(SUM(c.duration), 0) AS total_duration, \
                        l.last_scanned_at \
                     FROM libraries l \
                     LEFT JOIN books b ON b.library_id = l.id \
                     LEFT JOIN chapters c ON c.book_id = b.id \
                     GROUP BY l.id, l.name, l.type, l.last_scanned_at \
                     ORDER BY total_books DESC, l.name ASC",
                )
                .map_err(TingError::DatabaseError)?;
            let library_breakdown = library_stmt
                .query_map([], |row| {
                    Ok(LibraryStatistics {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        library_type: row.get(2)?,
                        total_books: row.get(3)?,
                        total_chapters: row.get(4)?,
                        total_duration: row.get(5)?,
                        last_scanned_at: row.get(6)?,
                    })
                })
                .map_err(TingError::DatabaseError)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(TingError::DatabaseError)?;

            let mut user_stmt = conn
                .prepare(
                    "SELECT \
                        u.id, u.username, u.role, \
                        COALESCE((SELECT COUNT(DISTINCT e.book_id) FROM listening_totals e WHERE e.user_id = u.id), 0) AS listened_books, \
                        COALESCE((SELECT COUNT(*) FROM progress p WHERE p.user_id = u.id AND p.chapter_id IS NOT NULL), 0) AS progress_records, \
                        COALESCE((SELECT SUM(e.listen_seconds) FROM listening_totals e WHERE e.user_id = u.id), 0.0) AS listen_seconds, \
                        (SELECT MAX(e.last_active_at) FROM listening_totals e WHERE e.user_id = u.id) AS last_active_at \
                     FROM users u \
                     ORDER BY last_active_at IS NULL, last_active_at DESC, listen_seconds DESC",
                )
                .map_err(TingError::DatabaseError)?;
            let user_activity = user_stmt
                .query_map([], |row| {
                    Ok(UserActivityStatistics {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        role: row.get(2)?,
                        listened_books: row.get(3)?,
                        progress_records: row.get(4)?,
                        listen_seconds: row.get(5)?,
                        last_active_at: row.get(6)?,
                    })
                })
                .map_err(TingError::DatabaseError)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(TingError::DatabaseError)?;

            let mut recent_stmt = conn
                .prepare(
                    "SELECT activity_date, active_users, progress_updates, listen_seconds \
                     FROM ( \
                        SELECT \
                            activity_date, \
                            COUNT(DISTINCT user_id) AS active_users, \
                            COUNT(*) AS progress_updates, \
                            COALESCE(SUM(listen_seconds), 0.0) AS listen_seconds \
                        FROM listening_events \
                        WHERE activity_date IS NOT NULL \
                        GROUP BY activity_date \
                        ORDER BY activity_date DESC \
                        LIMIT 14 \
                     ) \
                     ORDER BY activity_date ASC",
                )
                .map_err(TingError::DatabaseError)?;
            let recent_activity = recent_stmt
                .query_map([], |row| {
                    Ok(RecentActivityPoint {
                        date: row.get(0)?,
                        active_users: row.get(1)?,
                        progress_updates: row.get(2)?,
                        listen_seconds: row.get(3)?,
                    })
                })
                .map_err(TingError::DatabaseError)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(TingError::DatabaseError)?;

            let mut top_books_stmt = conn
                .prepare(
                    "SELECT \
                        b.id, b.title, b.author, b.library_id, l.name, \
                        COALESCE((SELECT COUNT(DISTINCT d.user_id) FROM listening_events d \
                                  WHERE d.book_id = b.id AND d.activity_date >= DATE('now', ?1)), 0) AS listeners, \
                        COALESCE((SELECT COUNT(*) FROM listening_events d \
                                  WHERE d.book_id = b.id AND d.activity_date >= DATE('now', ?1)), 0) AS progress_updates, \
                        COALESCE((SELECT SUM(e.listen_seconds) FROM listening_totals e WHERE e.book_id = b.id), 0.0) AS listen_seconds \
                     FROM books b \
                     LEFT JOIN libraries l ON l.id = b.library_id \
                     WHERE EXISTS ( \
                        SELECT 1 FROM listening_totals listened \
                        WHERE listened.book_id = b.id AND listened.listen_seconds > 0 \
                     ) \
                     ORDER BY listeners DESC, listen_seconds DESC \
                     LIMIT 12",
                )
                .map_err(TingError::DatabaseError)?;
            let retention_modifier = format!("-{} days", LISTENING_EVENTS_RETENTION_DAYS);
            let top_books = top_books_stmt
                .query_map([retention_modifier], |row| {
                    Ok(BookActivityStatistics {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        author: row.get(2)?,
                        library_id: row.get(3)?,
                        library_name: row.get(4)?,
                        listeners: row.get(5)?,
                        progress_updates: row.get(6)?,
                        listen_seconds: row.get(7)?,
                    })
                })
                .map_err(TingError::DatabaseError)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(TingError::DatabaseError)?;

            Ok(AdminStatisticsResponse {
                overview: AdminStatisticsOverview {
                    total_books,
                    total_chapters,
                    total_duration,
                    total_libraries,
                    local_libraries,
                    webdav_libraries,
                    rss_libraries,
                    total_users,
                    admin_users,
                    active_users,
                    total_progress_records,
                    total_listen_seconds,
                },
                library_breakdown,
                user_activity,
                recent_activity,
                top_books,
                generated_at,
            })
        })
        .await?;

    Ok(Json(report))
}
