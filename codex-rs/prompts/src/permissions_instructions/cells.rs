//! On-request approval text for Code Mode Only sessions. Their `exec_command` takes no
//! `justification` or `prefix_rule`, so the text asks for the bare escalation.

use std::sync::LazyLock;

use super::AUTO_REVIEW_SUFFIX;
use super::ApprovalPromptContext;
use crate::ResolvedMessage;

const ON_REQUEST: &str =
    include_str!("../../templates/permissions/approval_policy/cell_on_request.md");
static ON_REQUEST_AUTO_REVIEW: LazyLock<String> =
    LazyLock::new(|| format!("{}\n\n{AUTO_REVIEW_SUFFIX}", ON_REQUEST.trim_end()));

impl ApprovalPromptContext<'_> {
    /// Like catalog text, the cell text is complete: nothing is appended to it.
    pub fn for_cells(mut self) -> Self {
        self.messages.on_request = ResolvedMessage::Catalog(ON_REQUEST);
        self.messages.on_request_auto_review =
            ResolvedMessage::Catalog(ON_REQUEST_AUTO_REVIEW.as_str());
        self
    }
}
