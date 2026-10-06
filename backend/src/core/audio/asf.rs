//! ASF tag updates preserve unknown header objects and the complete audio payload.

use super::metadata::AudioMetadataUpdate;
use crate::core::app::error::{Result, TingError};
use std::io::{Cursor, Read, Write};
use std::path::Path;

const HEADER: [u8; 16] = [
    0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0, 0xaa, 0, 0x62, 0xce, 0x6c,
];
const CONTENT: [u8; 16] = [
    0x33, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0, 0xaa, 0, 0x62, 0xce, 0x6c,
];
const EXTENDED: [u8; 16] = [
    0x40, 0xa4, 0xd0, 0xd2, 7, 0xe3, 0xd2, 0x11, 0x97, 0xf0, 0, 0xa0, 0xc9, 0x5e, 0xa8, 0x50,
];
const PROPERTIES: [u8; 16] = [
    0xa1, 0xdc, 0xab, 0x8c, 0x47, 0xa9, 0xcf, 0x11, 0x8e, 0xe4, 0, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];
const EXTENSION: [u8; 16] = [
    0xb5, 3, 0xbf, 0x5f, 0x2e, 0xa9, 0xcf, 0x11, 0x8e, 0xe3, 0, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];
const METADATA: [u8; 16] = [
    0xc5, 0xf8, 0xcb, 0xea, 0xaf, 0x5b, 0x77, 0x48, 0x84, 0x67, 0xaa, 0x8c, 0x44, 0xfa, 0x4c, 0xca,
];
const LIBRARY: [u8; 16] = [
    0x94, 0x1c, 0x23, 0x44, 0x98, 0x94, 0xd1, 0x49, 0xa1, 0x41, 0x1d, 0x13, 0x4e, 0x45, 0x70, 0x54,
];

fn invalid() -> TingError {
    TingError::InvalidRequest("Invalid or oversized ASF metadata".into())
}

fn read<const N: usize>(cursor: &mut Cursor<&[u8]>) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    cursor.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn take<'a>(cursor: &mut Cursor<&'a [u8]>, length: usize) -> Result<&'a [u8]> {
    let start = cursor.position() as usize;
    let end = start.checked_add(length).ok_or_else(invalid)?;
    let bytes = cursor.get_ref().get(start..end).ok_or_else(invalid)?;
    cursor.set_position(end as u64);
    Ok(bytes)
}

fn utf16(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn text(bytes: &[u8]) -> String {
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

fn length16(length: usize) -> Result<[u8; 2]> {
    Ok(u16::try_from(length).map_err(|_| invalid())?.to_le_bytes())
}

fn object(guid: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(24 + payload.len());
    bytes.extend_from_slice(guid);
    bytes.extend_from_slice(&((24 + payload.len()) as u64).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn objects(mut bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let header = bytes.get(..24).ok_or_else(invalid)?;
        let length = usize::try_from(u64::from_le_bytes(
            header[16..24].try_into().map_err(|_| invalid())?,
        ))
        .map_err(|_| invalid())?;
        if length < 24 || length > bytes.len() {
            return Err(invalid());
        }
        result.push(&bytes[..length]);
        bytes = &bytes[length..];
    }
    Ok(result)
}

fn content(existing: Option<&[u8]>, update: &AudioMetadataUpdate) -> Result<Vec<u8>> {
    let mut fields = vec![Vec::new(); 5];
    if let Some(bytes) = existing {
        let mut cursor = Cursor::new(bytes);
        let lengths = (0..5)
            .map(|_| read::<2>(&mut cursor).map(u16::from_le_bytes))
            .collect::<Result<Vec<_>>>()?;
        for (field, length) in fields.iter_mut().zip(lengths) {
            *field = take(&mut cursor, usize::from(length))?.to_vec();
        }
    }
    fields[0] = utf16(&update.title);
    fields[1] = utf16(&update.artist);
    fields[3] = utf16(&update.description);
    let mut payload = Vec::new();
    for field in &fields {
        payload.extend_from_slice(&length16(field.len())?);
    }
    for field in fields {
        payload.extend_from_slice(&field);
    }
    Ok(object(&CONTENT, &payload))
}

fn replaced(name: &str, cover: bool) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "title"
            | "author"
            | "description"
            | "wm/albumtitle"
            | "wm/albumartist"
            | "wm/genre"
            | "wm/composer"
    ) || (cover && name.eq_ignore_ascii_case("WM/Picture"))
}

fn extended(
    existing: Option<&[u8]>,
    update: &AudioMetadataUpdate,
    cover: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let mut attributes = Vec::new();
    let mut count = 0_u16;
    if let Some(bytes) = existing {
        let mut cursor = Cursor::new(bytes);
        let entries = u16::from_le_bytes(read(&mut cursor)?);
        for _ in 0..entries {
            let start = cursor.position() as usize;
            let length = u16::from_le_bytes(read(&mut cursor)?);
            let name = text(take(&mut cursor, usize::from(length))?);
            let _kind = read::<2>(&mut cursor)?;
            let length = u16::from_le_bytes(read(&mut cursor)?);
            take(&mut cursor, usize::from(length))?;
            if !replaced(&name, cover.is_some()) {
                attributes.extend_from_slice(&bytes[start..cursor.position() as usize]);
                count = count.checked_add(1).ok_or_else(invalid)?;
            }
        }
    }
    for (name, value) in [
        ("WM/AlbumTitle", &update.album),
        ("WM/AlbumArtist", &update.album_artist),
        ("WM/Genre", &update.genre),
        ("WM/Composer", &update.composer),
    ] {
        attribute(&mut attributes, name, 0, &utf16(value))?;
        count = count.checked_add(1).ok_or_else(invalid)?;
    }
    if let Some(jpeg) = cover {
        let mut picture = vec![3];
        picture.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
        picture.extend_from_slice(&utf16("image/jpeg"));
        picture.extend_from_slice(&utf16("Cover"));
        picture.extend_from_slice(jpeg);
        attribute(&mut attributes, "WM/Picture", 1, &picture)?;
        count = count.checked_add(1).ok_or_else(invalid)?;
    }
    let mut payload = count.to_le_bytes().to_vec();
    payload.extend_from_slice(&attributes);
    Ok(object(&EXTENDED, &payload))
}

fn attribute(bytes: &mut Vec<u8>, name: &str, kind: u16, value: &[u8]) -> Result<()> {
    let name = utf16(name);
    bytes.extend_from_slice(&length16(name.len())?);
    bytes.extend_from_slice(&name);
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(&length16(value.len())?);
    bytes.extend_from_slice(value);
    Ok(())
}

fn filter_metadata(bytes: &[u8], cover: bool) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(bytes);
    let count = u16::from_le_bytes(read(&mut cursor)?);
    let mut records = Vec::new();
    let mut kept = 0_u16;
    for _ in 0..count {
        let start = cursor.position() as usize;
        let _language_stream = read::<4>(&mut cursor)?;
        let name_length = u16::from_le_bytes(read(&mut cursor)?);
        let _kind = read::<2>(&mut cursor)?;
        let data_length = u32::from_le_bytes(read(&mut cursor)?);
        let name = text(take(&mut cursor, usize::from(name_length))?);
        take(&mut cursor, data_length as usize)?;
        if !replaced(&name, cover) {
            records.extend_from_slice(&bytes[start..cursor.position() as usize]);
            kept = kept.checked_add(1).ok_or_else(invalid)?;
        }
    }
    let mut payload = kept.to_le_bytes().to_vec();
    payload.extend_from_slice(&records);
    Ok(payload)
}

fn extension(bytes: &[u8], cover: bool) -> Result<Vec<u8>> {
    let prefix = bytes.get(..22).ok_or_else(invalid)?;
    let length = u32::from_le_bytes(prefix[18..22].try_into().map_err(|_| invalid())?) as usize;
    let mut children = Vec::new();
    for child in objects(bytes.get(22..22 + length).ok_or_else(invalid)?)? {
        if child[..16] == METADATA || child[..16] == LIBRARY {
            children.extend_from_slice(&object(
                &child[..16],
                &filter_metadata(&child[24..], cover)?,
            ));
        } else {
            children.extend_from_slice(child);
        }
    }
    let mut payload = prefix[..18].to_vec();
    payload.extend_from_slice(&(children.len() as u32).to_le_bytes());
    payload.extend_from_slice(&children);
    Ok(object(&EXTENSION, &payload))
}

pub(super) fn write_metadata(
    source: &Path,
    target: &Path,
    update: &AudioMetadataUpdate,
    cover: Option<&[u8]>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut source = std::fs::File::open(source)?;
    let original_size = source.metadata()?.len();
    let mut prefix = [0; 30];
    source.read_exact(&mut prefix)?;
    let size = u64::from_le_bytes(prefix[16..24].try_into().map_err(|_| invalid())?);
    if prefix[..16] != HEADER || !(30..=8 * 1024 * 1024).contains(&size) || size > original_size {
        return Err(invalid());
    }
    let mut header = vec![0; size as usize - 30];
    source.read_exact(&mut header)?;
    let originals = objects(&header)?;
    if originals.len()
        != u32::from_le_bytes(prefix[24..28].try_into().map_err(|_| invalid())?) as usize
    {
        return Err(invalid());
    }
    let mut result = Vec::new();
    let mut content_written = false;
    let mut extended_written = false;
    for original in originals {
        let updated = if original[..16] == CONTENT {
            content_written = true;
            content(Some(&original[24..]), update)?
        } else if original[..16] == EXTENDED {
            extended_written = true;
            extended(Some(&original[24..]), update, cover)?
        } else if original[..16] == EXTENSION {
            extension(&original[24..], cover.is_some())?
        } else if original[..16] == METADATA || original[..16] == LIBRARY {
            object(
                &original[..16],
                &filter_metadata(&original[24..], cover.is_some())?,
            )
        } else {
            original.to_vec()
        };
        result.push(updated);
    }
    if !content_written {
        result.push(content(None, update)?);
    }
    if !extended_written {
        result.push(extended(None, update, cover)?);
    }
    let new_size = 30 + result.iter().map(Vec::len).sum::<usize>();
    if new_size > 8 * 1024 * 1024 {
        return Err(invalid());
    }
    let file_size = original_size - size + new_size as u64;
    for child in &mut result {
        if child[..16] == PROPERTIES {
            child
                .get_mut(40..48)
                .ok_or_else(invalid)?
                .copy_from_slice(&file_size.to_le_bytes());
        }
    }
    prefix[16..24].copy_from_slice(&(new_size as u64).to_le_bytes());
    prefix[24..28].copy_from_slice(&(result.len() as u32).to_le_bytes());
    let mut target = std::fs::File::create(target)?;
    target.write_all(&prefix)?;
    for child in result {
        target.write_all(&child)?;
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
