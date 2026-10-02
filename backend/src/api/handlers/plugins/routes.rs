//! Plugin HTTP route matching, authorization, signing and response validation.

use super::authorization::{
    PluginRouteAccess, is_admin_user, plugin_invocation_context, plugin_route_context_json,
};
use crate::api::models::{SignPluginRouteRequest, SignPluginRouteResponse};
use crate::api::require_admin;
use crate::api::state::AppState;
use crate::auth::middleware::AuthUser;
use crate::core::app::error::{Result, TingError};
use crate::core::security::signing::{
    DEFAULT_PLUGIN_ROUTE_SIGNATURE_TTL_SECONDS, MAX_PLUGIN_ROUTE_SIGNATURE_TTL_SECONDS,
    constant_time_eq, normalize_plugin_route_sign_path, sign_plugin_route_request,
    signature_expires_from_ttl, signature_has_expired,
};
use crate::db::repository::Repository;
use crate::plugin::types::PluginCapability;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde_json::Value;

/// Handler for /api/v1/plugin-routes/*path - Invoke a plugin-declared HTTP route.
pub async fn call_plugin_route(
    State(state): State<AppState>,
    user: AuthUser,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    call_plugin_route_inner(
        state,
        Some(user),
        method,
        uri,
        headers,
        body,
        PluginRouteAccess::Authenticated,
    )
    .await
}

/// Handler for /api/v1/public/plugin-routes/*path - Invoke public plugin HTTP routes.
pub async fn call_public_plugin_route(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    call_plugin_route_inner(
        state,
        None,
        method,
        uri,
        headers,
        body,
        PluginRouteAccess::Public,
    )
    .await
}

/// Handler for POST /api/v1/plugin-route-signatures - Generate a signed public plugin route URL.
pub async fn sign_plugin_route(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SignPluginRouteRequest>,
) -> Result<impl IntoResponse> {
    let method = req.method.trim().to_uppercase();
    if method.is_empty() {
        return Err(TingError::InvalidRequest(
            "Plugin route method is required".to_string(),
        ));
    }

    let path = normalize_plugin_route_sign_path(&req.path);
    let matched = state
        .plugin_manager
        .find_http_route(&method, &path)
        .await
        .ok_or_else(|| {
            TingError::NotFound(format!(
                "Plugin route not found: {} {}",
                method.as_str(),
                path
            ))
        })?;

    if matched.registration.admin_only {
        require_admin(&user)?;
    }

    if !plugin_route_allows_public_access(&matched.registration.capability) {
        return Err(TingError::PermissionDenied(format!(
            "Plugin route cannot be exposed through public plugin-routes: {} {}",
            method.as_str(),
            path
        )));
    }

    let expires = signature_expires_from_ttl(
        req.expires_in_seconds,
        DEFAULT_PLUGIN_ROUTE_SIGNATURE_TTL_SECONDS,
        MAX_PLUGIN_ROUTE_SIGNATURE_TTL_SECONDS,
    );
    let signed_user_id = req
        .bind_current_user
        .unwrap_or(true)
        .then(|| user.id.clone());
    if matched.registration.admin_only && signed_user_id.is_none() {
        return Err(TingError::PermissionDenied(
            "Admin-only plugin routes must bind the current admin user".to_string(),
        ));
    }
    let signature = sign_plugin_route_request(
        state.encryption_key.as_ref(),
        method.as_str(),
        path.as_str(),
        expires,
        signed_user_id.as_deref(),
    );
    let signed_url = if let Some(user_id) = signed_user_id.as_deref() {
        format!(
            "/api/v1/public/plugin-routes{}?expires={}&user={}&signature={}",
            path,
            expires,
            urlencoding::encode(user_id),
            signature
        )
    } else {
        format!(
            "/api/v1/public/plugin-routes{}?expires={}&signature={}",
            path, expires, signature
        )
    };

    Ok(Json(SignPluginRouteResponse {
        path,
        expires,
        signature,
        user_id: signed_user_id,
        signed_url,
    }))
}

async fn call_plugin_route_inner(
    state: AppState,
    user: Option<AuthUser>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    access: PluginRouteAccess,
) -> Result<Response> {
    let route_path = plugin_route_path_from_uri(&uri);
    let matched = state
        .plugin_manager
        .find_http_route(method.as_str(), &route_path)
        .await
        .ok_or_else(|| {
            TingError::NotFound(format!(
                "Plugin route not found: {} {}",
                method.as_str(),
                route_path
            ))
        })?;

    let mut route_user = user;
    let mut route_access = access;

    if access == PluginRouteAccess::Public {
        validate_public_plugin_route_access_with_revocations(
            &matched.registration.capability,
            &method,
            &route_path,
            &uri,
            state.encryption_key.as_ref(),
            Some(&state.plugin_route_revocations),
        )?;

        if let Some(user_id) = signed_plugin_route_user(&uri) {
            let signed_user = state.user_repo.find_by_id(&user_id).await?.ok_or_else(|| {
                TingError::PermissionDenied("Signed route user not found".to_string())
            })?;
            route_user = Some(AuthUser {
                user_id: signed_user.id.clone(),
                id: signed_user.id,
                username: signed_user.username,
                role: signed_user.role,
            });
            route_access = PluginRouteAccess::SignedUser;
        }
    }
    if matched.registration.admin_only {
        let is_route_admin = route_user.as_ref().map(is_admin_user).unwrap_or(false);
        if !is_route_admin {
            return Err(TingError::PermissionDenied(
                "Admin access required for this plugin route".to_string(),
            ));
        }
    }

    let params = serde_json::json!({
        "method": method.as_str(),
        "path": route_path,
        "query": uri.query().unwrap_or(""),
        "headers": headers_to_json(&headers),
        "params": matched.params,
        "body_text": std::str::from_utf8(body.as_ref()).ok(),
        "body_base64": base64::engine::general_purpose::STANDARD.encode(body.as_ref()),
        "capability_id": matched.registration.capability.id(),
        "plugin_id": matched.registration.plugin_id,
        "context": plugin_route_context_json(route_access, route_user.as_ref()),
    });

    let result = state
        .plugin_manager
        .invoke_capability(
            &matched.registration.plugin_id,
            matched.registration.capability.id(),
            "handle",
            params,
            &plugin_invocation_context(if matches!(route_access, PluginRouteAccess::Public) {
                None
            } else {
                route_user.as_ref()
            }),
        )
        .await?;

    plugin_route_result_to_response(result)
}

pub(super) fn plugin_route_path_from_uri(uri: &Uri) -> String {
    let path = uri.path();
    let route_path = [
        "/api/v1/public/plugin-routes",
        "/api/public/plugin-routes",
        "/api/v1/plugin-routes",
        "/api/plugin-routes",
    ]
    .iter()
    .find_map(|prefix| path.strip_prefix(prefix))
    .unwrap_or(path);

    if route_path.is_empty() {
        "/".to_string()
    } else {
        route_path.to_string()
    }
}

fn headers_to_json(headers: &HeaderMap) -> Value {
    let mut object = serde_json::Map::new();
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            object.insert(name.as_str().to_string(), Value::String(value.to_string()));
        }
    }
    Value::Object(object)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginRouteAuthPolicy {
    User,
    Public,
    Signed,
    PublicOrSigned,
}

impl PluginRouteAuthPolicy {
    fn can_use_public_prefix(self) -> bool {
        matches!(self, Self::Public | Self::Signed | Self::PublicOrSigned)
    }
}

fn plugin_route_auth_policy(capability: &PluginCapability) -> PluginRouteAuthPolicy {
    use ting_plugin_contract::capability::RouteAuth;
    match capability {
        PluginCapability::HttpRoute(cap) => match cap.route.auth {
            RouteAuth::Public => PluginRouteAuthPolicy::Public,
            RouteAuth::Signed => PluginRouteAuthPolicy::Signed,
            RouteAuth::PublicOrSigned => PluginRouteAuthPolicy::PublicOrSigned,
            RouteAuth::User => PluginRouteAuthPolicy::User,
        },
        _ => PluginRouteAuthPolicy::User,
    }
}

pub(super) fn plugin_route_allows_public_access(capability: &PluginCapability) -> bool {
    plugin_route_auth_policy(capability).can_use_public_prefix()
}

pub(super) fn validate_public_plugin_route_access_with_revocations(
    capability: &PluginCapability,
    method: &Method,
    route_path: &str,
    uri: &Uri,
    signing_key: &[u8; 32],
    revocations: Option<&crate::core::security::signing::PluginRouteRevocations>,
) -> Result<()> {
    match plugin_route_auth_policy(capability) {
        PluginRouteAuthPolicy::Public => {
            if plugin_route_has_signature(uri) {
                validate_plugin_route_signature_with_revocations(
                    method.as_str(),
                    route_path,
                    uri,
                    signing_key,
                    revocations,
                )
            } else {
                Ok(())
            }
        }
        PluginRouteAuthPolicy::PublicOrSigned => {
            if plugin_route_has_signature(uri) {
                validate_plugin_route_signature_with_revocations(
                    method.as_str(),
                    route_path,
                    uri,
                    signing_key,
                    revocations,
                )
            } else {
                Ok(())
            }
        }
        PluginRouteAuthPolicy::Signed => validate_plugin_route_signature_with_revocations(
            method.as_str(),
            route_path,
            uri,
            signing_key,
            revocations,
        ),
        PluginRouteAuthPolicy::User => Err(TingError::PermissionDenied(format!(
            "Plugin route is not public: {} {}",
            method.as_str(),
            route_path
        ))),
    }
}

fn plugin_route_has_signature(uri: &Uri) -> bool {
    query_param(uri, "expires").is_some() || query_param(uri, "signature").is_some()
}

pub(super) fn signed_plugin_route_user(uri: &Uri) -> Option<String> {
    if plugin_route_has_signature(uri) {
        query_param(uri, "user")
    } else {
        None
    }
}

fn validate_plugin_route_signature_with_revocations(
    method: &str,
    route_path: &str,
    uri: &Uri,
    signing_key: &[u8; 32],
    revocations: Option<&crate::core::security::signing::PluginRouteRevocations>,
) -> Result<()> {
    let expires = query_param(uri, "expires")
        .ok_or_else(|| {
            TingError::PermissionDenied("Missing plugin route signature expiry".to_string())
        })?
        .parse::<i64>()
        .map_err(|_| {
            TingError::PermissionDenied("Invalid plugin route signature expiry".to_string())
        })?;

    if signature_has_expired(expires) {
        return Err(TingError::PermissionDenied(
            "Plugin route signature has expired".to_string(),
        ));
    }

    let signature = query_param(uri, "signature")
        .ok_or_else(|| TingError::PermissionDenied("Missing plugin route signature".to_string()))?;
    if revocations.is_some_and(|revocations| revocations.is_revoked(&signature)) {
        return Err(TingError::PermissionDenied(
            "Plugin route signature has been revoked".to_string(),
        ));
    }
    let signed_user_id = query_param(uri, "user");
    let expected = sign_plugin_route_request(
        signing_key,
        method,
        route_path,
        expires,
        signed_user_id.as_deref(),
    );

    if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
        return Err(TingError::PermissionDenied(
            "Invalid plugin route signature".to_string(),
        ));
    }

    Ok(())
}

fn query_param(uri: &Uri, name: &str) -> Option<String> {
    uri.query().and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
    })
}

pub(super) fn plugin_route_result_to_response(value: Value) -> Result<Response> {
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .unwrap_or(StatusCode::OK.as_u16() as u64);
    let status = u16::try_from(status)
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .ok_or_else(|| {
            TingError::PluginExecutionError("Plugin returned invalid HTTP status".to_string())
        })?;

    let mut builder = Response::builder().status(status);
    if let Some(headers) = value.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            let Some(value) = value.as_str() else {
                continue;
            };
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                TingError::PluginExecutionError(format!(
                    "Plugin returned invalid HTTP header name '{}': {}",
                    name, e
                ))
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|e| {
                TingError::PluginExecutionError(format!(
                    "Plugin returned invalid HTTP header value for '{}': {}",
                    name, e
                ))
            })?;
            builder = builder.header(header_name, header_value);
        }
    }

    let body = plugin_route_response_body(&value)?;
    builder.body(Body::from(body)).map_err(|e| {
        TingError::PluginExecutionError(format!("Plugin HTTP response build failed: {}", e))
    })
}

pub(super) fn plugin_route_response_body(value: &Value) -> Result<Vec<u8>> {
    if let Some(body_base64) = value.get("body_base64").and_then(Value::as_str) {
        return base64::engine::general_purpose::STANDARD
            .decode(body_base64)
            .map_err(|e| {
                TingError::PluginExecutionError(format!(
                    "Plugin returned invalid base64 response body: {}",
                    e
                ))
            });
    }

    if let Some(body) = value.get("body") {
        if let Some(body) = body.as_str() {
            return Ok(body.as_bytes().to_vec());
        }

        return serde_json::to_vec(body).map_err(|e| {
            TingError::SerializationError(format!(
                "Plugin response body serialization failed: {}",
                e
            ))
        });
    }

    if value.is_object() {
        Ok(Vec::new())
    } else {
        serde_json::to_vec(value).map_err(|e| {
            TingError::SerializationError(format!("Plugin response serialization failed: {}", e))
        })
    }
}
