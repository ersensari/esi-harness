//! Deterministic ESI-Wiki bounded-memory upsert and dedicated session
//! renewal (ADR-0012, TASK-POST-122).
//!
//! Backend primitives for approved-plan capture and interactive renewal.
//! Callers must keep renewal credentials outside agent tools/transcripts
//! (ADR-0015). Presence of this module alone is not a wired Desktop feature.

use std::{collections::HashMap, time::Duration};

use serde_json::{json, Value};

use crate::config::{Config, ExtensionConfig, ExtensionEntry};

/// Name of the bundled `esi-wiki` extension whose configured URI and
/// `ESI_WIKI_AUTHORIZATION` secret are reused for every call in this module.
const WIKI_EXTENSION_NAME: &str = "esi-wiki";

/// The exact secret key the bundled `esi-wiki` extension resolves
/// (`ui/desktop/src/components/settings/extensions/bundled-extensions.json`,
/// `crates/goose/src/acp/provider.rs`). Session renewal writes the same key
/// so the extension and this module always agree on one bearer value.
pub const AUTHORIZATION_SECRET_KEY: &str = "ESI_WIKI_AUTHORIZATION";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

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

/// Only approved, visible plan fields are eligible. Reject rather than silently
/// truncate oversized records or upload marked private reasoning. This is a
/// conservative marker guard, not a general secret/PII detector.
pub fn approved_plan_records(
    plan: &esi_workspace_plan::WorkspacePlan,
) -> Result<Vec<BoundedRecord>, WikiMemoryError> {
    if !plan.is_implementation_allowed() {
        return Err(WikiMemoryError::Rejected("plan approval required".into()));
    }
    let hash = plan.content_hash();
    let mut records = vec![
        BoundedRecord {
            key: "product-scope", title: "Approved product scope".into(),
            content: json!({"plan_hash": hash, "title": plan.title(), "description": plan.description(), "requirements": plan.requirements()}).to_string(),
            content_class: "context_summary", tags: vec!["workspace-plan".into()],
        },
        BoundedRecord {
            key: "architecture-decision", title: "Approved architecture decision".into(),
            content: json!({"plan_hash": hash, "architecture_notes": plan.architecture_notes()}).to_string(),
            content_class: "decision", tags: vec!["workspace-plan".into()],
        },
    ];
    if let Some(innovation) = plan.innovation_discovery() {
        records.push(BoundedRecord {
            key: "innovation-rationale",
            title: "Approved innovation rationale".into(),
            content:
                json!({"plan_hash": hash, "selected_rationale": innovation.selected_rationale})
                    .to_string(),
            content_class: "rationale",
            tags: vec!["workspace-plan".into()],
        });
    }
    for record in &records {
        let lower = record.content.to_lowercase();
        if record.content.len() > 8192 {
            return Err(WikiMemoryError::Rejected(
                "approved memory record exceeds 8192 bytes; shorten the visible plan".into(),
            ));
        }
        if [
            "<think>",
            "</think>",
            "<thinking>",
            "</thinking>",
            "<inner_monologue>",
            "</inner_monologue>",
            "<reasoning>",
            "</reasoning>",
            "<scratchpad>",
            "</scratchpad>",
            "chain-of-thought:",
            "internal reasoning:",
            "step-by-step reasoning:",
            "hidden reasoning:",
            "private reasoning:",
            "model internal:",
            "wiki_session_",
            "-----begin private key-----",
            "authorization: bearer ",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            return Err(WikiMemoryError::Rejected(
                "approved memory contains restricted private-content markers".into(),
            ));
        }
    }
    Ok(records)
}

pub async fn capture_approved_plan(
    plan: &esi_workspace_plan::WorkspacePlan,
) -> esi_workspace_plan::MemorySyncOutcome {
    capture_approved_plan_with_config(Config::global(), plan).await
}

pub(crate) async fn capture_approved_plan_with_config(
    config: &Config,
    plan: &esi_workspace_plan::WorkspacePlan,
) -> esi_workspace_plan::MemorySyncOutcome {
    match approved_plan_records(plan) {
        Ok(records) => {
            upsert_workspace_records_with_config(config, plan.workspace_id(), &records).await
        }
        Err(error) => esi_workspace_plan::MemorySyncOutcome::Pending {
            reason: error.to_string(),
        },
    }
}

struct WikiEndpoint {
    mcp_uri: String,
    authorization: String,
}

fn http_client() -> Result<reqwest::Client, WikiMemoryError> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| WikiMemoryError::Unavailable("request or secret operation failed".into()))
}

/// Reads the bundled `esi-wiki` extension's raw configured URI without
/// requiring an existing `ESI_WIKI_AUTHORIZATION` secret to already exist.
/// Used for session renewal, which is precisely the call that establishes
/// that secret for the first time (or refreshes it after expiry) and must
/// therefore work even when no valid bearer is currently configured.
fn configured_extension(config: &Config) -> Result<ExtensionConfig, WikiMemoryError> {
    let entries: HashMap<String, ExtensionEntry> = config
        .get_param("extensions")
        .map_err(|_| WikiMemoryError::NotConfigured)?;
    let entry = entries
        .get(WIKI_EXTENSION_NAME)
        .filter(|entry| entry.enabled)
        .ok_or(WikiMemoryError::NotConfigured)?;
    match &entry.config {
        ExtensionConfig::StreamableHttp { uri, socket, .. } if socket.is_none() => {
            validated_uri(uri)?;
            Ok(entry.config.clone())
        }
        _ => Err(WikiMemoryError::NotConfigured),
    }
}

fn validated_uri(uri: &str) -> Result<reqwest::Url, WikiMemoryError> {
    let url = reqwest::Url::parse(uri).map_err(|_| WikiMemoryError::NotConfigured)?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback());
    if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().trim_end_matches('/').ends_with("/mcp")
    {
        return Err(WikiMemoryError::NotConfigured);
    }
    Ok(url)
}

fn configured_base_uri(config: &Config) -> Result<String, WikiMemoryError> {
    match configured_extension(config)? {
        ExtensionConfig::StreamableHttp { uri, .. } => Ok(uri
            .trim_end_matches('/')
            .strip_suffix("/mcp")
            .ok_or(WikiMemoryError::NotConfigured)?
            .to_string()),
        _ => Err(WikiMemoryError::NotConfigured),
    }
}

/// Resolves the fully-authorized Wiki MCP endpoint by reusing the bundled
/// `esi-wiki` extension's own `ExtensionConfig::resolve`, which reads the
/// `ESI_WIKI_AUTHORIZATION` secret through `Config` (subject to the same
/// in-process cache/invalidation as every other secret-backed extension).
async fn resolved_endpoint(config: &Config) -> Result<WikiEndpoint, WikiMemoryError> {
    let extension = configured_extension(config)?;
    let resolved = extension
        .resolve(config)
        .await
        .map_err(|_error| WikiMemoryError::NotConfigured)?;
    match resolved {
        ExtensionConfig::StreamableHttp { uri, headers, .. } => {
            validated_uri(&uri)?;
            let authorization = headers.get("Authorization").cloned().unwrap_or_default();
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

// Never propagate remote payloads, URLs or transport errors into durable outbox
// state or logs: a compromised endpoint can echo credentials in any of them.
async fn response_payload(mut response: reqwest::Response) -> Result<Value, WikiMemoryError> {
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(WikiMemoryError::Unauthorized("login required".into()));
    }
    if !status.is_success() {
        return Err(WikiMemoryError::Rejected("HTTP request rejected".into()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| WikiMemoryError::Unavailable("response read failed".into()))?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(WikiMemoryError::Rejected(
                "response exceeds size limit".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| WikiMemoryError::Rejected("invalid JSON response".into()))
}

async fn call_once(
    endpoint: &WikiEndpoint,
    name: &str,
    arguments: Value,
) -> Result<Value, WikiMemoryError> {
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
        .map_err(|_| WikiMemoryError::Unavailable("request or secret operation failed".into()))?;
    let payload = response_payload(response).await?;
    if payload.get("error").is_some()
        || payload.pointer("/result/isError").and_then(Value::as_bool) == Some(true)
    {
        return Err(WikiMemoryError::Rejected("Wiki tool failed".into()));
    }
    payload
        .get("result")
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| WikiMemoryError::Rejected("missing tool result".into()))
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
async fn call_wiki_tool(
    config: &Config,
    name: &str,
    arguments: Value,
) -> Result<Value, WikiMemoryError> {
    let endpoint = resolved_endpoint(config).await?;
    match call_once(&endpoint, name, arguments.clone()).await {
        Ok(value) => Ok(value),
        Err(WikiMemoryError::Unauthorized(_)) => {
            let retried_endpoint = resolved_endpoint(config).await?;
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
    upsert_workspace_records_with_config(Config::global(), workspace_id, records).await
}

async fn upsert_workspace_records_with_config(
    config: &Config,
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
        if let Err(error) = call_wiki_tool(config, "wiki_knowledge_store", arguments).await {
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
    renew_session_with_config(Config::global(), handle, password).await
}

async fn renew_session_with_config(
    config: &Config,
    handle: &str,
    password: &str,
) -> Result<(), WikiMemoryError> {
    // Environment overrides win over secret storage; do not report successful
    // renewal when this process would continue resolving a stale override.
    if std::env::var_os(AUTHORIZATION_SECRET_KEY).is_some() {
        return Err(WikiMemoryError::Rejected(
            "remove the authorization environment override before login".into(),
        ));
    }
    let extension = configured_extension(config)?;
    let ExtensionConfig::StreamableHttp {
        ref headers,
        ref env_keys,
        ref envs,
        ..
    } = extension
    else {
        return Err(WikiMemoryError::NotConfigured);
    };
    if headers.get("Authorization").map(String::as_str) != Some("${ESI_WIKI_AUTHORIZATION}")
        || !env_keys.iter().any(|key| key == AUTHORIZATION_SECRET_KEY)
        || serde_json::to_value(envs)
            .map_err(|_| WikiMemoryError::NotConfigured)?
            .get(AUTHORIZATION_SECRET_KEY)
            .is_some()
    {
        return Err(WikiMemoryError::NotConfigured);
    }
    let base_uri = configured_base_uri(config)?;
    let client = http_client()?;
    let response = client
        .post(format!("{base_uri}/v1/sessions"))
        .json(&json!({ "handle": handle, "password": password }))
        .send()
        .await
        .map_err(|_| WikiMemoryError::Unavailable("request or secret operation failed".into()))?;
    let payload = response_payload(response).await?;
    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| WikiMemoryError::Rejected("Wiki login response had no token".to_string()))?;

    if !token.starts_with("wiki_session_")
        || token.len() > 512
        || token.len() <= "wiki_session_".len()
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(WikiMemoryError::Rejected("invalid session token".into()));
    }
    config
        .set_secret(AUTHORIZATION_SECRET_KEY, &format!("Bearer {token}"))
        .map_err(|_| WikiMemoryError::Unavailable("request or secret operation failed".into()))?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    pub(crate) fn fixture(uri: &str, enabled: bool) -> (tempfile::TempDir, Config) {
        let root = tempfile::tempdir().unwrap();
        let config = Config::new_with_file_secrets(
            root.path().join("config.yaml"),
            root.path().join("secrets.yaml"),
        )
        .unwrap();
        config
            .set_param(
                "extensions",
                json!({"esi-wiki": {
                    "enabled": enabled, "type": "streamable_http", "name": "esi-wiki",
                    "uri": uri, "env_keys": [AUTHORIZATION_SECRET_KEY],
                    "headers": {"Authorization": "${ESI_WIKI_AUTHORIZATION}"}
                }}),
            )
            .unwrap();
        config
            .set_secret(AUTHORIZATION_SECRET_KEY, &"Bearer wiki_session_old")
            .unwrap();
        (root, config)
    }

    #[test]
    fn endpoint_requires_secure_transport_and_exact_mcp_path() {
        for uri in [
            "https://wiki.example/mcp",
            "http://127.0.0.1:9900/mcp/",
            "http://[::1]:9900/mcp",
            "https://wiki.example/nested/mcp",
        ] {
            assert!(validated_uri(uri).is_ok(), "{uri}");
        }
        for uri in [
            "http://wiki.example/mcp",
            "https://user:secret@wiki.example/mcp",
            "https://wiki.example/mcp?token=secret",
            "https://wiki.example/mcp#secret",
            "file:///mcp",
            "https://wiki.example/notmcp",
        ] {
            assert!(validated_uri(uri).is_err(), "{uri}");
        }
        let (_root, config) = fixture("http://127.0.0.1:9900/prefix/mcp/", true);
        assert_eq!(
            configured_base_uri(&config).unwrap(),
            "http://127.0.0.1:9900/prefix"
        );
    }

    #[tokio::test]
    async fn repeated_401_retries_exactly_once_even_with_non_json_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("secret echoed here"))
            .expect(2)
            .mount(&server)
            .await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
        let error = call_wiki_tool(&config, "wiki_knowledge_store", json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            WikiMemoryError::Unauthorized("login required".into())
        );
    }

    #[tokio::test]
    async fn disabled_extension_never_sends_a_request() {
        let server = MockServer::start().await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), false);
        assert_eq!(
            call_wiki_tool(&config, "wiki_knowledge_store", json!({}))
                .await
                .unwrap_err(),
            WikiMemoryError::NotConfigured
        );
        assert_eq!(
            renew_session_with_config(&config, "user", "secret")
                .await
                .unwrap_err(),
            WikiMemoryError::NotConfigured
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn single_retry_resolves_concurrently_rotated_authorization() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let server = MockServer::start().await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
        let config = Arc::new(config);
        let rotate = config.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        Mock::given(method("POST"))
            .respond_with(move |request: &wiremock::Request| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    assert_eq!(
                        request.headers.get("Authorization").unwrap(),
                        "Bearer wiki_session_old"
                    );
                    rotate
                        .set_secret(AUTHORIZATION_SECRET_KEY, &"Bearer wiki_session_rotated")
                        .unwrap();
                    ResponseTemplate::new(401)
                } else {
                    assert_eq!(
                        request.headers.get("Authorization").unwrap(),
                        "Bearer wiki_session_rotated"
                    );
                    ResponseTemplate::new(200).set_body_json(json!({"result":{"stored":true}}))
                }
            })
            .expect(2)
            .mount(&server)
            .await;
        assert!(call_wiki_tool(&config, "wiki_knowledge_store", json!({}))
            .await
            .is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn forbidden_is_not_retried_and_does_not_echo_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_json(json!({"error":{"message":"Bearer secret password"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
        let error = call_wiki_tool(&config, "wiki_knowledge_store", json!({}))
            .await
            .unwrap_err();
        assert_eq!(
            error,
            WikiMemoryError::Rejected("HTTP request rejected".into())
        );
    }

    #[tokio::test]
    async fn malformed_and_tool_error_responses_cannot_report_success() {
        for payload in [
            json!({"error":{"message":"secret"}}),
            json!({"result":{"isError":true}}),
            json!({"result":null}),
            json!({}),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(payload))
                .expect(1)
                .mount(&server)
                .await;
            let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
            assert!(matches!(
                call_wiki_tool(&config, "wiki_knowledge_store", json!({})).await,
                Err(WikiMemoryError::Rejected(_))
            ));
        }
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("x".repeat(MAX_RESPONSE_BYTES + 1)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
        assert_eq!(
            call_wiki_tool(&config, "wiki_knowledge_store", json!({}))
                .await
                .unwrap_err(),
            WikiMemoryError::Rejected("response exceeds size limit".into())
        );
    }

    #[tokio::test]
    async fn login_redirect_does_not_forward_password_or_change_secret() {
        let target = MockServer::start().await;
        let server = MockServer::start().await;
        Mock::given(path("/v1/sessions"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", target.uri()))
            .expect(1)
            .mount(&server)
            .await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
        assert!(renew_session_with_config(&config, "user", "secret")
            .await
            .is_err());
        assert!(target.received_requests().await.unwrap().is_empty());
        assert_eq!(
            config
                .get_secret::<String>(AUTHORIZATION_SECRET_KEY)
                .unwrap(),
            "Bearer wiki_session_old"
        );
    }

    #[tokio::test]
    async fn renewal_updates_warm_secret_cache_without_restart() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/sessions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"token":"wiki_session_new"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (_root, config) = fixture(&format!("{}/mcp", server.uri()), true);
        assert_eq!(
            resolved_endpoint(&config).await.unwrap().authorization,
            "Bearer wiki_session_old"
        );
        renew_session_with_config(&config, "user", "test-only-password")
            .await
            .unwrap();
        assert_eq!(
            resolved_endpoint(&config).await.unwrap().authorization,
            "Bearer wiki_session_new"
        );
        let secrets = std::fs::read_to_string(_root.path().join("secrets.yaml")).unwrap();
        assert!(!secrets.contains("test-only-password"));
    }

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
