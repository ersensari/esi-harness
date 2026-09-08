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

#[tokio::test]
async fn controller_factory_requires_human_start_and_dispatches_real_validation() {
    let fixture = Fixture::new().await;
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Controller test"],
        vec!["config", "user.email", "controller@example.invalid"],
    ] {
        assert!(std::process::Command::new("git")
            .current_dir(fixture.root.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    std::fs::write(fixture.root.path().join("result.txt"), "original\n").unwrap();
    for args in [vec!["add", "result.txt"], vec!["commit", "-m", "fixture"]] {
        assert!(std::process::Command::new("git")
            .current_dir(fixture.root.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    fixture.draft();
    fixture.approve(false, true).await.unwrap();
    fixture
        .manager
        .add_extension(
            platform("controller"),
            Some(fixture.root.path().into()),
            None,
            Some(&fixture.ctx.session_id),
        )
        .await
        .unwrap();
    let args = json!({"task_id":"T1", "request_id":"start", "validators":[{
        "id":"check", "category":"targeted_tests", "program":"/bin/sh",
        "arguments":["-c","test \"$(cat result.txt)\" = original"], "required":true
    }]});
    let mut direct = fixture.ctx.clone();
    direct.tool_call_request_id = None;
    let result = fixture
        .manager
        .dispatch_tool_call(
            &direct,
            CallToolRequestParams::new("controller__start")
                .with_arguments(args.as_object().unwrap().clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .result
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let mut forged = args.clone();
    forged["approved_by"] = json!("desktop-user");
    assert_eq!(
        fixture
            .call("controller__start", forged)
            .await
            .unwrap()
            .is_error,
        Some(true)
    );

    let mut ctx = fixture.ctx.clone();
    ctx.tool_call_request_id = Some(uuid::Uuid::new_v4().to_string());
    let mut pending = fixture
        .manager
        .dispatch_tool_call(
            &ctx,
            CallToolRequestParams::new("controller__start")
                .with_arguments(args.as_object().unwrap().clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let future = tokio::spawn(pending.result);
    let message = tokio::time::timeout(
        Duration::from_secs(10),
        pending.action_required_stream.as_mut().unwrap().next(),
    )
    .await
    .unwrap()
    .unwrap();
    let id = match &message.content[0] {
        MessageContent::ActionRequired(action) => match &action.data {
            ActionRequiredData::Elicitation { id, .. } => id,
            _ => panic!(),
        },
        _ => panic!(),
    };
    fixture
        .sessions
        .action_required()
        .claim_response(&fixture.ctx.session_id, id)
        .await
        .unwrap()
        .submit(ElicitationOutcome::Accept(json!({"approve":true})))
        .unwrap();
    let result = future.await.unwrap().unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let parse = |result: CallToolResult| -> esi_development::DevelopmentState {
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let value = serde_json::to_value(result).unwrap();
        serde_json::from_str(value["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let state = parse(result);
    assert_eq!(state.stage(), esi_development::DevelopmentStage::Implement);
    assert_ne!(
        state.worktree().unwrap().identity.worktree_path,
        fixture.root.path()
    );
    let replay = parse(fixture.call("controller__start", args).await.unwrap());
    assert_eq!(replay, state);
    let validated = parse(
        fixture
            .call(
                "controller__validate",
                json!({"task_id":"T1", "request_id":"validate"}),
            )
            .await
            .unwrap(),
    );
    assert_eq!(validated.stage(), esi_development::DevelopmentStage::Review);
    assert!(validated.validation_runs().last().unwrap().passed);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("result.txt")).unwrap(),
        "original\n"
    );
    let mut changed = load_plan(fixture.root.path()).unwrap();
    changed.request_revision("source changed").unwrap();
    changed.save(fixture.root.path()).unwrap();
    assert_eq!(
        fixture
            .call(
                "controller__resume",
                json!({"task_id":"T1", "request_id":"stale"})
            )
            .await
            .unwrap()
            .is_error,
        Some(true)
    );
}

#[tokio::test]
async fn workspace_plan_template_factory_dispatch_does_not_grant_execution_approval() {
    let fixture = Fixture::new().await;
    let result = fixture
        .call(
            "workspaceplan__create_template",
            json!({
                "template":"small_change", "title":"Scoped fix", "objective":"Fix one behavior"
            }),
        )
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    let plan = WorkspacePlan::load(fixture.root.path()).unwrap().unwrap();
    assert_eq!(plan.tasks().len(), 3);
    assert!(!plan.is_implementation_allowed());
    assert!(require_receipt(fixture.root.path()).is_err());
    assert!(fixture
        .call("shell", json!({"command":"touch must-not-exist"}))
        .await
        .is_err());
    assert!(!fixture.root.path().join("must-not-exist").exists());
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

#[tokio::test]
async fn native_plan_review_exact_snapshot_cancel_replay_cross_session_and_expiry() {
    let fixture = Fixture::new().await;
    fixture.draft();
    let plan = WorkspacePlan::load(fixture.root.path()).unwrap().unwrap();
    let prepare = json!({"action":"prepare", "session_id":fixture.ctx.session_id,
        "hash":plan.content_hash(), "storage_revision":plan.storage_revision()});
    let mut wrong = prepare.clone();
    wrong["hash"] = json!("wrong");
    assert!(native_plan_review(&fixture.sessions, wrong).await.is_err());
    let review = native_plan_review(&fixture.sessions, prepare.clone())
        .await
        .unwrap();
    assert_eq!(review["scope"]["hash"], plan.content_hash());
    assert!(require_receipt(fixture.root.path()).is_err());
    let complete = json!({"action":"complete", "session_id":fixture.ctx.session_id,
        "token": review["token"], "approve":true});
    let other = fixture
        .sessions
        .create_session(
            fixture.root.path().into(),
            "other".into(),
            SessionType::User,
            GooseMode::Auto,
        )
        .await
        .unwrap();
    let mut cross = complete.clone();
    cross["session_id"] = json!(other.id);
    assert!(native_plan_review(&fixture.sessions, cross).await.is_err());
    let mut cancel = complete.clone();
    cancel["approve"] = json!(false);
    assert_eq!(
        native_plan_review(&fixture.sessions, cancel).await.unwrap()["approved"],
        false
    );
    assert!(native_plan_review(&fixture.sessions, complete)
        .await
        .is_err());
    let expired = native_plan_review(&fixture.sessions, prepare.clone())
        .await
        .unwrap();
    super::plan_review::expire_for_test(expired["token"].as_str().unwrap()).await;
    assert!(native_plan_review(
        &fixture.sessions,
        json!({"action":"complete", "session_id":fixture.ctx.session_id,
        "token":expired["token"], "approve":true})
    )
    .await
    .is_err());
    let final_review = native_plan_review(&fixture.sessions, prepare)
        .await
        .unwrap();
    let finish = json!({"action":"complete", "session_id":fixture.ctx.session_id, "token":final_review["token"], "approve":true});
    assert_eq!(
        native_plan_review(&fixture.sessions, finish.clone())
            .await
            .unwrap()["approved"],
        true
    );
    assert_eq!(
        require_receipt(fixture.root.path()).unwrap().content_hash(),
        plan.content_hash()
    );
    assert!(native_plan_review(&fixture.sessions, finish).await.is_err());
}

#[tokio::test]
async fn native_plan_review_same_session_id_in_distinct_workspaces_does_not_evict_or_consume() {
    let first = Fixture::new().await;
    let second = Fixture::new().await;
    assert_eq!(first.ctx.session_id, second.ctx.session_id);
    let mut tokens = Vec::new();
    for fixture in [&first, &second] {
        fixture.draft();
        let plan = WorkspacePlan::load(fixture.root.path()).unwrap().unwrap();
        tokens.push(
            native_plan_review(
                &fixture.sessions,
                json!({
                    "action":"prepare", "session_id":fixture.ctx.session_id,
                    "hash":plan.content_hash(), "storage_revision":plan.storage_revision()
                }),
            )
            .await
            .unwrap()["token"]
                .clone(),
        );
    }
    let cancel_first = json!({"action":"complete", "session_id":first.ctx.session_id,
        "token":tokens[0], "approve":false});
    assert!(native_plan_review(&second.sessions, cancel_first.clone())
        .await
        .is_err());
    assert_eq!(
        native_plan_review(&first.sessions, cancel_first)
            .await
            .unwrap()["approved"],
        false
    );
    assert_eq!(
        native_plan_review(
            &second.sessions,
            json!({"action":"complete",
        "session_id":second.ctx.session_id, "token":tokens[1], "approve":false})
        )
        .await
        .unwrap()["approved"],
        false
    );
}

#[tokio::test]
async fn native_plan_review_stale_snapshot_cannot_approve_newer_content_or_mint_receipt() {
    let fixture = Fixture::new().await;
    fixture.draft();
    let mut plan = WorkspacePlan::load(fixture.root.path()).unwrap().unwrap();
    let review = native_plan_review(
        &fixture.sessions,
        json!({"action":"prepare", "session_id":fixture.ctx.session_id,
        "hash":plan.content_hash(), "storage_revision":plan.storage_revision()}),
    )
    .await
    .unwrap();
    plan.set_plan_content(
        "Newer scope",
        plan.architecture_notes().to_string(),
        plan.tasks().to_vec(),
    )
    .unwrap();
    plan.save(fixture.root.path()).unwrap();
    let bytes = std::fs::read(WorkspacePlan::plan_path(fixture.root.path())).unwrap();
    assert!(native_plan_review(
        &fixture.sessions,
        json!({"action":"complete", "session_id":fixture.ctx.session_id,
        "token":review["token"], "approve":true})
    )
    .await
    .is_err());
    assert_eq!(
        bytes,
        std::fs::read(WorkspacePlan::plan_path(fixture.root.path())).unwrap()
    );
    assert!(require_receipt(fixture.root.path()).is_err());
    assert!(fixture
        .call("workspaceplan__native_plan_review", json!({"approve":true}))
        .await
        .is_err());
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

#[tokio::test]
async fn extension_trust_dispatch_checks_grant_revocation_permissions_and_client_origin() {
    use crate::config::extensions::{set_extension, ExtensionEntry};
    use crate::config::permission::PermissionLevel;
    let fixture = Fixture::new().await;
    let config = platform("todo");
    set_extension(ExtensionEntry {
        enabled: true,
        config: config.clone(),
    });
    crate::extension_trust::set_trusted(Config::global(), "todo", true).unwrap();
    assert!(crate::extension_trust::is_trusted(
        Config::global(),
        &config
    ));
    fixture
        .manager
        .add_extension(
            config.clone(),
            Some(fixture.root.path().into()),
            None,
            Some(&fixture.ctx.session_id),
        )
        .await
        .unwrap();
    let result = fixture
        .call("todo__todo_write", json!({"content":"trusted execution"}))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    fixture
        .permissions
        .update_user_permission("todo__todo_write", PermissionLevel::NeverAllow);
    assert!(fixture
        .call("todo__todo_write", json!({"content":"denied"}))
        .await
        .is_err());
    fixture
        .permissions
        .update_user_permission("todo__todo_write", PermissionLevel::AlwaysAllow);
    let pending = fixture
        .manager
        .dispatch_tool_call(
            &fixture.ctx,
            CallToolRequestParams::new("todo__todo_write")
                .with_arguments(json!({"content":"queued"}).as_object().unwrap().clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    crate::extension_trust::set_trusted(Config::global(), "todo", false).unwrap();
    assert!(pending.result.await.is_err());
    crate::extension_trust::set_trusted(Config::global(), "todo", true).unwrap();
    let fake = Arc::new(
        crate::agents::platform_extensions::todo::TodoClient::new(
            fixture.manager.get_context().clone(),
        )
        .unwrap(),
    );
    fixture
        .manager
        .add_client("todo".into(), config, fake, None, None)
        .await;
    assert!(fixture
        .call("todo__todo_write", json!({"content":"spoof"}))
        .await
        .is_err());
    crate::extension_trust::set_trusted(Config::global(), "todo", false).unwrap();
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
            json!({"command":"(while ! test -e timeout-release; do sleep 0.01; done; touch timeout-survived) & wait", "timeout_secs":1}),
        )
        .await
        .unwrap_err();
    assert!(timeout.to_string().contains("timed out"));
    std::fs::write(fixture.root.path().join("timeout-release"), "release").unwrap();
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
                rmcp::object!({"command":"(while ! test -e cancel-release; do sleep 0.01; done; touch cancel-survived) & touch cancel-ready; wait"}),
            ),
            cancel.clone(),
        )
        .await
        .unwrap();
    let running = tokio::spawn(call.result);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fixture.root.path().join("cancel-ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert!(tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    std::fs::write(fixture.root.path().join("cancel-release"), "release").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
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
