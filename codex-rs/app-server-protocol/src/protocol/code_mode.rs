//! Ownership adapters preserve native payloads and reject nested transcript scopes.

use crate::ExperimentalApi;
use crate::ServerNotification;
use crate::ThreadItem;
pub use codex_protocol::code_mode::CodeModeOwned;
pub use codex_protocol::code_mode::CodeModeOwner;
use codex_protocol::code_mode::CodeModePayload;

pub type CodeModeOwnedNotification = CodeModeOwned<Box<ServerNotification>>;

impl ExperimentalApi for CodeModeOwnedNotification {
    fn experimental_reason(&self) -> Option<&'static str> {
        self.payload().experimental_reason()
    }
}

impl CodeModePayload for ServerNotification {
    fn is_code_mode_owned(&self) -> bool {
        match self {
            Self::CodeModeOwned(_) => true,
            Self::ItemStarted(item) => item.item.is_code_mode_owned(),
            Self::ItemCompleted(item) => item.item.is_code_mode_owned(),
            _ => false,
        }
    }
}

impl CodeModePayload for ThreadItem {
    fn is_code_mode_owned(&self) -> bool {
        matches!(self, Self::CodeModeOwned(_))
    }
}

impl ThreadItem {
    pub fn into_native_item(self) -> Self {
        match self {
            Self::CodeModeOwned(owned) => *owned.into_parts().1,
            item => item,
        }
    }

    pub fn native_item(&self) -> &Self {
        match self {
            Self::CodeModeOwned(owned) => owned.payload(),
            item => item,
        }
    }
}

impl ServerNotification {
    pub fn into_native_notification(self) -> Self {
        match self {
            Self::CodeModeOwned(owned) => *owned.into_parts().1,
            notification => notification,
        }
    }

    pub fn native_notification(&self) -> &Self {
        match self {
            Self::CodeModeOwned(owned) => owned.payload(),
            notification => notification,
        }
    }

    /// Non-transcript side effects, including approvals and thread status, stay native.
    pub fn with_code_mode_owner(self, owner: &CodeModeOwner) -> Self {
        let thread_id = match &self {
            Self::ItemStarted(item) => Some(item.thread_id.as_str()),
            Self::ItemCompleted(item) => Some(item.thread_id.as_str()),
            Self::CommandExecutionOutputDelta(item) => Some(item.thread_id.as_str()),
            Self::TerminalInteraction(item) => Some(item.thread_id.as_str()),
            Self::FileChangeOutputDelta(item) => Some(item.thread_id.as_str()),
            Self::FileChangePatchUpdated(item) => Some(item.thread_id.as_str()),
            Self::McpToolCallProgress(item) => Some(item.thread_id.as_str()),
            Self::CodeModeOwned(_) => panic!("nested Code Mode ownership is forbidden"),
            _ => None,
        };
        if thread_id.is_some_and(|thread_id| thread_id == owner.thread_id.to_string()) {
            Self::CodeModeOwned(CodeModeOwned::new(owner.clone(), Box::new(self)))
        } else {
            self
        }
    }
}
