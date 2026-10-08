//! What each model request cost and what its step admitted, as measured by the server.
//! A pending step shows only what its response reported; a sealed one adds its own results.

use super::TranscriptView;
use super::text::token_label;
use codex_app_server_protocol::StepAdmissionItem;
use codex_protocol::step_admission::Admission;
use codex_protocol::step_admission::AdmissionPrecision;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct StepBills {
    by_response: HashMap<String, StepAdmissionItem>,
}

/// A step's bill as the heading shows it: the total in the gutter, then
/// `(thinking output self)` and any arrivals or releases after the title.
pub(super) struct StepBill {
    pub(super) total: u64,
    pub(super) decoration: String,
}

impl TranscriptView {
    pub(crate) fn record_step_admission(&mut self, item: StepAdmissionItem) {
        let bills = &mut self.step_bills.by_response;
        // A live pending record must not overwrite the record that sealed it.
        if item.admission == Admission::Pending && bills.contains_key(&item.response_id) {
            return;
        }
        if bills.get(&item.response_id) != Some(&item) {
            bills.insert(item.response_id.clone(), item);
            self.cache.clear();
            self.live_key = None;
        }
    }

    pub(super) fn step_bill(&self, response_id: &str) -> Option<StepBill> {
        let item = self.step_bills.by_response.get(response_id)?;
        let produced = item.thinking_tokens.unwrap_or(0) + item.output_tokens;
        let thinking = item
            .thinking_tokens
            .map_or_else(|| "—".to_string(), token_label);
        let output = token_label(item.output_tokens);
        Some(match &item.admission {
            Admission::Pending => StepBill {
                total: produced,
                decoration: format!("  ({thinking} {output} …)"),
            },
            Admission::Severed => StepBill {
                total: produced,
                decoration: format!("  ({thinking} {output} —) severed"),
            },
            Admission::Sealed(sealed) => {
                let approx = match sealed.precision {
                    AdmissionPrecision::Exact => "",
                    AdmissionPrecision::Apportioned | AdmissionPrecision::Disturbed => "≈",
                };
                let mut decoration = format!(
                    "  ({thinking} {output} {approx}{})",
                    token_label(sealed.self_tokens)
                );
                let arrived = sealed.arrivals.iter().map(|arrival| arrival.tokens).sum();
                if !sealed.arrivals.is_empty() {
                    decoration.push_str(&format!(" · arrivals {approx}{}", token_label(arrived)));
                }
                if sealed.released_tokens > 0 {
                    decoration.push_str(&format!(
                        " · released {}",
                        token_label(sealed.released_tokens)
                    ));
                }
                StepBill {
                    total: produced + sealed.self_tokens,
                    decoration,
                }
            }
        })
    }
}
