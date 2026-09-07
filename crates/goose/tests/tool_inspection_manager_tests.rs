use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::config::GooseMode;
use goose::conversation::message::{Message, ToolRequest};
use goose::tool_inspection::{
    InspectionAction, InspectionResult, ToolInspectionManager, ToolInspector,
};

struct MockInspectorOk {
    name: &'static str,
    results: Vec<InspectionResult>,
}

struct MockInspectorErr {
    name: &'static str,
    required: bool,
    enabled: bool,
}

#[async_trait]
impl ToolInspector for MockInspectorOk {
    fn name(&self) -> &'static str {
        self.name
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn inspect(
        &self,
        _session_id: &str,
        _tool_requests: &[ToolRequest],
        _messages: &[Message],
        _goose_mode: GooseMode,
    ) -> Result<Vec<InspectionResult>> {
        Ok(self.results.clone())
    }
}

#[async_trait]
impl ToolInspector for MockInspectorErr {
    fn is_required(&self) -> bool {
        self.required
    }
    fn is_enabled(&self) -> bool {
        self.enabled
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn inspect(
        &self,
        _session_id: &str,
        _tool_requests: &[ToolRequest],
        _messages: &[Message],
        _goose_mode: GooseMode,
    ) -> Result<Vec<InspectionResult>> {
        Err(anyhow!("simulated failure"))
    }
}

#[tokio::test]
async fn test_inspect_tools_aggregates_and_handles_errors() {
    // Arrange: create a manager with one successful and one failing inspector
    let ok_results = vec![
        InspectionResult {
            tool_request_id: "req_1".to_string(),
            action: InspectionAction::Allow,
            reason: "looks safe".to_string(),
            confidence: 0.95,
            inspector_name: "ok".to_string(),
            finding_id: None,
        },
        InspectionResult {
            tool_request_id: "req_2".to_string(),
            action: InspectionAction::RequireApproval(Some("double check".to_string())),
            reason: "needs user confirmation".to_string(),
            confidence: 0.7,
            inspector_name: "ok".to_string(),
            finding_id: Some("FND-123".to_string()),
        },
    ];

    let mut manager = ToolInspectionManager::new();
    manager.add_inspector(Box::new(MockInspectorOk {
        name: "ok",
        results: ok_results.clone(),
    }));
    manager.add_inspector(Box::new(MockInspectorErr {
        name: "err",
        required: false,
        enabled: true,
    }));

    // No specific input is required for this aggregation behavior
    let tool_requests: Vec<ToolRequest> = vec![];
    let messages: Vec<Message> = vec![];

    // Act
    let results = manager
        .inspect_tools(
            goose_test_support::TEST_SESSION_ID,
            &tool_requests,
            &messages,
            GooseMode::Approve,
        )
        .await
        .expect("inspect_tools should not fail when one inspector errors");

    // Assert: results from the successful inspector are returned; failing inspector is ignored
    assert_eq!(
        results.len(),
        2,
        "Should aggregate results from successful inspectors only"
    );
    // Also verify inspector_names() order/presence
    let names = manager.inspector_names();
    assert_eq!(
        names,
        vec!["ok", "err"],
        "Inspector names should reflect registration order"
    );

    // Verify that specific actions are preserved
    assert!(results
        .iter()
        .any(|r| matches!(r.action, InspectionAction::Allow)));
    assert!(results
        .iter()
        .any(|r| matches!(r.action, InspectionAction::RequireApproval(_))));
}

#[tokio::test]
async fn required_gate_failure_denies_every_request_regardless_of_allow_order_or_mode() {
    use goose::permission::permission_judge::PermissionCheckResult;
    use goose::tool_inspection::apply_inspection_results_to_permissions;
    use rmcp::model::CallToolRequestParams;
    let requests: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|id| ToolRequest {
            id: id.into(),
            tool_call: Ok(CallToolRequestParams::new("fixture__write")),
            metadata: None,
            tool_meta: None,
        })
        .collect();
    for enabled in [true, false] {
        for failure_first in [true, false] {
            for mode in [GooseMode::Auto, GooseMode::Approve, GooseMode::SmartApprove] {
                let mut manager = ToolInspectionManager::new();
                let required: Box<dyn ToolInspector> = Box::new(MockInspectorErr {
                    name: "trusted_fixture_gate",
                    required: true,
                    enabled,
                });
                let allow: Box<dyn ToolInspector> = Box::new(MockInspectorOk {
                    name: "allow",
                    results: requests
                        .iter()
                        .map(|r| InspectionResult {
                            tool_request_id: r.id.clone(),
                            action: InspectionAction::Allow,
                            reason: "preapproved".into(),
                            confidence: 1.0,
                            inspector_name: "allow".into(),
                            finding_id: None,
                        })
                        .collect(),
                });
                let inspectors = if failure_first {
                    [required, allow]
                } else {
                    [allow, required]
                };
                for inspector in inspectors {
                    manager.add_inspector(inspector);
                }
                let results = manager
                    .inspect_tools("fixture", &requests, &[], mode)
                    .await
                    .unwrap();
                assert_eq!(
                    results
                        .iter()
                        .filter(|r| r.action == InspectionAction::Deny)
                        .count(),
                    2
                );
                assert!(results
                    .iter()
                    .all(|r| !r.reason.contains("simulated failure")));
                let permissions = apply_inspection_results_to_permissions(
                    PermissionCheckResult {
                        approved: requests.clone(),
                        needs_approval: vec![],
                        denied: vec![],
                    },
                    &results,
                );
                assert!(permissions.approved.is_empty());
                assert!(permissions.needs_approval.is_empty());
                assert_eq!(permissions.denied.len(), 2);
            }
        }
    }
}
