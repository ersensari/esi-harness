use esi_development::{
    Brief, CompletionApproval, DevelopmentError, DevelopmentStage, DevelopmentState,
    ImplementationPlan, RepairPolicy, ReviewDecision, ValidationCategory, ValidationCommand,
    ValidationPlan, WorktreeReadyApproval,
};
use esi_development_visualizer::{
    app_html, DevelopmentLoopView, DevelopmentVisualizerServer, ShowDevelopmentLoopParams,
    VisualizerStatus, DEVELOPMENT_LOOP_RESOURCE_URI, MCP_APPS_MIME_TYPE,
};
use esi_workspace::{CleanupApproval, LifecycleState, SessionId, WorkspaceManager};
use rmcp::{handler::server::wrapper::Parameters, model::ServerInfo, ServerHandler};
use serde_json::Value;
use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

const RUN_ID: &str = "team-smoke-run";
const APPROVER: &str = "team-member@example.invalid";

struct TeamEnvironment {
    _root: Option<TempDir>,
    path: PathBuf,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "core.hooksPath=", "-c", "credential.helper="])
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command(
    id: &str,
    category: ValidationCategory,
    program: &str,
    arguments: &[&str],
) -> ValidationCommand {
    ValidationCommand {
        id: id.to_string(),
        category,
        program: program.to_string(),
        arguments: arguments.iter().map(|value| value.to_string()).collect(),
        required: true,
    }
}

fn clean_team_environment() -> TeamEnvironment {
    let (root, fixture_root) = match std::env::var("ESI_TEAM_SMOKE_ROOT") {
        Ok(path) => (None, PathBuf::from(path)),
        Err(_) => {
            let root = TempDir::new().unwrap();
            let path = root.path().to_path_buf();
            for directory in ["home", "config", "cache", "data", "tmp"] {
                fs::create_dir_all(path.join(directory)).unwrap();
            }
            std::env::set_var("ESI_TEAM_SMOKE_ROOT", &path);
            std::env::set_var("HOME", path.join("home"));
            std::env::set_var("XDG_CONFIG_HOME", path.join("config"));
            std::env::set_var("XDG_CACHE_HOME", path.join("cache"));
            std::env::set_var("XDG_DATA_HOME", path.join("data"));
            std::env::set_var("FORGELOOP_BASE_URL", "http://127.0.0.1:9");
            std::env::set_var("LITELLM_BASE_URL", "http://127.0.0.1:9");
            (Some(root), path)
        }
    };
    for name in ["HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME"] {
        let value = PathBuf::from(std::env::var(name).unwrap());
        assert!(
            value.starts_with(&fixture_root),
            "{name} escaped fixture root"
        );
    }
    assert!(!fixture_root.join("home/.codex").exists());
    assert!(!fixture_root.join("home/.claude").exists());
    assert_eq!(
        std::env::var("FORGELOOP_BASE_URL").unwrap(),
        "http://127.0.0.1:9"
    );
    assert_eq!(
        std::env::var("LITELLM_BASE_URL").unwrap(),
        "http://127.0.0.1:9"
    );
    let private_endpoint = SocketAddr::from(([127, 0, 0, 1], 9));
    assert!(
        TcpStream::connect_timeout(&private_endpoint, Duration::from_millis(100)).is_err(),
        "private endpoint sentinel unexpectedly accepted a connection"
    );
    TeamEnvironment {
        _root: root,
        path: fixture_root,
    }
}

fn assert_provider_contracts() {
    let profiles = repository_root().join("provider-profiles");
    let manifest: Value =
        serde_json::from_slice(&fs::read(profiles.join("manifest.json")).unwrap()).unwrap();
    let team = manifest["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| profile["id"] == "team")
        .unwrap();
    let providers = team["providers"].as_array().unwrap();
    assert_eq!(team["default_provider"], "chatgpt_codex");
    assert!(providers.iter().any(|provider| {
        provider["id"] == "chatgpt_codex"
            && provider["role"] == "primary"
            && provider["authentication"] == "browser_oauth"
            && provider["credential_owner"] == "provider_client"
    }));
    assert!(providers.iter().any(|provider| {
        provider["id"] == "claude-acp"
            && provider["role"] == "alternative"
            && provider["authentication"] == "official_client"
            && provider["credential_owner"] == "official_claude_client"
    }));
    assert_eq!(team["allow_private_litellm"], false);
    assert_eq!(team["allow_private_forgeloop"], false);

    let profile_content = ["manifest.json", "team.yaml", "operator.yaml"]
        .into_iter()
        .map(|name| fs::read_to_string(profiles.join(name)).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    for forbidden in [
        "litellm_host:",
        "litellm_api_key:",
        "forgeloop_server:",
        "forgeloop_server_bearer_token:",
        "/.codex/",
        "/.claude/",
        "access_token:",
        "refresh_token:",
        "api_token:",
    ] {
        assert!(!profile_content.contains(forbidden), "found {forbidden}");
    }
}

fn initialize_repository(root: &Path) -> PathBuf {
    let repository = root.join("source");
    fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "--initial-branch=main"]);
    git(&repository, &["config", "user.name", "ESI Team Smoke"]);
    git(
        &repository,
        &["config", "user.email", "team-smoke@example.invalid"],
    );
    fs::write(repository.join("README.md"), "# Clean team fixture\n").unwrap();
    git(&repository, &["add", "README.md"]);
    git(
        &repository,
        &["commit", "-m", "Initialize clean team fixture"],
    );
    repository
}

#[tokio::test]
async fn clean_team_install_completes_local_workflow_without_private_services() {
    let environment = clean_team_environment();
    assert_provider_contracts();

    let fixture = TempDir::new_in(&environment.path).unwrap();
    let repository = initialize_repository(fixture.path());
    let manager = WorkspaceManager::new(
        fixture.path().join("state/workspaces"),
        fixture.path().join("worktrees"),
    );
    let session_id = SessionId::new("clean-team-session").unwrap();
    let initial = manager
        .create(&repository, session_id.clone(), "HEAD")
        .unwrap();
    assert_ne!(
        initial.record.identity.main_worktree,
        initial.record.identity.worktree_path
    );

    let worktree = initial.record.identity.worktree_path.clone();
    let main_readme = fs::read_to_string(repository.join("README.md")).unwrap();
    let validation = ValidationPlan::new(vec![
        command(
            "scope",
            ValidationCategory::Scope,
            "/bin/sh",
            &[
                "-c",
                "test \"$PWD\" != \"$(git worktree list --porcelain | sed -n '1s/^worktree //p')\"",
            ],
        ),
        command(
            "syntax",
            ValidationCategory::Syntax,
            "/bin/sh",
            &["-n", "implementation.sh"],
        ),
        command(
            "private-boundary",
            ValidationCategory::StaticPolicy,
            "/bin/sh",
            &[
                "-c",
                "! grep -Eri 'forgeloop|litellm|access[_-]?token|api[_-]?key' implementation.sh",
            ],
        ),
        command(
            "implementation",
            ValidationCategory::TargetedTests,
            "/bin/sh",
            &["implementation.sh"],
        ),
        command(
            "repository",
            ValidationCategory::BroadTests,
            "/bin/sh",
            &["-c", "git diff --check && test -f README.md"],
        ),
    ])
    .unwrap();
    let mut state = DevelopmentState::new(RUN_ID, RepairPolicy::default()).unwrap();
    state
        .record_brief(Brief {
            objective: "Implement a deterministic local team change".to_string(),
            acceptance_criteria: vec![
                "The managed worktree contains a passing implementation".to_string(),
                "Private provider services remain unused".to_string(),
            ],
        })
        .unwrap();
    state
        .record_plan(ImplementationPlan {
            summary: "Implement, validate, review, approve, and clean up locally".to_string(),
            validation,
        })
        .unwrap();

    let mismatched_worktree_approval = WorktreeReadyApproval {
        run_id: RUN_ID.to_string(),
        repository_id: initial.record.identity.repository_id.clone(),
        worktree_path: worktree.clone(),
        snapshot_id: "wrong-snapshot".to_string(),
        approved_by: APPROVER.to_string(),
    };
    assert_eq!(
        state.approve_worktree(&initial, mismatched_worktree_approval),
        Err(DevelopmentError::WorktreeApprovalMismatch)
    );
    state
        .approve_worktree(
            &initial,
            WorktreeReadyApproval {
                run_id: RUN_ID.to_string(),
                repository_id: initial.record.identity.repository_id.clone(),
                worktree_path: worktree.clone(),
                snapshot_id: initial.snapshot_id.clone(),
                approved_by: APPROVER.to_string(),
            },
        )
        .unwrap();
    state.begin_implementation().unwrap();

    fs::write(
        worktree.join("implementation.sh"),
        "#!/bin/sh\nset -eu\ntest implemented = broken\n",
    )
    .unwrap();
    let failed_inspection = manager.inspect(&repository, &session_id).unwrap();
    let failed_run = state.validate(&failed_inspection).unwrap();
    assert!(!failed_run.passed);
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::Repair);

    fs::write(
        worktree.join("implementation.sh"),
        "#!/bin/sh\nset -eu\ntest implemented = implemented\n",
    )
    .unwrap();
    let repaired_inspection = manager.inspect(&repository, &session_id).unwrap();
    let repaired_run = state.validate(&repaired_inspection).unwrap();
    assert!(repaired_run.passed);
    assert_eq!(state.stage(), DevelopmentStage::Review);
    state
        .record_review(
            &repaired_inspection,
            ReviewDecision::Approved {
                summary: "All acceptance criteria and deterministic evidence passed".to_string(),
            },
        )
        .unwrap();

    assert_eq!(
        state.approve_completion(
            &repaired_inspection,
            CompletionApproval {
                run_id: "wrong-run".to_string(),
                snapshot_id: repaired_inspection.snapshot_id.clone(),
                approved_by: APPROVER.to_string(),
            },
        ),
        Err(DevelopmentError::ApprovalMismatch)
    );
    state
        .approve_completion(
            &repaired_inspection,
            CompletionApproval {
                run_id: RUN_ID.to_string(),
                snapshot_id: repaired_inspection.snapshot_id.clone(),
                approved_by: APPROVER.to_string(),
            },
        )
        .unwrap();
    assert_eq!(state.stage(), DevelopmentStage::Completed);
    assert_eq!(
        fs::read_to_string(repository.join("README.md")).unwrap(),
        main_readme
    );

    let state_path = fixture.path().join("state/development.json");
    state.save(&state_path).unwrap();
    let view = DevelopmentLoopView::load(&state_path).unwrap();
    assert_eq!(view.status, VisualizerStatus::Completed);
    assert_eq!(view.current_stage.as_deref(), Some("completed"));
    assert_eq!(view.validation_evidence.len(), 9);
    assert_eq!(view.fingerprints.len(), 1);
    assert_eq!(view.worktree.unwrap().changed_files, ["implementation.sh"]);
    assert!(view.approvals.iter().any(|approval| {
        approval.gate_id == "worktree_ready"
            && approval.state == "completed"
            && approval.approved_by.as_deref() == Some(APPROVER)
    }));
    assert!(view.approvals.iter().any(|approval| {
        approval.gate_id == "completion"
            && approval.state == "completed"
            && approval.approved_by.as_deref() == Some(APPROVER)
    }));

    let visualizer = DevelopmentVisualizerServer::new();
    let server_info: ServerInfo = visualizer.get_info();
    assert_eq!(server_info.server_info.name, "esi-development-visualizer");
    assert!(server_info.capabilities.resources.is_some());
    assert!(server_info.capabilities.tools.is_some());
    let tool_result = visualizer
        .show_development_loop(Parameters(ShowDevelopmentLoopParams {
            state_path: state_path.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(
        tool_result.structured_content.unwrap()["status"],
        "completed"
    );
    assert_eq!(
        tool_result
            .meta
            .unwrap()
            .0
            .get("ui")
            .and_then(|ui| ui.get("resourceUri"))
            .and_then(Value::as_str),
        Some(DEVELOPMENT_LOOP_RESOURCE_URI)
    );
    assert_eq!(MCP_APPS_MIME_TYPE, "text/html;profile=mcp-app");
    assert!(app_html().contains("ui/notifications/tool-result"));
    assert!(!app_html().contains("tools/call"));

    git(&worktree, &["add", "implementation.sh"]);
    git(&worktree, &["commit", "-m", "Implement fixture change"]);
    let clean = manager.inspect(&repository, &session_id).unwrap();
    assert!(!clean.dirty);
    let cleanup_request = manager.prepare_cleanup(&repository, &session_id).unwrap();
    let cleaned = manager
        .cleanup(
            &repository,
            CleanupApproval {
                request: cleanup_request,
                delete_branch: false,
            },
        )
        .unwrap();
    assert_eq!(cleaned.state, LifecycleState::Cleaned);
    assert!(!worktree.exists());
}
