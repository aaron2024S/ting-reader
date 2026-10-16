//! JWT token generation and validation

use crate::core::app::error::AuthenticationErrorKind;
use crate::core::app::error::{Result, TingError};
use jsonwebtoken::{
    DecodingKey, EncodingKey, Header, Validation, decode, encode, errors::ErrorKind,
};
use serde::{Deserialize, Serialize};

/// JWT Claims structure
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub user_id: String,
    pub exp: usize,
}

/// Generate a JWT token for a user
pub fn generate_token(user_id: &str, secret: &str) -> Result<String> {
    let expiration = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::days(7))
        .ok_or_else(|| {
            TingError::authentication(
                AuthenticationErrorKind::ExpiredCredentials,
                "generate_token::authentication",
                "无法计算令牌过期时间".to_string(),
            )
        })?
        .timestamp() as usize;

    let claims = Claims {
        user_id: user_id.to_string(),
        exp: expiration,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| {
        let message = format!("生成令牌失败: {}", e);
        TingError::authentication(
            AuthenticationErrorKind::TokenCreation,
            "generate_token::authentication",
            message,
        )
        .with_source(anyhow::anyhow!(e))
    })
}

/// Validate a JWT token and extract claims (single secret)
pub fn validate_token(token: &str, secret: &str) -> Result<Claims> {
    validate_token_with_secrets(token, &[secret.to_string()])
}

/// Validate a JWT token with multiple secrets (for key rotation)
pub fn validate_token_with_secrets(token: &str, secrets: &[String]) -> Result<Claims> {
    let mut last_error = None;

    // 尝试用每个密钥验证
    for secret in secrets {
        match decode::<Claims>(
            token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &Validation::default(),
        ) {
            Ok(token_data) => return Ok(token_data.claims),
            Err(e) => {
                // Only a signature mismatch can be fixed by trying another key.
                let signature_mismatch = matches!(e.kind(), ErrorKind::InvalidSignature);
                last_error = Some(e);
                if !signature_mismatch {
                    break;
                }
            }
        }
    }

    // 所有密钥都失败，返回错误
    if let Some(e) = last_error {
        let message = match e.kind() {
            ErrorKind::ExpiredSignature => "令牌已过期",
            ErrorKind::InvalidSignature => "令牌签名无效",
            ErrorKind::InvalidToken | ErrorKind::Base64(_) | ErrorKind::Json(_) => "令牌格式无效",
            _ => "令牌验证失败",
        };
        return Err(TingError::authentication(
            AuthenticationErrorKind::InvalidCredentials,
            "validate_token_with_secrets::authentication",
            message.to_string(),
        ));
    }

    Err(TingError::authentication(
        AuthenticationErrorKind::InvalidCredentials,
        "validate_token_with_secrets::authentication",
        "令牌验证失败".to_string(),
    ))
}

#[cfg(test)]
#[path = "../../tests/unit/auth/jwt.rs"]
mod tests;
