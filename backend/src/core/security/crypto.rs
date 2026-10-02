//! Cryptographic utilities for encrypting and decrypting sensitive data
//!
//! This module provides encryption and decryption functions for sensitive data
//! such as WebDAV credentials. It uses AES-256-GCM for authenticated encryption.

use crate::core::app::error::{Result, TingError};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, OsRng},
};
use base64::{Engine as _, engine::general_purpose};

/// Encrypt a string value using AES-256-GCM
///
/// # Arguments
/// * `value` - The plaintext string to encrypt
/// * `key` - 32-byte encryption key
///
/// # Returns
/// Base64-encoded string containing nonce + ciphertext
///
/// # Example
/// ```ignore
/// let key = [0u8; 32];
/// let encrypted = encrypt("my_password", &key)?;
/// ```
pub fn encrypt(value: &str, key: &[u8; 32]) -> Result<String> {
    let cipher = Aes256Gcm::new(key.into());

    // Generate random nonce
    let mut nonce_bytes = [0u8; 12];
    use aes_gcm::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Encrypt
    let ciphertext = cipher
        .encrypt(nonce, value.as_bytes())
        .map_err(|e| TingError::ConfigError(format!("Encryption failed: {}", e)))?;

    // Combine nonce + ciphertext and encode as base64
    let mut combined = nonce_bytes.to_vec();
    combined.extend_from_slice(&ciphertext);

    Ok(general_purpose::STANDARD.encode(&combined))
}

/// Decrypt a string value using AES-256-GCM
///
/// # Arguments
/// * `encrypted` - Base64-encoded string containing nonce + ciphertext
/// * `key` - 32-byte encryption key (must be the same key used for encryption)
///
/// # Returns
/// Decrypted plaintext string
///
/// # Example
/// ```ignore
/// let key = [0u8; 32];
/// let decrypted = decrypt(&encrypted_value, &key)?;
/// ```
pub fn decrypt(encrypted: &str, key: &[u8; 32]) -> Result<String> {
    let cipher = Aes256Gcm::new(key.into());

    // Decode from base64
    let combined = general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|e| TingError::ConfigError(format!("Invalid encrypted data: {}", e)))?;

    if combined.len() < 12 {
        return Err(TingError::ConfigError(
            "Invalid encrypted data length".to_string(),
        ));
    }

    // Split nonce and ciphertext
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    // Decrypt
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| TingError::ConfigError(format!("Decryption failed: {}", e)))?;

    String::from_utf8(plaintext)
        .map_err(|e| TingError::ConfigError(format!("Invalid UTF-8 in decrypted data: {}", e)))
}
