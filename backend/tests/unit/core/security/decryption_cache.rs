use super::*;
use tempfile::TempDir;

#[test]
fn test_cache_key_generation() {
    let service = DecryptionCacheService::new(DecryptionCacheConfig::default()).unwrap();

    let path1 = Path::new("/path/to/file1.xm");
    let path2 = Path::new("/path/to/file2.xm");

    let key1 = service.cache_key(path1);
    let key2 = service.cache_key(path2);

    assert_ne!(key1, key2);
    assert_eq!(key1, service.cache_key(path1)); // 一致性
}

#[test]
fn test_service_creation() {
    let temp_dir = TempDir::new().unwrap();
    let config = DecryptionCacheConfig {
        cache_dir: temp_dir.path().to_path_buf(),
        ..Default::default()
    };

    let service = DecryptionCacheService::new(config);
    assert!(service.is_ok());
    assert!(temp_dir.path().exists());
}

#[test]
fn test_temp_path_generation() {
    let temp_dir = TempDir::new().unwrap();
    let config = DecryptionCacheConfig {
        cache_dir: temp_dir.path().to_path_buf(),
        ..Default::default()
    };

    let service = DecryptionCacheService::new(config).unwrap();

    let path = Path::new("/test/file.xm");
    let temp_path = service.generate_temp_path(path, "xm").unwrap();

    assert!(temp_path.to_string_lossy().contains("decrypted_xm_"));
    assert!(temp_path.to_string_lossy().ends_with(".mp3"));
}

#[tokio::test]
async fn test_cache_stats() {
    let temp_dir = TempDir::new().unwrap();
    let config = DecryptionCacheConfig {
        cache_dir: temp_dir.path().to_path_buf(),
        ..Default::default()
    };

    let service = DecryptionCacheService::new(config).unwrap();
    let stats = service.stats().unwrap();

    assert_eq!(stats.total_entries, 0);
    assert_eq!(stats.total_size, 0);
}
