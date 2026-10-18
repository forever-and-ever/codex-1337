//! Human transcript facts. These are never inserted into model-visible history.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Durable transcript identity, independent of the runtime and attached clients.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOwner {
    pub thread_id: crate::ThreadId,
    pub exec_call_id: String,
    pub cell_id: Option<String>,
    #[ts(type = "number")]
    pub started_at_ms: i64,
}

/// A payload boundary must reject another owned envelope, rather than flattening it.
pub trait CodeModePayload {
    fn is_code_mode_owned(&self) -> bool;
}

/// One ownership layer around an unchanged human-transcript payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOwned<T> {
    pub owner: CodeModeOwner,
    payload: T,
}

impl<T: CodeModePayload> CodeModeOwned<T> {
    pub fn new(owner: CodeModeOwner, payload: T) -> Self {
        assert!(
            !payload.is_code_mode_owned(),
            "nested Code Mode ownership is forbidden"
        );
        Self { owner, payload }
    }
}

impl<T> CodeModeOwned<T> {
    pub fn payload(&self) -> &T {
        &self.payload
    }

    pub fn into_parts(self) -> (CodeModeOwner, T) {
        (self.owner, self.payload)
    }
}

impl<'de, T: Deserialize<'de> + CodeModePayload> Deserialize<'de> for CodeModeOwned<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Input<T> {
            owner: CodeModeOwner,
            payload: T,
        }
        let Input { owner, payload }: Input<T> = Input::deserialize(deserializer)?;
        if payload.is_code_mode_owned() {
            return Err(serde::de::Error::custom(
                "nested Code Mode ownership is forbidden",
            ));
        }
        Ok(Self::new(owner, payload))
    }
}

impl<T: CodeModePayload + ?Sized> CodeModePayload for Box<T> {
    fn is_code_mode_owned(&self) -> bool {
        self.as_ref().is_code_mode_owned()
    }
}

impl CodeModePayload for crate::protocol::EventMsg {
    fn is_code_mode_owned(&self) -> bool {
        matches!(self, Self::CodeModeOwned(_))
    }
}

impl crate::protocol::EventMsg {
    /// Completed children release attribution; a yielded PTY still owns later updates.
    pub fn code_mode_completed_child(&self) -> Option<std::borrow::Cow<'_, str>> {
        use crate::items::CommandExecutionStatus;
        use crate::items::TurnItem;
        match self.native_event() {
            Self::ItemCompleted(event) => match &event.item {
                TurnItem::ToolInput(_) => None,
                TurnItem::CommandExecution(item)
                    if item.status == CommandExecutionStatus::InProgress =>
                {
                    None
                }
                _ => self.code_mode_call_id(),
            },
            Self::ExecCommandEnd(_) | Self::McpToolCallEnd(_) | Self::PatchApplyEnd(_) => {
                self.code_mode_call_id()
            }
            _ => None,
        }
    }

    /// Observers outside human-transcript projection retain native event semantics.
    pub fn native_event(&self) -> &Self {
        match self {
            Self::CodeModeOwned(owned) => owned.payload(),
            event => event,
        }
    }

    /// Only transcript payloads keyed to an invocation are eligible for ownership.
    pub fn code_mode_call_id(&self) -> Option<std::borrow::Cow<'_, str>> {
        use crate::items::TurnItem;
        use crate::protocol::EventMsg;
        use std::borrow::Cow;
        match self {
            EventMsg::CodeModeOwned(owned) => owned.payload().code_mode_call_id(),
            EventMsg::ItemStarted(event) => Some(Cow::Owned(event.item.id())),
            EventMsg::ItemCompleted(event) => match &event.item {
                TurnItem::CodeMode(item) => Some(Cow::Borrowed(&item.exec_call_id)),
                TurnItem::FunctionCallOutput(item) => item.call_id.as_deref().map(Cow::Borrowed),
                TurnItem::ToolInput(item) => Some(Cow::Borrowed(&item.call_id)),
                TurnItem::ToolOutputTokens(item) => Some(Cow::Borrowed(&item.call_id)),
                item => Some(Cow::Owned(item.id())),
            },
            EventMsg::McpToolCallBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::McpToolCallEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::ExecApprovalRequest(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::GuardianAssessment(event) => {
                event.target_item_id.as_deref().map(Cow::Borrowed)
            }
            EventMsg::ExecCommandBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::ExecCommandEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::ExecCommandOutputDelta(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::TerminalInteraction(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::PatchApplyBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::PatchApplyEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::PatchApplyUpdated(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::DynamicToolCallResponse(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::WebSearchBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::WebSearchEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::ViewImageToolCall(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::ImageGenerationBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::ImageGenerationEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabAgentSpawnBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabAgentSpawnEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabAgentInteractionBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabAgentInteractionEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabWaitingBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabWaitingEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabCloseBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabCloseEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabResumeBegin(event) => Some(Cow::Borrowed(&event.call_id)),
            EventMsg::CollabResumeEnd(event) => Some(Cow::Borrowed(&event.call_id)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeItem {
    pub id: String,
    /// Full runtime identity, including its session generation.
    pub cell_id: Option<String>,
    /// Original exec invocation: durable within the thread even if a runtime restarts.
    pub exec_call_id: String,
    #[ts(type = "number")]
    pub created_at: i64,
    pub event: CodeModeEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export_to = "v2/")]
pub enum CodeModeEvent {
    Started {
        call_id: String,
        /// The exec input as the model wrote it; records from before it was kept have none.
        script: Option<String>,
    },
    Allocated,
    ToolCall {
        call_id: String,
    },
    WaitStarted {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        target: Option<CodeModeOwner>,
    },
    WaitCompleted {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        target: Option<CodeModeOwner>,
    },
    Finished {
        status: CodeModeStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum CodeModeStatus {
    Completed,
    Failed,
    Interrupted,
}
