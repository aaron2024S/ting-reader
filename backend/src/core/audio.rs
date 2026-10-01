//! Core-owned FFmpeg binary locations.
//!
//! Builds ship FFmpeg and FFprobe in `bin/` beside the service executable.
//! Plugin declarations and PATH never affect binary selection.

use crate::core::error::{Result, TingError};
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
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    fn fixture_process(mode: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::audio::tests::stream_process_fixture",
                "--nocapture",
            ])
            .env("TING_AUDIO_STREAM_TEST_MODE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[test]
    fn stream_process_fixture() {
        let Ok(mode) = std::env::var("TING_AUDIO_STREAM_TEST_MODE") else {
            return;
        };
        std::io::stdout().write_all(b"encoder-ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        match mode.as_str() {
            "success" => {
                std::thread::sleep(Duration::from_millis(100));
                std::io::stdout().write_all(b"encoder-finished\n").unwrap();
            }
            "failure" => std::process::exit(7),
            "running" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            _ => panic!("Unknown encoder fixture mode"),
        }
    }

    #[tokio::test]
    async fn response_stream_keeps_encoder_alive_until_all_output_is_read() {
        let stream = AudioService::output_stream(fixture_process("success")).unwrap();
        // The handler has already returned, but the body may be polled later.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let output = tokio::time::timeout(Duration::from_secs(5), async move {
            futures::pin_mut!(stream);
            let mut output = Vec::new();
            while let Some(chunk) = stream.next().await {
                output.extend_from_slice(&chunk.unwrap());
            }
            output
        })
        .await
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("encoder-ready"));
        assert!(output.contains("encoder-finished"));
    }

    #[tokio::test]
    async fn dropping_response_stream_terminates_encoder() {
        let mut child = fixture_process("running");
        let mut stderr = child.stderr.take().unwrap();
        let mut stream = Box::pin(AudioService::output_stream(child).unwrap());
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut output = Vec::new();
            while !String::from_utf8_lossy(&output).contains("encoder-ready") {
                output.extend_from_slice(&stream.next().await.unwrap().unwrap());
            }
        })
        .await
        .unwrap();
        drop(stream);
        // The sleeping fixture keeps stderr open until its process is killed.
        tokio::time::timeout(Duration::from_secs(5), stderr.read_to_end(&mut Vec::new()))
            .await
            .expect("Encoder survived response cancellation")
            .unwrap();
    }

    #[tokio::test]
    async fn encoder_failure_is_reported_as_a_stream_error() {
        let stream = AudioService::output_stream(fixture_process("failure")).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), async move {
            futures::pin_mut!(stream);
            while let Some(chunk) = stream.next().await {
                if let Err(error) = chunk {
                    return error;
                }
            }
            panic!("Failed encoder returned a successful EOF");
        })
        .await
        .unwrap();
        assert!(error.to_string().contains("FFmpeg exited with"));
        assert!(error.to_string().contains('7'));
    }

    #[test]
    fn binary_paths_are_fixed_beside_the_service_executable() {
        let executable = Path::new("root/bin/ting-reader");
        assert_eq!(
            bundled_path(executable, "ffmpeg").unwrap(),
            Path::new("root/bin/bin").join(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn process_factories_use_the_bundled_tool_names() {
        let executable = Path::new("root/ting-reader.exe");
        let ffmpeg = bundled_path(executable, "ffmpeg").unwrap();
        let ffprobe = bundled_path(executable, "ffprobe").unwrap();
        assert!(ffmpeg.ends_with(format!("ffmpeg{}", std::env::consts::EXE_SUFFIX)));
        assert!(ffprobe.ends_with(format!("ffprobe{}", std::env::consts::EXE_SUFFIX)));
    }
}
