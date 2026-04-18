use anyhow::Result;
use axum::{
    extract::State,
    http::{
        header::{AUTHORIZATION, COOKIE, HOST, ORIGIN, REFERER, SET_COOKIE, WWW_AUTHENTICATE},
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
    },
    middleware::Next,
    response::IntoResponse,
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde_json::json;
use std::io::Read;
use tracing::warn;

use crate::config::Config;

use super::{WebState, CONTENT_SECURITY_POLICY_VALUE};

const BROWSER_SESSION_COOKIE: &str = "symlinkarr_browser_session";

pub(super) fn ensure_remote_bind_allowed(config: &Config) -> Result<()> {
    if config.web.requires_remote_ack() && !config.web.allow_remote {
        anyhow::bail!(
            "Refusing to start web UI on {} without web.allow_remote=true",
            config.web.normalized_bind_address()
        );
    }
    if config.web.requires_remote_ack() && config.web.allow_remote && !config.web.has_basic_auth() {
        anyhow::bail!(
            "Refusing to start web UI on {} with web.allow_remote=true unless web.username/web.password are configured",
            config.web.normalized_bind_address()
        );
    }
    Ok(())
}

fn method_requires_same_origin(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn method_receives_browser_session(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD)
}

fn header_value_str(value: &HeaderValue) -> Option<&str> {
    value
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn header_authority(value: &HeaderValue) -> Option<String> {
    header_value_str(value).map(|value| value.to_ascii_lowercase())
}

fn uri_authority(value: &HeaderValue) -> Option<String> {
    let uri: Uri = header_value_str(value)?.parse().ok()?;
    uri.authority()
        .map(|authority| authority.as_str().to_ascii_lowercase())
}

fn request_has_browser_metadata(headers: &HeaderMap) -> bool {
    headers.contains_key(ORIGIN) || headers.contains_key(REFERER)
}

fn request_cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(COOKIE).and_then(header_value_str)?;
    raw.split(';').find_map(|entry| {
        let (cookie_name, cookie_value) = entry.trim().split_once('=')?;
        if cookie_name.trim() == name {
            Some(cookie_value.trim().to_string())
        } else {
            None
        }
    })
}

fn browser_session_cookie_header(token: &str) -> String {
    format!("{BROWSER_SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict")
}

fn is_same_origin_browser_mutation(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(HOST).and_then(header_authority) else {
        return false;
    };

    if let Some(origin) = headers.get(ORIGIN).and_then(uri_authority) {
        return origin == host;
    }

    if let Some(referer) = headers.get(REFERER).and_then(uri_authority) {
        return referer == host;
    }

    false
}

fn has_valid_browser_session(headers: &HeaderMap, state: &WebState) -> bool {
    request_cookie_value(headers, BROWSER_SESSION_COOKIE)
        .as_deref()
        .map(|token| constant_time_str_eq(token, state.browser_session_token()))
        .unwrap_or(false)
}

pub(super) fn has_valid_browser_csrf_token(token: &str, state: &WebState) -> bool {
    constant_time_str_eq(token.trim(), state.browser_session_token())
}

pub(super) fn constant_time_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();

    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());

    for idx in 0..max_len {
        let left_byte = left.get(idx).copied().unwrap_or_default();
        let right_byte = right.get(idx).copied().unwrap_or_default();
        diff |= usize::from(left_byte ^ right_byte);
    }

    diff == 0
}

fn request_basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let authorization = headers.get(AUTHORIZATION).and_then(header_value_str)?;
    let encoded = authorization
        .strip_prefix("Basic ")
        .or_else(|| authorization.strip_prefix("basic "))?;
    let decoded = BASE64_STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

fn request_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(api_key) = headers.get("x-api-key").and_then(header_value_str) {
        return Some(api_key.to_string());
    }

    let authorization = headers.get(AUTHORIZATION).and_then(header_value_str)?;
    authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
        .map(|value| value.to_string())
}

fn has_valid_basic_auth(headers: &HeaderMap, state: &WebState) -> bool {
    if !state.config.web.has_basic_auth() {
        return false;
    }

    let Some((username, password)) = request_basic_credentials(headers) else {
        return false;
    };

    constant_time_str_eq(&username, state.config.web.username.trim())
        && constant_time_str_eq(&password, &state.config.web.password)
}

fn has_valid_api_key(headers: &HeaderMap, state: &WebState) -> bool {
    if !state.config.web.has_api_key_auth() {
        return false;
    }

    let Some(api_key) = request_api_key(headers) else {
        return false;
    };

    constant_time_str_eq(&api_key, &state.config.web.api_key)
}

fn unauthorized_auth_response(path: &str, offer_basic: bool) -> axum::response::Response {
    let mut response = if path.starts_with("/api/") {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "authentication required"
            })),
        )
            .into_response()
    } else {
        (StatusCode::UNAUTHORIZED, "Authentication required.").into_response()
    };

    if offer_basic {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Symlinkarr\""),
        );
    }

    response
}

fn forbidden_origin_response(path: &str) -> axum::response::Response {
    if path.starts_with("/api/") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "cross-origin mutation blocked; use the same origin as the web UI or a non-browser client without Origin/Referer headers"
            })),
        )
            .into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Cross-origin mutation blocked; submit the form from the same Symlinkarr origin.",
    )
        .into_response()
}

fn missing_browser_session_response(path: &str) -> axum::response::Response {
    if path.starts_with("/api/") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "browser mutation blocked; refresh the Symlinkarr UI from the same origin and retry with the issued browser session"
            })),
        )
            .into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Browser mutation blocked; refresh the Symlinkarr UI from the same origin and retry.",
    )
        .into_response()
}

pub(super) fn invalid_browser_csrf_response(path: &str) -> axum::response::Response {
    if path.starts_with("/api/") {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "browser mutation blocked; reload the Symlinkarr UI and retry with the issued CSRF token"
            })),
        )
            .into_response();
    }

    (
        StatusCode::FORBIDDEN,
        "Browser mutation blocked; reload the Symlinkarr UI and retry with the issued CSRF token.",
    )
        .into_response()
}

pub(super) async fn add_security_headers(
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(CONTENT_SECURITY_POLICY_VALUE),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("same-origin"));
    response.headers_mut().insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    response
}

pub(super) fn generate_browser_session_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_ok() {
        return Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }

    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        return Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect());
    }

    anyhow::bail!("OS entropy unavailable for browser session token generation")
}

pub(super) async fn guard_web_auth(
    State(state): State<WebState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let path = request.uri().path().to_string();
    let is_api = path.starts_with("/api/");
    let require_basic = state.config.web.has_basic_auth();
    let require_api_auth =
        is_api && (state.config.web.has_basic_auth() || state.config.web.has_api_key_auth());

    if !require_basic && !require_api_auth {
        return next.run(request).await;
    }

    let basic_ok = has_valid_basic_auth(request.headers(), &state);
    let api_key_ok = is_api && has_valid_api_key(request.headers(), &state);

    let authorized = if is_api {
        (!require_api_auth) || basic_ok || api_key_ok
    } else {
        !require_basic || basic_ok
    };

    if !authorized {
        let offer_basic = state.config.web.has_basic_auth();
        if is_api {
            warn!(path, "blocked API request without configured auth");
        } else {
            warn!(path, "blocked web request without configured basic auth");
        }
        return unauthorized_auth_response(&path, offer_basic);
    }

    next.run(request).await
}

pub(super) async fn guard_browser_mutations(
    State(state): State<WebState>,
    request: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let is_api = path.starts_with("/api/");
    let origin = request.headers().get(ORIGIN).and_then(header_value_str);
    let referer = request.headers().get(REFERER).and_then(header_value_str);
    let host = request.headers().get(HOST).and_then(header_value_str);
    let has_browser_metadata = request_has_browser_metadata(request.headers());
    let has_valid_session = has_valid_browser_session(request.headers(), &state);
    let should_issue_session = method_receives_browser_session(&method) && !has_valid_session;
    let enforce_browser_guard = state.browser_mutation_guard_enabled();

    if enforce_browser_guard && method_requires_same_origin(&method) {
        let require_browser_session = !is_api || has_browser_metadata;

        if has_browser_metadata && !is_same_origin_browser_mutation(request.headers()) {
            warn!(
                method = %method,
                path,
                host = host.unwrap_or("<missing>"),
                origin = origin.unwrap_or("<missing>"),
                referer = referer.unwrap_or("<missing>"),
                "blocked cross-origin mutation request"
            );
            return forbidden_origin_response(request.uri().path());
        }

        if require_browser_session && !has_valid_session {
            warn!(
                method = %method,
                path,
                host = host.unwrap_or("<missing>"),
                origin = origin.unwrap_or("<missing>"),
                referer = referer.unwrap_or("<missing>"),
                "blocked browser mutation without issued session cookie"
            );
            return missing_browser_session_response(request.uri().path());
        }
    }

    let mut response = next.run(request).await;
    if should_issue_session {
        if let Ok(value) = HeaderValue::from_str(&browser_session_cookie_header(
            state.browser_session_token(),
        )) {
            response.headers_mut().append(SET_COOKIE, value);
        }
    }
    response
}
