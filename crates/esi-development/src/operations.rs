//! Durable operation reservations, not an approval or execution interface.
//! The host adapter must resolve and authorize every binding before reserving.
use crate::{DevelopmentError, DevelopmentStage, DevelopmentState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Start,
    Validate,
    Resume,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRequest {
    pub request_id: String,
    pub kind: OperationKind,
    pub source_plan_hash: String,
    pub source_revision: u64,
    pub task_id: String,
    pub session_id: String,
    pub policy_digest: String,
    pub expected_snapshot: String,
}

impl OperationRequest {
    pub(crate) fn valid(&self) -> bool {
        !self.request_id.is_empty()
            && self.request_id.len() <= 128
            && self
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            && [&self.task_id, &self.session_id, &self.expected_snapshot]
                .iter()
                .all(|s| !s.trim().is_empty() && s.len() <= 256)
            && [&self.source_plan_hash, &self.policy_digest]
                .iter()
                .all(|s| {
                    s.len() == 64
                        && s.bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
    }

    fn digest(&self) -> Result<String, DevelopmentError> {
        // A struct with fixed field order, no caller JSON/map ordering ambiguity.
        Ok(Sha256::digest(serde_json::to_vec(self)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Started,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationRecord {
    pub request: OperationRequest,
    pub payload_digest: String,
    pub status: OperationStatus,
    pub started_stage: DevelopmentStage,
    pub result_stage: Option<DevelopmentStage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationReservation {
    Reserved,
    Recorded(OperationRecord),
}

impl DevelopmentState {
    pub fn operations(&self) -> &BTreeMap<String, OperationRecord> {
        &self.operations
    }

    /// Returns Reserved only after intent was durably committed with CAS.
    /// Recorded must never be interpreted as permission to repeat side effects.
    pub fn reserve_operation(
        &mut self,
        path: impl AsRef<Path>,
        request: OperationRequest,
    ) -> Result<OperationReservation, DevelopmentError> {
        if !request.valid() {
            return Err(operation_error("invalid operation identity"));
        }
        let digest = request.digest()?;
        if let Some(existing) = self.operations.get(&request.request_id) {
            if existing.request != request || existing.payload_digest != digest {
                return Err(operation_error(
                    "request id reused with a different payload",
                ));
            }
            return Ok(OperationReservation::Recorded(existing.clone()));
        }
        if self
            .operations
            .values()
            .any(|r| r.status == OperationStatus::Started)
        {
            return Err(operation_error(
                "an operation is already started; reconcile before retry",
            ));
        }
        let legal = match request.kind {
            OperationKind::Start => self.stage == DevelopmentStage::Plan,
            OperationKind::Validate => matches!(
                self.stage,
                DevelopmentStage::Implement | DevelopmentStage::Repair
            ),
            OperationKind::Resume => !matches!(
                self.stage,
                DevelopmentStage::Completed | DevelopmentStage::Abandoned
            ),
        };
        if !legal {
            return Err(operation_error("operation is illegal in the current stage"));
        }
        let mut candidate = self.clone();
        candidate.operations.insert(
            request.request_id.clone(),
            OperationRecord {
                request,
                payload_digest: digest,
                status: OperationStatus::Started,
                started_stage: self.stage,
                result_stage: None,
            },
        );
        candidate.save(path)?;
        *self = candidate;
        Ok(OperationReservation::Reserved)
    }

    /// Records transport/lifecycle completion only, never validation PASS or
    /// task completion. Those remain the responsibility of the typed FSM.
    pub fn finish_operation(
        &mut self,
        path: impl AsRef<Path>,
        request_id: &str,
        status: OperationStatus,
    ) -> Result<(), DevelopmentError> {
        if status == OperationStatus::Started {
            return Err(operation_error(
                "finish requires a terminal operation outcome",
            ));
        }
        let mut candidate = self.clone();
        let record = candidate
            .operations
            .get_mut(request_id)
            .ok_or_else(|| operation_error("unknown operation"))?;
        if record.status != OperationStatus::Started {
            return Err(operation_error("operation is already terminal"));
        }
        record.status = status;
        record.result_stage = Some(self.stage);
        candidate.save(path)?;
        *self = candidate;
        Ok(())
    }

    pub(crate) fn operations_valid(&self) -> bool {
        self.operations
            .values()
            .filter(|r| r.status == OperationStatus::Started)
            .count()
            <= 1
            && self.operations.iter().all(|(id, r)| {
                id == &r.request.request_id
                    && r.request.valid()
                    && r.request
                        .digest()
                        .is_ok_and(|digest| digest == r.payload_digest)
                    && (r.status == OperationStatus::Started) == r.result_stage.is_none()
            })
    }
}

fn operation_error(message: &str) -> DevelopmentError {
    DevelopmentError::InvalidInput(message.into())
}
