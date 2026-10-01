use super::*;

fn scope(dir: &Path) -> ResourceScope {
    ResourceScope::new(
        "format@2.0.0".into(),
        Uuid::new_v4(),
        Some("alice".into()),
        dir.into(),
        ResourceLimits::default(),
    )
}

#[test]
fn owner_includes_user_and_generation_and_ids_are_not_authority() {
    let dir = tempfile::tempdir().unwrap();
    let first = scope(dir.path());
    let second = scope(dir.path());
    let input = first.grant_bytes(Arc::from(&b"secret"[..]), None).unwrap();
    assert_eq!(
        first
            .authorize("format@2.0.0", first.generation, Some("bob"))
            .unwrap_err()
            .code,
        PluginErrorCode::PermissionDenied
    );
    assert!(
        first
            .authorize("format@2.0.0", second.generation, Some("alice"))
            .is_err()
    );
    assert!(second.read_at(&input, 0, 6).is_err());
    first
        .authorize("format@2.0.0", first.generation, Some("alice"))
        .unwrap();
    assert_eq!(
        first.read_at(&input, 0, 6).unwrap(),
        (b"secret".to_vec(), true)
    );
}

#[test]
fn prefix_grant_and_chunk_backpressure_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let scope = scope(dir.path());
    let file = dir.path().join("input");
    std::fs::write(&file, b"0123456789").unwrap();
    let input = scope.grant_file(&file, 4, None).unwrap();
    assert_eq!(
        scope.read_at(&input, 0, 10).unwrap(),
        (b"0123".to_vec(), false)
    );
    assert!(scope.read_at(&input, 4, 1).is_err());
    let mut chunks = Vec::new();
    for _ in 0..4 {
        chunks.push(scope.create_chunk(b"chunk").unwrap());
    }
    assert_eq!(
        scope.create_chunk(b"overflow").unwrap_err().code,
        PluginErrorCode::ResourceLimit
    );
    scope.release_chunk(&chunks[0]).unwrap();
    assert!(scope.chunk(&chunks[0]).is_err());
    scope.create_chunk(b"allowed").unwrap();
    assert_eq!(scope.managed_bytes(), 22);
    scope.cancel();
    assert_eq!(scope.managed_bytes(), 0);
    assert_eq!(
        scope.read_at(&input, 0, 1).unwrap_err().code,
        PluginErrorCode::Cancelled
    );
    scope.close(&input).unwrap();
    scope.close(&input).unwrap();
}

#[test]
fn memory_sources_observe_scope_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let scope = scope(dir.path());
    let input = scope.grant_bytes(Arc::from(&b"payload"[..]), None).unwrap();
    scope.cancel();
    assert_eq!(
        scope.read_at(&input, 0, 1).unwrap_err().code,
        PluginErrorCode::Cancelled
    );
}

#[test]
fn staging_output_is_private_bounded_and_deleted_on_scope_drop() {
    let dir = tempfile::tempdir().unwrap();
    let scope = scope(dir.path());
    let output = scope.create_output(None).unwrap();
    let path = dir.path().join(format!("{}.staging", output.0));
    assert_eq!(scope.write_at(&output, 0, b"replacement").unwrap(), 11);
    assert!(scope.read_at(&output, 0, 1).is_err());
    scope.finish(&output).unwrap();
    assert!(scope.write_at(&output, 0, b"x").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    drop(scope);
    assert!(!path.exists());
}

#[test]
fn asset_commit_is_bounded_atomic_and_requires_finished_output() {
    let dir = tempfile::tempdir().unwrap();
    let scope = scope(&dir.path().join("staging"));
    let destination = dir.path().join("cover.png");
    std::fs::write(&destination, b"old").unwrap();
    let output = scope.create_output(Some("image/png".into())).unwrap();
    scope.write_at(&output, 0, b"new-image").unwrap();
    assert!(
        scope
            .commit_asset_output(&output, &destination, 20)
            .is_err()
    );
    scope.finish(&output).unwrap();
    assert_eq!(
        scope
            .commit_asset_output(&output, &destination, 2)
            .unwrap_err()
            .code,
        PluginErrorCode::ResourceLimit,
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"old");
    assert_eq!(
        scope
            .commit_asset_output(&output, &destination, 20)
            .unwrap(),
        9
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"new-image");
    scope.close(&output).unwrap();
}
