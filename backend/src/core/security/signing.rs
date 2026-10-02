use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub const DEFAULT_PLUGIN_ROUTE_SIGNATURE_TTL_SECONDS: u64 = 60 * 60;
pub const MAX_PLUGIN_ROUTE_SIGNATURE_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const DEFAULT_MEDIA_SIGNATURE_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
pub const MAX_MEDIA_SIGNATURE_TTL_SECONDS: u64 = 365 * 24 * 60 * 60;

#[derive(Clone)]
pub struct PluginRouteRevocations {
    path: PathBuf,
    entries: Arc<RwLock<HashMap<String, i64>>>,
}

impl PluginRouteRevocations {
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let entries = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path)?).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            entries: Arc::new(RwLock::new(entries)),
        })
    }

    pub fn revoke(&self, signature: &str, expires: i64) -> std::io::Result<()> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| std::io::Error::other("route revocation lock poisoned"))?;
        entries.insert(signature.to_string(), expires);
        if entries.len() > 10_000 {
            let oldest = entries
                .iter()
                .min_by_key(|(_, revoked_at)| **revoked_at)
                .map(|(signature, _)| signature.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        let bytes = serde_json::to_vec(&*entries)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(temporary, &self.path)?;
        Ok(())
    }

    pub fn is_revoked(&self, signature: &str) -> bool {
        self.entries
            .read()
            .ok()
            .is_some_and(|entries| entries.contains_key(signature))
    }
}

pub fn signature_expires_from_ttl(
    ttl_seconds: Option<u64>,
    default_ttl_seconds: u64,
    max_ttl_seconds: u64,
) -> i64 {
    match ttl_seconds {
        Some(0) => 0,
        Some(ttl) => chrono::Utc::now().timestamp() + ttl.clamp(1, max_ttl_seconds) as i64,
        None => chrono::Utc::now().timestamp() + default_ttl_seconds as i64,
    }
}

pub fn signature_has_expired(expires: i64) -> bool {
    expires > 0 && expires < chrono::Utc::now().timestamp()
}

pub fn normalize_plugin_route_sign_path(path: &str) -> String {
    let trimmed = path.trim().split('?').next().unwrap_or("").trim();
    let normalized = if trimmed.is_empty() {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };

    [
        "/api/v1/public/plugin-routes",
        "/api/public/plugin-routes",
        "/api/v1/plugin-routes",
        "/api/plugin-routes",
    ]
    .iter()
    .find_map(|prefix| normalized.strip_prefix(prefix))
    .map(|path| {
        if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        }
    })
    .unwrap_or(normalized)
}

pub fn sign_plugin_route_request(
    signing_key: &[u8; 32],
    method: &str,
    route_path: &str,
    expires: i64,
    user_id: Option<&str>,
) -> String {
    let payload = plugin_route_signature_payload(method, route_path, expires, user_id);
    hmac_sha256_base64_url(signing_key, payload.as_bytes())
}

pub fn plugin_route_signature_payload(
    method: &str,
    route_path: &str,
    expires: i64,
    user_id: Option<&str>,
) -> String {
    let base = format!("{}\n{}\n{}", method.to_uppercase(), route_path, expires);
    match user_id {
        Some(user_id) => format!("{}\nuser:{}", base, user_id),
        None => base,
    }
}

pub fn sign_media_stream_request(
    signing_key: &[u8; 32],
    chapter_id: &str,
    expires: i64,
    user_id: &str,
    transcode: Option<&str>,
    seek: Option<&str>,
    download: bool,
) -> String {
    let payload =
        media_stream_signature_payload(chapter_id, expires, user_id, transcode, seek, download);
    hmac_sha256_base64_url(signing_key, payload.as_bytes())
}

pub fn media_stream_signature_payload(
    chapter_id: &str,
    expires: i64,
    user_id: &str,
    transcode: Option<&str>,
    seek: Option<&str>,
    download: bool,
) -> String {
    format!(
        "media-stream\nchapter:{}\nexpires:{}\nuser:{}\ntranscode:{}\nseek:{}\ndownload:{}",
        chapter_id,
        expires,
        user_id,
        transcode.unwrap_or(""),
        seek.unwrap_or(""),
        if download { "1" } else { "0" }
    )
}

pub fn hmac_sha256_base64_url(key: &[u8], payload: &[u8]) -> String {
    let signature = hmac_sha256(key, payload);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature)
}

pub fn hmac_sha256(key: &[u8], payload: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;

    let mut key_block = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(payload);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    let signature = outer.finalize();

    let mut out = [0_u8; 32];
    out.copy_from_slice(&signature);
    out
}

pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    left.iter()
        .zip(right.iter())
        .fold(0_u8, |diff, (left, right)| diff | (*left ^ *right))
        == 0
}
