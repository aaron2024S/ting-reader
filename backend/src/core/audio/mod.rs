//! Core audio services for metadata, local file ranges and FFmpeg processes.

pub mod ffmpeg;
pub mod streamer;

pub use ffmpeg::AudioService;
pub use streamer::{AudioFormat, AudioMetadata, AudioStreamer, StreamerConfig};
