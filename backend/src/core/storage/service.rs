use crate::core::app::error::{Result, TingError};
use crate::db::models::Library;
use futures::stream::TryStreamExt;
use reqwest::{Client, Url, header};
use std::io::SeekFrom;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};
use tokio_util::io::{ReaderStream, StreamReader};

#[derive(Clone)]
pub struct StorageService {
    client: Client,
}

impl Default for StorageService {
    fn default() -> Self {
        Self::new()
    }
}

impl StorageService {
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self { client }
    }

    /// Get a reader for a local file
    pub async fn get_local_reader(
        &self,
        path: &Path,
        range: Option<(u64, u64)>,
    ) -> Result<(File, u64)> {
        let mut file = File::open(path).await.map_err(TingError::IoError)?;
        let metadata = file.metadata().await.map_err(TingError::IoError)?;
        let file_size = metadata.len();

        if let Some((start, _)) = range {
            file.seek(SeekFrom::Start(start))
                .await
                .map_err(TingError::IoError)?;
        }

        Ok((file, file_size))
    }

    /// Get a reader for a WebDAV file
    pub async fn get_webdav_reader(
        &self,
        library: &Library,
        relative_path: &str,
        range: Option<(u64, u64)>,
        decryption_key: &[u8; 32],
    ) -> Result<(Box<dyn AsyncRead + Send + Unpin>, u64)> {
        // Handle case where relative_path is actually an absolute URL (legacy/bug fix)
        // If it starts with the library URL, we strip it to get the relative path
        // OR we just use it as is if it matches the base.

        let base_url =
            Url::parse(&library.url).map_err(|e| TingError::ValidationError(e.to_string()))?;
        let mut url = base_url.clone();

        if relative_path.starts_with("http://") || relative_path.starts_with("https://") {
            // It's a full URL. Check if it belongs to this library.
            // We blindly trust it for now but ideally should verify host.
            // If it's a full URL, we parse it directly.
            url =
                Url::parse(relative_path).map_err(|e| TingError::ValidationError(e.to_string()))?;
        } else {
            // Construct from components
            let root = library.root_path.as_str();
            let root = if root.is_empty() { "/" } else { root };

            // Ensure paths are joined correctly without double slashes
            let root_trimmed = root.trim_matches('/');
            let rel_trimmed = relative_path.trim_matches('/');
            let full_path_str = if root_trimmed.is_empty() {
                rel_trimmed.to_string()
            } else {
                format!("{}/{}", root_trimmed, rel_trimmed)
            };

            // Mutate the URL to append path segments
            // IMPORTANT: We must NOT manually encode segments if we use push().
            // Url::push() automatically percent-encodes the segment.
            // If `full_path_str` is already encoded (e.g. "foo%20bar"), push() will make it "foo%2520bar".
            // So we must decode first if it is encoded.
            // BUT, detecting if it is encoded is hard.
            // Assumption: `relative_path` coming from internal logic (like scanner) might be encoded or not.
            // If it comes from DB `path` column, and we store decoded path in DB, we are good.
            // If we store encoded path in DB (current state), we need to decode here.

            // Try to decode percent-encoded string
            let decoded_path = urlencoding::decode(&full_path_str)
                .map_err(|e| TingError::ValidationError(e.to_string()))?;

            {
                let mut segments = url
                    .path_segments_mut()
                    .map_err(|_| TingError::ValidationError("Invalid URL".to_string()))?;
                for segment in decoded_path.split('/') {
                    let s: &str = segment;
                    if !s.is_empty() {
                        segments.push(s);
                    }
                }
            }
        }

        let mut req = self.client.get(url.clone());

        // Add browser-like headers
        req = req
            .header("Accept", "*/*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("Accept-Encoding", "identity")
            .header("Connection", "keep-alive");

        if let (Some(u), Some(p)) = (&library.username, &library.password) {
            match crate::core::security::crypto::decrypt(p, decryption_key) {
                Ok(decrypted) => {
                    req = req.basic_auth(u, Some(decrypted));
                }
                Err(_) => {
                    req = req.basic_auth(u, Some(p));
                }
            }
        }

        if let Some((start, end)) = range {
            let end_byte = if end > 0 { end.saturating_sub(1) } else { 0 };
            if end > start {
                req = req.header("Range", format!("bytes={}-{}", start, end_byte));
            } else {
                req = req.header("Range", format!("bytes={}-", start));
            }
        }

        let res = req
            .send()
            .await
            .map_err(|e| TingError::NetworkError(e.to_string()))?;

        remote_response_reader(res, range).await
    }

    /// Get a reader for a plain HTTP/HTTPS media URL.
    pub async fn get_http_reader(
        &self,
        media_url: &str,
        range: Option<(u64, u64)>,
    ) -> Result<(Box<dyn AsyncRead + Send + Unpin>, u64)> {
        let url = Url::parse(media_url).map_err(|e| TingError::ValidationError(e.to_string()))?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(TingError::ValidationError(
                "Remote media URL must start with http:// or https://".to_string(),
            ));
        }

        let mut req = self
            .client
            .get(url.clone())
            .header("Accept", "*/*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("Accept-Encoding", "identity")
            .header("Connection", "keep-alive");

        if let Some((start, end)) = range {
            let end_byte = if end > 0 { end.saturating_sub(1) } else { 0 };
            if end > start {
                req = req.header("Range", format!("bytes={}-{}", start, end_byte));
            } else {
                req = req.header("Range", format!("bytes={}-", start));
            }
        }

        let res = req
            .send()
            .await
            .map_err(|e| TingError::NetworkError(e.to_string()))?;

        remote_response_reader(res, range).await
    }

    /// Write a sidecar to the book's WebDAV folder, using the library credentials.
    pub async fn put_webdav_sidecar(
        &self,
        library: &Library,
        book_path: &str,
        filename: &str,
        bytes: Vec<u8>,
        decryption_key: &[u8; 32],
    ) -> Result<()> {
        let url = webdav_sidecar_url(library, book_path, filename)?;
        let content_type = match filename {
            "metadata.json" => "application/json",
            "book.nfo" => "application/xml",
            _ => "application/octet-stream",
        };
        let mut request = self
            .client
            .put(url)
            .header("Content-Type", content_type)
            .body(bytes);
        if let Some(username) = &library.username {
            let password = library.password.as_deref().unwrap_or_default();
            let password = crate::core::security::crypto::decrypt(password, decryption_key)
                .unwrap_or_else(|_| password.to_string());
            request = request.basic_auth(username, Some(password));
        }
        let response = request.send().await.map_err(|error| {
            TingError::NetworkError(format!("WebDAV {filename} upload failed: {error}"))
        })?;
        if !response.status().is_success() {
            return Err(TingError::NetworkError(format!(
                "Local metadata was saved, but WebDAV {filename} upload failed (HTTP {})",
                response.status()
            )));
        }
        Ok(())
    }
}

fn webdav_sidecar_url(library: &Library, book_path: &str, filename: &str) -> Result<Url> {
    if !matches!(
        filename,
        "metadata.json" | "book.nfo" | "cover.jpg" | "cover.jpeg" | "cover.png" | "cover.webp"
    ) {
        return Err(TingError::InvalidRequest(
            "Invalid WebDAV sidecar filename".into(),
        ));
    }
    let mut target = webdav_book_url(library, book_path)?;
    target
        .path_segments_mut()
        .map_err(|_| TingError::ValidationError("Invalid WebDAV book URL".into()))?
        .pop_if_empty()
        .push(filename);
    Ok(target)
}

fn webdav_book_url(library: &Library, book_path: &str) -> Result<Url> {
    let mut root =
        Url::parse(&library.url).map_err(|error| TingError::ValidationError(error.to_string()))?;
    {
        let mut segments = root
            .path_segments_mut()
            .map_err(|_| TingError::ValidationError("Invalid WebDAV library URL".into()))?;
        segments.pop_if_empty();
        for segment in library
            .root_path
            .trim_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
        {
            segments.push(
                &urlencoding::decode(segment)
                    .map_err(|error| TingError::ValidationError(error.to_string()))?,
            );
        }
        segments.push("");
    }
    let target =
        Url::parse(book_path).map_err(|error| TingError::ValidationError(error.to_string()))?;
    if !matches!(target.scheme(), "http" | "https")
        || target.origin() != root.origin()
        || !format!("{}/", target.path().trim_end_matches('/')).starts_with(root.path())
        || !target.username().is_empty()
        || target.password().is_some()
        || target.query().is_some()
        || target.fragment().is_some()
    {
        return Err(TingError::PermissionDenied(
            "Book folder is outside its WebDAV library".into(),
        ));
    }
    Ok(target)
}

async fn remote_response_reader(
    response: reqwest::Response,
    range: Option<(u64, u64)>,
) -> Result<(Box<dyn AsyncRead + Send + Unpin>, u64)> {
    let status = response.status();
    let content_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok());
    // An unknown-length prefix can end exactly at EOF. A range at that
    // boundary has an empty remainder, rather than a failed playback.
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE
        && let Some((start, _)) = range
        && let Some(total) = content_range
            .and_then(|value| value.strip_prefix("bytes */"))
            .and_then(|value| value.parse::<u64>().ok())
        && start == total
    {
        return Ok((Box::new(tokio::io::empty()), total));
    }
    if !status.is_success() {
        return Err(TingError::NetworkError(format!(
            "Remote media request failed: {status}"
        )));
    }
    let partial = status == reqwest::StatusCode::PARTIAL_CONTENT;
    let total_size = if partial {
        let (bounds, total) = content_range
            .and_then(|value| value.strip_prefix("bytes "))
            .and_then(|value| value.split_once('/'))
            .ok_or_else(|| TingError::NetworkError("Missing remote Content-Range".into()))?;
        let (start, end) = bounds
            .split_once('-')
            .and_then(|(start, end)| Some((start.parse::<u64>().ok()?, end.parse::<u64>().ok()?)))
            .ok_or_else(|| TingError::NetworkError("Invalid remote Content-Range".into()))?;
        if start != range.map_or(0, |range| range.0) || end < start {
            return Err(TingError::NetworkError(
                "Remote Content-Range does not match the requested offset".into(),
            ));
        }
        if total == "*" {
            0
        } else {
            let total = total
                .parse::<u64>()
                .map_err(|_| TingError::NetworkError("Invalid remote total length".into()))?;
            if end >= total {
                return Err(TingError::NetworkError(
                    "Remote Content-Range exceeds the source length".into(),
                ));
            }
            total
        }
    } else {
        response.content_length().unwrap_or(0)
    };
    let stream = response.bytes_stream().map_err(std::io::Error::other);
    let mut reader: Box<dyn AsyncRead + Send + Unpin> = Box::new(StreamReader::new(stream));
    if let Some((start, end)) = range {
        if !partial && start > 0 {
            // Some sources ignore Range and return 200 from byte zero. Discard
            // only the prefix with a bounded buffer; never concatenate it twice.
            let skipped =
                tokio::io::copy(&mut (&mut reader).take(start), &mut tokio::io::sink()).await?;
            if skipped != start {
                return Err(TingError::IoError(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Remote source ended before the requested offset",
                )));
            }
        }
        if end > start {
            reader = Box::new(reader.take(end - start));
        }
    }
    Ok((reader, total_size))
}

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
        let password = crate::core::security::crypto::decrypt(password, key)
            .unwrap_or_else(|_| password.to_string());
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
#[path = "../../../tests/unit/core/storage/service.rs"]
mod tests;
