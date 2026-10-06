//! Audio format detection, metadata reading and local file ranges.

use crate::core::app::error::{Result, TingError};
use id3::TagLike;
use std::fs::File;
use std::io::SeekFrom;
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, info};

/// Audio format enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    M4a,
    Aac,
    Flac,
    Ogg,
    Opus,
    Wma,
    Unknown,
}

impl AudioFormat {
    /// Get the MIME type for this audio format
    pub fn mime_type(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "audio/mpeg",
            AudioFormat::M4a => "audio/mp4",
            AudioFormat::Aac => "audio/aac",
            AudioFormat::Flac => "audio/flac",
            AudioFormat::Ogg => "audio/ogg",
            AudioFormat::Opus => "audio/opus",
            AudioFormat::Wma => "audio/x-ms-wma",
            AudioFormat::Unknown => "application/octet-stream",
        }
    }

    /// Get the file extension for this audio format
    pub fn extension(&self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::M4a => "m4a",
            AudioFormat::Aac => "aac",
            AudioFormat::Flac => "flac",
            AudioFormat::Ogg => "ogg",
            AudioFormat::Opus => "opus",
            AudioFormat::Wma => "wma",
            AudioFormat::Unknown => "bin",
        }
    }
}

/// Audio metadata structure
#[derive(Debug, Clone)]
pub struct AudioMetadata {
    pub format: AudioFormat,
    pub duration: Duration,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
}

/// Audio stream configuration
#[derive(Debug, Clone)]
pub struct StreamerConfig {
    pub cache_enabled: bool,
    pub cache_size: usize,
    pub buffer_size: usize,
    pub supported_formats: Vec<AudioFormat>,
}

impl Default for StreamerConfig {
    fn default() -> Self {
        Self {
            cache_enabled: true,
            cache_size: 100 * 1024 * 1024, // 100 MB
            buffer_size: 64 * 1024,        // 64 KB
            supported_formats: vec![
                AudioFormat::Mp3,
                AudioFormat::M4a,
                AudioFormat::Aac,
                AudioFormat::Flac,
                AudioFormat::Wma, // Add Wma support explicitly
            ],
        }
    }
}

/// Audio cache entry
#[derive(Debug, Clone)]
struct CacheEntry {
    metadata: AudioMetadata,
    file_size: u64,
    last_accessed: std::time::SystemTime,
}

/// Audio cache
pub(crate) struct AudioCache {
    entries: std::collections::HashMap<String, CacheEntry>,
    total_size: usize,
    max_size: usize,
}

impl AudioCache {
    pub(crate) fn new(max_size: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            total_size: 0,
            max_size,
        }
    }

    pub(crate) fn get(&mut self, key: &str) -> Option<AudioMetadata> {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_accessed = std::time::SystemTime::now();
            Some(entry.metadata.clone())
        } else {
            None
        }
    }

    pub(crate) fn insert(&mut self, key: String, metadata: AudioMetadata, file_size: u64) {
        // Simple LRU eviction if cache is full
        while self.total_size + file_size as usize > self.max_size && !self.entries.is_empty() {
            if let Some(oldest_key) = self.find_oldest_entry() {
                if let Some(entry) = self.entries.remove(&oldest_key) {
                    self.total_size = self.total_size.saturating_sub(entry.file_size as usize);
                }
            } else {
                break;
            }
        }

        self.entries.insert(
            key,
            CacheEntry {
                metadata,
                file_size,
                last_accessed: std::time::SystemTime::now(),
            },
        );
        self.total_size += file_size as usize;
    }

    fn find_oldest_entry(&self) -> Option<String> {
        self.entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_accessed)
            .map(|(key, _)| key.clone())
    }
}

pub struct AudioStreamer {
    cache: Arc<RwLock<AudioCache>>,
    config: StreamerConfig,
}

impl AudioStreamer {
    /// Create a new audio streamer with the given configuration
    pub fn new(config: StreamerConfig) -> Self {
        let cache_size = if config.cache_enabled {
            config.cache_size
        } else {
            0
        };

        Self {
            cache: Arc::new(RwLock::new(AudioCache::new(cache_size))),
            config,
        }
    }

    /// Detect the audio format of a file
    pub fn detect_format(&self, file_path: &Path) -> Result<AudioFormat> {
        let extension = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let format = match extension.as_str() {
            "mp3" => AudioFormat::Mp3,
            "m4a" | "mp4" => AudioFormat::M4a,
            "aac" => AudioFormat::Aac,
            "flac" => AudioFormat::Flac,
            "ogg" => AudioFormat::Ogg,
            "opus" => AudioFormat::Opus,
            "wma" => AudioFormat::Wma,
            _ => AudioFormat::Unknown,
        };

        debug!("Detected format {:?} for file: {:?}", format, file_path);
        Ok(format)
    }

    /// Validate that an audio file exists and is readable
    pub fn validate_audio(&self, file_path: &Path) -> Result<()> {
        if !file_path.exists() {
            return Err(TingError::NotFound(format!(
                "Audio file not found: {:?}",
                file_path
            )));
        }

        if !file_path.is_file() {
            return Err(TingError::InvalidRequest(format!(
                "Path is not a file: {:?}",
                file_path
            )));
        }

        let format = self.detect_format(file_path)?;

        // Skip validation for .strm files (they are URL redirects, not audio files)
        if format == AudioFormat::Unknown {
            let ext = file_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext == "strm" {
                return Ok(());
            }
        }

        if !self.config.supported_formats.contains(&format) {
            return Err(TingError::InvalidRequest(format!(
                "Unsupported audio format: {:?}",
                format
            )));
        }

        Ok(())
    }

    /// Read audio metadata from a file
    pub fn read_metadata(&self, file_path: &Path) -> Result<AudioMetadata> {
        // Check cache first
        if self.config.cache_enabled {
            let cache_key = file_path.to_string_lossy().to_string();
            if let Ok(mut cache) = self.cache.write()
                && let Some(metadata) = cache.get(&cache_key)
            {
                debug!("Cache hit for metadata: {:?}", file_path);
                return Ok(metadata);
            }
        }

        // Skip metadata extraction for .strm files (they are URL redirects, not audio files)
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext == "strm" {
            return Err(TingError::InvalidRequest(
                "Cannot extract metadata from .strm files (URL redirects)".to_string(),
            ));
        }

        // Validate file
        self.validate_audio(file_path)?;

        // Open file and probe format
        let file = File::open(file_path).map_err(|e| {
            TingError::IoError(std::io::Error::new(
                e.kind(),
                format!("Failed to open audio file {:?}: {}", file_path, e),
            ))
        })?;

        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        // Configure format options
        // Note: Symphonia 0.5 doesn't have a configurable probe_limit
        // The default limit is sufficient for most files
        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };

        let metadata_opts = MetadataOptions::default();

        let mut probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| {
                let error_msg = e.to_string();
                if error_msg.contains("probe limit") || error_msg.contains("unsupported format") {
                    tracing::warn!(
                        path = %file_path.display(),
                        error = %error_msg,
                        message_key = "audio.format.probe_failed",
                        message_params = %serde_json::json!({
                            "path": file_path.display().to_string(),
                            "error": error_msg,
                        }),
                        "Audio format probe failed"
                    );
                    TingError::InvalidRequest(format!(
                        "Audio format probe failed; the file may be damaged or unsupported: {}",
                        error_msg
                    ))
                } else {
                    tracing::warn!(
                        path = %file_path.display(),
                        error = %error_msg,
                        message_key = "audio.format.probe_failed",
                        message_params = %serde_json::json!({
                            "path": file_path.display().to_string(),
                            "error": error_msg,
                        }),
                        "Audio format probe failed"
                    );
                    TingError::InvalidRequest(format!("Audio format probe failed: {}", error_msg))
                }
            })?;

        let mut format_reader = probed.format;
        let track = format_reader
            .default_track()
            .ok_or_else(|| TingError::InvalidRequest("No audio track found".to_string()))?;

        let codec_params = &track.codec_params;

        // Extract metadata
        let format = self.detect_format(file_path)?;
        let sample_rate = codec_params.sample_rate.unwrap_or(44100);
        let channels = codec_params.channels.map(|c| c.count()).unwrap_or(2) as u8;

        // Calculate duration
        let duration = if let Some(n_frames) = codec_params.n_frames {
            Duration::from_secs_f64(n_frames as f64 / sample_rate as f64)
        } else {
            Duration::from_secs(0)
        };

        // Calculate bitrate
        let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        let bitrate = if duration.as_secs() > 0 {
            ((file_size * 8) / duration.as_secs()) as u32
        } else {
            0
        };

        let mut tags = Default::default();
        if let Some(metadata) = probed.metadata.get() {
            read_symphonia_tags(metadata, &mut tags);
        }
        read_symphonia_tags(format_reader.metadata(), &mut tags);
        let [
            mut title,
            mut artist,
            mut album,
            mut album_artist,
            composer,
            mut genre,
        ] = tags;

        // Try to use id3 crate for MP3 AND M4A files as fallback if metadata is missing
        // Some M4A files might contain ID3v2 tags (non-standard but common)
        // Or the file might be an MP3 renamed as M4A
        if (format == AudioFormat::Mp3 || format == AudioFormat::M4a)
            && (title.is_none() || artist.is_none() || album.is_none())
        {
            debug!("Using id3 crate fallback for {:?}", file_path);
            if let Ok(tag) = id3::Tag::read_from_path(file_path) {
                if title.is_none() {
                    title = tag.title().map(|s| s.to_string());
                }
                if artist.is_none() {
                    artist = tag.artist().map(|s| s.to_string());
                }
                if album.is_none() {
                    album = tag.album().map(|s| s.to_string());
                }
                if album_artist.is_none() {
                    album_artist = tag.album_artist().map(|s| s.to_string());
                }
                if genre.is_none() {
                    genre = tag.genre().map(|s| s.to_string());
                }
            }
        }

        let metadata = AudioMetadata {
            format,
            duration,
            bitrate,
            sample_rate,
            channels,
            title,
            artist,
            album,
            album_artist,
            composer,
            genre,
        };

        // Cache the metadata
        if self.config.cache_enabled {
            let cache_key = file_path.to_string_lossy().to_string();
            if let Ok(mut cache) = self.cache.write() {
                cache.insert(cache_key, metadata.clone(), file_size);
            }
        }

        debug!("Read metadata for {:?}: {:?}", file_path, metadata);
        Ok(metadata)
    }

    /// Get the duration of an audio file
    pub fn get_duration(&self, file_path: &Path) -> Result<Duration> {
        let metadata = self.read_metadata(file_path)?;
        Ok(metadata.duration)
    }

    /// Parse HTTP Range header
    /// Supports formats: "bytes=0-499", "bytes=500-", "bytes=-500"
    pub fn parse_range_header(&self, header: &str, file_size: u64) -> Result<Range<u64>> {
        if !header.starts_with("bytes=") {
            return Err(TingError::InvalidRequest(
                "Invalid Range header format".to_string(),
            ));
        }

        let range_str = &header[6..]; // Skip "bytes="
        let parts: Vec<&str> = range_str.split('-').collect();

        if parts.len() != 2 {
            return Err(TingError::InvalidRequest(
                "Invalid Range header format".to_string(),
            ));
        }

        let (start, end) = if parts[0].is_empty() {
            // Suffix range: bytes=-500
            let suffix_len: u64 = parts[1].parse().map_err(|_| {
                TingError::InvalidRequest("Invalid Range header: invalid suffix length".to_string())
            })?;
            let start = file_size.saturating_sub(suffix_len);
            (start, file_size)
        } else {
            let start: u64 = parts[0].parse().map_err(|_| {
                TingError::InvalidRequest("Invalid Range header: invalid start".to_string())
            })?;

            let end = if parts[1].is_empty() {
                // Open-ended range: bytes=500-
                file_size
            } else {
                let end_val: u64 = parts[1].parse().map_err(|_| {
                    TingError::InvalidRequest("Invalid Range header: invalid end".to_string())
                })?;
                end_val.saturating_add(1).min(file_size) // Inclusive end
            };

            (start, end)
        };

        if start >= file_size {
            return Err(TingError::InvalidRequest(format!(
                "Range start {} exceeds file size {}",
                start, file_size
            )));
        }

        if start >= end {
            return Err(TingError::InvalidRequest(format!(
                "Invalid range: start {} >= end {}",
                start, end
            )));
        }

        Ok(start..end)
    }

    /// Stream audio file with optional range support
    /// Returns the content type, content length, range, and the file data
    pub async fn stream_audio(
        &self,
        file_path: &Path,
        range: Option<Range<u64>>,
    ) -> Result<(String, u64, Option<Range<u64>>, Vec<u8>)> {
        // Validate file
        self.validate_audio(file_path)?;

        // Get file size
        let file_size = std::fs::metadata(file_path)
            .map(|m| m.len())
            .map_err(TingError::IoError)?;

        // Determine content type
        let format = self.detect_format(file_path)?;
        let content_type = format.mime_type().to_string();

        // Determine range to read
        let (start, end) = if let Some(ref r) = range {
            (r.start, r.end)
        } else {
            (0, file_size)
        };

        let content_length = end - start;

        // Read file data
        let mut file = tokio::fs::File::open(file_path)
            .await
            .map_err(TingError::IoError)?;

        // Seek to start position
        file.seek(SeekFrom::Start(start))
            .await
            .map_err(TingError::IoError)?;

        // Read the requested range
        let mut buffer = vec![0u8; content_length as usize];
        file.read_exact(&mut buffer)
            .await
            .map_err(TingError::IoError)?;

        info!(
            "Streaming audio: {:?}, range: {:?}, size: {}",
            file_path, range, content_length
        );

        Ok((content_type, content_length, range, buffer))
    }

    /// Build HTTP response headers for range request
    /// Returns (status_code, content_type, content_length, content_range, accept_ranges)
    pub fn build_range_response(
        &self,
        content_type: String,
        range: Option<Range<u64>>,
        total_size: u64,
    ) -> (u16, String, u64, Option<String>, String) {
        if let Some(r) = range {
            let content_length = r.end - r.start;
            let content_range = format!("bytes {}-{}/{}", r.start, r.end - 1, total_size);
            (
                206, // 206 Partial Content
                content_type,
                content_length,
                Some(content_range),
                "bytes".to_string(),
            )
        } else {
            (
                200, // 200 OK
                content_type,
                total_size,
                None,
                "bytes".to_string(),
            )
        }
    }
}

fn read_symphonia_tags(
    mut metadata: symphonia::core::meta::Metadata<'_>,
    values: &mut [Option<String>; 6],
) {
    use symphonia::core::meta::StandardTagKey;
    loop {
        if let Some(revision) = metadata.current() {
            for tag in revision.tags() {
                let index = match tag.std_key {
                    Some(StandardTagKey::TrackTitle) => 0,
                    Some(StandardTagKey::Artist) => 1,
                    Some(StandardTagKey::Album) => 2,
                    Some(StandardTagKey::AlbumArtist) => 3,
                    Some(StandardTagKey::Composer) => 4,
                    Some(StandardTagKey::Genre) => 5,
                    _ => continue,
                };
                let value = tag.value.to_string();
                if values[index].is_none() && !value.trim().is_empty() {
                    values[index] = Some(value);
                }
            }
        }
        // pop advances only when a newer revision exists; it retains the last one.
        if metadata.pop().is_none() {
            break;
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/core/audio/streamer.rs"]
mod tests;
