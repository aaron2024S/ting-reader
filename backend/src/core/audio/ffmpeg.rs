//! Bundled FFmpeg and FFprobe locations, commands and process lifetime.
//!
//! Builds ship FFmpeg and FFprobe in `bin/` beside the service executable.
//! Plugin declarations and PATH never affect binary selection.

use crate::core::app::error::{Result, TingError};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::path::{Path, PathBuf};
use tokio::process::{Child, Command};
use tokio_util::io::ReaderStream;

#[derive(Debug, Clone, Copy, Default)]
pub struct AudioService;

fn bundled_path(executable: &Path, name: &str) -> Result<PathBuf> {
    let parent = executable.parent().ok_or_else(|| {
        TingError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Cannot locate Ting Reader executable directory",
        ))
    })?;
    Ok(parent
        .join("bin")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn tool_path(name: &str) -> Result<String> {
    let executable = std::env::current_exe().map_err(TingError::IoError)?;
    let binary = bundled_path(&executable, name)?;
    if !binary.is_file() {
        return Err(TingError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Bundled {name} binary missing from {}", binary.display()),
        )));
    }
    Ok(binary.to_string_lossy().into_owned())
}

fn command(name: &str) -> Result<Command> {
    let mut command = Command::new(tool_path(name)?);
    command.kill_on_drop(true);
    Ok(command)
}

/// Create a core-owned FFmpeg process command.
///
/// Every streaming caller uses this factory so dropping a response or HLS
/// session cannot leave an orphaned encoder behind.
impl AudioService {
    pub fn ffmpeg_command() -> Result<Command> {
        command("ffmpeg")
    }

    /// Create a core-owned FFprobe process command.
    pub fn ffprobe_command() -> Result<Command> {
        command("ffprobe")
    }

    /// Read the full source duration when scanner metadata is unavailable.
    pub async fn probe_duration(path: &Path) -> Result<Option<f64>> {
        let mut probe = Self::ffprobe_command()?;
        probe
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(path);
        let output = tokio::time::timeout(std::time::Duration::from_secs(10), probe.output())
            .await
            .map_err(|_| {
                TingError::IoError(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "FFprobe duration detection timed out",
                ))
            })?
            .map_err(TingError::IoError)?;
        if !output.status.success() {
            return Err(TingError::IoError(std::io::Error::other(format!(
                "FFprobe exited with {}",
                output.status
            ))));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|duration| duration.is_finite() && *duration > 0.0))
    }

    /// Keep the encoder alive for the response lifetime and reap it at EOF.
    /// Dropping the stream also drops the child, cancelling the encoder.
    pub fn output_stream(
        mut child: Child,
    ) -> Result<impl Stream<Item = std::io::Result<Bytes>> + Send + 'static> {
        let stdout = child.stdout.take().ok_or_else(|| {
            TingError::IoError(std::io::Error::other("Failed to capture ffmpeg stdout"))
        })?;

        Ok(futures::stream::try_unfold(
            (ReaderStream::new(stdout), child),
            |(mut stdout, mut child)| async move {
                if let Some(chunk) = stdout.next().await {
                    return chunk.map(|bytes| Some((bytes, (stdout, child))));
                }

                let status = child.wait().await?;
                if !status.success() {
                    return Err(std::io::Error::other(format!(
                        "FFmpeg exited with {status}"
                    )));
                }
                Ok(None)
            },
        ))
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/core/audio/ffmpeg.rs"]
mod tests;
