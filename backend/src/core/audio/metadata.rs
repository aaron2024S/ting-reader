//! Standard-container tags, embedded covers and transactional metadata writes.

use super::AudioService;
use crate::core::app::error::{Result, TingError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

pub(crate) const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct AudioInput {
    source: OsString,
    authorization: Option<String>,
}

impl AudioInput {
    pub(crate) fn local(path: &Path) -> Self {
        Self {
            source: path.as_os_str().into(),
            authorization: None,
        }
    }

    pub(crate) fn remote(url: url::Url, authorization: Option<String>) -> Self {
        Self {
            source: url.to_string().into(),
            authorization,
        }
    }

    fn configure(&self, command: &mut Command) {
        if let Some(authorization) = &self.authorization {
            command.args(["-headers", authorization]);
        }
        command.args(["-rw_timeout", "15000000"]);
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AudioFileMetadata {
    pub(crate) title: Option<String>,
    pub(crate) album: Option<String>,
    pub(crate) artist: Option<String>,
    pub(crate) album_artist: Option<String>,
    pub(crate) composer: Option<String>,
    pub(crate) genre: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) duration: f64,
    pub(crate) cover_url: Option<String>,
}

impl AudioFileMetadata {
    pub(crate) fn author_narrator(&self) -> (Option<String>, Option<String>) {
        let author = self.album_artist.clone().or_else(|| self.artist.clone());
        let narrator = self
            .artist
            .clone()
            .filter(|artist| Some(artist) != author.as_ref())
            .or_else(|| self.composer.clone());
        (author, narrator)
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AudioMetadataUpdate {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
    pub(crate) album_artist: String,
    pub(crate) composer: String,
    pub(crate) genre: String,
    pub(crate) description: String,
}

#[derive(Default, Deserialize)]
struct ProbeSection {
    #[serde(default)]
    tags: BTreeMap<String, String>,
    duration: Option<String>,
}

#[derive(Default, Deserialize)]
struct ProbeStream {
    index: usize,
    codec_name: Option<String>,
    codec_type: Option<String>,
    #[serde(default)]
    disposition: BTreeMap<String, i32>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    format: ProbeSection,
    #[serde(default)]
    streams: Vec<ProbeStream>,
}

fn parse_probe(bytes: &[u8]) -> Result<(AudioFileMetadata, Option<usize>)> {
    let probe: ProbeOutput = serde_json::from_slice(bytes)
        .map_err(|error| TingError::InvalidRequest(format!("Invalid FFprobe metadata: {error}")))?;
    let mut tags = BTreeMap::new();
    for section in std::iter::once(&probe.format.tags).chain(
        probe
            .streams
            .iter()
            .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
            .map(|stream| &stream.tags),
    ) {
        for (key, value) in section {
            if !value.trim().is_empty() {
                tags.entry(key.to_ascii_lowercase())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    let tag = |keys: &[&str]| keys.iter().find_map(|key| tags.get(*key).cloned());
    let metadata = AudioFileMetadata {
        title: tag(&["title", "name", "nam"]),
        artist: tag(&["artist", "author", "art"]),
        album: tag(&["album", "wm/albumtitle", "alb"]),
        album_artist: tag(&["album_artist", "albumartist", "wm/albumartist"]),
        composer: tag(&["composer", "wm/composer"]),
        genre: tag(&["genre", "wm/genre"]),
        description: tag(&["description", "synopsis", "comment", "desc"]),
        duration: probe
            .format
            .duration
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or_default(),
        cover_url: None,
    };
    let cover = probe
        .streams
        .iter()
        .find(|stream| stream.disposition.get("attached_pic") == Some(&1))
        .or_else(|| {
            probe.streams.iter().find(|stream| {
                stream.codec_type.as_deref() == Some("video")
                    && matches!(
                        stream.codec_name.as_deref(),
                        Some("mjpeg" | "png" | "webp" | "gif")
                    )
            })
        })
        .map(|stream| stream.index);
    Ok((metadata, cover))
}

pub(crate) struct TemporaryAudioFile(pub(crate) PathBuf);

impl Drop for TemporaryAudioFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl AudioService {
    pub(crate) async fn read_file_metadata(
        input: &AudioInput,
        cover_dir: Option<&Path>,
    ) -> Result<AudioFileMetadata> {
        let mut command = Self::ffprobe_command()?;
        command.args(["-v", "error"]);
        input.configure(&mut command);
        command.args([
            "-show_entries",
            "format=duration:format_tags:stream=index,codec_name,codec_type:stream_tags:stream_disposition=attached_pic",
            "-of", "json",
        ]).arg(&input.source);
        let bytes = Self::bounded_output(command, 1024 * 1024, Duration::from_secs(30)).await?;
        let (mut metadata, cover) = parse_probe(&bytes)?;
        if let (Some(dir), Some(index)) = (cover_dir, cover) {
            match Self::extract_cover(input, index, dir).await {
                Ok(cover) => metadata.cover_url = Some(cover),
                Err(error) => {
                    tracing::warn!(error = %error, "Could not extract embedded audio cover")
                }
            }
        }
        Ok(metadata)
    }

    async fn extract_cover(input: &AudioInput, index: usize, dir: &Path) -> Result<String> {
        for extension in ["jpg", "jpeg", "png", "webp", "gif"] {
            let path = dir.join(format!("cover.{extension}"));
            if path.is_file() {
                return Ok(path.to_string_lossy().replace('\\', "/"));
            }
        }
        let mut command = Self::ffmpeg_command()?;
        command.args(["-v", "error", "-nostdin"]);
        input.configure(&mut command);
        command.arg("-i").arg(&input.source).args([
            "-map",
            &format!("0:{index}"),
            "-frames:v",
            "1",
            "-c:v",
            "copy",
            "-f",
            "image2",
            "-update",
            "1",
            "pipe:1",
        ]);
        let bytes = Self::bounded_output(command, MAX_COVER_BYTES, Duration::from_secs(30)).await?;
        let extension = match image::guess_format(&bytes) {
            Ok(image::ImageFormat::Jpeg) => "jpg",
            Ok(image::ImageFormat::Png) => "png",
            Ok(image::ImageFormat::WebP) => "webp",
            Ok(image::ImageFormat::Gif) => "gif",
            _ => {
                return Err(TingError::InvalidRequest(
                    "Invalid embedded cover image".into(),
                ));
            }
        };
        tokio::fs::create_dir_all(dir).await?;
        let target = dir.join(format!("cover.{extension}"));
        let staging = TemporaryAudioFile(dir.join(format!(".cover-{}.tmp", uuid::Uuid::new_v4())));
        tokio::task::spawn_blocking(move || {
            std::fs::write(&staging.0, bytes)?;
            std::fs::rename(&staging.0, &target)?;
            Ok::<_, TingError>(target.to_string_lossy().replace('\\', "/"))
        })
        .await
        .map_err(|error| TingError::TaskError(error.to_string()))?
    }

    pub(crate) async fn write_file_metadata(
        path: &Path,
        update: AudioMetadataUpdate,
        cover_path: Option<&Path>,
    ) -> Result<()> {
        let original = tokio::fs::metadata(path).await?;
        if original.permissions().readonly() {
            return Err(TingError::PermissionDenied(
                "Audio file is read-only".into(),
            ));
        }
        let format = detect_audio_format(path)
            .or_else(|| {
                path.extension()
                    .and_then(|value| value.to_str())
                    .map(str::to_ascii_lowercase)
            })
            .unwrap_or_default();
        if !matches!(
            format.as_str(),
            "mp3" | "m4a" | "m4b" | "mp4" | "flac" | "ogg" | "opus" | "wav" | "aac" | "wma"
        ) {
            return Err(TingError::InvalidRequest(format!(
                "Unsupported audio container: {format}"
            )));
        }
        let parent = path
            .parent()
            .ok_or_else(|| TingError::InvalidRequest("Missing audio folder".into()))?;
        let staging = TemporaryAudioFile(parent.join(format!(
            ".metadata-{}.{}",
            uuid::Uuid::new_v4(),
            format
        )));
        let is_wma = format == "wma";
        let cover = if let Some(path) = cover_path {
            let path = path.to_path_buf();
            Some(
                tokio::task::spawn_blocking(move || normalized_cover(&path, is_wma))
                    .await
                    .map_err(|error| TingError::TaskError(error.to_string()))??,
            )
        } else {
            None
        };
        let source = path.to_path_buf();
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let staging = tokio::task::spawn_blocking(move || {
            if is_wma {
                super::asf::write_metadata(
                    &source,
                    &staging.0,
                    &update,
                    cover.as_deref(),
                    &cancel,
                )?;
            } else {
                copy_audio(&source, &staging.0, &format, &cancel)?;
                write_lofty_metadata(&staging.0, &format, &update, cover)?;
            }
            Ok::<_, TingError>(staging)
        })
        .await
        .map_err(|error| TingError::TaskError(error.to_string()))??;
        if tokio::fs::metadata(&staging.0).await?.len() == 0 {
            return Err(TingError::InvalidRequest(
                "Metadata writer created an empty audio file".into(),
            ));
        }
        let current = tokio::fs::metadata(path).await?;
        if current.len() != original.len() || current.modified()? != original.modified()? {
            return Err(TingError::InvalidRequest(
                "Audio file changed during metadata writing".into(),
            ));
        }
        tokio::fs::set_permissions(&staging.0, original.permissions()).await?;
        tokio::fs::rename(&staging.0, path).await?;
        Ok(())
    }
}

pub(super) fn copy_audio(
    source: &Path,
    target: &Path,
    format: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    use std::io::Write;
    let mut source = std::fs::File::open(source)?;
    let mut target = std::fs::File::create(target)?;
    if matches!(format, "m4a" | "m4b" | "mp4") && prepare_mp4_source(&mut source)? {
        target.write_all(&[0, 0, 0])?;
    }
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(TingError::TaskError(
                "Audio metadata writing was cancelled".into(),
            ));
        }
        let length = source.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        target.write_all(&buffer[..length])?;
    }
    target.sync_all()?;
    Ok(())
}

fn prepare_mp4_source(source: &mut std::fs::File) -> Result<bool> {
    let mut header = [0; 10];
    let length = source.read(&mut header)?;
    if length < 10 || &header[..3] != b"ID3" {
        source.seek(SeekFrom::Start(0))?;
        return Ok(false);
    }
    if header[6..10].iter().any(|byte| byte & 0x80 != 0) {
        return Err(TingError::InvalidRequest("Invalid leading ID3 tag".into()));
    }
    let offset = 10
        + ((u64::from(header[6]) & 0x7f) << 21)
        + ((u64::from(header[7]) & 0x7f) << 14)
        + ((u64::from(header[8]) & 0x7f) << 7)
        + (u64::from(header[9]) & 0x7f)
        + if header[3] == 4 && header[5] & 0x10 != 0 {
            10
        } else {
            0
        };
    source.seek(SeekFrom::Start(offset))?;
    let mut media = [0; 8];
    source.read_exact(&mut media)?;
    let repair = &media[1..5] == b"ftyp";
    if &media[4..8] != b"ftyp" && !repair {
        return Err(TingError::InvalidRequest(
            "Leading ID3 tag does not contain MP4 audio".into(),
        ));
    }
    source.seek(SeekFrom::Start(offset))?;
    Ok(repair)
}

fn write_lofty_metadata(
    path: &Path,
    format: &str,
    update: &AudioMetadataUpdate,
    cover: Option<Vec<u8>>,
) -> Result<()> {
    use lofty::config::{ParseOptions, ParsingMode, WriteOptions};
    use lofty::prelude::*;
    let options = ParseOptions::new()
        .read_properties(false)
        .parsing_mode(ParsingMode::Relaxed);
    let mut probe = lofty::probe::Probe::open(path)
        .map_err(|error| TingError::InvalidRequest(error.to_string()))?
        .guess_file_type()?;
    if matches!(format, "m4a" | "mp4" | "m4b") {
        probe = probe.set_file_type(lofty::file::FileType::Mp4);
    }
    let mut file = probe
        .options(options)
        .read()
        .map_err(|error| TingError::InvalidRequest(error.to_string()))?;
    let kind = file.file_type().primary_tag_type();
    if file.tag(kind).is_none() {
        file.insert_tag(lofty::tag::Tag::new(kind));
    }
    let tag = file
        .tag_mut(kind)
        .ok_or_else(|| TingError::InvalidRequest("Missing audio tag".into()))?;
    tag.set_title(update.title.clone());
    tag.set_artist(update.artist.clone());
    tag.set_album(update.album.clone());
    tag.set_genre(update.genre.clone());
    tag.set_comment(update.description.clone());
    tag.insert_text(ItemKey::AlbumArtist, update.album_artist.clone());
    tag.insert_text(ItemKey::Composer, update.composer.clone());
    if let Some(bytes) = cover {
        while !tag.pictures().is_empty() {
            tag.remove_picture(0);
        }
        let mut picture = lofty::picture::Picture::from_reader(&mut std::io::Cursor::new(bytes))
            .map_err(|error| TingError::InvalidRequest(error.to_string()))?;
        picture.set_pic_type(lofty::picture::PictureType::CoverFront);
        tag.push_picture(picture);
    }
    file.save_to_path(path, WriteOptions::default())
        .map_err(|error| TingError::InvalidRequest(error.to_string()))?;
    Ok(())
}

fn normalized_cover(path: &Path, small: bool) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_COVER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_COVER_BYTES {
        return Err(TingError::InvalidRequest("Cover image is too large".into()));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| TingError::InvalidRequest(error.to_string()))?;
    for size in if small {
        &[600, 500, 400, 300, 240][..]
    } else {
        &[8192][..]
    } {
        let rgb = if image.width() > *size || image.height() > *size {
            image.thumbnail(*size, *size).to_rgb8()
        } else {
            image.to_rgb8()
        };
        for quality in if small {
            &[88, 80, 72, 64, 56][..]
        } else {
            &[90][..]
        } {
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, *quality)
                .encode_image(&rgb)
                .map_err(|error| TingError::InvalidRequest(error.to_string()))?;
            if !small || jpeg.len() < 65_490 {
                return Ok(jpeg);
            }
        }
    }
    Err(TingError::InvalidRequest(
        "Cannot fit cover into the ASF attribute limit".into(),
    ))
}

pub(crate) fn detect_audio_format(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0_u8; 12];
    if file.read(&mut header).ok()? < 4 {
        return None;
    }
    let mut media_offset = 0_u64;
    if &header[..3] == b"ID3" {
        media_offset = 10
            + ((u64::from(header[6]) & 0x7f) << 21)
            + ((u64::from(header[7]) & 0x7f) << 14)
            + ((u64::from(header[8]) & 0x7f) << 7)
            + (u64::from(header[9]) & 0x7f);
        file.seek(SeekFrom::Start(media_offset)).ok()?;
        header.fill(0);
        if file.read(&mut header).ok()? < 4 {
            return None;
        }
    }
    let format = if &header[4..8] == b"ftyp" || (media_offset > 0 && &header[1..5] == b"ftyp") {
        "m4a"
    } else if header.starts_with(b"fLaC") {
        "flac"
    } else if header.starts_with(b"OggS") {
        "ogg"
    } else if header.starts_with(b"RIFF") && &header[8..12] == b"WAVE" {
        "wav"
    } else if header
        == [
            0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa,
        ]
    {
        "wma"
    } else if header[0] == 0xff && header[1] & 0xf6 == 0xf0 {
        "aac"
    } else if header[0] == 0xff && header[1] & 0xe0 == 0xe0 {
        "mp3"
    } else {
        return None;
    };
    Some(format.into())
}

#[cfg(test)]
#[path = "../../../tests/unit/core/audio/metadata.rs"]
mod tests;
