use crate::JsonSchema;
use crate::TS;
use codex_utils_path_uri::LegacyAppPathString;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum JiraCategory {
    Bug,
    Feature,
}

impl JiraCategory {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Feature => "feature",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct JiraKey {
    /// Absolute stored project namespace; null selects the browsing cwd's default project.
    pub project: Option<LegacyAppPathString>,
    pub category: JiraCategory,
    pub slug: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct JiraIssue {
    pub revision: JiraRevision,
    pub body: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct JiraRevision {
    pub key: JiraKey,
    pub epic: Option<String>,
    /// SHA-256 of the exact stored UTF-8 bytes; optimistic external-edit guard.
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    export_to = "v2/"
)]
pub enum JiraAction {
    List {
        /// Include stored descendant projects. Older clients retain exact-project lists.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        recursive: bool,
        #[ts(optional = nullable)]
        cursor: Option<String>,
        #[ts(optional = nullable)]
        limit: Option<u32>,
    },
    Read {
        key: JiraKey,
    },
    Save {
        /// Omitted for creation. For edits, includes the fetched version and immutable epic.
        #[ts(optional = nullable)]
        previous: Option<JiraRevision>,
        key: JiraKey,
        body: String,
    },
    Delete {
        revision: JiraRevision,
    },
}

/// Server-local issue storage scoped by the persisted conversation cwd.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadJiraParams {
    pub thread_id: String,
    /// Scope returned by the initial list. Required for all reads and mutations.
    #[ts(optional = nullable)]
    pub expected_scope: Option<String>,
    pub action: JiraAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    export_to = "v2/"
)]
pub enum JiraResult {
    List {
        data: Vec<JiraKey>,
        next_cursor: Option<String>,
    },
    Issue {
        issue: JiraIssue,
    },
    Deleted,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadJiraResponse {
    /// The server's current conversation cwd, not the Git administrative/store identity.
    pub project: LegacyAppPathString,
    /// Opaque binding to both current cwd and the issue-store identity.
    pub scope: String,
    pub result: JiraResult,
}
