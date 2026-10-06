use super::*;

#[test]
fn reads_single_and_final_symphonia_metadata_revisions() {
    use symphonia::core::meta::{MetadataBuilder, MetadataLog, StandardTagKey, Tag, Value};
    let mut log = MetadataLog::default();
    let mut first = MetadataBuilder::new();
    first.add_tag(Tag::new(
        Some(StandardTagKey::TrackTitle),
        "title",
        Value::String("Chapter".into()),
    ));
    log.push(first.metadata());
    let mut values = Default::default();
    read_symphonia_tags(log.metadata(), &mut values);
    assert_eq!(values[0].as_deref(), Some("Chapter"));
    let mut last = MetadataBuilder::new();
    last.add_tag(Tag::new(
        Some(StandardTagKey::Album),
        "album",
        Value::String("Book".into()),
    ));
    log.push(last.metadata());
    read_symphonia_tags(log.metadata(), &mut values);
    assert_eq!(values[0].as_deref(), Some("Chapter"));
    assert_eq!(values[2].as_deref(), Some("Book"));
}

#[test]
fn test_audio_format_mime_types() {
    assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
    assert_eq!(AudioFormat::M4a.mime_type(), "audio/mp4");
    assert_eq!(AudioFormat::Aac.mime_type(), "audio/aac");
    assert_eq!(AudioFormat::Flac.mime_type(), "audio/flac");
}

#[test]
fn test_parse_range_header() {
    let streamer = AudioStreamer::new(StreamerConfig::default());
    let file_size = 1000u64;

    // Test normal range
    let range = streamer
        .parse_range_header("bytes=0-499", file_size)
        .unwrap();
    assert_eq!(range, 0..500);

    // Test open-ended range
    let range = streamer
        .parse_range_header("bytes=500-", file_size)
        .unwrap();
    assert_eq!(range, 500..1000);

    // Test suffix range
    let range = streamer
        .parse_range_header("bytes=-500", file_size)
        .unwrap();
    assert_eq!(range, 500..1000);

    // Test invalid range
    assert!(
        streamer
            .parse_range_header("bytes=1000-", file_size)
            .is_err()
    );
    assert!(
        streamer
            .parse_range_header("bytes=500-400", file_size)
            .is_err()
    );
}

#[test]
fn test_build_range_response() {
    let streamer = AudioStreamer::new(StreamerConfig::default());

    // Test partial content response
    let (status, content_type, content_length, content_range, accept_ranges) =
        streamer.build_range_response("audio/mpeg".to_string(), Some(0..500), 1000);

    assert_eq!(status, 206);
    assert_eq!(content_type, "audio/mpeg");
    assert_eq!(content_length, 500);
    assert_eq!(content_range, Some("bytes 0-499/1000".to_string()));
    assert_eq!(accept_ranges, "bytes");

    // Test full content response
    let (status, content_type, content_length, content_range, accept_ranges) =
        streamer.build_range_response("audio/mpeg".to_string(), None, 1000);

    assert_eq!(status, 200);
    assert_eq!(content_type, "audio/mpeg");
    assert_eq!(content_length, 1000);
    assert_eq!(content_range, None);
    assert_eq!(accept_ranges, "bytes");
}
