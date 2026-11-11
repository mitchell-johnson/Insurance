//! Evidence: what is asked of doctors, laboratories, previous insurers and
//! financial sources. Each request has a status, a deadline and a count of
//! chasers. The content arrives in the vault; a request holds a reference to
//! it and never the content.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::case::Queue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    GpReport,
    SpecialistReport,
    BloodTest,
    MedicalExam,
    /// Records from somewhere other than a local doctor, for a person whose
    /// history is partly overseas or missing.
    AlternativeMedicalHistory,
    PreviousInsurer,
    FinancialStatement,
}

impl EvidenceKind {
    /// The queue whose people read this kind of evidence.
    pub fn queue(self) -> Queue {
        match self {
            EvidenceKind::FinancialStatement => Queue::Financial,
            _ => Queue::Medical,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            EvidenceKind::GpReport => "gp_report",
            EvidenceKind::SpecialistReport => "specialist_report",
            EvidenceKind::BloodTest => "blood_test",
            EvidenceKind::MedicalExam => "medical_exam",
            EvidenceKind::AlternativeMedicalHistory => "alternative_medical_history",
            EvidenceKind::PreviousInsurer => "previous_insurer",
            EvidenceKind::FinancialStatement => "financial_statement",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Requested,
    Received,
    /// The source did not or could not answer. Never a decision in itself.
    Unobtainable,
    /// The proposal ended before the evidence arrived.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRequest {
    /// Stable for the proposal, life and kind, so a retried step never asks
    /// a doctor twice.
    pub id: String,
    pub kind: EvidenceKind,
    pub status: EvidenceStatus,
    pub requested_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    /// Automatic chasers sent so far.
    pub chasers: u32,
    pub last_chased_at: Option<DateTime<Utc>>,
    /// The provider's own reference for the request.
    pub provider_ref: Option<String>,
    /// Where the evidence is in the vault, once it has arrived.
    pub vault_ref: Option<String>,
    /// What the evidence cost, for management reporting.
    pub cost_minor: i64,
}

impl EvidenceRequest {
    pub fn outstanding(&self) -> bool {
        self.status == EvidenceStatus::Requested
    }
}
