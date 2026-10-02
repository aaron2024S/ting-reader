use super::*;
use std::path::PathBuf;

#[test]
fn test_derive_master_key() {
    let db_path = PathBuf::from("./test.db");
    let key1 = MasterKeyManager::derive_master_key(&db_path).unwrap();
    let key2 = MasterKeyManager::derive_master_key(&db_path).unwrap();

    // 相同路径应该生成相同密钥
    assert_eq!(key1, key2);

    // 密钥不应该全为零
    assert!(MasterKeyManager::validate_master_key(&key1));
}

#[test]
fn test_different_paths_different_keys() {
    let key1 = MasterKeyManager::derive_master_key(&PathBuf::from("./test1.db")).unwrap();
    let key2 = MasterKeyManager::derive_master_key(&PathBuf::from("./test2.db")).unwrap();

    // 不同路径应该生成不同密钥
    assert_ne!(key1, key2);
}

#[test]
fn test_get_machine_id() {
    let id1 = MasterKeyManager::get_machine_id().unwrap();
    let id2 = MasterKeyManager::get_machine_id().unwrap();

    // 机器 ID 应该稳定
    assert_eq!(id1, id2);
    assert!(!id1.is_empty());
}
