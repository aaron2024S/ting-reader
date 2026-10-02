use super::*;

#[test]
fn rejects_non_images_and_truncated_images() {
    assert!(normalize_cover(b"not an image".to_vec()).is_err());
    assert!(normalize_cover(b"\x89PNG\r\n\x1a\n".to_vec()).is_err());
}

#[test]
fn normalizes_transparent_images_to_a_white_jpeg() {
    let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let jpeg = normalize_cover(png.into_inner()).unwrap();
    assert_eq!(
        image::guess_format(&jpeg).unwrap(),
        image::ImageFormat::Jpeg
    );
    let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
    assert!(decoded.get_pixel(0, 0).0.iter().all(|value| *value >= 250));
}
