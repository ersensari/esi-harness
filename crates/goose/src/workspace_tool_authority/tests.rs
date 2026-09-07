use super::*;
use crate::agents::extension::ExtensionConfig;
use crate::agents::extension_manager::ExtensionManager;
use crate::config::GooseMode;
use crate::conversation::message::{ActionRequiredData, MessageContent};
use crate::session::SessionType;
use esi_workspace_plan::{PlannedTask, PlannedTaskStatus, Priority, Requirement};
use futures::StreamExt;
use rmcp::model::CallToolRequestParams;
use tempfile::TempDir;

struct Fixture {
    _data: TempDir,
    root: TempDir,
    manager: Arc<ExtensionManager>,
    sessions: Arc<SessionManager>,
    permissions: Arc<crate::config::permission::PermissionManager>,
    ctx: ToolCallContext,
}

fn platform(name: &str) -> ExtensionConfig {
    ExtensionConfig::Platform {
        name: name.into(),
        description: String::new(),
        display_name: None,
        bundled: None,
        available_tools: vec![],
    }
}

impl Fixture {
    async fn new() -> Self {
        // Config's singleton remains isolated by the repository test launcher.
        Config::global()
            .set_param("ESI_AUTHORITY_TEST_INITIALIZED", true)
            .unwrap();
        let data = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let permissions = Arc::new(crate::config::permission::PermissionManager::new(
            data.path().join("permissions"),
        ));
        let manager = Arc::new(
            ExtensionManager::new_without_provider(data.path().into())
                .with_managed_authority(true, permissions.clone()),
        );
        let sessions = manager.get_context().session_manager.clone();
        let session = sessions
            .create_session(
                root.path().into(),
                "authority".into(),
                SessionType::User,
                GooseMode::Auto,
            )
            .await
            .unwrap();
        for name in ["developer", "workspaceplan"] {
            manager
                .add_extension(
                    platform(name),
                    Some(root.path().into()),
                    None,
                    Some(&session.id),
                )
                .await
                .unwrap();
        }
        let ctx = ToolCallContext::new(
            session.id,
            Some(root.path().into()),
            Some("human-call".into()),
        );
        Self {
            _data: data,
            root,
            manager,
            sessions,
            permissions,
            ctx,
        }
    }

    fn draft(&self) {
        let mut plan = WorkspacePlan::new(self.root.path(), "Authority fixture").unwrap();
        plan.set_requirements(vec![Requirement {
            id: "R1".into(),
            description: "Write a file".into(),
            acceptance_criteria: vec!["File exists".into()],
            priority: Priority::Must,
        }])
        .unwrap();
        plan.set_plan_content(
            "Implement file",
            "One local file",
            vec![PlannedTask {
                id: "T1".into(),
                title: "Write".into(),
                description: "Write file".into(),
                status: PlannedTaskStatus::Pending,
            }],
        )
        .unwrap();
        plan.save(self.root.path()).unwrap();
    }

    async fn call(&self, name: &str, args: Value) -> Result<CallToolResult> {
        let mut ctx = self.ctx.clone();
        ctx.tool_call_request_id = Some(uuid::Uuid::new_v4().to_string());
        let call = CallToolRequestParams::new(name.to_string())
            .with_arguments(args.as_object().unwrap().clone());
        Ok(self
            .manager
            .dispatch_tool_call(&ctx, call, CancellationToken::new())
            .await?
            .result
            .await?)
    }

    async fn approve(&self, revise: bool, accept: bool) -> Result<CallToolResult> {
        let mut ctx = self.ctx.clone();
        ctx.tool_call_request_id = Some(uuid::Uuid::new_v4().to_string());
        let mut pending = self
            .manager
            .dispatch_tool_call(
                &ctx,
                CallToolRequestParams::new("workspaceplan__approve"),
                CancellationToken::new(),
            )
            .await?;
        let future = tokio::spawn(pending.result);
        let message = tokio::time::timeout(
            Duration::from_secs(3),
            pending.action_required_stream.as_mut().unwrap().next(),
        )
        .await?
        .unwrap();
        let id = match &message.content[0] {
            MessageContent::ActionRequired(action) => match &action.data {
                ActionRequiredData::Elicitation { id, .. } => id.clone(),
                _ => panic!("Expected human elicitation"),
            },
            _ => panic!("Expected human elicitation"),
        };
        let bridge = self.sessions.action_required();
        assert!(bridge.claim_response("other-session", &id).await.is_err());
        if revise {
            let mut plan = load_plan(self.root.path())?;
            plan.request_revision("Changed while user was deciding")?;
            plan.save(self.root.path())?;
        }
        bridge
            .claim_response(&self.ctx.session_id, &id)
            .await?
            .submit(ElicitationOutcome::Accept(json!({"approve":accept})))?;
        let result = future.await?;
        assert!(bridge
            .claim_response(&self.ctx.session_id, &id)
            .await
            .is_err());
        Ok(result?)
    }
}

#[tokio::test]
async fn authority_forged_approval_direct_call_and_stale_human_decision_denied() {
    let fixture = Fixture::new().await;
    fixture.draft();
    assert!(fixture
        .call("shell", json!({"command":"touch denied"}))
        .await
        .is_err());
    assert!(!fixture.root.path().join("denied").exists());
    let mut forged = load_plan(fixture.root.path()).unwrap();
    forged.approve("desktop-user").unwrap();
    forged.save(fixture.root.path()).unwrap();
    assert!(fixture
        .call("shell", json!({"command":"touch forged"}))
        .await
        .is_err());
    let mut direct = fixture.ctx.clone();
    direct.tool_call_request_id = None;
    let result = fixture
        .manager
        .dispatch_tool_call(
            &direct,
            CallToolRequestParams::new("workspaceplan__approve"),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .result
        .await;
    assert!(result.unwrap_err().message.contains("interactive human"));
    assert!(fixture.approve(false, false).await.is_err());
    assert!(fixture.approve(true, true).await.is_err());
    assert!(!fixture.root.path().join("forged").exists());
}

#[tokio::test]
async fn authority_dispatch_rechecks_revocation_and_session_binding() {
    let fixture = Fixture::new().await;
    fixture.draft();
    fixture.approve(false, true).await.unwrap();
    let queued = fixture
        .manager
        .dispatch_tool_call(
            &fixture.ctx,
            CallToolRequestParams::new("shell")
                .with_arguments(rmcp::object!({"command":"touch stale"})),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let approved_bytes =
        std::fs::read(fixture.root.path().join(".esi/workspace-plan.json")).unwrap();
    let revised = fixture.call("workspaceplan__save_draft", json!({
        "title":"Revised plan", "description":"Changed implementation", "architecture_notes":"One file",
        "requirements":[{"id":"R1","description":"New requirement","acceptance_criteria":["New file"],"priority":"must"}],
        "tasks":[{"id":"T1","title":"New task","description":"Changed task"}]
    })).await.unwrap();
    assert_ne!(revised.is_error, Some(true));
    assert!(queued.result.await.is_err());
    assert!(!fixture.root.path().join("stale").exists());
    // Even restoring a previously valid approval JSON cannot restore the
    // revoked host receipt. This write represents a forgery, not a tool grant.
    std::fs::write(
        fixture.root.path().join(".esi/workspace-plan.json"),
        approved_bytes,
    )
    .unwrap();
    assert!(fixture
        .call("shell", json!({"command":"touch replayed"}))
        .await
        .is_err());
    let mut other = fixture.ctx.clone();
    other.working_dir = Some(fixture._data.path().into());
    assert!(fixture
        .manager
        .dispatch_tool_call(
            &other,
            CallToolRequestParams::new("workspaceplan__status"),
            CancellationToken::new()
        )
        .await
        .unwrap()
        .result
        .await
        .is_err());
}

#[tokio::test]
async fn authority_unknown_factory_cannot_connect_or_spoof_provider_exception() {
    let fixture = Fixture::new().await;
    for name in ["codex", "claude", "shell", "extensionmanager", "apps"] {
        assert!(fixture
            .manager
            .add_extension(platform(name), Some(fixture.root.path().into()), None, None)
            .await
            .is_err());
    }
    let client = Arc::new(
        crate::agents::platform_extensions::developer::DeveloperClient::new(
            fixture.manager.get_context().clone(),
        )
        .unwrap(),
    );
    fixture
        .manager
        .add_client(
            "developer".into(),
            platform("developer"),
            client,
            None,
            None,
        )
        .await;
    let error = fixture.call("tree", json!({"path":"."})).await.unwrap_err();
    assert!(error.to_string().contains("Untrusted tool registration"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn authority_contained_discovery_mutation_and_escape_regressions() {
    let fixture = Fixture::new().await;
    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret"), "host-only").unwrap();
    assert_ne!(
        fixture
            .call("tree", json!({"path":"."}))
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
    fixture.draft();
    fixture.approve(false, true).await.unwrap();
    assert_ne!(
        fixture.call("shell", json!({"command":"test -z \"$GOOSE_PATH_ROOT\" && test -z \"$ESI_WIKI_AUTHORIZATION\" && printf bounded > shell-ok"})).await.unwrap().is_error,
        Some(true)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("shell-ok")).unwrap(),
        "bounded"
    );
    assert_ne!(
        fixture
            .call("write", json!({"path":"src/file.txt","content":"before"}))
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
    assert_ne!(
        fixture
            .call(
                "edit",
                json!({"path":"src/file.txt","before":"before","after":"after"})
            )
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("src/file.txt")).unwrap(),
        "after"
    );
    for path in [
        "../outside".to_string(),
        ".esi/workspace-plan.json".into(),
        outside.path().join("bad").to_string_lossy().into_owned(),
    ] {
        assert!(fixture
            .call("write", json!({"path":path,"content":"bad"}))
            .await
            .is_err());
    }
    std::os::unix::fs::symlink(outside.path(), fixture.root.path().join("escape")).unwrap();
    let plan_before = std::fs::read(fixture.root.path().join(".esi/workspace-plan.json")).unwrap();
    for command in [
        "echo bad > .esi/workspace-plan.json".to_string(),
        "echo bad > escape/bad".into(),
        format!("cat '{}'", outside.path().join("secret").display()),
        "python3 -c 'import socket; socket.create_connection((\"1.1.1.1\",443),1)'".into(),
    ] {
        assert_eq!(
            fixture
                .call("shell", json!({"command":command}))
                .await
                .unwrap()
                .is_error,
            Some(true)
        );
    }
    assert_eq!(
        plan_before,
        std::fs::read(fixture.root.path().join(".esi/workspace-plan.json")).unwrap()
    );
    assert!(!outside.path().join("bad").exists());
    std::fs::hard_link(
        outside.path().join("secret"),
        fixture.root.path().join("hardlink"),
    )
    .unwrap();
    assert!(fixture
        .call("shell", json!({"command":"echo bad > hardlink"}))
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret")).unwrap(),
        "host-only"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn authority_local_process_timeout_output_and_cancellation_are_bounded() {
    let fixture = Fixture::new().await;
    fixture.draft();
    fixture.approve(false, true).await.unwrap();
    let timeout = fixture
        .call(
            "shell",
            json!({"command":"(sleep 2; touch timeout-survived) & wait", "timeout_secs":1}),
        )
        .await
        .unwrap_err();
    assert!(timeout.to_string().contains("timed out"));
    let output = fixture
        .call("shell", json!({"command":"yes excessive"}))
        .await
        .unwrap_err();
    assert!(output.to_string().contains("output exceeded"));
    let cancel = CancellationToken::new();
    let call = fixture
        .manager
        .dispatch_tool_call(
            &fixture.ctx,
            CallToolRequestParams::new("shell").with_arguments(
                rmcp::object!({"command":"(sleep 2; touch cancel-survived) & wait"}),
            ),
            cancel.clone(),
        )
        .await
        .unwrap();
    let running = tokio::spawn(call.result);
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    assert!(tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(!fixture.root.path().join("timeout-survived").exists());
    assert!(!fixture.root.path().join("cancel-survived").exists());
}

#[tokio::test]
async fn authority_queued_extension_removal_denies_execution() {
    let fixture = Fixture::new().await;
    let queued = fixture
        .manager
        .dispatch_tool_call(
            &fixture.ctx,
            CallToolRequestParams::new("workspaceplan__status"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    fixture
        .manager
        .remove_extension("workspaceplan")
        .await
        .unwrap();
    assert!(queued
        .result
        .await
        .unwrap_err()
        .message
        .contains("Untrusted tool registration"));
}

#[tokio::test]
async fn authority_final_dispatch_respects_user_deny_and_interactive_permission() {
    use crate::config::permission::PermissionLevel;
    let fixture = Fixture::new().await;
    fixture
        .permissions
        .update_user_permission("developer__tree", PermissionLevel::NeverAllow);
    assert!(fixture
        .call("tree", json!({"path":"."}))
        .await
        .unwrap_err()
        .to_string()
        .contains("explicitly forbidden"));
    fixture
        .permissions
        .update_user_permission("developer__tree", PermissionLevel::AskBefore);
    let mut direct = fixture.ctx.clone();
    direct.tool_call_request_id = None;
    let call = fixture
        .manager
        .dispatch_tool_call(
            &direct,
            CallToolRequestParams::new("tree").with_arguments(rmcp::object!({"path":"."})),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(call
        .result
        .await
        .unwrap_err()
        .message
        .contains("interactive human permission"));
}
