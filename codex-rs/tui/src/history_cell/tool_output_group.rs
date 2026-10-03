//! Returned batches share their enclosing activity's disclosure; they are not additional calls.

use super::HistoryCell;
use super::PatchHistoryCell;
use super::ReasoningSummaryCell;
use super::ToolOutputCell;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::Turn;
use ratatui::text::Line;
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct ToolOutputGroup {
    pub(super) members: Vec<Arc<dyn HistoryCell>>,
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
    /// Attach a matching outer return without inferring ownership from activity proximity.
    pub(crate) fn attach(
        cells: &[Arc<dyn HistoryCell>],
        output: Arc<dyn HistoryCell>,
    ) -> Option<(Range<usize>, Vec<Arc<dyn HistoryCell>>)> {
        output.as_any().downcast_ref::<ToolOutputCell>()?;
        if let Some(start) = Self::patch_owner(cells) {
            if cells[start].outer_call_id() != output.outer_call_id() {
                return None;
            }
            let details = cells[start + 1..].iter().filter(|cell| {
                cell.as_any()
                    .downcast_ref::<ReasoningSummaryCell>()
                    .is_some_and(ReasoningSummaryCell::is_transcript_only)
            });
            let group = Self::from_cells(
                std::iter::once(Arc::clone(&cells[start]))
                    .chain(details.cloned())
                    .chain([output]),
            );
            return Some((
                start..cells.len(),
                std::iter::once(group)
                    .chain(
                        cells[start + 1..]
                            .iter()
                            .filter(|cell| {
                                !cell
                                    .as_any()
                                    .downcast_ref::<ReasoningSummaryCell>()
                                    .is_some_and(ReasoningSummaryCell::is_transcript_only)
                            })
                            .cloned(),
                    )
                    .collect(),
            ));
        }
        // Only a concrete matching invocation owns this return. Legacy activity proximity
        // cannot establish parentage, and independently disclosed files remain separate.
        let cell = cells.last()?;
        if cell.as_any().is::<PatchHistoryCell>()
            || cell.outer_call_id().is_none()
            || cell.outer_call_id() != output.outer_call_id()
        {
            return None;
        }
        let start = cells.len() - 1;
        Some((
            start..cells.len(),
            vec![Self::from_cells(
                cells[start..].iter().cloned().chain([output]),
            )],
        ))
    }

    pub(crate) fn is_patch(cell: &dyn HistoryCell) -> bool {
        cell.as_any().is::<PatchHistoryCell>()
            || cell.as_any().downcast_ref::<Self>().is_some_and(|group| {
                group
                    .members
                    .first()
                    .is_some_and(|cell| cell.as_any().is::<PatchHistoryCell>())
            })
    }

    pub(crate) fn patch_owner(cells: &[Arc<dyn HistoryCell>]) -> Option<usize> {
        let mut owner = None;
        for (index, cell) in cells.iter().enumerate().rev() {
            if Self::is_patch(cell.as_ref()) {
                owner = Some(index);
            } else if !cell
                .as_any()
                .downcast_ref::<ReasoningSummaryCell>()
                .is_some_and(ReasoningSummaryCell::is_transcript_only)
            {
                break;
            }
            if owner.is_some() && !cell.is_stream_continuation() {
                break;
            }
        }
        owner
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
        // Page boundaries can split the computer summary inside one validated outer call.
        let mut members: Vec<Arc<dyn HistoryCell>> = Vec::new();
        for cell in group.members {
            if let Some(older) = members.last_mut()
                && let Some(previous) = older.as_any().downcast_ref::<super::ComputerActivityCell>()
                && let Some(next) = cell.as_any().downcast_ref::<super::ComputerActivityCell>()
            {
                let mut merged = next.clone();
                merged.prepend(previous.clone());
                *older = Arc::new(merged);
            } else {
                members.push(cell);
            }
        }
        group.members = members;
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

    /// Reunite every nested call from an older page, stopping at the previous outer return.
    pub(crate) fn join_prefix(
        cells: &[Arc<dyn HistoryCell>],
        mut start: usize,
        mut group: Arc<dyn HistoryCell>,
        turns: &[Turn],
    ) -> (usize, Arc<dyn HistoryCell>) {
        while start > 0 {
            let Some(joined) = Self::join(&cells[start - 1], &group, turns) else {
                break;
            };
            start -= 1;
            group = joined;
        }
        (start, group)
    }

    /// Page joins require persisted adjacency within one turn, not matching text or proximity.
    pub(crate) fn join(
        older: &Arc<dyn HistoryCell>,
        newer: &Arc<dyn HistoryCell>,
        turns: &[Turn],
    ) -> Option<Arc<dyn HistoryCell>> {
        if newer.as_any().is::<PatchHistoryCell>() {
            return None;
        }
        let same_call =
            older.outer_call_id().is_some() && older.outer_call_id() == newer.outer_call_id();
        if !same_call {
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
    fn activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        self.members
            .iter()
            .find(|cell| {
                !cell.as_any().is::<ToolOutputCell>()
                    && !cell
                        .as_any()
                        .downcast_ref::<ReasoningSummaryCell>()
                        .is_some_and(ReasoningSummaryCell::is_transcript_only)
                    && !cell
                        .as_any()
                        .downcast_ref::<crate::exec_cell::ExecCell>()
                        .is_some_and(crate::exec_cell::ExecCell::compact_header_hidden)
            })
            .or_else(|| {
                self.members
                    .iter()
                    .rev()
                    .find(|cell| cell.as_any().is::<ToolOutputCell>())
            })
            .and_then(|cell| cell.activity_header())
    }
    fn expanded_activity_header(&self) -> Option<super::activity_parts::ActivityHeader> {
        let leading_command_is_hidden = self
            .members
            .iter()
            .find(|cell| {
                !cell.as_any().is::<ToolOutputCell>()
                    && !cell
                        .as_any()
                        .downcast_ref::<ReasoningSummaryCell>()
                        .is_some_and(ReasoningSummaryCell::is_transcript_only)
            })
            .is_some_and(|cell| {
                cell.as_any()
                    .downcast_ref::<crate::exec_cell::ExecCell>()
                    .is_some_and(crate::exec_cell::ExecCell::compact_header_hidden)
            });
        // Expansion reveals bare arguments for this interaction, not the next member's header.
        if leading_command_is_hidden {
            None
        } else {
            self.activity_header()
        }
    }
    fn outer_call_id(&self) -> Option<&str> {
        self.members
            .iter()
            .find_map(|member| member.outer_call_id())
    }

    fn activity_parts(
        &self,
        width: u16,
        header_reserve: usize,
        expanded: &HashSet<String>,
    ) -> Option<super::activity_parts::ActivityParts> {
        let mut commands = Vec::new();
        let mut batches = Vec::new();
        for member in &self.members {
            if let Some(command) = member.as_any().downcast_ref::<crate::exec_cell::ExecCell>() {
                if !command.is_agent_command_group() {
                    return None;
                }
                commands.push(command);
            } else if let Some(batch) = member.as_any().downcast_ref::<ToolOutputCell>() {
                batches.push(batch);
            } else if !member.as_any().is::<ReasoningSummaryCell>() {
                return None;
            }
        }
        (!commands.is_empty()).then(|| {
            crate::exec_cell::rows::command_parts(
                &commands,
                &batches,
                width,
                header_reserve,
                expanded,
                &self.activity_ids(),
            )
        })
    }

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
            lines.extend(cell.compact_hyperlink_lines(width));
        }
        if lines.is_empty() {
            // A code-only call can yield before finishing; its last return owns the status.
            return self
                .members
                .iter()
                .rev()
                .find_map(|cell| {
                    let preview = cell.compact_hyperlink_lines(width);
                    (!preview.is_empty()).then_some(preview)
                })
                .unwrap_or_default();
        }
        lines
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        if !self
            .activity_ids()
            .iter()
            .any(|id| id.starts_with("output:"))
        {
            return self
                .members
                .iter()
                .flat_map(|cell| cell.expanded_hyperlink_lines(width))
                .collect();
        }
        // The batch owns the bill, not the first file's disclosure. That reveals only its diff.
        if let Some(cell) = self.members.first()
            && cell.as_any().is::<PatchHistoryCell>()
        {
            return cell.expanded_hyperlink_lines(width);
        }
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
                if cell.activity_disclosure(width).is_none() {
                    continue;
                }
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
                let next = if cell.as_any().is::<PatchHistoryCell>() {
                    cell.expanded_hyperlink_lines(width)
                } else {
                    cell.activity_argument_lines(width)
                };
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

    fn activity_disclosure(&self, width: u16) -> Option<super::ActivityDisclosure> {
        if !self
            .activity_ids()
            .iter()
            .any(|id| id.starts_with("output:"))
        {
            return self
                .members
                .iter()
                .find_map(|cell| cell.activity_disclosure(width));
        }
        match self.members.first() {
            Some(cell) if cell.as_any().is::<PatchHistoryCell>() => cell.activity_disclosure(width),
            _ => self
                .members
                .iter()
                .any(|cell| {
                    if cell
                        .activity_ids()
                        .iter()
                        .any(|id| id.starts_with("output:"))
                    {
                        cell.activity_disclosure(width).is_some()
                    } else {
                        cell.activity_arguments_truncated(width)
                    }
                })
                .then_some(super::ActivityDisclosure::Generic),
        }
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
