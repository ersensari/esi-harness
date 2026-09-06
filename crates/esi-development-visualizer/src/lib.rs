use esi_development::{
    DevelopmentEventKind, DevelopmentStage, DevelopmentState, FailureCategory, HumanGateReason,
    ValidationOutcome,
};
use esi_workspace_plan::{PlannedTaskStatus, Priority, WorkspacePlan, WorkspacePlanStatus};
use ignore::WalkBuilder;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, InitializeResult,
        ListResourcesResult, MetaObject, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
        ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

pub const DEVELOPMENT_LOOP_RESOURCE_URI: &str = "ui://esi-development/run";
pub const MCP_APPS_MIME_TYPE: &str = "text/html;profile=mcp-app";
const MAX_TREE_ENTRIES: usize = 2_000;
const MAX_TREE_DEPTH: usize = 16;
const MAX_FILE_BYTES: usize = 512 * 1024;
const MAX_DIFF_BYTES: usize = 768 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceFileEntryView {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceSnapshotView {
    pub root: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub dirty: bool,
    pub changed_files: Vec<String>,
    pub files: Vec<WorkspaceFileEntryView>,
    pub tree_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceFileView {
    pub workspace_root: String,
    pub path: String,
    pub language: String,
    pub content: String,
    pub bytes: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceDiffView {
    pub workspace_root: String,
    pub path: Option<String>,
    pub diff: String,
    pub bytes: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VisualizerStatus {
    Empty,
    Running,
    Failed,
    Blocked,
    Completed,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StageHistoryItem {
    pub sequence: u64,
    pub stage: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorktreeView {
    pub repository_id: String,
    pub session_id: String,
    pub branch: String,
    pub worktree_path: String,
    pub head: String,
    pub snapshot_id: String,
    pub dirty: bool,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ValidationEvidenceView {
    pub attempt: u32,
    pub validator_id: String,
    pub category: String,
    pub command: Vec<String>,
    pub required: bool,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub failure_fingerprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FingerprintView {
    pub fingerprint: String,
    pub category: Option<String>,
    pub source_id: Option<String>,
    pub summary: Option<String>,
    pub occurrences: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RepairBudgetView {
    pub category: String,
    pub attempts: u32,
    pub base_budget: u32,
    pub extensions: u32,
    pub remaining: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ApprovalView {
    pub gate_id: String,
    pub state: String,
    pub summary: String,
    pub approved_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EventView {
    pub sequence: u64,
    pub stage: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceRequirementView {
    pub id: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceTaskView {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspaceInnovationView {
    pub brief: String,
    pub research_findings: Vec<String>,
    pub candidates: Vec<String>,
    pub selected_rationale: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspacePlanApprovalView {
    pub approved_by: String,
    pub approved_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorkspacePlanView {
    pub exists: bool,
    pub status: String,
    pub message: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub requirements: Vec<WorkspaceRequirementView>,
    pub tasks: Vec<WorkspaceTaskView>,
    pub innovation: Option<WorkspaceInnovationView>,
    pub approval: Option<WorkspacePlanApprovalView>,
    pub implementation_allowed: bool,
    pub revision_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DevelopmentLoopView {
    pub source: String,
    pub status: VisualizerStatus,
    pub run_id: Option<String>,
    pub objective: Option<String>,
    pub current_stage: Option<String>,
    pub workspace_plan: WorkspacePlanView,
    pub stage_history: Vec<StageHistoryItem>,
    pub worktree: Option<WorktreeView>,
    pub validation_evidence: Vec<ValidationEvidenceView>,
    pub fingerprints: Vec<FingerprintView>,
    pub repair_budgets: Vec<RepairBudgetView>,
    pub approvals: Vec<ApprovalView>,
    pub events: Vec<EventView>,
    pub workspace: Option<WorkspaceSnapshotView>,
}

#[derive(Clone, Default)]
struct FingerprintDetails {
    category: Option<String>,
    source_id: Option<String>,
    summary: Option<String>,
}

impl DevelopmentLoopView {
    pub fn empty() -> Self {
        Self {
            source: "empty".to_string(),
            status: VisualizerStatus::Empty,
            run_id: None,
            objective: None,
            current_stage: None,
            workspace_plan: WorkspacePlanView::missing(),
            stage_history: Vec::new(),
            worktree: None,
            validation_evidence: Vec::new(),
            fingerprints: Vec::new(),
            repair_budgets: Vec::new(),
            approvals: Vec::new(),
            events: Vec::new(),
            workspace: None,
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, esi_development::DevelopmentError> {
        DevelopmentState::load(path.into()).map(|state| Self::from_state(&state))
    }

    pub fn from_workspace(path: impl AsRef<Path>) -> Result<Self, String> {
        let workspace = canonical_workspace(path.as_ref())?;
        let plan = match WorkspacePlan::load(&workspace) {
            Ok(Some(plan)) => WorkspacePlanView::from_plan(&plan),
            Ok(None) => WorkspacePlanView::missing(),
            Err(error) => WorkspacePlanView::invalid(&error),
        };
        let status = if plan.implementation_allowed {
            VisualizerStatus::Running
        } else {
            VisualizerStatus::Blocked
        };
        let current_stage = Some(
            if plan.implementation_allowed {
                "workspace_ready"
            } else {
                "workspace_plan"
            }
            .to_string(),
        );
        let objective = plan.title.clone();
        let workspace_snapshot = inspect_workspace(&workspace)?;

        Ok(Self {
            source: "live_workspace".to_string(),
            status,
            run_id: None,
            objective,
            current_stage,
            workspace_plan: plan,
            stage_history: Vec::new(),
            worktree: None,
            validation_evidence: Vec::new(),
            fingerprints: Vec::new(),
            repair_budgets: Vec::new(),
            approvals: Vec::new(),
            events: Vec::new(),
            workspace: Some(workspace_snapshot),
        })
    }

    pub fn from_state(state: &DevelopmentState) -> Self {
        let status = match state.stage() {
            DevelopmentStage::Completed => VisualizerStatus::Completed,
            DevelopmentStage::Abandoned => VisualizerStatus::Abandoned,
            DevelopmentStage::HumanGate => VisualizerStatus::Blocked,
            DevelopmentStage::Diagnose | DevelopmentStage::Repair
                if state.pending_failure().is_some() =>
            {
                VisualizerStatus::Failed
            }
            _ => VisualizerStatus::Running,
        };
        let stage_history = state
            .events()
            .iter()
            .filter_map(|event| match event.event {
                DevelopmentEventKind::RunStarted => Some(StageHistoryItem {
                    sequence: event.sequence,
                    stage: stage_name(event.stage).to_string(),
                }),
                DevelopmentEventKind::StageTransition { to, .. } => Some(StageHistoryItem {
                    sequence: event.sequence,
                    stage: stage_name(to).to_string(),
                }),
                _ => None,
            })
            .collect();
        let workspace_plan = workspace_plan_view(state);
        let worktree = state.worktree().and_then(|binding| {
            state.worktree_snapshot().map(|snapshot| WorktreeView {
                repository_id: binding.identity.repository_id.clone(),
                session_id: binding.identity.session_id.as_str().to_string(),
                branch: binding.identity.branch.clone(),
                worktree_path: binding
                    .identity
                    .worktree_path
                    .to_string_lossy()
                    .into_owned(),
                head: snapshot.head.clone(),
                snapshot_id: snapshot.snapshot_id.clone(),
                dirty: snapshot.dirty,
                changed_files: snapshot.changed_files.clone(),
            })
        });
        let validation_evidence = state
            .validation_runs()
            .iter()
            .flat_map(|run| {
                run.evidence
                    .iter()
                    .map(move |evidence| ValidationEvidenceView {
                        attempt: run.attempt,
                        validator_id: evidence.validator_id.clone(),
                        category: validation_category_name(evidence.category).to_string(),
                        command: evidence.command.clone(),
                        required: evidence.required,
                        outcome: validation_outcome_name(evidence.outcome).to_string(),
                        exit_code: evidence.exit_code,
                        output: if evidence.stderr.is_empty() {
                            evidence.stdout.clone()
                        } else {
                            evidence.stderr.clone()
                        },
                        failure_fingerprint: evidence
                            .failure_fingerprint
                            .as_ref()
                            .map(|fingerprint| fingerprint.as_str().to_string()),
                    })
            })
            .collect();
        let fingerprints = fingerprint_views(state);
        let repair_budgets = state
            .repair_policy()
            .budgets
            .iter()
            .map(|(category, base_budget)| {
                let attempts = state
                    .repair_attempts()
                    .get(category)
                    .copied()
                    .unwrap_or_default();
                let extensions = state
                    .repair_extensions()
                    .get(category)
                    .copied()
                    .unwrap_or_default();
                RepairBudgetView {
                    category: failure_category_name(*category).to_string(),
                    attempts,
                    base_budget: *base_budget,
                    extensions,
                    remaining: (*base_budget + extensions).saturating_sub(attempts),
                }
            })
            .collect();
        let approvals = approval_views(state);
        let events = state
            .events()
            .iter()
            .map(|event| {
                let (kind, detail) = event_description(&event.event);
                EventView {
                    sequence: event.sequence,
                    stage: stage_name(event.stage).to_string(),
                    kind: kind.to_string(),
                    detail,
                }
            })
            .collect();

        let workspace = state
            .worktree()
            .and_then(|binding| inspect_workspace(&binding.identity.worktree_path).ok());
        Self {
            source: "controller_state".to_string(),
            status,
            run_id: Some(state.run_id().to_string()),
            objective: state.brief().map(|brief| brief.objective.clone()),
            current_stage: Some(stage_name(state.stage()).to_string()),
            workspace_plan,
            stage_history,
            worktree,
            validation_evidence,
            fingerprints,
            repair_budgets,
            approvals,
            events,
            workspace,
        }
    }
}

impl WorkspacePlanView {
    fn missing() -> Self {
        Self {
            exists: false,
            status: "missing".to_string(),
            message:
                "No workspace plan exists. Complete discovery and approve a plan before building."
                    .to_string(),
            title: None,
            description: None,
            requirements: Vec::new(),
            tasks: Vec::new(),
            innovation: None,
            approval: None,
            implementation_allowed: false,
            revision_count: 0,
        }
    }

    fn invalid(error: &esi_workspace_plan::WorkspacePlanError) -> Self {
        Self {
            status: "invalid".to_string(),
            message: format!("Workspace plan cannot be loaded: {error}"),
            ..Self::missing()
        }
    }

    fn from_plan(plan: &WorkspacePlan) -> Self {
        Self {
            exists: true,
            status: workspace_plan_status_name(plan.status()).to_string(),
            message: plan.status().display_message().to_string(),
            title: Some(plan.title().to_string()),
            description: Some(plan.description().to_string()),
            requirements: plan
                .requirements()
                .iter()
                .map(|requirement| WorkspaceRequirementView {
                    id: requirement.id.clone(),
                    description: requirement.description.clone(),
                    acceptance_criteria: requirement.acceptance_criteria.clone(),
                    priority: priority_name(requirement.priority).to_string(),
                })
                .collect(),
            tasks: plan
                .tasks()
                .iter()
                .map(|task| WorkspaceTaskView {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    description: task.description.clone(),
                    status: planned_task_status_name(task.status).to_string(),
                })
                .collect(),
            innovation: plan
                .innovation_discovery()
                .map(|innovation| WorkspaceInnovationView {
                    brief: innovation.brief.clone(),
                    research_findings: innovation.research_findings.clone(),
                    candidates: innovation.candidates.clone(),
                    selected_rationale: innovation.selected_rationale.clone(),
                }),
            approval: plan.approval().map(|approval| WorkspacePlanApprovalView {
                approved_by: approval.approved_by.clone(),
                approved_at: approval.approved_at.clone(),
            }),
            implementation_allowed: plan.is_implementation_allowed(),
            revision_count: plan.revision_count(),
        }
    }
}

fn workspace_plan_view(state: &DevelopmentState) -> WorkspacePlanView {
    let Some(worktree) = state.worktree() else {
        return WorkspacePlanView::missing();
    };
    match WorkspacePlan::load(&worktree.identity.source_repository) {
        Ok(Some(plan)) => WorkspacePlanView::from_plan(&plan),
        Ok(None) => WorkspacePlanView::missing(),
        Err(error) => WorkspacePlanView::invalid(&error),
    }
}

fn canonical_workspace(path: &Path) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve workspace {}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "Workspace is not a directory: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("File path must be a non-empty workspace-relative path".to_string());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("File path cannot escape the workspace".to_string());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("File path must name a file".to_string());
    }
    Ok(normalized)
}

fn resolve_workspace_file(workspace: &Path, relative: &Path) -> Result<(PathBuf, PathBuf), String> {
    let root = canonical_workspace(workspace)?;
    let relative = normalize_relative_path(relative)?;
    let candidate = root.join(&relative);
    let canonical = candidate
        .canonicalize()
        .map_err(|error| format!("Cannot resolve {}: {error}", relative.display()))?;
    if !canonical.starts_with(&root) {
        return Err(format!(
            "File resolves outside the workspace: {}",
            relative.display()
        ));
    }
    if !canonical.is_file() {
        return Err(format!("Not a regular file: {}", relative.display()));
    }
    Ok((root, relative))
}

fn relative_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn ignored_directory(entry: &ignore::DirEntry) -> bool {
    entry.depth() > 0
        && entry.file_type().is_some_and(|kind| kind.is_dir())
        && matches!(
            entry.file_name().to_str(),
            Some(".git" | "node_modules" | "target" | ".venv" | "venv" | "__pycache__")
        )
}

fn git_output(workspace: &Path, arguments: &[&OsStr]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("--no-pager")
        .arg("-C")
        .arg(workspace)
        .arg("--literal-pathspecs")
        .args(arguments)
        .output()
        .map_err(|error| format!("Cannot execute git: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn git_text(workspace: &Path, arguments: &[&str]) -> Option<String> {
    let args = arguments.iter().map(OsStr::new).collect::<Vec<_>>();
    git_output(workspace, &args)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn git_changed_files(workspace: &Path) -> Vec<String> {
    let arguments = [
        OsStr::new("status"),
        OsStr::new("--porcelain=v1"),
        OsStr::new("--untracked-files=all"),
    ];
    let Ok(output) = git_output(workspace, &arguments) else {
        return Vec::new();
    };
    let status = String::from_utf8_lossy(&output);
    let mut files = BTreeSet::new();
    for line in status.lines() {
        let Some(raw) = line.get(3..) else {
            continue;
        };
        let path = raw.rsplit(" -> ").next().unwrap_or(raw).trim_matches('"');
        if !path.is_empty() {
            files.insert(path.to_string());
        }
    }
    files.into_iter().collect()
}

pub fn inspect_workspace(path: impl AsRef<Path>) -> Result<WorkspaceSnapshotView, String> {
    let root = canonical_workspace(path.as_ref())?;
    let changed_files = git_changed_files(&root);
    let changed = changed_files.iter().cloned().collect::<BTreeSet<_>>();
    let mut files = Vec::new();
    let mut tree_truncated = false;
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .max_depth(Some(MAX_TREE_DEPTH))
        .filter_entry(|entry| !ignored_directory(entry))
        .build();

    for entry in walker.skip(1) {
        let entry = entry.map_err(|error| format!("Cannot inspect workspace tree: {error}"))?;
        if files.len() == MAX_TREE_ENTRIES {
            tree_truncated = true;
            break;
        }
        let Ok(relative) = entry.path().strip_prefix(&root) else {
            continue;
        };
        let path = relative_string(relative);
        let metadata = entry
            .metadata()
            .map_err(|error| format!("Cannot inspect {path}: {error}"))?;
        let kind = if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else if metadata.file_type().is_symlink() {
            "symlink"
        } else {
            "other"
        };
        let entry_changed = changed.contains(&path)
            || changed
                .iter()
                .any(|changed_path| changed_path.starts_with(&format!("{path}/")));
        files.push(WorkspaceFileEntryView {
            path,
            kind: kind.to_string(),
            size: if metadata.is_file() {
                metadata.len()
            } else {
                0
            },
            changed: entry_changed,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(WorkspaceSnapshotView {
        root: root.to_string_lossy().into_owned(),
        branch: git_text(&root, &["branch", "--show-current"]),
        head: git_text(&root, &["rev-parse", "--short=12", "HEAD"]),
        dirty: !changed_files.is_empty(),
        changed_files,
        files,
        tree_truncated,
    })
}

pub fn read_workspace_file(
    workspace: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<WorkspaceFileView, String> {
    let (root, relative) = resolve_workspace_file(workspace.as_ref(), path.as_ref())?;
    let bytes = fs::read(root.join(&relative))
        .map_err(|error| format!("Cannot read {}: {error}", relative.display()))?;
    if bytes.iter().take(MAX_FILE_BYTES).any(|byte| *byte == 0) {
        return Err(format!(
            "Binary file preview is not supported: {}",
            relative.display()
        ));
    }
    let truncated = bytes.len() > MAX_FILE_BYTES;
    let content = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FILE_BYTES)]).into_owned();
    Ok(WorkspaceFileView {
        workspace_root: root.to_string_lossy().into_owned(),
        path: relative_string(&relative),
        language: language_for_path(&relative).to_string(),
        content,
        bytes: bytes.len(),
        truncated,
    })
}

fn language_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(OsStr::to_str).unwrap_or_default() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" => "markdown",
        "sh" | "bash" => "shell",
        "sql" => "sql",
        _ => "text",
    }
}

fn truncate_utf8(bytes: Vec<u8>, limit: usize) -> (String, bool) {
    let truncated = bytes.len() > limit;
    let mut end = bytes.len().min(limit);
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    (
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        truncated,
    )
}

fn untracked_file_diff(root: &Path, relative: &Path) -> Result<String, String> {
    let file = read_workspace_file(root, relative)?;
    let line_count = file.content.lines().count();
    let mut diff = format!(
        "diff --git a/{0} b/{0}\nnew file mode 100644\n--- /dev/null\n+++ b/{0}\n@@ -0,0 +1,{line_count} @@\n",
        file.path
    );
    for line in file.content.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    if file.truncated {
        diff.push_str("+... preview truncated ...\n");
    }
    Ok(diff)
}

pub fn read_workspace_diff(
    workspace: impl AsRef<Path>,
    path: Option<impl AsRef<Path>>,
) -> Result<WorkspaceDiffView, String> {
    let root = canonical_workspace(workspace.as_ref())?;
    let relative = path
        .map(|path| normalize_relative_path(path.as_ref()))
        .transpose()?;
    if let Some(relative) = &relative {
        resolve_workspace_file(&root, relative)?;
    }

    let mut arguments = vec![
        OsStr::new("diff"),
        OsStr::new("--no-ext-diff"),
        OsStr::new("--no-color"),
        OsStr::new("--unified=3"),
        OsStr::new("HEAD"),
        OsStr::new("--"),
    ];
    if let Some(relative) = &relative {
        arguments.push(relative.as_os_str());
    }
    let mut bytes = git_output(&root, &arguments).unwrap_or_default();
    if bytes.is_empty() {
        if let Some(relative) = &relative {
            let relative_text = relative_string(relative);
            if git_changed_files(&root).contains(&relative_text) {
                bytes = untracked_file_diff(&root, relative)?.into_bytes();
            }
        }
    }
    let original_bytes = bytes.len();
    let (diff, truncated) = truncate_utf8(bytes, MAX_DIFF_BYTES);
    Ok(WorkspaceDiffView {
        workspace_root: root.to_string_lossy().into_owned(),
        path: relative.as_deref().map(relative_string),
        diff,
        bytes: original_bytes,
        truncated,
    })
}

fn discover_controller_state(workspace: &Path) -> Option<PathBuf> {
    let esi = workspace.join(".esi");
    let fixed = [
        esi.join("development-state.json"),
        esi.join("development").join("state.json"),
    ];
    for candidate in fixed {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let runs = esi.join("development-runs");
    let mut candidates = fs::read_dir(runs)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(OsStr::to_str) == Some("json"))
        .filter_map(|path| {
            let modified = path
                .metadata()
                .ok()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn workspace_plan_status_name(status: WorkspacePlanStatus) -> &'static str {
    match status {
        WorkspacePlanStatus::Discovery => "discovery",
        WorkspacePlanStatus::Planning => "planning",
        WorkspacePlanStatus::Approved => "approved",
        WorkspacePlanStatus::Revising => "revising",
    }
}

fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Must => "must",
        Priority::Should => "should",
        Priority::Could => "could",
        Priority::Wont => "wont",
    }
}

fn planned_task_status_name(status: PlannedTaskStatus) -> &'static str {
    match status {
        PlannedTaskStatus::Pending => "pending",
        PlannedTaskStatus::Active => "active",
        PlannedTaskStatus::Completed => "completed",
        PlannedTaskStatus::Skipped => "skipped",
    }
}

fn fingerprint_views(state: &DevelopmentState) -> Vec<FingerprintView> {
    let mut details: BTreeMap<&str, FingerprintDetails> = BTreeMap::new();
    for run in state.validation_runs() {
        for evidence in &run.evidence {
            if let Some(fingerprint) = &evidence.failure_fingerprint {
                details
                    .entry(fingerprint.as_str())
                    .or_insert_with(|| FingerprintDetails {
                        category: Some(validation_category_name(evidence.category).to_string()),
                        source_id: Some(evidence.validator_id.clone()),
                        summary: Some(if evidence.stderr.is_empty() {
                            evidence.stdout.clone()
                        } else {
                            evidence.stderr.clone()
                        }),
                    });
            }
        }
    }
    if let Some(failure) = state.pending_failure() {
        details.insert(
            failure.fingerprint.as_str(),
            FingerprintDetails {
                category: Some(failure_category_name(failure.category).to_string()),
                source_id: Some(failure.source_id.clone()),
                summary: Some(failure.summary.clone()),
            },
        );
    }
    state
        .fingerprint_occurrences()
        .iter()
        .map(|(fingerprint, occurrences)| {
            let detail = details
                .get(fingerprint.as_str())
                .cloned()
                .unwrap_or_default();
            FingerprintView {
                fingerprint: fingerprint.as_str().to_string(),
                category: detail.category,
                source_id: detail.source_id,
                summary: detail.summary,
                occurrences: *occurrences,
            }
        })
        .collect()
}

fn approval_views(state: &DevelopmentState) -> Vec<ApprovalView> {
    let mut approvals: Vec<_> = state
        .events()
        .iter()
        .filter_map(|event| match &event.event {
            DevelopmentEventKind::HumanApprovalRecorded { gate, approved_by } => {
                Some(ApprovalView {
                    gate_id: gate.clone(),
                    state: "completed".to_string(),
                    summary: format!("Approved during {}", stage_name(event.stage)),
                    approved_by: Some(approved_by.clone()),
                })
            }
            _ => None,
        })
        .collect();
    if let Some(gate) = state.pending_human_gate() {
        approvals.push(ApprovalView {
            gate_id: gate.gate_id.clone(),
            state: "pending".to_string(),
            summary: human_gate_summary(&gate.reason),
            approved_by: None,
        });
    } else if state.stage() == DevelopmentStage::CompletionGate {
        approvals.push(ApprovalView {
            gate_id: "completion".to_string(),
            state: "pending".to_string(),
            summary: "Approve the exact validated snapshot for completion".to_string(),
            approved_by: None,
        });
    } else if state.stage() == DevelopmentStage::Plan && state.plan().is_some() {
        approvals.push(ApprovalView {
            gate_id: "worktree_ready".to_string(),
            state: "pending".to_string(),
            summary: "Approve the exact ESI-managed worktree binding".to_string(),
            approved_by: None,
        });
    }
    approvals
}

fn human_gate_summary(reason: &HumanGateReason) -> String {
    match reason {
        HumanGateReason::RepeatedFailure {
            failure,
            occurrences,
        } => format!(
            "Fingerprint {} repeated {} times",
            failure.fingerprint.as_str(),
            occurrences
        ),
        HumanGateReason::RepairBudgetExhausted {
            failure,
            attempts,
            budget,
        } => format!(
            "{} repair budget exhausted after {} of {} attempts",
            failure_category_name(failure.category),
            attempts,
            budget
        ),
        HumanGateReason::AbandonRequested { reason } => {
            format!("Abandonment requested: {reason}")
        }
    }
}

fn event_description(event: &DevelopmentEventKind) -> (&'static str, String) {
    match event {
        DevelopmentEventKind::RunStarted => ("run_started", "Run created".to_string()),
        DevelopmentEventKind::BriefRecorded => ("brief_recorded", "Brief recorded".to_string()),
        DevelopmentEventKind::PlanRecorded => ("plan_recorded", "Plan recorded".to_string()),
        DevelopmentEventKind::StageTransition { from, to } => (
            "stage_transition",
            format!("{} to {}", stage_name(*from), stage_name(*to)),
        ),
        DevelopmentEventKind::WorktreeBound {
            repository_id,
            worktree_path,
            snapshot_id,
        } => (
            "worktree_bound",
            format!(
                "{} at {} ({})",
                repository_id,
                worktree_path.display(),
                snapshot_id
            ),
        ),
        DevelopmentEventKind::WorktreeInspected { snapshot } => (
            "worktree_inspected",
            format!(
                "{} changed files at {}",
                snapshot.changed_files.len(),
                snapshot.snapshot_id
            ),
        ),
        DevelopmentEventKind::ValidationFinished { run } => (
            "validation_finished",
            format!(
                "Attempt {} {}",
                run.attempt,
                if run.passed { "passed" } else { "failed" }
            ),
        ),
        DevelopmentEventKind::FailureRouted {
            failure,
            destination,
        } => (
            "failure_routed",
            format!(
                "{} to {}",
                failure.fingerprint.as_str(),
                stage_name(*destination)
            ),
        ),
        DevelopmentEventKind::ReviewRecorded { approved, summary } => (
            "review_recorded",
            format!(
                "{}: {}",
                if *approved { "approved" } else { "rejected" },
                summary
            ),
        ),
        DevelopmentEventKind::HumanApprovalRecorded { gate, approved_by } => (
            "human_approval_recorded",
            format!("{gate} approved by {approved_by}"),
        ),
    }
}

fn stage_name(stage: DevelopmentStage) -> &'static str {
    match stage {
        DevelopmentStage::Brief => "brief",
        DevelopmentStage::Plan => "plan",
        DevelopmentStage::WorktreeReady => "worktree_ready",
        DevelopmentStage::Implement => "implement",
        DevelopmentStage::DeterministicValidate => "deterministic_validate",
        DevelopmentStage::Diagnose => "diagnose",
        DevelopmentStage::Repair => "repair",
        DevelopmentStage::HumanGate => "human_gate",
        DevelopmentStage::Review => "review",
        DevelopmentStage::CompletionGate => "completion_gate",
        DevelopmentStage::Completed => "completed",
        DevelopmentStage::Abandoned => "abandoned",
    }
}

fn validation_category_name(category: esi_development::ValidationCategory) -> &'static str {
    match category {
        esi_development::ValidationCategory::Scope => "scope",
        esi_development::ValidationCategory::Syntax => "syntax",
        esi_development::ValidationCategory::StaticPolicy => "static_policy",
        esi_development::ValidationCategory::LintTypeBuild => "lint_type_build",
        esi_development::ValidationCategory::TargetedTests => "targeted_tests",
        esi_development::ValidationCategory::BroadTests => "broad_tests",
    }
}

fn failure_category_name(category: FailureCategory) -> &'static str {
    match category {
        FailureCategory::Scope => "scope",
        FailureCategory::Syntax => "syntax",
        FailureCategory::StaticPolicy => "static_policy",
        FailureCategory::Build => "build",
        FailureCategory::Test => "test",
        FailureCategory::Environment => "environment",
        FailureCategory::Review => "review",
    }
}

fn validation_outcome_name(outcome: ValidationOutcome) -> &'static str {
    match outcome {
        ValidationOutcome::Passed => "passed",
        ValidationOutcome::Failed => "failed",
    }
}

pub fn app_html() -> &'static str {
    include_str!("app.html")
}

fn ui_resource_meta() -> MetaObject {
    let mut meta = MetaObject::new();
    meta.0.insert(
        "ui".to_string(),
        json!({ "resourceUri": DEVELOPMENT_LOOP_RESOURCE_URI }),
    );
    meta
}

fn app_only_meta() -> MetaObject {
    let mut meta = MetaObject::new();
    meta.0
        .insert("ui".to_string(), json!({ "visibility": ["app"] }));
    meta
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowDevelopmentLoopParams {
    #[serde(default)]
    pub workspace_path: Option<PathBuf>,
    #[serde(default)]
    pub state_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadWorkspaceFileParams {
    pub workspace_path: PathBuf,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadWorkspaceDiffParams {
    pub workspace_path: PathBuf,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct DevelopmentVisualizerServer {
    tool_router: ToolRouter<Self>,
}

impl Default for DevelopmentVisualizerServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DevelopmentVisualizerServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            "esi-development-visualizer",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Display ESI development controller state or an automatic live workspace snapshot. This server is read-only and has no workflow transition or file mutation tools.",
        )
    }

    async fn list_resources(
        &self,
        _pagination: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new(DEVELOPMENT_LOOP_RESOURCE_URI, "ESI Development Run")
                    .with_title("ESI Development Run")
                    .with_description("Read-only local development stages and evidence")
                    .with_mime_type(MCP_APPS_MIME_TYPE),
            ],
            next_cursor: None,
            meta: None,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if params.uri != DEVELOPMENT_LOOP_RESOURCE_URI {
            return Err(ErrorData::new(
                ErrorCode::INVALID_REQUEST,
                format!("Unknown resource URI: {}", params.uri),
                None,
            ));
        }
        let mut meta = MetaObject::new();
        meta.0.insert(
            "ui".to_string(),
            json!({
                "prefersBorder": false,
                "csp": {
                    "connectDomains": [],
                    "resourceDomains": [],
                    "frameDomains": [],
                    "baseUriDomains": []
                }
            }),
        );
        Ok(
            ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
                uri: params.uri,
                mime_type: Some(MCP_APPS_MIME_TYPE.to_string()),
                text: app_html().to_string(),
                meta: Some(meta),
            }])
            .into(),
        )
    }
}

#[tool_router(router = tool_router)]
impl DevelopmentVisualizerServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "show_development_loop",
        description = "Render a read-only ESI workspace. Prefer workspace_path; persisted controller state is discovered automatically when available. state_path remains available for explicit compatibility.",
        meta = ui_resource_meta()
    )]
    pub async fn show_development_loop(
        &self,
        Parameters(params): Parameters<ShowDevelopmentLoopParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let view = match (params.workspace_path, params.state_path) {
            (Some(workspace), explicit_state) => {
                let root = canonical_workspace(&workspace).map_err(invalid_params)?;
                let state_path = explicit_state.or_else(|| discover_controller_state(&root));
                if let Some(state_path) = state_path {
                    let mut view = DevelopmentLoopView::load(&state_path).map_err(|error| {
                        invalid_params(format!("Cannot load ESI development state: {error}"))
                    })?;
                    view.workspace = Some(inspect_workspace(&root).map_err(invalid_params)?);
                    view
                } else {
                    DevelopmentLoopView::from_workspace(&root).map_err(invalid_params)?
                }
            }
            (None, Some(state_path)) => {
                DevelopmentLoopView::load(&state_path).map_err(|error| {
                    invalid_params(format!("Cannot load ESI development state: {error}"))
                })?
            }
            (None, None) => {
                return Err(invalid_params(
                    "workspace_path or state_path is required".to_string(),
                ));
            }
        };
        let structured = serde_json::to_value(&view).map_err(|error| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Cannot render ESI development state: {error}"),
                None,
            )
        })?;
        let mut result = CallToolResult::structured(structured);
        result.content = vec![ContentBlock::text(format!(
            "ESI development run {} is {} at {}",
            view.run_id.as_deref().unwrap_or("unknown"),
            serde_json::to_value(view.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string()),
            view.current_stage.as_deref().unwrap_or("unknown")
        ))];
        Ok(result.with_meta(Some(ui_resource_meta())))
    }

    #[tool(
        name = "read_workspace_file",
        description = "Read a bounded UTF-8 file preview inside the visualized workspace",
        meta = app_only_meta()
    )]
    pub async fn read_workspace_file_tool(
        &self,
        Parameters(params): Parameters<ReadWorkspaceFileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let file =
            read_workspace_file(params.workspace_path, params.path).map_err(invalid_params)?;
        structured_result(file, "Workspace file loaded")
    }

    #[tool(
        name = "read_workspace_diff",
        description = "Read a bounded Git diff inside the visualized workspace",
        meta = app_only_meta()
    )]
    pub async fn read_workspace_diff_tool(
        &self,
        Parameters(params): Parameters<ReadWorkspaceDiffParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let diff =
            read_workspace_diff(params.workspace_path, params.path).map_err(invalid_params)?;
        structured_result(diff, "Workspace diff loaded")
    }
}

fn invalid_params(message: String) -> ErrorData {
    ErrorData::new(ErrorCode::INVALID_PARAMS, message, None)
}

fn structured_result<T: Serialize>(
    value: T,
    message: &'static str,
) -> Result<CallToolResult, ErrorData> {
    let structured = serde_json::to_value(value).map_err(|error| {
        ErrorData::new(
            ErrorCode::INTERNAL_ERROR,
            format!("Cannot serialize visualizer response: {error}"),
            None,
        )
    })?;
    let mut result = CallToolResult::structured(structured);
    result.content = vec![ContentBlock::text(message)];
    Ok(result.with_meta(Some(app_only_meta())))
}
