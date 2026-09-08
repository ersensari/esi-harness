use super::*;
use crate::agents::extension_manager::get_parameter_names;
use crate::agents::reply_parts::is_tool_visible_to_app;
use crate::config::permission::PermissionLevel;
use goose_sdk_types::custom_requests::{ToolListItem, ToolPermissionLevel};
use rmcp::model::CallToolRequestParams;

impl GooseAcpAgent {
    pub(super) async fn on_get_tools(
        &self,
        req: GetToolsRequest,
    ) -> Result<GetToolsResponse, agent_client_protocol::Error> {
        let session_id = &req.session_id;
        let agent = self.get_session_agent(&req.session_id).await?;
        let goose_mode = agent.goose_mode().await;
        let permission_manager = self.permission_manager();

        let mut tools: Vec<ToolListItem> = agent
            .list_tools(session_id, req.extension_name)
            .await
            .into_iter()
            .map(|tool| {
                let permission = permission_manager
                    .get_user_permission(&tool.name)
                    .or_else(|| {
                        if goose_mode == GooseMode::SmartApprove {
                            permission_manager.get_smart_approve_permission(&tool.name)
                        } else if goose_mode == GooseMode::Approve {
                            Some(PermissionLevel::AskBefore)
                        } else {
                            None
                        }
                    })
                    .map(|p| match p {
                        PermissionLevel::AlwaysAllow => ToolPermissionLevel::AlwaysAllow,
                        PermissionLevel::AskBefore => ToolPermissionLevel::AskBefore,
                        PermissionLevel::NeverAllow => ToolPermissionLevel::NeverAllow,
                    });
                ToolListItem {
                    name: tool.name.to_string(),
                    description: tool
                        .description
                        .as_ref()
                        .map(|d| d.as_ref().to_string())
                        .unwrap_or_default(),
                    parameters: get_parameter_names(&tool),
                    permission,
                    input_schema: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
                    output_schema: tool
                        .output_schema
                        .as_ref()
                        .map(|s| serde_json::to_value(s).unwrap_or(serde_json::Value::Null)),
                }
            })
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(GetToolsResponse { tools })
    }

    pub(super) async fn on_call_tool(
        &self,
        req: GooseToolCallRequest,
    ) -> Result<GooseToolCallResponse, agent_client_protocol::Error> {
        let session_id = &req.session_id;
        let agent = self.get_session_agent(&req.session_id).await?;
        let tools = agent.list_tools(session_id, None).await;

        let Some(tool) = tools.iter().find(|t| *t.name == req.name) else {
            return Err(agent_client_protocol::Error::invalid_params().data("tool not found"));
        };

        if !is_tool_visible_to_app(tool) {
            return Err(agent_client_protocol::Error::invalid_params()
                .data("tool is not visible to app clients"));
        }

        let arguments = match req.arguments {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            _ => {
                return Err(agent_client_protocol::Error::invalid_params()
                    .data("tool arguments must be an object"));
            }
        };

        let tool_call = {
            let mut params = CallToolRequestParams::new(req.name);
            if let Some(args) = arguments {
                params = params.with_arguments(args);
            }
            params
        };

        let session = self
            .session_manager
            .get_session(session_id, false)
            .await
            .map_err(|_| {
                agent_client_protocol::Error::resource_not_found(Some(session_id.to_string()))
                    .data(format!("Session not found: {}", session_id))
            })?;

        let ctx = crate::agents::ToolCallContext::new(
            session_id.clone(),
            Some(session.working_dir),
            None,
        );
        let tool_result = agent
            .extension_manager
            .dispatch_tool_call(&ctx, tool_call, CancellationToken::new())
            .await
            .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;

        let result = tool_result
            .result
            .await
            .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;

        let content = result
            .content
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;

        Ok(GooseToolCallResponse {
            content,
            structured_content: result.structured_content,
            is_error: result.is_error.unwrap_or(false),
            meta: result.meta.and_then(|m| serde_json::to_value(m).ok()),
        })
    }

    pub(super) async fn on_set_tool_permissions(
        &self,
        req: SetToolPermissionsRequest,
    ) -> Result<SetToolPermissionsResponse, agent_client_protocol::Error> {
        let permission_manager = self.permission_manager();
        for entry in &req.tool_permissions {
            let level = match entry.permission {
                ToolPermissionLevel::AlwaysAllow => PermissionLevel::AlwaysAllow,
                ToolPermissionLevel::AskBefore => PermissionLevel::AskBefore,
                ToolPermissionLevel::NeverAllow => PermissionLevel::NeverAllow,
            };
            permission_manager.update_user_permission(&entry.tool_name, level);
        }
        Ok(SetToolPermissionsResponse {})
    }
}

#[cfg(test)]
mod authority_tests {
    use super::*;
    use crate::agents::{AgentConfig, GoosePlatform};

    #[tokio::test]
    async fn authority_acp_app_calls_cannot_approve_or_mutate_without_receipt() {
        crate::config::Config::global()
            .set_param("ESI_AUTHORITY_TEST_INITIALIZED", true)
            .unwrap();
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let server = GooseAcpAgent::new(GooseAcpAgentOptions {
            provider_factory: Arc::new(|_, _, _, _| {
                Box::pin(async { Err(anyhow::anyhow!("No live provider in authority fixture")) })
            }),
            builtin_selection: AcpBuiltinSelection::default(),
            data_dir: data.path().into(),
            config_dir: data.path().into(),
            disable_session_naming: true,
            goose_platform: GoosePlatform::GooseDesktop,
            additional_source_roots: vec![],
            scheduler: None,
            session_cwd: None,
            active_prompt_runs: Default::default(),
        })
        .await
        .unwrap();
        let session = server
            .session_manager
            .create_session(
                workspace.path().into(),
                "authority ACP".into(),
                SessionType::Acp,
                GooseMode::Auto,
            )
            .await
            .unwrap();
        let agent = Arc::new(Agent::with_config(AgentConfig::new(
            server.session_manager.clone(),
            server.permission_manager.clone(),
            None,
            GooseMode::Auto,
            true,
            GoosePlatform::GooseDesktop,
        )));
        for name in ["developer", "workspaceplan", "controller"] {
            agent
                .extension_manager
                .add_extension(
                    crate::agents::ExtensionConfig::Platform {
                        name: name.into(),
                        description: String::new(),
                        display_name: None,
                        bundled: None,
                        available_tools: vec![],
                    },
                    Some(workspace.path().into()),
                    None,
                    Some(&session.id),
                )
                .await
                .unwrap();
        }
        server
            .register_acp_session(session.id.clone(), agent.clone())
            .await;
        let status = server
            .on_call_tool(GooseToolCallRequest {
                session_id: session.id.clone(),
                name: "workspaceplan__status".into(),
                arguments: serde_json::json!({}),
            })
            .await
            .unwrap();
        assert!(!status.is_error);
        for (name, args) in [
            ("shell", serde_json::json!({"command":"touch bypass"})),
            (
                "write",
                serde_json::json!({"path":"bypass","content":"bad"}),
            ),
            ("workspaceplan__approve", serde_json::json!({})),
        ] {
            let result = server
                .on_call_tool(GooseToolCallRequest {
                    session_id: session.id.clone(),
                    name: name.into(),
                    arguments: args,
                })
                .await;
            let error = result.unwrap_err();
            assert!(error.data.unwrap().to_string().contains("ESI authority"));
        }
        assert!(!workspace.path().join("bypass").exists());

        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "ACP fixture"],
            vec!["config", "user.email", "acp@example.invalid"],
            vec!["commit", "--allow-empty", "-m", "fixture"],
        ] {
            assert!(std::process::Command::new("git")
                .current_dir(workspace.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        }
        let mut plan =
            esi_workspace_plan::WorkspacePlan::new(workspace.path(), "ACP bound task").unwrap();
        plan.set_requirements(vec![esi_workspace_plan::Requirement {
            id: "R1".into(),
            description: "Bound file".into(),
            acceptance_criteria: vec!["Main unchanged".into()],
            priority: esi_workspace_plan::Priority::Must,
        }])
        .unwrap();
        plan.set_plan_content(
            "Write in owned worktree",
            "Local",
            vec![esi_workspace_plan::PlannedTask {
                id: "T1".into(),
                title: "Write".into(),
                description: "Owned file".into(),
                status: esi_workspace_plan::PlannedTaskStatus::Pending,
            }],
        )
        .unwrap();
        plan.save(workspace.path()).unwrap();
        let prepared = crate::workspace_tool_authority::native_plan_review(&server.session_manager, serde_json::json!({
            "action":"prepare", "session_id":session.id, "hash":plan.content_hash(), "storage_revision":plan.storage_revision()
        })).await.unwrap();
        crate::workspace_tool_authority::native_plan_review(&server.session_manager, serde_json::json!({
            "action":"complete", "session_id":session.id, "token":prepared["token"], "approve":true
        })).await.unwrap();
        let service = esi_development::ControllerService::new(
            workspace.path(),
            server.session_manager.controller_data_dir(),
            session.id.clone(),
        )
        .unwrap();
        let validators = vec![esi_development::ValidationCommand {
            id: "test".into(),
            category: esi_development::ValidationCategory::TargetedTests,
            program: "/bin/true".into(),
            arguments: vec![],
            required: true,
        }];
        let denied = server.on_call_tool(GooseToolCallRequest {
            session_id:session.id.clone(), name:"controller__start".into(),
            arguments:serde_json::json!({"task_id":"T1", "request_id":"app-start", "validators":validators})
        }).await.unwrap();
        assert!(
            denied.is_error,
            "ACP app cannot synthesize human execution approval"
        );
        let esi_development::StartPreparation::Ready(prepared) = service
            .prepare_start(
                "T1",
                "host-start",
                esi_development::ValidationPlan::new(validators).unwrap(),
            )
            .unwrap()
        else {
            panic!();
        };
        let state = crate::workspace_tool_authority::controller_binding::commit_start(
            &server.session_manager,
            &session.id,
            workspace.path(),
            service,
            *prepared,
            true,
        )
        .await
        .unwrap();
        let write = server
            .on_call_tool(GooseToolCallRequest {
                session_id: session.id.clone(),
                name: "write".into(),
                arguments: serde_json::json!({"path":"acp-owned.txt", "content":"owned"}),
            })
            .await
            .unwrap();
        assert!(!write.is_error);
        assert!(state
            .worktree()
            .unwrap()
            .identity
            .worktree_path
            .join("acp-owned.txt")
            .exists());
        assert!(!workspace.path().join("acp-owned.txt").exists());
        let other = server
            .session_manager
            .create_session(
                workspace.path().into(),
                "other".into(),
                SessionType::Acp,
                GooseMode::Auto,
            )
            .await
            .unwrap();
        server.register_acp_session(other.id.clone(), agent).await;
        assert!(server
            .on_call_tool(GooseToolCallRequest {
                session_id: other.id,
                name: "write".into(),
                arguments: serde_json::json!({"path":"wrong-task.txt", "content":"bad"})
            })
            .await
            .is_err());
        assert!(!workspace.path().join("wrong-task.txt").exists());
    }
}
