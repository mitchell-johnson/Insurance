//! What the business is told about its documents, read from the state.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::kinds::DocumentKind;
use crate::model::{Channel, DeliveryStatus};
use crate::state::{nz_date, DocumentsState, TemplateStatus};
use crate::template::TemplateRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTemplate {
    pub template: TemplateRef,
    pub kind: DocumentKind,
    pub content_hash: String,
    pub products: Vec<String>,
    pub effective_from: Option<NaiveDate>,
    pub review_by: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Report {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub active_templates: Vec<ActiveTemplate>,
    /// Active templates whose review date is on or before the period's end.
    pub due_for_review: Vec<TemplateRef>,
    pub issued_by_kind: BTreeMap<DocumentKind, u64>,
    /// Finished deliveries in the period, by the channel that succeeded.
    pub delivered_by_channel: BTreeMap<Channel, u64>,
    pub deliveries_in_progress: u64,
    pub undeliverable: u64,
    /// Undeliverable recipients per thousand recipients.
    pub undeliverable_per_mille: u64,
    /// New versions issued in the period, by the reason given.
    pub reissues_by_reason: BTreeMap<String, u64>,
    pub rendering_test_failures: u64,
    pub open_defects: u64,
    pub defective_notices_outstanding: u64,
    pub documents_awaiting_trigger: u64,
}

impl DocumentsState {
    pub fn report(&self, from: NaiveDate, to: NaiveDate) -> Report {
        let mut report = Report { from, to, ..Report::default() };
        for record in self.templates.values() {
            report.rendering_test_failures += record.failed_runs.iter().filter(|at| (from..=to).contains(&nz_date(**at))).count() as u64;
            if record.status != TemplateStatus::Active {
                continue;
            }
            let template = &record.template;
            if template.review_by <= to {
                report.due_for_review.push(template.reference());
            }
            report.active_templates.push(ActiveTemplate {
                template: template.reference(),
                kind: template.kind,
                content_hash: record.content_hash.clone(),
                products: template.products.iter().map(|p| format!("{} v{}", p.code, p.version)).collect(),
                effective_from: record.effective_from,
                review_by: template.review_by,
            });
        }
        let mut recipients = 0u64;
        for doc in self.documents.values().filter(|d| !d.legacy && (from..=to).contains(&d.issued_on)) {
            *report.issued_by_kind.entry(doc.kind).or_default() += 1;
            if doc.supersedes.is_some() {
                *report.reissues_by_reason.entry(doc.reason.clone().unwrap_or_default()).or_default() += 1;
            }
            for delivery in &doc.deliveries {
                recipients += 1;
                match &delivery.status {
                    DeliveryStatus::Delivered { channel, .. } => *report.delivered_by_channel.entry(*channel).or_default() += 1,
                    DeliveryStatus::InProgress => report.deliveries_in_progress += 1,
                    DeliveryStatus::Undeliverable => report.undeliverable += 1,
                }
            }
        }
        if let Some(rate) = (report.undeliverable * 1000).checked_div(recipients) {
            report.undeliverable_per_mille = rate;
        }
        for defect in self.defects.values().filter(|d| d.closed.is_none()) {
            report.open_defects += 1;
            report.defective_notices_outstanding += defect
                .notices
                .values()
                .filter(|i| matches!(i, crate::model::DefectItem::Outstanding | crate::model::DefectItem::Corrected { .. }))
                .count() as u64;
        }
        report.documents_awaiting_trigger = self.triggers.len() as u64;
        report
    }
}
