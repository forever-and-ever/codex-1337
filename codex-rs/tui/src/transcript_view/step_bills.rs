//! What each model request cost and what its step admitted, as measured by the server.
//!
//! A step's bill heads the row of its first call, or its Thought when it made none; a row
//! holding several steps' first calls, such as a Code Mode block with waits, sums them. A
//! pending step shows only what its response reported; a sealed one adds its own results.
//! Each arrival heads its own row with its share of the seal; user input, which has no
//! arrival row, is summed on the step's bill.

use super::TranscriptView;
use super::text::token_label;
use crate::history_cell::ArrivalCell;
use crate::history_cell::CodeModeCell;
use crate::history_cell::CodeModeWaitCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::ThoughtCell;
use codex_app_server_protocol::StepAdmissionItem;
use codex_protocol::code_mode::CodeModeEvent;
use codex_protocol::step_admission::Admission;
use codex_protocol::step_admission::AdmissionPrecision;
use codex_protocol::step_admission::ArrivalKind;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct StepBills {
    by_response: HashMap<String, StepAdmissionItem>,
    response_by_call: HashMap<String, String>,
}

/// The heading's share of a row's steps: the total in the gutter, then
/// `(thinking output self)` and what else the steps admitted or released after the title.
/// An arrival's row shows only its own share, once sealed.
pub(super) struct StepBill {
    pub(super) total: Option<u64>,
    pub(super) decoration: String,
}

impl TranscriptView {
    pub(crate) fn record_step_admission(&mut self, item: StepAdmissionItem) {
        let bills = &mut self.step_bills;
        // A live pending record must not overwrite the record that sealed it.
        if item.admission == Admission::Pending && bills.by_response.contains_key(&item.response_id)
        {
            return;
        }
        if bills.by_response.get(&item.response_id) == Some(&item) {
            return;
        }
        for call_id in &item.call_ids {
            bills
                .response_by_call
                .insert(call_id.clone(), item.response_id.clone());
        }
        bills.by_response.insert(item.response_id.clone(), item);
        self.cache.clear();
        self.live_key = None;
    }

    /// The bill of the steps this row heads, if any.
    pub(super) fn step_bill(&self, cell: &dyn HistoryCell) -> Option<StepBill> {
        let bills = &self.step_bills;
        if let Some(arrival) = cell.as_any().downcast_ref::<ArrivalCell>() {
            return Some(arrival_bill(
                bills.by_response.get(arrival.response_id()),
                arrival.id(),
            ));
        }
        let steps: Vec<&StepAdmissionItem> = match cell.as_any().downcast_ref::<ThoughtCell>() {
            // A Thought heads only a step that made no calls.
            Some(thought) => bills
                .by_response
                .get(thought.response_id())
                .filter(|step| step.call_ids.is_empty())
                .into_iter()
                .collect(),
            None => {
                let mut steps = Vec::new();
                for call_id in call_ids(cell) {
                    let Some(step) = bills
                        .response_by_call
                        .get(call_id)
                        .and_then(|response| bills.by_response.get(response))
                    else {
                        continue;
                    };
                    if step.call_ids.first().map(String::as_str) == Some(call_id)
                        && !steps
                            .iter()
                            .any(|known: &&StepAdmissionItem| known.id == step.id)
                    {
                        steps.push(step);
                    }
                }
                steps
            }
        };
        compose(&steps)
    }
}

/// The model calls a row presents, including a Code Mode block's exec and its waits.
fn call_ids(cell: &dyn HistoryCell) -> Vec<&str> {
    if let Some(block) = cell.as_any().downcast_ref::<CodeModeCell>() {
        return std::iter::once(block.exec_call_id.as_str())
            .chain(block.facts().iter().filter_map(|fact| match &fact.event {
                CodeModeEvent::WaitStarted { call_id } => Some(call_id.as_str()),
                _ => None,
            }))
            .collect();
    }
    if let Some(wait) = cell.as_any().downcast_ref::<CodeModeWaitCell>()
        && let CodeModeEvent::WaitStarted { call_id } | CodeModeEvent::WaitCompleted { call_id } =
            &wait.0.0.event
    {
        return vec![call_id.as_str()];
    }
    cell.tool_call_ids()
}

fn arrival_bill(step: Option<&StepAdmissionItem>, id: &str) -> StepBill {
    let unbilled = |label: &str| StepBill {
        total: None,
        decoration: format!("  ({label})"),
    };
    let sealed = match step.map(|step| &step.admission) {
        Some(Admission::Sealed(sealed)) => sealed,
        Some(Admission::Severed) => return unbilled("—"),
        Some(Admission::Pending) | None => return unbilled("…"),
    };
    let Some(arrival) = sealed.arrivals.iter().find(|arrival| arrival.id == id) else {
        return unbilled("—");
    };
    let approx = if sealed.precision == AdmissionPrecision::Exact {
        ""
    } else {
        "≈"
    };
    StepBill {
        total: Some(arrival.tokens),
        decoration: format!("  ({approx}{})", token_label(arrival.tokens)),
    }
}

fn compose(steps: &[&StepAdmissionItem]) -> Option<StepBill> {
    if steps.is_empty() {
        return None;
    }
    let thinking = steps
        .iter()
        .map(|step| step.thinking_tokens)
        .sum::<Option<u64>>();
    let output = steps.iter().map(|step| step.output_tokens).sum::<u64>();
    let mut own = 0;
    let mut user = 0;
    let mut released = 0;
    let mut approximate = false;
    let mut unsealed = 0;
    let mut severed = 0;
    for step in steps {
        match &step.admission {
            Admission::Pending => unsealed += 1,
            Admission::Severed => severed += 1,
            Admission::Sealed(sealed) => {
                own += sealed.self_tokens;
                user += sealed
                    .arrivals
                    .iter()
                    .filter(|arrival| arrival.kind == ArrivalKind::User)
                    .map(|arrival| arrival.tokens)
                    .sum::<u64>();
                released += sealed.released_tokens;
                approximate |= sealed.precision != AdmissionPrecision::Exact;
            }
        }
    }
    let approx = if approximate { "≈" } else { "" };
    let measured = steps.len() - unsealed - severed;
    let own_label = match (measured, unsealed) {
        (0, 0) => "—".to_string(),
        (0, _) => "…".to_string(),
        (_, 0) => format!("{approx}{}", token_label(own)),
        (_, _) => format!("{approx}{}+…", token_label(own)),
    };
    let thinking_label = thinking.map_or_else(|| "—".to_string(), token_label);
    let mut decoration = format!("  ({thinking_label} {} {own_label})", token_label(output));
    if steps.len() > 1 {
        decoration.push_str(&format!(" ×{}", steps.len()));
    }
    if user > 0 {
        decoration.push_str(&format!(" · user {approx}{}", token_label(user)));
    }
    if released > 0 {
        decoration.push_str(&format!(" · released {}", token_label(released)));
    }
    if severed > 0 {
        decoration.push_str(" · severed");
    }
    Some(StepBill {
        total: Some(thinking.unwrap_or(0) + output + own),
        decoration,
    })
}
