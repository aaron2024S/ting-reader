//! Plugin visibility, UI grants and host-owned invocation identity.

use crate::api::require_admin;
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::plugin::PluginHostUser;
use crate::plugin::manager::capabilities::RegisteredCapability;
use crate::plugin::types::{PluginInvocationContext, PluginMetadata};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) fn is_admin_user(user: &AuthUser) -> bool {
    user.role == "admin"
}

pub(super) fn plugin_visible_to_user(admin_only: bool, is_admin: bool) -> bool {
    !admin_only || is_admin
}

pub(super) fn require_plugin_visible(metadata: &PluginMetadata, user: &AuthUser) -> Result<()> {
    if metadata.admin_only {
        require_admin(user)?;
    }
    Ok(())
}

pub(super) fn require_plugin_system_write(user: &AuthUser) -> Result<()> {
    require_admin(user)
}

pub(super) fn registration_visible_to_user(
    registration: &RegisteredCapability,
    is_admin: bool,
) -> bool {
    plugin_visible_to_user(registration.admin_only, is_admin)
}

const PLUGIN_CLIENT_GRANT_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PluginClientGrantClaims {
    pub(super) sub: String,
    pub(super) plugin_id: String,
    pub(super) capability_id: String,
    pub(super) exp: usize,
    pub(super) grant_type: String,
}

async fn plugin_client_grant_secret(state: &AppState) -> String {
    if let Some(key_manager) = &state.jwt_key_manager {
        key_manager.get_signing_secret().await
    } else {
        state.jwt_secret.as_ref().clone()
    }
}

pub(super) async fn issue_plugin_client_grant(
    state: &AppState,
    user: &AuthUser,
    plugin_id: &str,
    capability_id: &str,
) -> Result<String> {
    let expiration = Utc::now()
        .checked_add_signed(chrono::Duration::seconds(PLUGIN_CLIENT_GRANT_TTL_SECONDS))
        .ok_or_else(|| TingError::AuthenticationError("Failed to calculate grant expiry".into()))?
        .timestamp() as usize;
    let claims = PluginClientGrantClaims {
        sub: user.id.clone(),
        plugin_id: plugin_id.to_string(),
        capability_id: capability_id.to_string(),
        exp: expiration,
        grant_type: "plugin_client".to_string(),
    };
    let secret = plugin_client_grant_secret(state).await;
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|error| {
        TingError::AuthenticationError(format!("Failed to issue plugin client grant: {error}"))
    })
}

pub(super) async fn require_plugin_client_grant(
    state: &AppState,
    user: &AuthUser,
    grant: &str,
    plugin_id: &str,
    capability_id: Option<&str>,
) -> Result<PluginClientGrantClaims> {
    let claims = decode_plugin_client_grant(state, grant).await?;
    if claims.sub != user.id
        || claims.plugin_id != plugin_id
        || capability_id.is_some_and(|expected| claims.capability_id != expected)
    {
        return Err(TingError::PermissionDenied(
            "Plugin client grant does not match this request".to_string(),
        ));
    }
    Ok(claims)
}

pub(super) async fn decode_plugin_client_grant(
    state: &AppState,
    grant: &str,
) -> Result<PluginClientGrantClaims> {
    if grant.len() > 4096 {
        return Err(TingError::PermissionDenied(
            "Invalid plugin client grant".to_string(),
        ));
    }
    let secrets = if let Some(key_manager) = &state.jwt_key_manager {
        key_manager.get_validation_secrets().await
    } else {
        vec![state.jwt_secret.as_ref().clone()]
    };
    decode_plugin_client_grant_with_secrets(grant, &secrets)
}

pub(super) fn decode_plugin_client_grant_with_secrets(
    grant: &str,
    secrets: &[String],
) -> Result<PluginClientGrantClaims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;
    let claims = secrets
        .iter()
        .find_map(|secret| {
            decode::<PluginClientGrantClaims>(
                grant,
                &DecodingKey::from_secret(secret.as_bytes()),
                &validation,
            )
            .ok()
            .map(|token| token.claims)
        })
        .ok_or_else(|| TingError::PermissionDenied("Invalid plugin client grant".into()))?;
    if claims.grant_type != "plugin_client" {
        return Err(TingError::PermissionDenied(
            "Invalid plugin client grant".to_string(),
        ));
    }
    Ok(claims)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PluginRouteAccess {
    Authenticated,
    SignedUser,
    Public,
}

pub(super) fn plugin_route_context_json(
    access: PluginRouteAccess,
    user: Option<&AuthUser>,
) -> Value {
    let authenticated = matches!(
        access,
        PluginRouteAccess::Authenticated | PluginRouteAccess::SignedUser
    ) && user.is_some();
    let user_value = if authenticated {
        user.map(|user| {
            serde_json::json!({
                "id": user.id,
                "username": user.username,
                "role": user.role,
            })
        })
        .unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    serde_json::json!({
        "access": match access {
            PluginRouteAccess::Authenticated => "authenticated",
            PluginRouteAccess::SignedUser => "signed",
            PluginRouteAccess::Public => "public",
        },
        "authenticated": authenticated,
        "user": user_value,
    })
}

pub(super) fn attach_plugin_invocation_context(
    params: Value,
    plugin_id: &str,
    capability_id: &str,
    access: PluginRouteAccess,
    user: Option<&AuthUser>,
) -> Value {
    let context = serde_json::json!({
        "plugin_id": plugin_id,
        "capability_id": capability_id,
        "route": plugin_route_context_json(access, user),
    });

    match params {
        Value::Object(mut object) => {
            object.insert("_context".to_string(), context);
            Value::Object(object)
        }
        value => serde_json::json!({
            "input": value,
            "_context": context,
        }),
    }
}

pub(super) fn plugin_invocation_context(user: Option<&AuthUser>) -> PluginInvocationContext {
    PluginInvocationContext {
        user: user.map(|user| PluginHostUser {
            id: user.id.clone(),
            username: user.username.clone(),
            role: user.role.clone(),
        }),
        resources: None,
    }
}
