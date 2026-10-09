//! What each model request admitted, as measured by the server, and the figures a Feed
//! Item's header shows for it.
//!
//! A step's figures are its total, then its split: thought and output from the response's
//! own usage, self from the next request's seal. A header over several steps sums them ×N. A
//! pending step shows only what its response reported; a sealed one adds its own results. An
//! arrival's figure is its share of its step's seal. User input has no arrival item: the feed
//! bills it on the user's own item when it can place it, and on the admitting step otherwise.

use super::text::token_label;
use codex_app_server_protocol::StepAdmissionItem;
use codex_protocol::step_admission::Admission;
use codex_protocol::step_admission::AdmissionPrecision;
use codex_protocol::step_admission::ArrivalKind;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct StepBills {
    by_response: HashMap<String, StepAdmissionItem>,
    response_by_call: HashMap<String, String>,
    response_by_message: HashMap<String, String>,
    revision: u64,
}

/// What a feed header shows after its title.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Figures {
    /// The admitted total, or what stands in for it: `…` pending, `—` severed.
    pub(super) total: Total,
    /// What a step's total is made of.
    pub(super) split: Option<Split>,
    pub(super) notes: Vec<String>,
}

/// A step's thought, output and own results; zero or unreported parts are absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Split {
    pub(super) thought: Option<String>,
    pub(super) output: Option<String>,
    /// Its own results, or what stands in for them: `…` pending, `—` severed.
    pub(super) own: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Total {
    #[default]
    Unknown,
    Pending,
    Severed,
    Exact(u64),
    Approximate(u64),
}

impl StepBills {
    /// Advances whenever a record changes, so derived presentation can tell it is stale.
    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn step(&self, response_id: &str) -> Option<&StepAdmissionItem> {
        self.by_response.get(response_id)
    }

    pub(super) fn step_of_call(&self, call_id: &str) -> Option<&str> {
        self.response_by_call.get(call_id).map(String::as_str)
    }

    pub(super) fn step_of_message(&self, message_id: &str) -> Option<&str> {
        self.response_by_message.get(message_id).map(String::as_str)
    }

    pub(super) fn record(&mut self, item: StepAdmissionItem) -> bool {
        // A live pending record must not overwrite the record that sealed it.
        if item.admission == Admission::Pending && self.by_response.contains_key(&item.response_id)
        {
            return false;
        }
        if self.by_response.get(&item.response_id) == Some(&item) {
            return false;
        }
        for call_id in &item.call_ids {
            self.response_by_call
                .insert(call_id.clone(), item.response_id.clone());
        }
        for message_id in &item.message_ids {
            self.response_by_message
                .insert(message_id.clone(), item.response_id.clone());
        }
        self.by_response.insert(item.response_id.clone(), item);
        self.revision += 1;
        true
    }
}

/// The figures of several steps, less the user input the feed billed on users' own items.
pub(super) fn steps_figures(
    steps: &[&StepAdmissionItem],
    placed_users: &dyn Fn(&str) -> usize,
) -> Figures {
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
                    .skip(placed_users(&step.response_id))
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
        (0, 0) => Some("—".to_string()),
        (0, _) => Some("…".to_string()),
        (_, 0) => (own > 0).then(|| format!("{approx}{}", token_label(own))),
        (_, _) => Some(format!("{approx}{}+…", token_label(own))),
    };
    let mut notes = Vec::new();
    if steps.len() > 1 {
        notes.push(format!("×{}", steps.len()));
    }
    // A step's calls fold under its first; say how many when one step made several.
    let calls = steps.iter().map(|step| step.call_ids.len()).sum::<usize>();
    if calls > steps.len() {
        notes.push(format!("{calls} calls"));
    }
    if user > 0 {
        notes.push(format!("user {approx}{}", token_label(user)));
    }
    if released > 0 {
        notes.push(format!("released {}", token_label(released)));
    }
    if severed > 0 {
        notes.push("severed".to_string());
    }
    Figures {
        total: Total::Exact(thinking.unwrap_or(0) + output + own),
        split: Some(Split {
            thought: thinking.filter(|thinking| *thinking > 0).map(token_label),
            output: (output > 0).then(|| token_label(output)),
            own: own_label,
        }),
        notes,
    }
}

/// An arrival's share of its step's seal.
pub(super) fn arrival_figures(step: Option<&StepAdmissionItem>, id: &str) -> Figures {
    let total = match step.map(|step| &step.admission) {
        Some(Admission::Sealed(sealed)) => sealed
            .arrivals
            .iter()
            .find(|arrival| arrival.id == id)
            .map_or(Total::Severed, |arrival| {
                share(sealed.precision, arrival.tokens)
            }),
        Some(Admission::Severed) => Total::Severed,
        Some(Admission::Pending) | None => Total::Pending,
    };
    Figures {
        total,
        ..Figures::default()
    }
}

/// The share of the `ordinal`th user input the step admitted.
pub(super) fn user_figures(step: Option<&StepAdmissionItem>, ordinal: usize) -> Figures {
    let total = match step.map(|step| &step.admission) {
        Some(Admission::Sealed(sealed)) => sealed
            .arrivals
            .iter()
            .filter(|arrival| arrival.kind == ArrivalKind::User)
            .nth(ordinal)
            .map_or(Total::Unknown, |arrival| {
                share(sealed.precision, arrival.tokens)
            }),
        Some(Admission::Severed) => Total::Severed,
        Some(Admission::Pending) => Total::Pending,
        None => Total::Unknown,
    };
    Figures {
        total,
        ..Figures::default()
    }
}

fn share(precision: AdmissionPrecision, tokens: u64) -> Total {
    if precision == AdmissionPrecision::Exact {
        Total::Exact(tokens)
    } else {
        Total::Approximate(tokens)
    }
}
