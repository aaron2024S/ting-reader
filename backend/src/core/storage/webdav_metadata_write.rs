use super::{StorageService, remote_response_reader, webdav_book_url};
use crate::core::error::{Result, TingError};
use crate::db::models::Library;
use reqwest::{Client, Url, header};
use std::path::Path;
use tokio::io::AsyncRead;
use tokio_util::io::ReaderStream;

#[derive(Debug, Default)]
pub struct WebDavFileRevision {
    etag: Option<String>,
    modified: Option<String>,
}

pub struct WebDavWriteSource {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub length: u64,
    pub revision: WebDavFileRevision,
}

fn chapter_url(library: &Library, book_path: &str, chapter_path: &str) -> Result<Url> {
    let book = webdav_book_url(library, book_path)?;
    let chapter = webdav_book_url(library, chapter_path)?;
    if !chapter
        .path()
        .starts_with(&format!("{}/", book.path().trim_end_matches('/')))
        || chapter.path().ends_with('/')
    {
        return Err(TingError::PermissionDenied(
            "Chapter is outside the WebDAV book folder".into(),
        ));
    }
    Ok(chapter)
}

fn authenticated_request(
    library: &Library,
    key: &[u8; 32],
    request: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    if let Some(username) = &library.username {
        let password = library.password.as_deref().unwrap_or_default();
        let password =
            crate::core::crypto::decrypt(password, key).unwrap_or_else(|_| password.to_string());
        request.basic_auth(username, Some(password))
    } else {
        request
    }
}

fn transfer_client() -> Result<Client> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|error| TingError::NetworkError(error.to_string()))
}

impl StorageService {
    /// Read the complete audio source while retaining its remote revision.
    pub async fn get_webdav_write_source(
        &self,
        library: &Library,
        book_path: &str,
        chapter_path: &str,
        key: &[u8; 32],
    ) -> Result<WebDavWriteSource> {
        let url = chapter_url(library, book_path, chapter_path)?;
        let response = authenticated_request(
            library,
            key,
            transfer_client()?
                .get(url)
                .header(header::ACCEPT_ENCODING, "identity"),
        )
        .send()
        .await
        .map_err(|error| TingError::NetworkError(error.to_string()))?;
        let revision = WebDavFileRevision {
            etag: response
                .headers()
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.starts_with("W/"))
                .map(ToOwned::to_owned),
            modified: response
                .headers()
                .get(header::LAST_MODIFIED)
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned),
        };
        let (reader, length) = remote_response_reader(response, None).await?;
        Ok(WebDavWriteSource {
            reader,
            length,
            revision,
        })
    }

    /// Stream the modified file back without buffering an entire chapter in RAM.
    pub async fn put_webdav_audio_file(
        &self,
        library: &Library,
        book_path: &str,
        chapter_path: &str,
        local_file: &Path,
        revision: &WebDavFileRevision,
        key: &[u8; 32],
    ) -> Result<()> {
        let url = chapter_url(library, book_path, chapter_path)?;
        let file = tokio::fs::File::open(local_file).await?;
        let length = file.metadata().await?.len();
        let mut request = transfer_client()?
            .put(url)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, length)
            .body(reqwest::Body::wrap_stream(ReaderStream::new(file)));
        if let Some(etag) = &revision.etag {
            request = request.header(header::IF_MATCH, etag);
        } else if let Some(modified) = &revision.modified {
            request = request.header(header::IF_UNMODIFIED_SINCE, modified);
        }
        let response = authenticated_request(library, key, request)
            .send()
            .await
            .map_err(|error| TingError::NetworkError(error.to_string()))?;
        if !response.status().is_success() {
            return Err(TingError::NetworkError(format!(
                "WebDAV audio metadata upload failed (HTTP {})",
                response.status()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapters_must_stay_in_their_own_book_folder() {
        let library = Library {
            id: "lib".into(),
            name: "lib".into(),
            library_type: "webdav".into(),
            url: "https://example.test/dav".into(),
            root_path: "/books".into(),
            username: None,
            password: None,
            last_scanned_at: None,
            created_at: String::new(),
            scraper_config: None,
        };
        let book = "https://example.test/dav/books/Book%20One";
        assert!(
            chapter_url(
                &library,
                book,
                "https://example.test/dav/books/Book%20One/part/001.mp3"
            )
            .is_ok()
        );
        for chapter in [
            "https://example.test/dav/books/Book%20Other/001.mp3",
            "https://example.test/dav/books/Book%20One/../Other/001.mp3",
            "https://other.test/dav/books/Book%20One/001.mp3",
            "https://example.test/dav/books/Book%20One/",
        ] {
            assert!(chapter_url(&library, book, chapter).is_err());
        }
    }
}
