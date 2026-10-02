use super::*;

fn metadata_chapter(id: u32, title: &str, duration: f64) -> AudiobookshelfChapter {
    AudiobookshelfChapter {
        id,
        start: 0.0,
        end: duration,
        title: title.to_string(),
    }
}

fn db_chapter(id: &str, path: &str, title: &str, duration: i32) -> Chapter {
    Chapter {
        id: id.to_string(),
        book_id: "book".to_string(),
        title: Some(title.to_string()),
        path: path.to_string(),
        duration: Some(duration),
        chapter_index: Some(1),
        is_extra: 0,
        hash: None,
        manual_corrected: 0,
        created_at: String::new(),
    }
}

#[test]
fn aligns_interleaved_extra_chapters_to_file_order() {
    let chapters = vec![
        metadata_chapter(0, "山河稷-第0001章-凶案（一）", 465.0),
        metadata_chapter(1, "山河稷-第0471章-元慕鱼番外（1）", 285.0),
        metadata_chapter(2, "山河稷-第0002章-凶案（二）", 519.0),
    ];
    let file_stems = vec![
        "山河稷-第0001章-凶案（一）".to_string(),
        "山河稷-第0002章-凶案（二）".to_string(),
        "山河稷-第0471章-元慕鱼番外（1）".to_string(),
    ];

    let (aligned, matched) = align_chapters_to_file_stems(chapters, &file_stems);

    assert!(matched);
    assert_eq!(aligned[1].title, file_stems[1]);
    assert_eq!(aligned[1].end - aligned[1].start, 519.0);
    assert_eq!(aligned[2].end - aligned[2].start, 285.0);
}

#[test]
fn keeps_original_order_when_titles_do_not_all_match() {
    let chapters = vec![
        metadata_chapter(0, "Chapter 1", 10.0),
        metadata_chapter(1, "Chapter 2", 20.0),
    ];
    let file_stems = vec!["Chapter 1".to_string(), "02".to_string()];

    let (aligned, matched) = align_chapters_to_file_stems(chapters, &file_stems);

    assert!(!matched);
    assert_eq!(aligned[0].title, "Chapter 1");
    assert_eq!(aligned[1].title, "Chapter 2");
}

#[test]
fn writes_offsets_in_natural_file_order_not_extra_display_order() {
    let chapters = vec![
        db_chapter("extra", "/book/0471-extra.mp3", "extra 1", 285),
        db_chapter("main-2", "/book/0002.mp3", "main 2", 519),
        db_chapter("main-1", "/book/0001.mp3", "main 1", 465),
    ];

    let metadata = build_audiobookshelf_chapters(chapters);

    assert_eq!(metadata[0].title, "main 1");
    assert_eq!(metadata[1].title, "main 2");
    assert_eq!(metadata[1].start, 465.0);
    assert_eq!(metadata[1].end, 984.0);
    assert_eq!(metadata[2].title, "extra 1");
}
