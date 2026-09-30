//! Returned batches share their enclosing activity's disclosure; they are not additional calls.

use super::HistoryCell;
use super::ReasoningSummaryCell;
use super::ToolOutputCell;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use ratatui::text::Line;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct ToolOutputGroup {
    members: Vec<Arc<dyn HistoryCell>>,
}

impl ToolOutputGroup {
    pub(crate) fn detail_members(&self) -> Option<&[Arc<dyn HistoryCell>]> {
        self.members
            .iter()
            .all(|cell| {
                cell.as_any().is::<ToolOutputCell>()
                    || cell
                        .as_any()
                        .downcast_ref::<ReasoningSummaryCell>()
                        .is_some_and(ReasoningSummaryCell::is_transcript_only)
            })
            .then_some(&self.members)
    }
    /// Consume only the contiguous activity suffix. Messages and turn separators are barriers.
    pub(crate) fn attach(
        cells: &[Arc<dyn HistoryCell>],
        output: Arc<dyn HistoryCell>,
    ) -> Option<(usize, Arc<dyn HistoryCell>)> {
        output.as_any().downcast_ref::<ToolOutputCell>()?;
        let mut start = cells.len();
        let mut has_activity = false;
        for cell in cells.iter().rev() {
            // Code-only batches have no activity header to share; leave them standalone.
            if cell.as_any().is::<ToolOutputCell>() {
                break;
            }
            let ids = cell.activity_ids();
            if ids.is_empty() {
                if cell
                    .as_any()
                    .downcast_ref::<ReasoningSummaryCell>()
                    .is_some_and(ReasoningSummaryCell::is_transcript_only)
                {
                    start -= 1;
                    continue;
                }
                break;
            }
            if ids.iter().any(|id| id.starts_with("output:")) {
                if !has_activity {
                    start -= 1;
                    has_activity = true;
                }
                break;
            }
            has_activity = true;
            start -= 1;
        }
        if !has_activity || start == cells.len() {
            return None;
        }
        Some((
            start,
            Self::from_cells(cells[start..].iter().cloned().chain([output])),
        ))
    }

    pub(crate) fn from_cells(
        cells: impl IntoIterator<Item = Arc<dyn HistoryCell>>,
    ) -> Arc<dyn HistoryCell> {
        let mut group = Self {
            members: Vec::new(),
        };
        for cell in cells {
            if let Some(old) = cell.as_any().downcast_ref::<Self>() {
                group.members.extend(old.members.iter().cloned());
            } else {
                group.members.push(cell);
            }
        }
        let mut seen = HashSet::new();
        group.members.retain(|cell| {
            let mut ids = cell
                .activity_ids()
                .into_iter()
                .filter(|id| id.starts_with("output:"));
            if cell.as_any().is::<ToolOutputCell>() {
                ids.any(|id| seen.insert(id))
            } else {
                seen.extend(ids);
                true
            }
        });
        Arc::new(group)
    }

    /// Page joins require persisted adjacency within one turn, not matching text or proximity.
    pub(crate) fn join(
        older: &Arc<dyn HistoryCell>,
        newer: &Arc<dyn HistoryCell>,
        turns: &[Turn],
    ) -> Option<Arc<dyn HistoryCell>> {
        if !older.as_any().is::<Self>() && !newer.as_any().is::<Self>() {
            return None;
        }
        if older
            .activity_ids()
            .iter()
            .any(|id| id.starts_with("output:"))
            && newer
                .as_any()
                .downcast_ref::<Self>()
                .is_none_or(|group| group.detail_members().is_none())
        {
            return None;
        }
        let ids = older
            .activity_ids()
            .into_iter()
            .chain(newer.activity_ids())
            .collect::<Vec<_>>();
        let ids = ids
            .iter()
            .map(|id| {
                let (_, source) = id.split_once(':')?;
                Some(source.strip_prefix("tool-output-tokens:").unwrap_or(source))
            })
            .collect::<Option<HashSet<_>>>()?;
        if ids.is_empty() {
            return None;
        }
        turns.iter().find_map(|turn| {
            let start = turn.items.iter().position(|item| ids.contains(item.id()))?;
            let end = turn
                .items
                .iter()
                .rposition(|item| ids.contains(item.id()))?;
            let items = &turn.items[start..=end];
            let present = items
                .iter()
                .map(codex_app_server_protocol::ThreadItem::id)
                .collect::<HashSet<_>>();
            (ids.is_subset(&present)
                && items.iter().all(|item| {
                    ids.contains(item.id())
                        || matches!(
                            item,
                            ThreadItem::Reasoning { .. } | ThreadItem::ToolOutputTokens { .. }
                        )
                }))
            .then(|| Self::from_cells([Arc::clone(older), Arc::clone(newer)]))
        })
    }
}

impl HistoryCell for ToolOutputGroup {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.compact_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.compact_hyperlink_lines(width)
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = Vec::new();
        for cell in self
            .members
            .iter()
            .filter(|cell| !cell.as_any().is::<ToolOutputCell>())
        {
            let next = cell.compact_hyperlink_lines(width);
            if !next.is_empty() {
                if !lines.is_empty() {
                    lines.push("".into());
                }
                lines.extend(next);
            }
        }
        if lines.is_empty() {
            return self
                .members
                .iter()
                .find_map(|cell| {
                    let preview = cell.compact_hyperlink_lines(width);
                    (!preview.is_empty()).then_some(preview)
                })
                .unwrap_or_default();
        }
        lines
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = if self
            .members
            .iter()
            .filter(|cell| !cell.as_any().is::<ToolOutputCell>())
            .all(|cell| cell.compact_hyperlink_lines(width).is_empty())
        {
            self.compact_hyperlink_lines(width)
        } else {
            Vec::new()
        };
        for cell in &self.members {
            if cell.as_any().is::<ToolOutputCell>() {
                if !lines.is_empty() {
                    lines.push("".into());
                }
                lines.extend(cell.transcript_hyperlink_lines(width));
                continue;
            }
            // An existing exploration group can already own earlier returned batches.
            if cell
                .activity_ids()
                .iter()
                .any(|id| id.starts_with("output:"))
            {
                if !lines.is_empty() {
                    lines.push("".into());
                }
                lines.extend(cell.expanded_hyperlink_lines(width));
            } else {
                let next = cell.activity_argument_lines(width);
                if !next.is_empty() && !lines.is_empty() {
                    lines.push("".into());
                }
                lines.extend(next);
            }
        }
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.expanded_hyperlink_lines(width))
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn activity_ids(&self) -> Vec<String> {
        self.members
            .iter()
            .flat_map(|cell| cell.activity_ids())
            .collect()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.members
            .iter()
            .flat_map(|cell| cell.raw_lines())
            .collect()
    }

    fn is_stream_continuation(&self) -> bool {
        self.members
            .first()
            .is_none_or(|cell| cell.is_stream_continuation())
    }
}
