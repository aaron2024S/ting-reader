use super::{collapse_local_scan_roots, local_group_is_changed, mark_changed_local_directory};
use crate::core::library_scanner::shared::CoalescedRangeDirs;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[test]
fn collapses_overlapping_scan_roots() {
    let roots = vec![
        PathBuf::from("library/book/chapters"),
        PathBuf::from("library/book"),
        PathBuf::from("library/other"),
        PathBuf::from("library/book"),
    ];

    assert_eq!(
        collapse_local_scan_roots(roots),
        vec![
            PathBuf::from("library/book"),
            PathBuf::from("library/other")
        ]
    );
}

#[test]
fn changed_directory_does_not_wake_siblings() {
    let mut changed_dirs = HashSet::new();
    let mut changed_ancestors = HashSet::new();
    mark_changed_local_directory(
        Some(Path::new("library/book-a")),
        &mut changed_dirs,
        &mut changed_ancestors,
    );

    assert!(local_group_is_changed(
        Path::new("library/book-a"),
        None,
        &changed_dirs,
        &changed_ancestors
    ));
    assert!(!local_group_is_changed(
        Path::new("library/book-b"),
        None,
        &changed_dirs,
        &changed_ancestors
    ));
}

#[test]
fn changed_range_child_wakes_only_its_coalesced_book() {
    let mut changed_dirs = HashSet::new();
    let mut changed_ancestors = HashSet::new();
    mark_changed_local_directory(
        Some(Path::new("library/book/001-100")),
        &mut changed_dirs,
        &mut changed_ancestors,
    );
    let range_dirs = CoalescedRangeDirs {
        child_dirs: vec![
            PathBuf::from("library/book/001-100"),
            PathBuf::from("library/book/101-200"),
        ],
        title_override: None,
    };

    assert!(local_group_is_changed(
        Path::new("library/book"),
        Some(&range_dirs),
        &changed_dirs,
        &changed_ancestors
    ));
    assert!(!local_group_is_changed(
        Path::new("library/other-book"),
        None,
        &changed_dirs,
        &changed_ancestors
    ));
}
