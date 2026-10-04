//! Physical metadata observations, without canonical/history or integrity audits.
use super::{Catalog, DoctorCacheMetadata, scan, selector, sql};
use crate::{
    changes::operation_authority::{self as operations, Presence, Publication},
    domain::{ErrorCode, Result, WikiError},
};
use std::time::Duration;

impl Catalog {
    pub(crate) fn doctor_cache_metadata(&self) -> DoctorCacheMetadata {
        let mut report = DoctorCacheMetadata::default();
        if let Err(error) = self.inspect_doctor_metadata(&mut report) {
            report.state = "unavailable".into();
            report.error = Some(error);
        }
        report
    }

    fn inspect_doctor_metadata(&self, report: &mut DoctorCacheMetadata) -> Result<()> {
        if self.options.busy_timeout_ms > 30_000 {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "catalog busy timeout exceeds 30 seconds",
            ));
        }
        let activated = match selector::has_activation_evidence(&self.fs) {
            Ok(activated) => activated,
            Err(error) => {
                report.operation_state = "unavailable".into();
                return Err(error);
            }
        };
        if activated {
            report.layout = "normalized".into();
        }
        let presence = if activated {
            Presence::Required
        } else {
            Presence::LegacyMayBeAbsent
        };
        let authority = match operations::load(&self.fs, &self.vault_id, presence) {
            Ok(authority) => authority,
            Err(error) => {
                report.operation_state = "unavailable".into();
                return Err(error);
            }
        };
        sql::capability()?;
        if let Some(authority) = authority {
            report.layout = "normalized".into();
            report.operation_state = if authority.active().is_some() {
                "active"
            } else {
                "idle"
            }
            .into();
            report.active_change = authority
                .active()
                .map(|active| active.change.change_id.clone());
            let (_, header) = selector::maintenance_header(
                &self.fs,
                &self.vault_id,
                Duration::from_millis(self.options.busy_timeout_ms.min(2000)),
            )?
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "normalized authority exists without a selected catalog",
                )
            })?;
            let publication = header
                .snapshot
                .publication()
                .expect("normalized header has published binding");
            authority.require_read_publication(&Publication {
                file_id: publication.file_id.clone(),
                epoch: header.snapshot.generation,
            })?;
            if self.operation_state()?.as_ref() != Some(&authority) {
                report.operation_state = "changed".into();
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "operation authority changed during doctor metadata observation; retry doctor",
                ));
            }
            report.parser_compatible =
                Some(header.snapshot.parser_fingerprint == scan::parser_fingerprint());
            report.header_snapshot = Some(header.snapshot);
            report.header_check_performed = true;
            report.state = "header_available".into();
            if authority.active().is_some() {
                report.note = Some("An active operation is recorded; this physical header observation does not establish current-read or write admission. Use recover for operation recovery.".into());
            }
        } else {
            report.layout = "legacy".into();
            report.operation_state = "legacy_without_slot".into();
            match selector::legacy_doctor_header(&self.fs)? {
                selector::LegacyDoctorHeader::Absent => report.state = "absent".into(),
                selector::LegacyDoctorHeader::Uninspected(note) => {
                    report.state = "present_uninspected".into();
                    report.note = Some(note.into());
                }
                selector::LegacyDoctorHeader::Header(snapshot) => {
                    report.parser_compatible =
                        Some(snapshot.parser_fingerprint == scan::parser_fingerprint());
                    report.header_snapshot = Some(snapshot);
                    report.header_check_performed = true;
                    report.state = "header_available".into();
                }
            }
            if self.operation_state()?.is_some() {
                report.operation_state = "changed".into();
                report.header_snapshot = None;
                report.parser_compatible = None;
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "normalized activation appeared during legacy doctor observation; retry doctor",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
