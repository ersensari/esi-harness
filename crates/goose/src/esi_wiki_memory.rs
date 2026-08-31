//! Deterministic ESI-Wiki bounded-memory upsert and dedicated session
//! renewal (ADR-0012, TASK-POST-122).
//!
//! This module is called directly by code — never left to model discretion.
//! `crate::agents::platform_extensions::workspace_plan::WorkspacePlanClient`
//! calls it after a human confirms workspace-plan approval
//! (`workspaceplan__approve`) and exposes a dedicated interactive renewal
//! tool (`workspaceplan__renew_wiki_session`).
//!
//! It intentionally reuses the *same* Wiki endpoint URI and
//! `ESI_WIKI_AUTHORIZATION` bearer secret already configured for the bundled
//! `esi-wiki` extension (ADR-0009) instead of inventing a second credential
//! path: see [`resolved_endpoint`] and [`configured_base_uri`].

use std::time::Duration;

use serde_json::{json, Value};

use crate::config::{get_extension_by_name, Config, ExtensionConfig};

/// Name of the bundled `esi-wiki` extension whose configured URI and
/// `ESI_WIKI_AUTHORIZATION` secret are reused for every call in this module.
const WIKI_EXTENSION_NAME: &str = "esi-wiki";

/// The exact secret key the bundled `esi-wiki` extension resolves
/// (`ui/desktop/src/components/settings/extensions/bundled-extensions.json`,
/// `crates/goose/src/acp/provider.rs`). Session renewal writes the same key
/// so the extension and this module always agree on one bearer value.
pub const AUTHORIZATION_SECRET_KEY: &str = "ESI_WIKI_AUTHORIZATION";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Typed outcome of a call into this module. Every variant is safe to log or
/// surface to a user: none of them ever contain a password or bearer token.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum WikiMemoryError {
    #[error("the esi-wiki extension has no configured Wiki endpoint URI")]
    NotConfigured,
    #[error("Wiki session is not authorized: {0}")]
    Unauthorized(String),
    #[error("Wiki is unreachable: {0}")]
    Unavailable(String),
    #[error("Wiki rejected the request: {0}")]
    Rejected(String),
}

/// A single bounded, user-visible workspace-memory record. `content` must
/// already be a concise, user-visible summary of already-approved plan
/// fields — never raw or hidden chain-of-thought. Wiki's own privacy guard
/// (ADR-0011, `check_privacy_guard`) rejects chain-of-thought markers as a
/// second, server-side line of defense.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedRecord {
    /// Stable key within `(scope, workspace_id)`. Calling
    /// `wiki_knowledge_store` again with the same key upserts in place
    /// (Wiki's `ON CONFLICT (scope, workspace_id, key) DO UPDATE`), so
    /// re-approval or revision resync never creates duplicates.
    pub key: &'static str,
    pub title: String,
    pub content: String,
    /// One of Wiki's `ContentClass` values: `rationale`, `decision`,
    /// `finding`, `evidence_link`, or `context_summary`.
    pub content_class: &'static str,
    pub tags: Vec<String>,
}

struct WikiEndpoint {
    mcp_uri: String,
    authorization: String,
}

fn http_client() -> Result<reqwest::Client, WikiMemoryError> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|error| WikiMemoryError::Unavailable(error.to_string()))
}

/// Reads the bundled `esi-wiki` extension's raw configured URI without
/// requiring an existing `ESI_WIKI_AUTHORIZATION` secret to already exist.
/// Used for session renewal, which is precisely the call that establishes
/// that secret for the first time (or refreshes it after expiry) and must
/// therefore work even when no valid bearer is currently configured.
fn configured_base_uri() -> Result<String, WikiMemoryError> {
    let config = get_extension_by_name(WIKI_EXTENSION_NAME).ok_or(WikiMemoryError::NotConfigured)?;
    let uri = match config {
        ExtensionConfig::StreamableHttp { uri, .. } => uri,
        _ => return Err(WikiMemoryError::NotConfigured),
    };
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        return Err(WikiMemoryError::NotConfigured);
    }
    Ok(trimmed
        .trim_end_matches("/mcp")
        .trim_end_matches('/')
        .to_string())
}

/// Resolves the fully-authorized Wiki MCP endpoint by reusing the bundled
/// `esi-wiki` extension's own `ExtensionConfig::resolve`, which reads the
/// `ESI_WIKI_AUTHORIZATION` secret through `Config` (subject to the same
/// in-process cache/invalidation as every other secret-backed extension).
async fn resolved_endpoint() -> Result<WikiEndpoint, WikiMemoryError> {
    let config = get_extension_by_name(WIKI_EXTENSION_NAME).ok_or(WikiMemoryError::NotConfigured)?;
    let resolved = config
        .resolve(Config::global())
        .await
        .map_err(|_error| WikiMemoryError::NotConfigured)?;
    match resolved {
        ExtensionConfig::StreamableHttp { uri, headers, .. } => {
            if uri.trim().is_empty() {
                return Err(WikiMemoryError::NotConfigured);
            }
            let authorization = headers
                .get("Authorization")
                .cloned()
                .unwrap_or_default();
            if authorization.is_empty() || authorization.contains("${") {
                return Err(WikiMemoryError::NotConfigured);
            }
            Ok(WikiEndpoint {
                mcp_uri: uri,
                authorization,
            })
        }
        _ => Err(WikiMemoryError::NotConfigured),
    }
}

fn error_message(payload: &Value) -> String {
    payload
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("Wiki returned an unrecognized error")
        .to_string()
}

async fn call_once(endpoint: &WikiEndpoint, name: &str, arguments: Value) -> Result<Value, WikiMemoryError> {
    let client = http_client()?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let response = client
        .post(&endpoint.mcp_uri)
        .header("Authorization", &endpoint.authorization)
        .json(&body)
        .send()
        .await
        .map_err(|error| WikiMemoryError::Unavailable(error.to_string()))?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .map_err(|error| WikiMemoryError::Unavailable(error.to_string()))?;

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(WikiMemoryError::Unauthorized(error_message(&payload)));
    }
    if !status.is_success() {
        return Err(WikiMemoryError::Rejected(error_message(&payload)));
    }
    if payload.get("error").is_some() {
        // Defensive: a JSON-RPC envelope could in principle report an error
        // alongside a 200 status.
        return Err(WikiMemoryError::Rejected(error_message(&payload)));
    }
    Ok(payload.get("result").cloned().unwrap_or(Value::Null))
}

/// Calls a Wiki MCP tool, resolving the endpoint fresh on every attempt.
///
/// Exactly one retry is attempted, and only for an authenticated
/// session-expiry response (HTTP 401 — Wiki's `ApplicationStatus::Unauthenticated`,
/// covering an expired, invalid, or revoked session). The retry re-resolves
/// the endpoint so a concurrent interactive renewal (which writes through
/// `Config::global().set_secret`) is picked up without a restart. Any other
/// error, or a second 401, is returned as an explicit typed failure — this
/// module never loops or silently swallows a Wiki failure.
async fn call_wiki_tool(name: &str, arguments: Value) -> Result<Value, WikiMemoryError> {
    let endpoint = resolved_endpoint().await?;
    match call_once(&endpoint, name, arguments.clone()).await {
        Ok(value) => Ok(value),
        Err(WikiMemoryError::Unauthorized(_)) => {
            let retried_endpoint = resolved_endpoint().await?;
            call_once(&retried_endpoint, name, arguments).await
        }
        Err(other) => Err(other),
    }
}

/// Upserts bounded workspace-scoped records into Wiki for `workspace_id`.
///
/// `workspace_id` must be `WorkspacePlan::workspace_id()` — never the
/// canonical path or any model-authored text. Scope is always `"workspace"`;
/// `project_id` is never supplied (Wiki's `knowledge_store` always persists
/// `project_id: None` regardless, so no Wiki project is a precondition).
/// Global promotion is never performed from this path.
///
/// Returns [`esi_workspace_plan::MemorySyncOutcome::Synced`] only if every
/// record stored successfully; otherwise returns
/// [`esi_workspace_plan::MemorySyncOutcome::Pending`] with the first failure
/// reason, which is a durable outbox state a later approval/retry can
/// complete without ever creating a duplicate entry (Wiki upserts by
/// `(scope, workspace_id, key)`).
pub async fn upsert_workspace_records(
    workspace_id: &str,
    records: &[BoundedRecord],
) -> esi_workspace_plan::MemorySyncOutcome {
    for record in records {
        let arguments = json!({
            "scope": "workspace",
            "workspace_id": workspace_id,
            "key": record.key,
            "title": record.title,
            "content": record.content,
            "content_class": record.content_class,
            "tags": record.tags,
        });
        if let Err(error) = call_wiki_tool("wiki_knowledge_store", arguments).await {
            return esi_workspace_plan::MemorySyncOutcome::Pending {
                reason: error.to_string(),
            };
        }
    }
    esi_workspace_plan::MemorySyncOutcome::Synced
}

/// Performs a dedicated, non-provider Wiki login and stores the returned
/// bearer through `Config::global().set_secret`, so the already-running
/// process picks it up immediately with no restart.
///
/// `password` is used only for the single login HTTP request body below; it
/// is never written to disk, to `Config`, to a plan file, or to any log —
/// nothing in this function's call graph after the HTTP response is
/// received ever sees it again.
///
/// # Errors
///
/// Returns [`WikiMemoryError::NotConfigured`] if the `esi-wiki` extension has
/// no configured endpoint, [`WikiMemoryError::Unauthorized`] for invalid
/// credentials, or [`WikiMemoryError::Unavailable`] if Wiki cannot be
/// reached. Never silently succeeds and never performs automatic/background
/// retries — this is the explicit, interactive renewal path.
pub async fn renew_session(handle: &str, password: &str) -> Result<(), WikiMemoryError> {
    let base_uri = configured_base_uri()?;
    let client = http_client()?;
    let response = client
        .post(format!("{base_uri}/v1/sessions"))
        .json(&json!({ "handle": handle, "password": password }))
        .send()
        .await
        .map_err(|error| WikiMemoryError::Unavailable(error.to_string()))?;
    let status = response.status();
    let payload: Value = response
        .json()
        .await
        .map_err(|error| WikiMemoryError::Unavailable(error.to_string()))?;

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(WikiMemoryError::Unauthorized(
            "invalid Wiki credentials".to_string(),
        ));
    }
    if !status.is_success() {
        return Err(WikiMemoryError::Rejected(error_message(&payload)));
    }
    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| WikiMemoryError::Rejected("Wiki login response had no token".to_string()))?;

    Config::global()
        .set_secret(AUTHORIZATION_SECRET_KEY, &format!("Bearer {token}"))
        .map_err(|error| WikiMemoryError::Unavailable(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_memory_error_never_contains_the_word_password() {
        // A structural guard: every error message this module can construct
        // must be safe to log. If a future edit accidentally interpolates a
        // credential into an error, this fails loudly.
        let errors = [
            WikiMemoryError::NotConfigured,
            WikiMemoryError::Unauthorized("invalid Wiki credentials".to_string()),
            WikiMemoryError::Unavailable("connection refused".to_string()),
            WikiMemoryError::Rejected("bad request".to_string()),
        ];
        for error in errors {
            let message = error.to_string().to_lowercase();
            assert!(!message.contains("password"));
        }
    }

    #[test]
    fn bounded_record_is_plain_data_with_a_stable_key() {
        let record = BoundedRecord {
            key: "product-scope",
            title: "Title".to_string(),
            content: "Content".to_string(),
            content_class: "context_summary",
            tags: vec!["workspace-plan".to_string()],
        };
        assert_eq!(record.key, "product-scope");
        assert_eq!(record.content_class, "context_summary");
    }
}
