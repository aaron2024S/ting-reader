use super::*;

#[test]
fn test_lru_cache_basic() {
    let mut cache = LruCache::<String, String>::new(100);
    cache.insert("a".into(), "value_a".into(), 40);
    cache.insert("b".into(), "value_b".into(), 30);
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.total_size(), 70);

    let val = cache.get(&"a".to_string());
    assert_eq!(val, Some(&"value_a".to_string()));
}

#[test]
fn test_lru_cache_eviction() {
    let mut cache = LruCache::<String, String>::new(100);
    cache.insert("a".into(), "value_a".into(), 50);
    cache.insert("b".into(), "value_b".into(), 30);

    // Access "a" to make "b" the oldest
    cache.get(&"a".to_string());

    // Insert 40 more: 50 + 30 + 40 = 120 > 100, evicts oldest ("b")
    cache.insert("c".into(), "value_c".into(), 40);
    assert!(!cache.contains_key(&"b".to_string()));
    assert!(cache.contains_key(&"a".to_string()));
    assert!(cache.contains_key(&"c".to_string()));
}

#[test]
fn test_lru_cache_remove() {
    let mut cache = LruCache::<String, String>::new(100);
    cache.insert("a".into(), "value_a".into(), 40);
    assert_eq!(cache.total_size(), 40);
    let removed = cache.remove(&"a".to_string());
    assert_eq!(removed, Some("value_a".to_string()));
    assert_eq!(cache.total_size(), 0);
    assert!(cache.is_empty());
}
