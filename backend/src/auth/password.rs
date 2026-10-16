//! Password hashing and verification using bcrypt

use crate::core::app::error::AuthenticationErrorKind;
use crate::core::app::error::{Result, TingError};

pub const DEFAULT_ADMIN_USERNAME: &str = "admin";
pub const DEFAULT_ADMIN_PASSWORD: &str = "admin123";

/// Hash a password using bcrypt
pub fn hash_password(password: &str) -> Result<String> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(|e| {
        let message = format!("Failed to hash password: {}", e);
        TingError::authentication(
            AuthenticationErrorKind::PasswordProcessing,
            "hash_password::authentication",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })
}

/// Verify a password against a hash
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    bcrypt::verify(password, hash).map_err(|e| {
        let message = format!("Failed to verify password: {}", e);
        TingError::authentication(
            AuthenticationErrorKind::PasswordProcessing,
            "verify_password::authentication",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })
}
