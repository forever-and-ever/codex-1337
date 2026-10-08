//! What each model request admitted into its context, a human-only record that is never
//! model input.
//!
//! A step is one main-model request and its response. The next request's provider-reported
//! input, less this request's input and the response's own re-sent output, is what the step's
//! results and the arrivals after them added. Counts are gross: tokens that later leave the
//! context, such as dropped earlier thinking, are reported where they leave.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// One request's bill: what it read, what its response produced and what its step admitted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct StepAdmissionItem {
    pub id: String,
    pub response_id: String,
    #[ts(type = "number")]
    pub completed_at_ms: i64,
    /// Provider-reported input the request read, cached or not.
    #[ts(type = "number")]
    pub read_tokens: u64,
    /// Provider-reported reasoning, when the provider breaks it out of the output.
    #[ts(type = "number | null")]
    pub thinking_tokens: Option<u64>,
    /// Provider-reported output, less any reported reasoning.
    #[ts(type = "number")]
    pub output_tokens: u64,
    pub admission: Admission,
}

/// The step's admission, known once the next request completes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(tag = "state", rename_all = "camelCase")]
#[ts(tag = "state", rename_all = "camelCase", export_to = "v2/")]
pub enum Admission {
    /// The next request has not completed.
    Pending,
    /// The context was rewritten or the model changed before the next request completed.
    Severed,
    Sealed(SealedAdmission),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SealedAdmission {
    /// The calls whose results the step admitted.
    pub call_ids: Vec<String>,
    /// The step's own results, with the framing of its items.
    #[ts(type = "number")]
    pub self_tokens: u64,
    /// Context written after the step's results by neither the step nor its model.
    pub arrivals: Vec<Arrival>,
    /// Tokens that left the context during the step.
    #[ts(type = "number")]
    pub released_tokens: u64,
    pub precision: AdmissionPrecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AdmissionPrecision {
    /// The provider's delta alone measured every share.
    Exact,
    /// Local o200k counts split the provider's delta among the shares.
    Apportioned,
    /// The delta fell short of the local counts: the shares are local estimates and the
    /// shortfall left the context.
    Disturbed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Arrival {
    pub kind: ArrivalKind,
    pub label: String,
    #[ts(type = "number")]
    pub tokens: u64,
    /// The readable content of an injection or of a plaintext message from another agent.
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ArrivalKind {
    /// Context the harness wrote: world-state updates, interrupt markers, receipts, reminders.
    Injection,
    /// A message from another agent.
    InterAgent,
    /// User input, steered into a running turn or starting the next one.
    User,
}
