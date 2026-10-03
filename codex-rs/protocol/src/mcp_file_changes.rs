//! Admission of the inline, human-transcript-only MCP file-change receipt.
//!
//! `mcp-file-changes/v1.schema.json` (including `x-codex-validation`) owns the contract.
//! These types implement that contract; they do not grant filesystem or replay authority.

use std::collections::HashSet;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;
use std::io;
use std::io::Write;
use url::Url;

pub const FILE_CHANGES_META_KEY: &str = "codex/fileChanges";
const COUNT_MAX: u64 = 9_007_199_254_740_991;

struct AttachmentBudget(usize);

impl Write for AttachmentBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        if self.0 > 262_144 {
            return Err(io::Error::other("receipt exceeds inline budget"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A bounded receipt admitted for human presentation, never model input.
#[derive(Clone, Debug)]
pub struct FileChangeReceipt(Receipt);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    version: u8,
    outcome: PublicationOutcome,
    files: Vec<FileChangeRecord>,
    omitted_files: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PublicationOutcome {
    Preview,
    Applied,
    NotApplied,
    Partial,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FilePublication {
    Preview,
    Applied,
    NotApplied,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FileOperation {
    Create,
    Modify,
    Delete,
    Rename,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryReason {
    Binary,
    NonUtf8,
    Oversized,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileSnapshot {
    pub uri: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileChangeRecord {
    #[serde(rename_all = "camelCase")]
    Text {
        publication: FilePublication,
        operation: FileOperation,
        #[serde(deserialize_with = "required_nullable")]
        before: Option<FileSnapshot>,
        #[serde(deserialize_with = "required_nullable")]
        after: Option<FileSnapshot>,
        unified_diff: String,
    },
    Summary {
        publication: FilePublication,
        operation: FileOperation,
        #[serde(deserialize_with = "required_nullable")]
        before: Option<FileSnapshot>,
        #[serde(deserialize_with = "required_nullable")]
        after: Option<FileSnapshot>,
        reason: SummaryReason,
    },
    #[serde(rename_all = "camelCase")]
    PathOnly {
        publication: FilePublication,
        operation: FileOperation,
        #[serde(deserialize_with = "required_nullable")]
        before_uri: Option<String>,
        #[serde(deserialize_with = "required_nullable")]
        after_uri: Option<String>,
    },
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

impl FileChangeReceipt {
    /// Rejects an attachment as a whole; rejection never changes the actual tool result.
    pub fn parse(value: &Value) -> Option<Self> {
        serde_json::to_writer(AttachmentBudget(0), value).ok()?;
        let receipt: Receipt = serde_json::from_value(value.clone()).ok()?;
        if receipt.version != 1 || receipt.files.len() > 128 || receipt.omitted_files > COUNT_MAX {
            return None;
        }
        let mut before_uris = HashSet::new();
        let mut after_uris = HashSet::new();
        for file in &receipt.files {
            let (publication, operation, before, after) = file.identities();
            let allowed = match receipt.outcome {
                PublicationOutcome::Preview => publication == FilePublication::Preview,
                PublicationOutcome::Applied => publication == FilePublication::Applied,
                PublicationOutcome::NotApplied => publication == FilePublication::NotApplied,
                PublicationOutcome::Partial => matches!(
                    publication,
                    FilePublication::Applied | FilePublication::NotApplied
                ),
                PublicationOutcome::Unknown => publication != FilePublication::Preview,
            };
            if !allowed || !valid_operation(operation, before, after) || !file.valid_content() {
                return None;
            }
            for (uri, seen) in [(before, &mut before_uris), (after, &mut after_uris)] {
                if let Some(uri) = uri
                    && (!valid_uri(uri) || !seen.insert(uri))
                {
                    return None;
                }
            }
        }
        if receipt.outcome == PublicationOutcome::Partial
            && receipt.omitted_files == 0
            && !receipt
                .files
                .iter()
                .any(|file| file.identities().0 == FilePublication::Applied)
        {
            return None;
        }
        Some(Self(receipt))
    }

    pub fn outcome(&self) -> PublicationOutcome {
        self.0.outcome
    }
    pub fn files(&self) -> &[FileChangeRecord] {
        &self.0.files
    }
    pub fn into_files(self) -> Vec<FileChangeRecord> {
        self.0.files
    }
    pub fn omitted_files(&self) -> u64 {
        self.0.omitted_files
    }

    /// Reuses the existing `_meta` carrier only on the human transcript copy.
    pub fn into_value(self) -> Value {
        serde_json::to_value(self.0).expect("validated receipt is serializable")
    }
}

impl FileChangeRecord {
    pub fn identities(&self) -> (FilePublication, FileOperation, Option<&str>, Option<&str>) {
        match self {
            Self::Text {
                publication,
                operation,
                before,
                after,
                ..
            }
            | Self::Summary {
                publication,
                operation,
                before,
                after,
                ..
            } => (
                *publication,
                *operation,
                before.as_ref().map(|s| s.uri.as_str()),
                after.as_ref().map(|s| s.uri.as_str()),
            ),
            Self::PathOnly {
                publication,
                operation,
                before_uri,
                after_uri,
            } => (
                *publication,
                *operation,
                before_uri.as_deref(),
                after_uri.as_deref(),
            ),
        }
    }

    fn valid_content(&self) -> bool {
        match self {
            Self::PathOnly { publication, .. } => matches!(
                publication,
                FilePublication::NotApplied | FilePublication::Unknown
            ),
            Self::Text {
                publication,
                operation,
                before,
                after,
                unified_diff,
            } => {
                valid_snapshots(*publication, *operation, before, after)
                    && valid_diff(*operation, before, after, unified_diff)
            }
            Self::Summary {
                publication,
                operation,
                before,
                after,
                ..
            } => valid_snapshots(*publication, *operation, before, after),
        }
    }
}

fn valid_operation(operation: FileOperation, before: Option<&str>, after: Option<&str>) -> bool {
    match operation {
        FileOperation::Create => before.is_none() && after.is_some(),
        FileOperation::Delete => before.is_some() && after.is_none(),
        FileOperation::Modify => before.is_some() && before == after,
        FileOperation::Rename => before.is_some() && after.is_some() && before != after,
    }
}

fn valid_uri(uri: &str) -> bool {
    let Some(tail) = uri.strip_prefix("file://") else {
        return false;
    };
    if uri.len() > 4096
        || uri.contains('\\')
        || uri.chars().any(|c| c.is_control() || c.is_whitespace())
        || !tail.contains('/')
        || tail
            .split('/')
            .next()
            .is_some_and(|host| host.contains('@'))
    {
        return false;
    }
    let bytes = uri.as_bytes();
    if bytes.iter().enumerate().any(|(i, b)| {
        *b == b'%'
            && (!bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                || !bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
    }) {
        return false;
    }
    Url::parse(uri).is_ok_and(|url| {
        url.scheme() == "file"
            && url.path().starts_with('/')
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

fn same_content(before: &Option<FileSnapshot>, after: &Option<FileSnapshot>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if before.sha256 == after.sha256 && before.bytes == after.bytes)
}

fn valid_snapshots(
    publication: FilePublication,
    operation: FileOperation,
    before: &Option<FileSnapshot>,
    after: &Option<FileSnapshot>,
) -> bool {
    matches!(
        publication,
        FilePublication::Applied | FilePublication::Preview
    ) && before.iter().chain(after).all(|s| {
        s.bytes <= COUNT_MAX
            && s.sha256.len() == 64
            && s.sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) && (operation != FileOperation::Modify || !same_content(before, after))
}

fn valid_diff(
    operation: FileOperation,
    before: &Option<FileSnapshot>,
    after: &Option<FileSnapshot>,
    diff: &str,
) -> bool {
    if diff.len() > 65_536 {
        return false;
    }
    if diff.is_empty() {
        return match operation {
            FileOperation::Create => after.as_ref().is_some_and(|s| s.bytes == 0),
            FileOperation::Delete => before.as_ref().is_some_and(|s| s.bytes == 0),
            FileOperation::Rename => same_content(before, after),
            FileOperation::Modify => false,
        };
    }
    let mut lines = diff.lines();
    if !lines.next().is_some_and(|l| l.starts_with("--- "))
        || !lines.next().is_some_and(|l| l.starts_with("+++ "))
    {
        return false;
    }
    // diffy's range arithmetic is unchecked. Bound hostile header numbers before parsing.
    for line in diff.lines().filter(|line| line.starts_with("@@ ")) {
        for range in line.split_whitespace().skip(1).take(2) {
            let mut numbers = range.get(1..).unwrap_or_default().split(',');
            let Ok(start) = numbers.next().unwrap_or_default().parse::<u64>() else {
                return false;
            };
            let Ok(len) = numbers.next().unwrap_or("1").parse::<u64>() else {
                return false;
            };
            if start > COUNT_MAX
                || len > COUNT_MAX
                || start
                    .checked_add(len)
                    .and_then(|end| usize::try_from(end).ok())
                    .is_none()
            {
                return false;
            }
        }
    }
    let Ok(patch) = diffy::Patch::from_str(diff) else {
        return false;
    };
    // The parser checks complete hunk counts, ordering and trailing/multiple-file garbage.
    if patch.hunks().is_empty()
        || !patch
            .hunks()
            .iter()
            .flat_map(diffy::Hunk::lines)
            .any(|line| matches!(line, diffy::Line::Insert(_) | diffy::Line::Delete(_)))
    {
        return false;
    }
    match operation {
        FileOperation::Modify | FileOperation::Rename => true,
        FileOperation::Create | FileOperation::Delete => {
            let [hunk] = patch.hunks() else {
                return false;
            };
            let (absent, present, snapshot) = if operation == FileOperation::Create {
                (hunk.old_range(), hunk.new_range(), after)
            } else {
                (hunk.new_range(), hunk.old_range(), before)
            };
            absent.start() == 0
                && absent.is_empty()
                && present.start() == 1
                && snapshot.as_ref().is_some_and(|s| {
                    s.bytes
                        == hunk
                            .lines()
                            .iter()
                            .map(|line| match line {
                                diffy::Line::Insert(text)
                                | diffy::Line::Delete(text)
                                | diffy::Line::Context(text) => text.len() as u64,
                            })
                            .sum::<u64>()
                })
        }
    }
}

/// Removes private source before extension/Guardian callbacks, history, and model projection.
pub fn take_file_changes(result: &mut crate::mcp::CallToolResult) -> Option<FileChangeReceipt> {
    let value = result
        .meta
        .as_mut()?
        .as_object_mut()?
        .remove(FILE_CHANGES_META_KEY)?;
    FileChangeReceipt::parse(&value)
}

#[cfg(test)]
#[path = "mcp_file_changes_tests.rs"]
mod tests;
