//! Authentication middleware

use crate::auth::jwt::{Claims, validate_token_with_secrets};
use crate::core::app::error::{Result, TingError};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

fn bearer_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

fn cookie_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                if name == "ting_reader_token" || name == "auth_token" {
                    Some(value.trim().to_string())
                } else {
                    None
                }
            })
        })
        .filter(|token| !token.is_empty())
}

fn query_token(request: &Request) -> Option<String> {
    request.uri().query().and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "token")
            .map(|(_, value)| value.trim().to_string())
            .filter(|token| !token.is_empty())
    })
}

fn token_candidates(request: &Request) -> Vec<String> {
    let mut candidates = Vec::new();

    for token in [
        bearer_token_from_headers(request.headers()),
        query_token(request),
        cookie_token_from_headers(request.headers()),
    ]
    .into_iter()
    .flatten()
    {
        if !candidates.contains(&token) {
            candidates.push(token);
        }
    }

    candidates
}

fn validate_token_candidates(candidates: &[String], secrets: &[String]) -> Result<Claims> {
    let mut last_error = None;

    for token in candidates {
        match validate_token_with_secrets(token, secrets) {
            Ok(claims) => return Ok(claims),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| TingError::AuthenticationError("缺少认证令牌".to_string())))
}

/// Extension to store authenticated user info in request
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: String,
    pub id: String, // Alias for user_id for convenience
    pub username: String,
    pub role: String,
}

/// Authentication middleware
pub async fn authenticate(
    State(state): State<crate::api::state::AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let candidates = token_candidates(&request);
    let validation_secrets = if let Some(key_manager) = &state.jwt_key_manager {
        key_manager.get_validation_secrets().await
    } else {
        vec![state.jwt_secret.as_ref().clone()]
    };
    let claims = match validate_token_candidates(&candidates, &validation_secrets) {
        Ok(claims) => claims,
        Err(error) => return error.into_response(),
    };

    // Fetch user from database
    use crate::db::repository::Repository;
    let user_id = claims.user_id;

    // Check if user exists
    let user_result = state.user_repo.find_by_id(&user_id).await;

    let user = match user_result {
        Ok(Some(u)) => u,
        Ok(None) => {
            let error = TingError::AuthenticationError("用户不存在".to_string());
            return error.into_response();
        }
        Err(e) => return e.into_response(), // Database error
    };

    // Store authenticated user in request extensions
    request.extensions_mut().insert(AuthUser {
        user_id: user.id.clone(),
        id: user.id,
        username: user.username,
        role: user.role,
    });

    next.run(request).await
}

/// Extract authenticated user from request extensions
pub fn get_auth_user(request: &Request) -> Result<AuthUser> {
    request
        .extensions()
        .get::<AuthUser>()
        .cloned()
        .ok_or_else(|| TingError::AuthenticationError("用户未认证".to_string()))
}

// Implement FromRequestParts for AuthUser to enable extraction in handlers
use axum::{async_trait, extract::FromRequestParts, http::request::Parts};

#[async_trait]
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = TingError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or_else(|| TingError::AuthenticationError("用户未认证".to_string()))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/auth/middleware.rs"]
mod tests;
