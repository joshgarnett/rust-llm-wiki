//! Optional private per-attempt diagnostics. These bytes are never knowledge or billing authority.
use super::{AttemptPhase, AttemptRef, BillingDisposition, JobLedger};
use crate::{
    domain::*,
    vault::{
        ExpectedState,
        operational::{DiagnosticPart, RunFile},
    },
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    SemanticRejection,
    HttpError,
}
impl DiagnosticKind {
    fn parts(self) -> (DiagnosticPart, DiagnosticPart) {
        match self {
            Self::SemanticRejection => (
                DiagnosticPart::SemanticBody,
                DiagnosticPart::SemanticMetadata,
            ),
            Self::HttpError => (DiagnosticPart::HttpBody, DiagnosticPart::HttpMetadata),
        }
    }
    fn limit(self) -> usize {
        match self {
            Self::SemanticRejection => 256 * 1024,
            Self::HttpError => 16 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRef {
    pub version: u8,
    pub attempt: AttemptRef,
    pub kind: DiagnosticKind,
    pub retained_hash: Blake3Hash,
    pub retained_bytes: u64,
    pub observed_bytes: u64,
    pub truncated: bool,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticInspection {
    pub reference: DiagnosticRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_utf8: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_hex: Option<String>,
}

fn safe_reason(reason: &str) -> Result<()> {
    if reason.is_empty()
        || reason.len() > 64
        || !reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(WikiError::invalid(
            "diagnostic reason must be a fixed safe code",
        ));
    }
    Ok(())
}

impl JobLedger {
    pub fn retain_diagnostic(
        &self,
        attempt: &AttemptRef,
        kind: DiagnosticKind,
        body: &[u8],
        observed_bytes: u64,
        reason: &str,
    ) -> Result<DiagnosticRef> {
        self.local_write()?;
        safe_reason(reason)?;
        if observed_bytes < body.len() as u64 {
            return Err(WikiError::invalid(
                "diagnostic observed length is smaller than body",
            ));
        }
        self.with(true, |guard, loaded| {
            let recorded = loaded
                .state
                .inspection
                .attempts
                .iter()
                .find(|a| a.attempt == *attempt)
                .ok_or_else(|| WikiError::invalid("diagnostic attempt is not retained"))?;
            if recorded.phase == AttemptPhase::Reserved
                || recorded.phase == AttemptPhase::DispatchIntent
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "diagnostic response has no retained outcome",
                ));
            }
            guard.ensure_attempt_dir(attempt)?;
            let retained = &body[..body.len().min(kind.limit())];
            let reference = DiagnosticRef {
                version: 1,
                attempt: attempt.clone(),
                kind,
                retained_hash: Blake3Hash::digest(retained),
                retained_bytes: retained.len() as u64,
                observed_bytes,
                truncated: observed_bytes > retained.len() as u64,
                reason: reason.to_owned(),
            };
            let (body_part, meta_part) = kind.parts();
            match guard.read_diagnostic(attempt, body_part)? {
                Some(existing) if existing.bytes == retained => {}
                Some(_) => {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "retained diagnostic body differs",
                    ));
                }
                None => {
                    guard.secure_replace(
                        RunFile::Diagnostic {
                            attempt,
                            part: body_part,
                        },
                        ExpectedState::Absent,
                        retained,
                    )?;
                }
            }
            let metadata = serde_json::to_vec(&reference)
                .map_err(|_| WikiError::invalid("diagnostic metadata encoding"))?;
            match guard.read_diagnostic(attempt, meta_part)? {
                Some(existing) if existing.bytes == metadata => {}
                Some(_) => {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "retained diagnostic metadata differs",
                    ));
                }
                None => {
                    guard.secure_replace(
                        RunFile::Diagnostic {
                            attempt,
                            part: meta_part,
                        },
                        ExpectedState::Absent,
                        &metadata,
                    )?;
                }
            }
            Ok(reference)
        })
    }

    pub fn inspect_diagnostic(
        &self,
        attempt: &AttemptRef,
        kind: DiagnosticKind,
        raw: bool,
    ) -> Result<Option<DiagnosticInspection>> {
        self.with(true, |guard, loaded| {
            if !loaded
                .state
                .inspection
                .attempts
                .iter()
                .any(|a| a.attempt == *attempt)
            {
                return Err(WikiError::new(
                    ErrorCode::RecordNotFound,
                    "diagnostic attempt missing",
                ));
            }
            let (body_part, meta_part) = kind.parts();
            let Some(metadata) = guard.read_diagnostic(attempt, meta_part)? else {
                return Ok(None);
            };
            let reference: DiagnosticRef =
                serde_json::from_slice(&metadata.bytes).map_err(|_| {
                    WikiError::new(
                        ErrorCode::RecoveryRequired,
                        "private diagnostic metadata invalid",
                    )
                })?;
            if reference.version != 1 || reference.attempt != *attempt || reference.kind != kind {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "private diagnostic binding differs",
                ));
            }
            safe_reason(&reference.reason)?;
            let body = guard.read_diagnostic(attempt, body_part)?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "private diagnostic body missing",
                )
            })?;
            if body.hash != reference.retained_hash
                || body.bytes.len() as u64 != reference.retained_bytes
                || reference.observed_bytes < reference.retained_bytes
                || reference.truncated != (reference.observed_bytes > reference.retained_bytes)
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "private diagnostic bytes differ",
                ));
            }
            let (body_utf8, body_hex) = if raw {
                match String::from_utf8(body.bytes.clone()) {
                    Ok(text) => (Some(text), None),
                    Err(_) => (
                        None,
                        Some(body.bytes.iter().map(|b| format!("{b:02x}")).collect()),
                    ),
                }
            } else {
                (None, None)
            };
            Ok(Some(DiagnosticInspection {
                reference,
                body_utf8,
                body_hex,
            }))
        })
    }

    pub fn prune_diagnostic(&self, attempt: &AttemptRef, kind: DiagnosticKind) -> Result<bool> {
        self.local_write()?;
        self.with(true, |guard, loaded| {
            let recorded = loaded
                .state
                .inspection
                .attempts
                .iter()
                .find(|a| a.attempt == *attempt)
                .ok_or_else(|| {
                    WikiError::new(ErrorCode::RecordNotFound, "diagnostic attempt missing")
                })?;
            if recorded.phase != AttemptPhase::Settled
                || recorded.receipt.is_none()
                || recorded.billing == BillingDisposition::UnknownReserved
            {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "diagnostic protected until settled receipt and known billing",
                ));
            }
            let (body_part, meta_part) = kind.parts();
            let body = guard.read_diagnostic(attempt, body_part)?;
            let meta = guard.read_diagnostic(attempt, meta_part)?;
            if body.is_none() && meta.is_none() {
                return Ok(false);
            }
            if let Some(meta) = meta {
                guard.remove_diagnostic_file(attempt, meta_part, &meta.hash)?;
            }
            if let Some(body) = body {
                guard.remove_diagnostic_file(attempt, body_part, &body.hash)?;
            }
            guard.remove_empty_attempt_dir(attempt)?;
            Ok(true)
        })
    }
}
