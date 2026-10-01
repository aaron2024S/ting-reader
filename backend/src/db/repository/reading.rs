use crate::core::app::error::Result;
use crate::db::manager::DatabaseManager;
use rusqlite::{OptionalExtension, named_params};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Serialize)]
pub struct ActivityBook {
    pub book_id: String,
    pub book_title: Option<String>,
    pub cover_url: Option<String>,
    pub library_id: String,
    pub chapter_count: usize,
    pub updated_at: String,
    pub latest_chapter_id: Option<String>,
    pub latest_chapter_title: Option<String>,
    pub latest_position: f64,
    pub latest_duration: f64,
    pub latest_note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Bookmark {
    pub id: String,
    pub book_id: String,
    pub chapter_id: String,
    pub chapter_title: Option<String>,
    pub chapter_duration: f64,
    pub position: f64,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct HistorySummary {
    pub books: usize,
    pub chapters: usize,
    pub position_seconds: f64,
}

pub struct ReadingRepository {
    db: Arc<DatabaseManager>,
}

// Apply access rules in the paginated query, so revoked books neither appear
// in results nor distort totals. User ID always scopes the underlying records.
const ACCESS: &str = "(:admin OR b.library_id IN
    (SELECT library_id FROM user_library_access WHERE user_id = :user)
    OR b.id IN (SELECT book_id FROM user_book_access WHERE user_id = :user))";

impl ReadingRepository {
    pub fn new(db: Arc<DatabaseManager>) -> Self {
        Self { db }
    }

    pub async fn history_summary(&self, user: &str, admin: bool) -> Result<HistorySummary> {
        let user = user.to_owned();
        self.db
            .execute(move |conn| {
                Ok(conn.query_row(
                    &format!(
                        "SELECT COUNT(DISTINCT b.id),COUNT(*),COALESCE(SUM(MAX(0,r.position)),0)
                 FROM progress r JOIN books b ON b.id=r.book_id
                 WHERE r.user_id=:user AND r.chapter_id IS NOT NULL
                 AND r.history_hidden_at IS NULL AND {ACCESS}"
                    ),
                    named_params! {":user":user,":admin":admin},
                    |row| {
                        Ok(HistorySummary {
                            books: row.get(0)?,
                            chapters: row.get(1)?,
                            position_seconds: row.get(2)?,
                        })
                    },
                )?)
            })
            .await
    }

    pub async fn percentages(&self, user: &str) -> Result<HashMap<String, f64>> {
        let user = user.to_owned();
        self.db
            .execute(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT p.book_id, 100.0 * SUM(CASE
                    WHEN COALESCE(NULLIF(c.duration, 0), NULLIF(p.duration, 0), 0) > 0
                    THEN MIN(1.0, MAX(0.0, p.position) /
                         COALESCE(NULLIF(c.duration, 0), NULLIF(p.duration, 0)))
                    ELSE 0 END) / (SELECT COUNT(*) FROM chapters WHERE book_id=p.book_id)
                 FROM progress p JOIN chapters c ON c.id=p.chapter_id
                 WHERE p.user_id=?1 GROUP BY p.book_id",
                )?;
                let mut values = stmt
                    .query_map([&user], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                    })?
                    .collect::<std::result::Result<HashMap<_, _>, _>>()?;
                let mut marks =
                    conn.prepare("SELECT book_id FROM book_read_marks WHERE user_id = ?")?;
                for book in marks.query_map([&user], |row| row.get::<_, String>(0))? {
                    values.insert(book?, 100.0);
                }
                Ok(values)
            })
            .await
    }

    pub async fn mark_books(&self, user: &str, books: Vec<String>, read: bool) -> Result<()> {
        let user = user.to_owned();
        self.db
            .transaction(move |conn| {
                for book in books {
                    if read {
                        conn.execute(
                            "INSERT OR IGNORE INTO book_read_marks(user_id,book_id) VALUES(?,?)",
                            [&user, &book],
                        )?;
                    } else {
                        conn.execute(
                            "DELETE FROM progress WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                        conn.execute(
                            "DELETE FROM book_read_marks WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                    }
                }
                Ok(())
            })
            .await
    }

    pub async fn activity_books(
        &self,
        user: &str,
        admin: bool,
        bookmarks: bool,
        page: usize,
        page_size: usize,
    ) -> Result<Page<ActivityBook>> {
        let user = user.to_owned();
        self.db
            .execute(move |conn| {
                let (table, visible) = if bookmarks {
                    ("bookmarks", "")
                } else {
                    (
                        "progress",
                        "AND r.history_hidden_at IS NULL AND r.chapter_id IS NOT NULL",
                    )
                };
                let from = format!(
                    "FROM {table} r JOIN books b ON b.id=r.book_id
                WHERE r.user_id=:user {visible} AND {ACCESS}"
                );
                let total: usize = conn.query_row(
                    &format!("SELECT COUNT(DISTINCT b.id) {from}"),
                    named_params! {":user": user, ":admin": admin},
                    |row| row.get(0),
                )?;
                let duration = if bookmarks {
                    "COALESCE(c.duration,0)"
                } else {
                    "COALESCE(NULLIF(c.duration,0),latest.duration,0)"
                };
                let note = if bookmarks { "latest.note" } else { "NULL" };
                let mut stmt = conn.prepare(&format!(
                    "WITH activity AS (
                        SELECT b.id AS book_id,COUNT(*) AS count,MAX(r.updated_at) AS updated_at
                        {from} GROUP BY b.id ORDER BY MAX(r.updated_at) DESC,b.id
                        LIMIT :limit OFFSET :offset
                    )
                    SELECT b.id,b.title,b.cover_url,b.library_id,a.count,a.updated_at,
                           latest.chapter_id,c.title,latest.position,{duration},{note}
                    FROM activity a JOIN books b ON b.id=a.book_id
                    JOIN {table} latest ON latest.id=(
                        SELECT r.id FROM {table} r
                        WHERE r.user_id=:user AND r.book_id=a.book_id {visible}
                        ORDER BY r.updated_at DESC,r.id LIMIT 1
                    )
                    LEFT JOIN chapters c ON c.id=latest.chapter_id
                    ORDER BY a.updated_at DESC,b.id"
                ))?;
                let items = stmt.query_map(named_params!{
                ":user":user, ":admin":admin, ":limit":page_size, ":offset":(page-1)*page_size
            }, |row| Ok(ActivityBook {
                book_id: row.get(0)?, book_title: row.get(1)?, cover_url: row.get(2)?,
                library_id: row.get(3)?, chapter_count: row.get(4)?, updated_at: row.get(5)?,
                latest_chapter_id:row.get(6)?,latest_chapter_title:row.get(7)?,
                latest_position:row.get(8)?,latest_duration:row.get(9)?,latest_note:row.get(10)?,
            }))?.collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(Page {
                    items,
                    total,
                    page,
                    page_size,
                })
            })
            .await
    }

    pub async fn history_chapters(
        &self,
        user: &str,
        book: &str,
        page: usize,
        page_size: usize,
    ) -> Result<Page<crate::api::models::ProgressResponse>> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db.execute(move |conn| {
            let total = conn.query_row("SELECT COUNT(*) FROM progress
                WHERE user_id=? AND book_id=? AND history_hidden_at IS NULL AND chapter_id IS NOT NULL",
                [&user,&book], |row| row.get(0))?;
            let mut stmt = conn.prepare("SELECT p.id,p.chapter_id,p.position,p.duration,p.updated_at,c.title,c.duration
                FROM progress p JOIN chapters c ON c.id=p.chapter_id
                WHERE p.user_id=?1 AND p.book_id=?2 AND p.history_hidden_at IS NULL
                ORDER BY p.updated_at DESC,p.id LIMIT ?3 OFFSET ?4")?;
            let items = stmt.query_map(rusqlite::params![user,book,page_size,(page-1)*page_size], |row| {
                Ok(crate::api::models::ProgressResponse {
                    id:row.get(0)?, user_id:user.clone(),book_id:book.clone(),chapter_id:row.get(1)?,
                    position:row.get(2)?,duration:row.get(3)?,updated_at:row.get(4)?,
                    chapter_title:row.get(5)?,chapter_duration:row.get(6)?,
                    book_title:None,cover_url:None,library_id:None,
                })
            })?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(Page {items,total,page,page_size})
        }).await
    }

    pub async fn clear_history(
        &self,
        user: &str,
        all: bool,
        books: Vec<String>,
        ids: Vec<String>,
        chapters: Vec<String>,
        clear_progress: bool,
    ) -> Result<usize> {
        let user = user.to_owned();
        self.db
            .transaction(move |conn| {
                // Select each row once, regardless of overlapping book/chapter selections.
                let mut parameters = vec![rusqlite::types::Value::from(user.clone())];
                let mut clauses = Vec::new();
                let explicitly_selected_books = books.clone();
                for (column, values) in [("book_id", books), ("id", ids), ("chapter_id", chapters)]
                {
                    if !values.is_empty() {
                        let slots = std::iter::repeat_n("?", values.len())
                            .collect::<Vec<_>>()
                            .join(",");
                        clauses.push(format!("{column} IN ({slots})"));
                        parameters.extend(values.into_iter().map(rusqlite::types::Value::from));
                    }
                }
                if !all && clauses.is_empty() {
                    return Ok(0);
                }
                let scope = if all {
                    "1".to_string()
                } else {
                    clauses.join(" OR ")
                };
                if clear_progress {
                    let sql = format!(
                        "SELECT DISTINCT book_id FROM progress WHERE user_id=? AND ({scope})"
                    );
                    let affected_books = conn
                        .prepare(&sql)?
                        .query_map(rusqlite::params_from_iter(&parameters), |row| {
                            row.get::<_, String>(0)
                        })?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    for book in affected_books.into_iter().chain(explicitly_selected_books) {
                        conn.execute(
                            "DELETE FROM book_read_marks WHERE user_id=? AND book_id=?",
                            [&user, &book],
                        )?;
                    }
                    if all {
                        conn.execute("DELETE FROM book_read_marks WHERE user_id=?", [&user])?;
                    }
                }
                let action = if clear_progress {
                    "DELETE FROM progress"
                } else {
                    "UPDATE progress SET history_hidden_at=STRFTIME('%Y-%m-%dT%H:%M:%fZ','now')"
                };
                let visible = if clear_progress {
                    ""
                } else {
                    "AND history_hidden_at IS NULL"
                };
                Ok(conn.execute(
                    &format!("{action} WHERE user_id=? AND ({scope}) {visible}"),
                    rusqlite::params_from_iter(parameters),
                )?)
            })
            .await
    }

    pub async fn bookmarks(
        &self,
        user: &str,
        book: &str,
        page: usize,
        page_size: usize,
    ) -> Result<Page<Bookmark>> {
        let user = user.to_owned();
        let book = book.to_owned();
        self.db.execute(move |conn| {
            let total = conn.query_row("SELECT COUNT(*) FROM bookmarks WHERE user_id=? AND book_id=?",
                [&user,&book], |row| row.get(0))?;
            let mut stmt = conn.prepare("SELECT m.id,m.book_id,m.chapter_id,c.title,m.position,m.note,m.created_at,m.updated_at,
                COALESCE(NULLIF(c.duration,0),p.duration,0)
                FROM bookmarks m JOIN chapters c ON c.id=m.chapter_id
                LEFT JOIN progress p ON p.user_id=m.user_id AND p.book_id=m.book_id AND p.chapter_id=m.chapter_id
                WHERE m.user_id=?1 AND m.book_id=?2 ORDER BY m.updated_at DESC,m.id LIMIT ?3 OFFSET ?4")?;
            let items = stmt.query_map(rusqlite::params![user,book,page_size,(page-1)*page_size], |r| Ok(Bookmark {
                id:r.get(0)?, book_id:r.get(1)?, chapter_id:r.get(2)?, chapter_title:r.get(3)?,
                position:r.get(4)?,note:r.get(5)?,created_at:r.get(6)?,updated_at:r.get(7)?,
                chapter_duration:r.get(8)?,
            }))?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(Page {items,total,page,page_size})
        }).await
    }

    pub async fn add_bookmark(
        &self,
        user: &str,
        book: String,
        chapter: String,
        position: f64,
        note: String,
    ) -> Result<Bookmark> {
        let user = user.to_owned();
        self.db.execute(move |conn| {
            let now = chrono::Utc::now().to_rfc3339(); let id = uuid::Uuid::new_v4().to_string();
            conn.execute("INSERT INTO bookmarks(id,user_id,book_id,chapter_id,position,note,created_at,updated_at)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?7)", rusqlite::params![id,user,book,chapter,position,note,now])?;
            let (chapter_title, chapter_duration) = conn.query_row(
                "SELECT c.title,COALESCE(NULLIF(c.duration,0),p.duration,0) FROM chapters c
                 LEFT JOIN progress p ON p.chapter_id=c.id AND p.book_id=c.book_id AND p.user_id=?2 WHERE c.id=?1",
                [&chapter, &user], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(Bookmark {id,book_id:book,chapter_id:chapter,chapter_title,chapter_duration,position,note,created_at:now.clone(),updated_at:now})
        }).await
    }

    pub async fn bookmark_book(&self, user: &str, id: &str) -> Result<Option<String>> {
        let user = user.to_owned();
        let id = id.to_owned();
        self.db
            .execute(move |c| {
                Ok(c.query_row(
                    "SELECT book_id FROM bookmarks WHERE user_id=? AND id=?",
                    [user, id],
                    |r| r.get(0),
                )
                .optional()?)
            })
            .await
    }

    pub async fn change_bookmark(
        &self,
        user: &str,
        id: &str,
        note: Option<String>,
    ) -> Result<usize> {
        let user = user.to_owned();
        let id = id.to_owned();
        self.db
            .execute(move |c| {
                Ok(if let Some(note) = note {
                    c.execute(
                        "UPDATE bookmarks SET note=?,updated_at=? WHERE user_id=? AND id=?",
                        [note, chrono::Utc::now().to_rfc3339(), user, id],
                    )?
                } else {
                    c.execute("DELETE FROM bookmarks WHERE user_id=? AND id=?", [user, id])?
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::Progress;
    use crate::db::repository::ProgressRepository;

    async fn fixture(chapters: usize) -> (Arc<DatabaseManager>, ReadingRepository) {
        let db = Arc::new(DatabaseManager::new_in_memory().unwrap());
        db.transaction(move |conn| {
            conn.execute_batch(
                "INSERT INTO users(id,username,password_hash) VALUES('u1','one','hash'),('u2','two','hash');
                 INSERT INTO libraries(id,name,type,url) VALUES('library','Test','local','');
                 INSERT INTO books(id,library_id,title,path,hash) VALUES('book','library','Book','/book','hash');
                 INSERT INTO user_library_access(user_id,library_id) VALUES('u1','library');",
            )?;
            for index in 0..chapters {
                let id = format!("c{index:05}");
                conn.execute(
                    "INSERT INTO chapters(id,book_id,title,path,duration) VALUES(?1,'book',?1,?1,100)",
                    [&id],
                )?;
            }
            Ok(())
        }).await.unwrap();
        (db.clone(), ReadingRepository::new(db))
    }

    async fn add_progress(db: &Arc<DatabaseManager>, user: &str, chapter: &str, position: f64) {
        ProgressRepository::new(db.clone())
            .upsert(&Progress {
                id: format!("{user}:{chapter}"),
                user_id: user.into(),
                book_id: "book".into(),
                chapter_id: Some(chapter.into()),
                position,
                duration: Some(100.0),
                updated_at: chrono::Utc::now().to_rfc3339(),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn read_marks_are_user_scoped_and_do_not_invent_listening_activity() {
        let (db, repo) = fixture(2).await;
        add_progress(&db, "u1", "c00000", 50.0).await;
        add_progress(&db, "u2", "c00000", 100.0).await;
        assert_eq!(repo.percentages("u1").await.unwrap()["book"], 25.0);
        repo.mark_books("u1", vec!["book".into()], true)
            .await
            .unwrap();
        assert_eq!(repo.percentages("u1").await.unwrap()["book"], 100.0);
        assert_eq!(repo.percentages("u2").await.unwrap()["book"], 50.0);
        let totals_before: f64 = db
            .execute(|conn| {
                Ok(conn.query_row(
                    "SELECT SUM(listen_seconds) FROM listening_totals",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        repo.mark_books("u1", vec!["book".into()], false)
            .await
            .unwrap();
        assert!(!repo.percentages("u1").await.unwrap().contains_key("book"));
        assert_eq!(
            repo.history_chapters("u1", "book", 1, 40)
                .await
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            repo.history_chapters("u2", "book", 1, 40)
                .await
                .unwrap()
                .total,
            1
        );
        let totals_after: f64 = db
            .execute(|conn| {
                Ok(conn.query_row(
                    "SELECT SUM(listen_seconds) FROM listening_totals",
                    [],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(totals_before, totals_after);
    }

    #[tokio::test]
    async fn hiding_history_preserves_resume_and_clear_progress_removes_hidden_chapters() {
        let (db, repo) = fixture(2).await;
        add_progress(&db, "u1", "c00000", 50.0).await;
        add_progress(&db, "u1", "c00001", 100.0).await;
        let hidden = repo
            .clear_history(
                "u1",
                false,
                vec!["book".into()],
                vec!["u1:c00000".into()],
                vec![],
                false,
            )
            .await
            .unwrap();
        assert_eq!(hidden, 2);
        assert_eq!(
            repo.activity_books("u1", false, false, 1, 40)
                .await
                .unwrap()
                .total,
            0
        );
        assert_eq!(repo.percentages("u1").await.unwrap()["book"], 75.0);
        let cleared = repo
            .clear_history("u1", false, vec!["book".into()], vec![], vec![], true)
            .await
            .unwrap();
        assert_eq!(cleared, 2);
        assert!(repo.percentages("u1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clear_progress_removes_manual_read_marks_even_without_history() {
        let (_, repo) = fixture(1).await;
        repo.mark_books("u1", vec!["book".into()], true)
            .await
            .unwrap();
        repo.clear_history("u1", false, vec!["book".into()], vec![], vec![], true)
            .await
            .unwrap();
        assert!(repo.percentages("u1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn history_pages_are_bounded_stable_and_access_filtered() {
        let (db, repo) = fixture(1500).await;
        db.transaction(|conn| {
            conn.execute(
                "INSERT INTO progress(id,user_id,book_id,chapter_id,position,duration,updated_at)
                SELECT id,'u1','book',id,50,100,'2026-09-30T00:00:00Z' FROM chapters",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let first = repo.history_chapters("u1", "book", 1, 50).await.unwrap();
        let second = repo.history_chapters("u1", "book", 2, 50).await.unwrap();
        assert_eq!(first.total, 1500);
        assert_eq!(first.items.len(), 50);
        assert_eq!(second.items.len(), 50);
        assert!(
            first
                .items
                .iter()
                .all(|item| second.items.iter().all(|other| item.id != other.id))
        );
        let books = repo
            .activity_books("u1", false, false, 1, 40)
            .await
            .unwrap();
        assert_eq!(books.items[0].chapter_count, 1500);
        assert_eq!(books.items[0].latest_chapter_id.as_deref(), Some("c00000"));
        assert_eq!(books.items[0].latest_position, 50.0);
        assert_eq!(books.items[0].latest_duration, 100.0);
        db.execute(|conn| {
            conn.execute("DELETE FROM user_library_access WHERE user_id='u1'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            repo.activity_books("u1", false, false, 1, 40)
                .await
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            repo.activity_books("u1", true, false, 1, 40)
                .await
                .unwrap()
                .total,
            1
        );
    }

    #[tokio::test]
    async fn activity_summary_uses_latest_visible_entry_and_bookmark_note() {
        let (db, repo) = fixture(2).await;
        add_progress(&db, "u1", "c00000", 75.0).await;
        add_progress(&db, "u1", "c00001", 40.0).await;
        db.execute(|conn| {
            conn.execute_batch(
                "UPDATE progress SET updated_at='2026-09-29T00:00:00Z' WHERE chapter_id='c00000';
                 UPDATE progress SET updated_at='2026-09-30T00:00:00Z' WHERE chapter_id='c00001';",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let summary = repo
            .activity_books("u1", false, false, 1, 40)
            .await
            .unwrap();
        assert_eq!(
            summary.items[0].latest_chapter_title.as_deref(),
            Some("c00001")
        );
        assert_eq!(summary.items[0].latest_position, 40.0);
        repo.clear_history("u1", false, vec![], vec![], vec!["c00001".into()], false)
            .await
            .unwrap();
        let visible = repo
            .activity_books("u1", false, false, 1, 40)
            .await
            .unwrap();
        assert_eq!(visible.items[0].chapter_count, 1);
        assert_eq!(visible.items[0].latest_position, 75.0);
        let bookmark = repo
            .add_bookmark("u1", "book".into(), "c00001".into(), 12.5, "Note".into())
            .await
            .unwrap();
        repo.change_bookmark("u1", &bookmark.id, Some("Edited note".into()))
            .await
            .unwrap();
        let bookmarks = repo.activity_books("u1", false, true, 1, 40).await.unwrap();
        assert_eq!(
            bookmarks.items[0].latest_note.as_deref(),
            Some("Edited note")
        );
        assert_eq!(bookmarks.items[0].latest_position, 12.5);
        assert_eq!(bookmarks.items[0].latest_duration, 100.0);
    }

    #[tokio::test]
    async fn bookmark_duration_falls_back_to_owners_progress_when_metadata_is_unknown() {
        let (db, repo) = fixture(1).await;
        add_progress(&db, "u1", "c00000", 50.0).await;
        add_progress(&db, "u2", "c00000", 50.0).await;
        db.execute(|conn| {
            conn.execute_batch(
                "UPDATE chapters SET duration=0;
                 UPDATE progress SET duration=200 WHERE user_id='u2';",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let bookmark = repo
            .add_bookmark("u1", "book".into(), "c00000".into(), 25.0, "".into())
            .await
            .unwrap();
        assert_eq!(bookmark.chapter_duration, 100.0);
        assert_eq!(
            repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].chapter_duration,
            100.0
        );
        db.execute(|conn| {
            conn.execute("DELETE FROM progress WHERE user_id='u1'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].chapter_duration,
            0.0
        );
    }

    #[tokio::test]
    async fn bookmarks_enforce_ownership_and_cascade_when_chapters_are_deleted() {
        let (db, repo) = fixture(2).await;
        let bookmark = repo
            .add_bookmark("u1", "book".into(), "c00000".into(), 12.5, "备注".into())
            .await
            .unwrap();
        assert_eq!(bookmark.chapter_duration, 100.0);
        assert_eq!(
            repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].position,
            12.5
        );
        assert_eq!(
            repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].chapter_duration,
            100.0
        );
        assert!(
            repo.bookmark_book("u2", &bookmark.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repo.change_bookmark("u2", &bookmark.id, None)
                .await
                .unwrap(),
            0
        );
        repo.change_bookmark("u1", &bookmark.id, Some("new note".into()))
            .await
            .unwrap();
        assert_eq!(
            repo.bookmarks("u1", "book", 1, 40).await.unwrap().items[0].note,
            "new note"
        );
        assert_eq!(
            repo.activity_books("u1", false, true, 1, 40)
                .await
                .unwrap()
                .total,
            1
        );
        db.execute(|conn| {
            conn.execute("DELETE FROM chapters WHERE id='c00000'", [])?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(repo.bookmarks("u1", "book", 1, 40).await.unwrap().total, 0);
    }
}
