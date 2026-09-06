use crate::agents::extension::PlatformExtensionContext;
use crate::agents::mcp_client::{Error, McpClientTrait};
use crate::agents::tool_execution::ToolCallContext;
use crate::config::paths::Paths;
use crate::context_mgmt::store::ContextArchiveStore;
use crate::context_mgmt::{is_context_archived, CONTEXT_ARCHIVE_OPERATION};
use crate::conversation::message::Message;
use anyhow::Result;
use async_trait::async_trait;
use indoc::indoc;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeResult, JsonObject, ListToolsResult,
    ServerCapabilities, Tool, ToolAnnotations,
};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

pub const EXTENSION_NAME: &str = "esi_context_management";
const MAX_RESULTS: usize = 20;
const MAX_RESULT_CHARS: usize = 24_000;
const MAX_MESSAGE_CHARS: usize = 8_000;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum MemoryAction {
    Search,
    Get,
    Handoff,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SessionMemoryParams {
    /// Operation to perform against the current session's local memory.
    action: MemoryAction,
    /// Search terms for `search`.
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    /// Exact local message IDs for `get`.
    #[serde(skip_serializing_if = "Option::is_none")]
    ids: Option<Vec<String>>,
    /// Maximum search results (default 8, maximum 20).
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

pub struct ContextManagementClient {
    info: InitializeResult,
    context: PlatformExtensionContext,
    archive_path: PathBuf,
}

impl ContextManagementClient {
    pub fn new(context: PlatformExtensionContext) -> Result<Self> {
        let info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new(EXTENSION_NAME.to_string(), "1.0.0".to_string())
                    .with_title("ESI Context Management"),
            )
            .with_instructions(
                indoc! {r#"
                    ESI Context Management keeps compacted conversation evidence in the
                    current session's local Harness database. Search it when a compacted
                    handoff lacks an earlier decision, path, command, error, or result.
                    Handoff output is local and provider-neutral; publish it elsewhere only
                    after the user explicitly asks and through that destination's own tool.
                "#}
                .to_string(),
            );
        Ok(Self {
            info,
            context,
            archive_path: Paths::in_data_dir("context-management"),
        })
    }

    fn archived_messages(messages: &[Message]) -> Vec<&Message> {
        messages
            .iter()
            .filter(|message| is_context_archived(message))
            .collect()
    }

    fn bounded(text: String, max: usize) -> String {
        if text.chars().count() <= max {
            return text;
        }
        let mut bounded = text.chars().take(max).collect::<String>();
        bounded.push_str("\n[truncated by ESI Context Management]");
        bounded
    }

    fn render_message(message: &Message) -> String {
        let id = message.id.as_deref().unwrap_or("unknown");
        let body =
            crate::context_mgmt::format_message_for_compacting(&message.agent_visible_content());
        Self::bounded(format!("Message ID: {id}\n{body}"), MAX_MESSAGE_CHARS)
    }

    async fn handle(
        &self,
        current_session_id: &str,
        arguments: Option<JsonObject>,
    ) -> Result<Vec<ContentBlock>, String> {
        let arguments = arguments.ok_or("Missing arguments")?;
        let params: SessionMemoryParams = serde_json::from_value(arguments.into())
            .map_err(|error| format!("Invalid session-memory arguments: {error}"))?;
        let session = self
            .context
            .session_manager
            .get_session(current_session_id, true)
            .await
            .map_err(|error| format!("Could not load current session: {error}"))?;
        let conversation = session
            .conversation
            .as_ref()
            .ok_or("Current session has no conversation")?;
        let archived_source = Self::archived_messages(conversation.messages());
        let store = ContextArchiveStore::open(&self.archive_path)
            .map_err(|error| format!("Could not open local context archive: {error}"))?;
        store
            .replace_session(current_session_id, archived_source.iter().copied())
            .map_err(|error| format!("Could not synchronize local context archive: {error}"))?;
        let archived = store
            .messages(current_session_id)
            .map_err(|error| format!("Could not read local context archive: {error}"))?;

        let output = match params.action {
            MemoryAction::Search => {
                let query = params
                    .query
                    .as_deref()
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or("search requires a non-empty query")?;
                let terms = query
                    .split_whitespace()
                    .map(str::to_lowercase)
                    .collect::<Vec<_>>();
                let limit = params.limit.unwrap_or(8).clamp(1, MAX_RESULTS);
                let mut matches = archived
                    .iter()
                    .filter_map(|message| {
                        let rendered = Self::render_message(message);
                        let searchable = rendered.to_lowercase();
                        let score = terms
                            .iter()
                            .filter(|term| searchable.contains(term.as_str()))
                            .count();
                        (score > 0).then_some((score, message.created, rendered))
                    })
                    .collect::<Vec<_>>();
                matches.sort_by_key(|(score, created, _)| {
                    (std::cmp::Reverse(*score), std::cmp::Reverse(*created))
                });
                let body = matches
                    .into_iter()
                    .take(limit)
                    .map(|(_, _, rendered)| rendered)
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                if body.is_empty() {
                    format!("No compacted local context matched '{query}'.")
                } else {
                    Self::bounded(body, MAX_RESULT_CHARS)
                }
            }
            MemoryAction::Get => {
                let ids = params.ids.as_deref().ok_or("get requires ids")?;
                if ids.is_empty() || ids.len() > MAX_RESULTS {
                    return Err(format!("get requires 1 to {MAX_RESULTS} message IDs"));
                }
                let mut found = Vec::new();
                for id in ids {
                    if let Some(message) = store
                        .get(current_session_id, id)
                        .map_err(|error| format!("Could not retrieve archived message: {error}"))?
                    {
                        found.push(Self::render_message(&message));
                    }
                }
                let body = found.join("\n\n---\n\n");
                if body.is_empty() {
                    "No requested IDs belong to compacted context in this session.".to_string()
                } else {
                    Self::bounded(body, MAX_RESULT_CHARS)
                }
            }
            MemoryAction::Handoff => {
                let active = conversation
                    .messages()
                    .iter()
                    .filter(|message| message.is_agent_visible() && !message.is_turn_context())
                    .map(Self::render_message)
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                let archived_count = archived.len();
                Self::bounded(
                    format!(
                        "# ESI Session Handoff\n\nSession: {}\nWorking directory: {}\nArchived local messages: {}\nArchive operation: {}\n\n## Active Context\n\n{}",
                        session.id,
                        session.working_dir.display(),
                        archived_count,
                        CONTEXT_ARCHIVE_OPERATION,
                        active
                    ),
                    MAX_RESULT_CHARS,
                )
            }
        };
        Ok(vec![ContentBlock::text(output)])
    }

    fn tools() -> Vec<Tool> {
        let schema =
            serde_json::to_value(schema_for!(SessionMemoryParams)).expect("session-memory schema");
        vec![Tool::new(
            "session_memory".to_string(),
            "Search/get compacted evidence in the current local session, or prepare a bounded handoff. Never publishes to Wiki or another service.".to_string(),
            schema.as_object().expect("object schema").clone(),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Recall local compacted context".to_string()),
            Some(true),
            Some(false),
            Some(true),
            Some(false),
        ))]
    }
}

#[async_trait]
impl McpClientTrait for ContextManagementClient {
    async fn list_tools(
        &self,
        _session_id: &str,
        _next_cursor: Option<String>,
        _cancellation_token: CancellationToken,
    ) -> Result<ListToolsResult, Error> {
        Ok(ListToolsResult {
            tools: Self::tools(),
            next_cursor: None,
            meta: None,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        ctx: &ToolCallContext,
        name: &str,
        arguments: Option<JsonObject>,
        _cancellation_token: CancellationToken,
    ) -> Result<CallToolResult, Error> {
        let content = match name {
            "session_memory" => self.handle(&ctx.session_id, arguments).await,
            _ => Err(format!("Unknown tool: {name}")),
        };
        Ok(match content {
            Ok(content) => CallToolResult::success(content),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        })
    }

    fn get_info(&self) -> Option<&InitializeResult> {
        Some(&self.info)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextManagementClient, MemoryAction, SessionMemoryParams, MAX_RESULT_CHARS};
    use crate::agents::extension::PlatformExtensionContext;
    use crate::config::GooseMode;
    use crate::context_mgmt::CONTEXT_ARCHIVE_OPERATION;
    use crate::conversation::message::{Message, MessageMetadata};
    use crate::conversation::Conversation;
    use crate::session::session_manager::SessionType;
    use crate::session::SessionManager;
    use std::sync::Arc;

    #[test]
    fn archive_filter_requires_explicit_compaction_provenance() {
        let unrelated_hidden = Message::user()
            .with_text("private UI content")
            .with_metadata(MessageMetadata::invisible());
        let mut metadata = MessageMetadata::user_only();
        metadata.set_operation_note(
            CONTEXT_ARCHIVE_OPERATION,
            "archived",
            serde_json::json!(true),
        );
        let compacted = Message::assistant()
            .with_text("compacted evidence")
            .with_metadata(metadata);

        let messages = [unrelated_hidden, compacted];
        let archived = ContextManagementClient::archived_messages(&messages);
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].as_concat_text(), "compacted evidence");
    }

    #[test]
    fn result_bound_is_unicode_safe() {
        let oversized = "🧠".repeat(MAX_RESULT_CHARS + 5);
        let bounded = ContextManagementClient::bounded(oversized, MAX_RESULT_CHARS);
        assert!(bounded.ends_with("[truncated by ESI Context Management]"));
        assert_eq!(bounded.matches('🧠').count(), MAX_RESULT_CHARS);
    }

    #[tokio::test]
    async fn search_get_and_handoff_work_without_wiki() {
        let root = tempfile::tempdir().unwrap();
        let manager = Arc::new(SessionManager::new(root.path().join("sessions")));
        let session = manager
            .create_session(
                root.path().to_path_buf(),
                "local context".to_string(),
                SessionType::User,
                GooseMode::default(),
            )
            .await
            .unwrap();
        let mut archived_metadata = MessageMetadata::user_only();
        archived_metadata.set_operation_note(
            CONTEXT_ARCHIVE_OPERATION,
            "archived",
            serde_json::json!(true),
        );
        let conversation = Conversation::new_unvalidated([
            Message::assistant()
                .with_id("archived-1")
                .with_text("selected heed for fast local recall")
                .with_metadata(archived_metadata),
            Message::user()
                .with_id("private-1")
                .with_text("unrelated hidden content")
                .with_metadata(MessageMetadata::invisible()),
            Message::user()
                .with_id("active-1")
                .with_text("continue here"),
        ]);
        manager
            .replace_conversation(&session.id, &conversation)
            .await
            .unwrap();
        let context = PlatformExtensionContext {
            extension_manager: None,
            session_manager: manager,
            scheduler: None,
            session: None,
            use_login_shell_path: false,
        };
        let mut client = ContextManagementClient::new(context).unwrap();
        client.archive_path = root.path().join("archive");

        let arguments = |params: SessionMemoryParams| {
            serde_json::to_value(params)
                .unwrap()
                .as_object()
                .unwrap()
                .clone()
        };
        let search = client
            .handle(
                &session.id,
                Some(arguments(SessionMemoryParams {
                    action: MemoryAction::Search,
                    query: Some("heed recall".to_string()),
                    ids: None,
                    limit: None,
                })),
            )
            .await
            .unwrap();
        let search = &search[0].as_text().unwrap().text;
        assert!(search.contains("selected heed"));
        assert!(!search.contains("unrelated hidden"));

        let get = client
            .handle(
                &session.id,
                Some(arguments(SessionMemoryParams {
                    action: MemoryAction::Get,
                    query: None,
                    ids: Some(vec!["archived-1".to_string(), "private-1".to_string()]),
                    limit: None,
                })),
            )
            .await
            .unwrap();
        let get = &get[0].as_text().unwrap().text;
        assert!(get.contains("selected heed"));
        assert!(!get.contains("unrelated hidden"));

        let handoff = client
            .handle(
                &session.id,
                Some(arguments(SessionMemoryParams {
                    action: MemoryAction::Handoff,
                    query: None,
                    ids: None,
                    limit: None,
                })),
            )
            .await
            .unwrap();
        let handoff = &handoff[0].as_text().unwrap().text;
        assert!(handoff.contains("Archived local messages: 1"));
        assert!(handoff.contains("continue here"));
        assert!(!handoff.contains("selected heed"));
    }
}
