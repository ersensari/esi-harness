//! Native Desktop only: never register these operations as MCP tools.
use super::*;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;

struct PendingReview {
    session: String,
    plan: WorkspacePlan,
    created: Instant,
}
static PENDING: LazyLock<Mutex<HashMap<String, PendingReview>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const TTL: Duration = Duration::from_secs(300);

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Prepare {
        session_id: String,
        hash: String,
        storage_revision: u64,
    },
    Complete {
        session_id: String,
        token: String,
        approve: bool,
    },
}

pub(crate) async fn native_plan_review(sessions: &SessionManager, params: Value) -> Result<Value> {
    let request: Request = serde_json::from_value(params).context("Invalid native plan review")?;
    let session_id = match &request {
        Request::Prepare { session_id, .. } | Request::Complete { session_id, .. } => session_id,
    };
    let session = sessions.get_session(session_id, false).await?;
    let ctx = ToolCallContext::new(session_id.clone(), Some(session.working_dir), None);
    let root = workspace(sessions, &ctx).await?;
    let mut pending = PENDING.lock().await;
    pending.retain(|_, review| review.created.elapsed() < TTL);
    match request {
        Request::Prepare {
            session_id,
            hash,
            storage_revision,
        } => {
            let plan = load_plan(&root)?;
            ensure!(
                plan.content_hash() == hash && plan.storage_revision() == storage_revision,
                "Canvas plan changed; refresh the snapshot before review"
            );
            ensure!(
                matches!(
                    plan.status(),
                    esi_workspace_plan::WorkspacePlanStatus::Planning
                        | esi_workspace_plan::WorkspacePlanStatus::Revising
                        | esi_workspace_plan::WorkspacePlanStatus::Approved
                ),
                "Finish plan discovery first"
            );
            pending.retain(|_, review| {
                review.session != session_id || review.plan.canonical_path() != root
            });
            ensure!(
                pending.len() < 64,
                "Too many pending plan reviews; try again later"
            );
            let token = uuid::Uuid::new_v4().to_string();
            let response = json!({"token": token, "scope": plan_presentation(&plan)});
            pending.insert(
                token,
                PendingReview {
                    session: session_id,
                    plan,
                    created: Instant::now(),
                },
            );
            Ok(response)
        }
        Request::Complete {
            session_id,
            token,
            approve,
        } => {
            ensure!(
                pending.get(&token).is_some_and(|review| {
                    review.session == session_id && review.plan.canonical_path() == root
                }),
                "Review expired, already consumed, or belongs to another session"
            );
            let mut review = pending.remove(&token).expect("checked pending review");
            drop(pending);
            ensure!(
                review.plan.canonical_path() == root,
                "Review workspace changed"
            );
            if !approve {
                return Ok(json!({"approved": false}));
            }
            let _guard = EXECUTION.lock().await;
            ensure!(
                review.created.elapsed() < TTL,
                "Review expired while waiting"
            );
            commit_human_plan(&mut review.plan, &root)?;
            Ok(json!({"approved": true, "hash": review.plan.content_hash(),
                "storage_revision": review.plan.storage_revision(),
                "memory_sync": "not_requested"}))
        }
    }
}

#[cfg(test)]
pub(super) async fn expire_for_test(token: &str) {
    PENDING.lock().await.get_mut(token).unwrap().created = Instant::now() - TTL;
}
