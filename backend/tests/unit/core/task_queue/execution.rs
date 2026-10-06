use crate::core::audio::metadata::detect_audio_format;
use std::fs;

fn id3_header(payload_size: u32) -> [u8; 10] {
    [
        b'I',
        b'D',
        b'3',
        3,
        0,
        0,
        ((payload_size >> 21) & 0x7f) as u8,
        ((payload_size >> 14) & 0x7f) as u8,
        ((payload_size >> 7) & 0x7f) as u8,
        (payload_size & 0x7f) as u8,
    ]
}

#[test]
fn detects_mp4_hidden_behind_mp3_extension_and_id3() {
    let path = std::env::temp_dir().join(format!("ting-format-{}.mp3", uuid::Uuid::new_v4()));
    let mut bytes = id3_header(4).to_vec();
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&[0, 0, 0, 28, b'f', b't', b'y', b'p', b'M', b'4', b'A', b' ']);
    fs::write(&path, bytes).unwrap();

    assert_eq!(detect_audio_format(&path).as_deref(), Some("m4a"));
    fs::remove_file(path).unwrap();
}

#[test]
fn detects_mp4_with_box_size_damaged_by_id3_rewrite() {
    let path = std::env::temp_dir().join(format!("ting-format-{}.mp3", uuid::Uuid::new_v4()));
    let mut bytes = id3_header(0).to_vec();
    bytes.extend_from_slice(&[28, b'f', b't', b'y', b'p', b'M', b'4', b'A', b' ']);
    fs::write(&path, bytes).unwrap();

    assert_eq!(detect_audio_format(&path).as_deref(), Some("m4a"));
    fs::remove_file(path).unwrap();
}

#[test]
fn detects_standard_mp3_after_id3() {
    let path = std::env::temp_dir().join(format!("ting-format-{}.mp3", uuid::Uuid::new_v4()));
    let mut bytes = id3_header(0).to_vec();
    bytes.extend_from_slice(&[0xff, 0xfb, 0x50, 0x00]);
    fs::write(&path, bytes).unwrap();

    assert_eq!(detect_audio_format(&path).as_deref(), Some("mp3"));
    fs::remove_file(path).unwrap();
}

#[test]
fn detects_wma_asf_header() {
    let path = std::env::temp_dir().join(format!("ting-format-{}.bin", uuid::Uuid::new_v4()));
    fs::write(
        &path,
        [
            0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa,
        ],
    )
    .unwrap();

    assert_eq!(detect_audio_format(&path).as_deref(), Some("wma"));
    fs::remove_file(path).unwrap();
}
