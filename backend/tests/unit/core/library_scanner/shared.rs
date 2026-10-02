use super::*;

fn select_mergeable_range_group(
    parent_name: &str,
    ranges: &[ChapterRangeDir],
) -> Option<Vec<usize>> {
    let groups = select_mergeable_range_groups(parent_name, ranges);
    if groups.len() == 1 {
        Some(groups[0].indices.clone())
    } else {
        None
    }
}

mod series {

    use super::{SeriesDirectoryCandidate, infer_series_directories};

    #[test]
    fn infers_series_from_sibling_volume_directories() {
        let candidates = vec![
            SeriesDirectoryCandidate {
                key: "root/book/season1".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "\u{4e00}\u{5ff5}\u{6c38}\u{6052}\u{7b2c}\u{4e00}\u{5b63}".to_string(),
            },
            SeriesDirectoryCandidate {
                key: "root/book/season2".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "\u{4e00}\u{5ff5}\u{6c38}\u{6052}\u{7b2c}\u{4e8c}\u{5b63}".to_string(),
            },
        ];

        let inferred = infer_series_directories(&candidates);
        assert_eq!(
            inferred["root/book/season1"].title,
            "\u{4e00}\u{5ff5}\u{6c38}\u{6052}"
        );
        assert_eq!(inferred["root/book/season1"].order, 1);
        assert_eq!(inferred["root/book/season2"].order, 2);
    }

    #[test]
    fn infers_series_from_zhi_separator() {
        let candidates = vec![
            SeriesDirectoryCandidate {
                key: "root/book/part1".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "\u{51e1}\u{4eba}\u{4fee}\u{4ed9}\u{4f20}\u{4e4b}\u{9b54}\u{9053}\u{4e89}\u{950b}".to_string(),
            },
            SeriesDirectoryCandidate {
                key: "root/book/part2".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "book".to_string(),
                name: "\u{51e1}\u{4eba}\u{4fee}\u{4ed9}\u{4f20}\u{4e4b}\u{521d}\u{5165}\u{661f}\u{6d77}".to_string(),
            },
        ];

        let inferred = infer_series_directories(&candidates);
        assert_eq!(
            inferred["root/book/part1"].title,
            "\u{51e1}\u{4eba}\u{4fee}\u{4ed9}\u{4f20}"
        );
        let mut orders = vec![
            inferred["root/book/part1"].order,
            inferred["root/book/part2"].order,
        ];
        orders.sort();
        assert_eq!(orders, vec![1, 2]);
    }

    #[test]
    fn infers_series_from_parent_when_only_season_code_is_present() {
        let candidates = vec![
            SeriesDirectoryCandidate {
                key: "root/book/s01".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "\u{4e09}\u{4f53}".to_string(),
                name: "S01".to_string(),
            },
            SeriesDirectoryCandidate {
                key: "root/book/s02".to_string(),
                parent_key: "root/book".to_string(),
                parent_name: "\u{4e09}\u{4f53}".to_string(),
                name: "S02".to_string(),
            },
        ];

        let inferred = infer_series_directories(&candidates);
        assert_eq!(inferred["root/book/s01"].title, "\u{4e09}\u{4f53}");
        assert_eq!(inferred["root/book/s01"].order, 1);
        assert_eq!(inferred["root/book/s02"].order, 2);
    }
}

mod titles_and_ranges {

    use super::{
        ChapterRangeDir, apply_chapter_title_template, chapter_title_template_preserves_raw,
        parse_chapter_range_dir_name, select_mergeable_range_group, select_mergeable_range_groups,
        strip_likely_file_extension,
    };

    #[test]
    fn applies_builtin_chapter_title_templates() {
        assert_eq!(
            apply_chapter_title_template(Some("章节名"), Some("书名"), 7, "正文"),
            "正文"
        );
        assert_eq!(
            apply_chapter_title_template(Some("书名-章节号-章节名"), Some("书名"), 7, "正文"),
            "书名-7-正文"
        );
        assert_eq!(
            apply_chapter_title_template(
                Some("{book_title} 第{chapter_number_padded}章 {chapter_title}"),
                Some("书名"),
                7,
                "正文"
            ),
            "书名 第0007章 正文"
        );
    }

    #[test]
    fn applies_raw_chapter_title_template_marker() {
        assert!(chapter_title_template_preserves_raw(Some(
            "raw:{chapter_title}"
        )));
        assert_eq!(
            apply_chapter_title_template(
                Some("raw:{chapter_number}-{chapter_title}"),
                None,
                7,
                "001.正文"
            ),
            "7-001.正文"
        );
    }

    #[test]
    fn strips_likely_file_extension_only() {
        assert_eq!(strip_likely_file_extension("001.mp3"), "001");
        assert_eq!(strip_likely_file_extension("Chapter.1"), "Chapter");
        assert_eq!(strip_likely_file_extension("第一章.开端"), "第一章.开端");
    }

    #[test]
    fn parses_common_chapter_range_directory_names() {
        let simple = parse_chapter_range_dir_name("1- 500").unwrap();
        assert_eq!(simple.start, 1);
        assert_eq!(simple.end, 500);
        assert_eq!(simple.context, "");

        let decorated = parse_chapter_range_dir_name("第001 - 050集").unwrap();
        assert_eq!(decorated.start, 1);
        assert_eq!(decorated.end, 50);
        assert_eq!(decorated.context, "");

        let tilde = parse_chapter_range_dir_name("[0501～1000]").unwrap();
        assert_eq!(tilde.start, 501);
        assert_eq!(tilde.end, 1000);

        let titled = parse_chapter_range_dir_name("【书名】001-050").unwrap();
        assert_eq!(titled.context, "书名");
        assert_eq!(titled.context_title.as_deref(), Some("书名"));

        let titled_no_space = parse_chapter_range_dir_name("[书名]101-150").unwrap();
        assert_eq!(titled_no_space.start, 101);
        assert_eq!(titled_no_space.end, 150);
        assert_eq!(titled_no_space.context, "书名");
        assert_eq!(titled_no_space.context_title.as_deref(), Some("书名"));

        let plain_no_space = parse_chapter_range_dir_name("书名001-050").unwrap();
        assert_eq!(plain_no_space.start, 1);
        assert_eq!(plain_no_space.end, 50);
        assert_eq!(plain_no_space.context, "书名");
        assert_eq!(plain_no_space.context_title.as_deref(), Some("书名"));
    }

    #[test]
    fn ignores_non_chapter_ranges() {
        assert!(parse_chapter_range_dir_name("2024-2025").is_none());
        assert!(parse_chapter_range_dir_name("500-1").is_none());
        assert!(parse_chapter_range_dir_name("1-50-100").is_none());
    }

    #[test]
    fn selects_only_safe_sibling_range_groups() {
        let ranges = vec![
            parse_chapter_range_dir_name("001-050").unwrap(),
            parse_chapter_range_dir_name("051-100").unwrap(),
        ];
        assert_eq!(
            select_mergeable_range_group("书名", &ranges),
            Some(vec![0, 1])
        );

        let prefixed = vec![
            parse_chapter_range_dir_name("书名 001-050").unwrap(),
            parse_chapter_range_dir_name("书名 051-100").unwrap(),
        ];
        assert_eq!(
            select_mergeable_range_group("书名", &prefixed),
            Some(vec![0, 1])
        );

        let different_books = vec![
            parse_chapter_range_dir_name("书A 001-050").unwrap(),
            parse_chapter_range_dir_name("书B 051-100").unwrap(),
        ];
        assert_eq!(select_mergeable_range_group("合集", &different_books), None);
    }

    #[test]
    fn selects_titled_range_groups_under_collection_parent() {
        let ranges = vec![
            parse_chapter_range_dir_name("【书名A】001-050").unwrap(),
            parse_chapter_range_dir_name("[书名A]051-100").unwrap(),
            parse_chapter_range_dir_name("书名B001-050").unwrap(),
            parse_chapter_range_dir_name("书名B 051-100").unwrap(),
        ];

        let groups = select_mergeable_range_groups("合集", &ranges);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].title.as_deref(), Some("书名A"));
        assert_eq!(groups[0].indices, vec![0, 1]);
        assert!(!groups[0].merge_into_parent);
        assert_eq!(groups[1].title.as_deref(), Some("书名B"));
        assert_eq!(groups[1].indices, vec![2, 3]);
        assert!(!groups[1].merge_into_parent);
    }

    #[test]
    fn rejects_overlapping_ranges() {
        let ranges = vec![
            ChapterRangeDir {
                start: 1,
                end: 100,
                context: String::new(),
                context_title: None,
            },
            ChapterRangeDir {
                start: 80,
                end: 150,
                context: String::new(),
                context_title: None,
            },
        ];

        assert_eq!(select_mergeable_range_group("书名", &ranges), None);
    }
}
