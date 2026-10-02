//! Host-owned persistent plugin cache records.

use crate::core::app::error::{Result, TingError};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const MAX_PLUGIN_CACHE_KEY_BYTES: usize = 512;
const MAX_PLUGIN_CACHE_VALUE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PluginCache {
    root_dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct PluginCacheRecord {
    key: String,
    value: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct PluginCacheItem {
    pub key: String,
    pub value: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PluginCache {
    pub fn new(root_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root_dir).map_err(TingError::IoError)?;
        Ok(Self { root_dir })
    }

    pub async fn get(&self, plugin_id: &str, key: &str) -> Result<Option<PluginCacheItem>> {
        let path = self.cache_path(plugin_id, key)?;
        if !path.exists() {
            return Ok(None);
        }

        let bytes = tokio::fs::read(&path).await.map_err(TingError::IoError)?;
        let record: PluginCacheRecord = serde_json::from_slice(&bytes)
            .map_err(|e| TingError::DeserializationError(e.to_string()))?;
        Ok(Some(PluginCacheItem {
            key: record.key,
            value: record.value,
            created_at: record.created_at,
            updated_at: record.updated_at,
        }))
    }

    pub async fn set(&self, plugin_id: &str, key: &str, value: Value) -> Result<PluginCacheItem> {
        let path = self.cache_path(plugin_id, key)?;
        let now = Utc::now();
        let created_at = match self.get(plugin_id, key).await? {
            Some(existing) => existing.created_at,
            None => now,
        };

        let record = PluginCacheRecord {
            key: key.to_string(),
            value,
            created_at,
            updated_at: now,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|e| TingError::SerializationError(e.to_string()))?;
        if bytes.len() > MAX_PLUGIN_CACHE_VALUE_BYTES {
            return Err(TingError::ResourceLimitExceeded(format!(
                "Plugin cache value exceeds {} bytes",
                MAX_PLUGIN_CACHE_VALUE_BYTES
            )));
        }

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(TingError::IoError)?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(TingError::IoError)?;

        Ok(PluginCacheItem {
            key: record.key,
            value: record.value,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }

    pub async fn delete(&self, plugin_id: &str, key: &str) -> Result<bool> {
        let path = self.cache_path(plugin_id, key)?;
        if !path.exists() {
            return Ok(false);
        }

        tokio::fs::remove_file(path)
            .await
            .map_err(TingError::IoError)?;
        Ok(true)
    }

    pub async fn has(&self, plugin_id: &str, key: &str) -> Result<bool> {
        Ok(self.cache_path(plugin_id, key)?.exists())
    }

    pub async fn list(&self, plugin_id: &str, prefix: Option<&str>) -> Result<Vec<String>> {
        let directory = self.plugin_cache_dir(plugin_id);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut entries = tokio::fs::read_dir(directory)
            .await
            .map_err(TingError::IoError)?;
        let mut keys = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(TingError::IoError)? {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = tokio::fs::read(path).await.map_err(TingError::IoError)?;
            let record: PluginCacheRecord = serde_json::from_slice(&bytes)
                .map_err(|error| TingError::DeserializationError(error.to_string()))?;
            if prefix.is_none_or(|prefix| record.key.starts_with(prefix)) {
                keys.push(record.key);
            }
            if keys.len() >= 1000 {
                break;
            }
        }
        keys.sort();
        Ok(keys)
    }

    pub async fn delete_plugin(&self, plugin_id: &str) -> Result<bool> {
        let path = self.plugin_cache_dir(plugin_id);
        if !path.exists() {
            return Ok(false);
        }

        tokio::fs::remove_dir_all(path)
            .await
            .map_err(TingError::IoError)?;
        Ok(true)
    }

    pub async fn migrate_plugin(&self, old_plugin_id: &str, new_plugin_id: &str) -> Result<bool> {
        if old_plugin_id == new_plugin_id {
            return Ok(false);
        }

        let old_path = self.plugin_cache_dir(old_plugin_id);
        if !old_path.exists() {
            return Ok(false);
        }

        let new_path = self.plugin_cache_dir(new_plugin_id);
        if new_path.exists() {
            tokio::fs::remove_dir_all(&new_path)
                .await
                .map_err(TingError::IoError)?;
        }

        tokio::fs::rename(old_path, new_path)
            .await
            .map_err(TingError::IoError)?;
        Ok(true)
    }

    fn cache_path(&self, plugin_id: &str, key: &str) -> Result<PathBuf> {
        validate_plugin_cache_key(key)?;
        Ok(self
            .plugin_cache_dir(plugin_id)
            .join(format!("{}.json", hash_segment(key))))
    }

    fn plugin_cache_dir(&self, plugin_id: &str) -> PathBuf {
        self.root_dir.join(hash_segment(plugin_id))
    }
}

fn validate_plugin_cache_key(key: &str) -> Result<()> {
    let bytes = key.as_bytes();
    if bytes.is_empty() {
        return Err(TingError::InvalidRequest(
            "Plugin cache key cannot be empty".to_string(),
        ));
    }
    if bytes.len() > MAX_PLUGIN_CACHE_KEY_BYTES {
        return Err(TingError::InvalidRequest(format!(
            "Plugin cache key exceeds {} bytes",
            MAX_PLUGIN_CACHE_KEY_BYTES
        )));
    }
    if key.chars().any(char::is_control) {
        return Err(TingError::InvalidRequest(
            "Plugin cache key cannot contain control characters".to_string(),
        ));
    }
    Ok(())
}

fn hash_segment(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
#[path = "../../../tests/unit/plugin/host_api/cache.rs"]
mod tests;
