//! Explicit execution ownership. Neither chronology nor cosmetic labels establish membership.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use super::ToolOutputCell;
use super::activity_parts::ActivityHeader;
use crate::motion::MotionMode;
use crate::motion::ReducedMotionIndicator;
use crate::motion::activity_indicator;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::CodeModeEvent;
use codex_app_server_protocol::CodeModeItem;
use codex_app_server_protocol::CodeModeStatus;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

#[path = "code_mode_projection.rs"]
mod projection;
pub(crate) use projection::reconcile_code_mode;
pub(crate) use projection::reunite_code_mode;
pub(crate) use projection::split_code_mode_members;

#[derive(Debug)]
pub(crate) struct CodeModeFactCell(pub(crate) CodeModeItem, pub(crate) bool);

fn label(identity: &str) -> String {
    const GREEK: &[char] = &[
        'α', 'β', 'γ', 'δ', 'ε', 'ζ', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ξ', 'ο', 'π', 'ρ', 'σ',
        'τ', 'υ', 'φ', 'χ', 'ψ', 'ω',
    ];
    // A fixed full-identity hash keeps labels stable across redraw, replay, and process restarts.
    let mut hash = identity.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    (0..4)
        .map(|_| {
            let glyph = GREEK[(hash % GREEK.len() as u64) as usize];
            hash /= GREEK.len() as u64;
            glyph
        })
        .collect()
}

impl HistoryCell for CodeModeFactCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let verb = match self.0.event {
            CodeModeEvent::WaitStarted { .. } => "Waiting",
            CodeModeEvent::WaitCompleted { .. } => "Waited",
            _ => return Vec::new(),
        };
        let mut spans = vec!["• ".dim()];
        spans.extend(
            HistoryTimestamp::from_millis(self.0.created_at.saturating_mul(1_000))
                .map(HistoryTimestamp::span),
        );
        spans.extend([
            verb.bold(),
            format!(" on {}", label(&self.0.exec_call_id)).into(),
        ]);
        vec![spans.into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        None
    }

    fn is_tool_entry(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CodeModeCell {
    pub(crate) exec_call_id: String,
    facts: Vec<CodeModeItem>,
    pub(crate) members: Vec<Arc<dyn HistoryCell>>,
    start_time: Instant,
    animations_enabled: bool,
}

impl CodeModeCell {
    pub(crate) fn is_active(&self) -> bool {
        self.status().is_none()
    }

    fn from_fact(fact: CodeModeItem, animations_enabled: bool) -> Self {
        Self {
            exec_call_id: fact.exec_call_id.clone(),
            facts: vec![fact],
            members: Vec::new(),
            start_time: Instant::now(),
            animations_enabled,
        }
    }

    fn status(&self) -> Option<(CodeModeStatus, i64)> {
        self.facts.iter().find_map(|fact| match fact.event {
            CodeModeEvent::Finished { status } => Some((status, fact.created_at)),
            _ => None,
        })
    }

    fn calls(&self) -> HashSet<&str> {
        std::iter::once(self.exec_call_id.as_str())
            .chain(self.facts.iter().filter_map(|fact| match &fact.event {
                CodeModeEvent::Started { call_id }
                | CodeModeEvent::ToolCall { call_id }
                | CodeModeEvent::WaitStarted { call_id }
                | CodeModeEvent::WaitCompleted { call_id } => Some(call_id.as_str()),
                CodeModeEvent::Finished { .. } => None,
            }))
            .collect()
    }

    pub(crate) fn owns(&self, cell: &dyn HistoryCell) -> bool {
        let ids = cell.tool_call_ids();
        !ids.is_empty() && ids.iter().all(|id| self.calls().contains(id))
    }

    fn insert_member(&mut self, incoming: Arc<dyn HistoryCell>) {
        if let Some(group) = incoming.as_any().downcast_ref::<super::ToolOutputGroup>() {
            for member in &group.members {
                self.insert_member(Arc::clone(member));
            }
            return;
        }
        let revisions = super::reconcile_tool_calls(&self.members, &incoming);
        for (index, replacement) in revisions.replacements {
            self.members[index] = replacement;
        }
        for member in revisions.remainder {
            if member.as_any().is::<super::ToolItemCell>()
                && let Some(index) = self.members.iter().position(|old| {
                    old.as_any().is::<super::ToolItemCell>()
                        && old.tool_call_ids() == member.tool_call_ids()
                })
            {
                self.members[index] = member;
                continue;
            }
            let ids = member.activity_ids();
            if !ids.is_empty() && self.members.iter().any(|old| old.activity_ids() == ids) {
                continue;
            }
            self.members.push(member);
        }
        self.members.sort_by_key(|cell| {
            let calls = cell.tool_call_ids();
            let index = self.facts.iter().position(|fact| matches!(&fact.event, CodeModeEvent::ToolCall { call_id } if calls.contains(&call_id.as_str())));
            (cell.as_any().is::<ToolOutputCell>(), index.unwrap_or(usize::MAX))
        });
    }

    fn insert_fact(&mut self, fact: CodeModeItem) {
        if !self.facts.iter().any(|old| old.id == fact.id) {
            self.facts.push(fact);
        }
    }

    fn header(&self) -> Option<Line<'static>> {
        let started = self
            .facts
            .iter()
            .find(|fact| matches!(fact.event, CodeModeEvent::Started { .. }));
        let terminal = self.status();
        let timestamp = terminal
            .map(|(_, timestamp)| timestamp)
            .or_else(|| started.map(|fact| fact.created_at))?;
        let (bullet, verb) = match terminal {
            Some((CodeModeStatus::Completed, _)) => ("•".green().bold(), "Called"),
            Some((CodeModeStatus::Failed, _)) => ("•".red().bold(), "Failed"),
            Some((CodeModeStatus::Interrupted, _)) => ("•".red().bold(), "Interrupted"),
            None => (
                activity_indicator(
                    Some(self.start_time),
                    MotionMode::from_animations_enabled(self.animations_enabled),
                    ReducedMotionIndicator::StaticBullet,
                )
                .unwrap_or_else(|| "•".dim()),
                "Calling",
            ),
        };
        let mut spans = vec![bullet, " ".into()];
        spans.extend(
            HistoryTimestamp::from_millis(timestamp.saturating_mul(1_000))
                .map(HistoryTimestamp::span),
        );
        spans.extend([
            verb.bold(),
            format!(" (Code Mode {})", label(&self.exec_call_id)).into(),
        ]);
        Some(spans.into())
    }

    fn detail_parts(
        &self,
        width: u16,
        expanded: &HashSet<String>,
    ) -> super::activity_parts::ActivityParts {
        let mut lines: Vec<_> = self.header().into_iter().map(HyperlinkLine::from).collect();
        let mut controls = Vec::new();
        for member in &self.members {
            if let Some(mut parts) = member.nested_activity_parts(
                width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
                expanded,
            ) {
                let offset = lines.len();
                for control in &mut parts.controls {
                    control.line += offset;
                }
                controls.extend(parts.controls);
                lines.extend(prefix_hyperlink_lines(
                    parts.lines,
                    crate::transcript_geometry::CHILD_PREFIX.into(),
                    crate::transcript_geometry::CHILD_PREFIX.into(),
                ));
            } else {
                lines.extend(prefix_hyperlink_lines(
                    member.nested_detail_lines(
                        width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
                    ),
                    crate::transcript_geometry::CHILD_PREFIX.into(),
                    crate::transcript_geometry::CHILD_PREFIX.into(),
                ));
            }
        }
        if self.status().is_some()
            && !self
                .members
                .iter()
                .any(|member| member.as_any().is::<ToolOutputCell>())
        {
            lines
                .push(Line::from("  Exact model output unavailable in this history.".dim()).into());
        }
        super::activity_parts::ActivityParts {
            lines,
            header: self.activity_header(),
            output: self.activity_disclosure(width),
            controls,
        }
    }
}

impl HistoryCell for CodeModeCell {
    fn transcript_animation_tick(&self) -> Option<u64> {
        (self.animations_enabled && self.status().is_none())
            .then(|| (self.start_time.elapsed().as_millis() / 50) as u64)
    }
    fn activity_header(&self) -> Option<ActivityHeader> {
        self.header().as_ref().map(ActivityHeader::at_end)
    }

    fn activity_ids(&self) -> Vec<String> {
        std::iter::once(format!("code-mode:{}", self.exec_call_id))
            .chain(
                self.members
                    .iter()
                    .filter(|cell| cell.as_any().is::<ToolOutputCell>())
                    .flat_map(|cell| cell.activity_ids()),
            )
            .collect()
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.compact_hyperlink_lines(width))
    }

    fn compact_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines: Vec<_> = self.header().into_iter().map(HyperlinkLine::from).collect();
        for member in self
            .members
            .iter()
            .filter(|member| !member.as_any().is::<ToolOutputCell>())
        {
            lines.extend(prefix_hyperlink_lines(
                member.nested_summary_lines(
                    width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
                ),
                crate::transcript_geometry::CHILD_PREFIX.into(),
                crate::transcript_geometry::CHILD_PREFIX.into(),
            ));
        }
        lines
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines: Vec<_> = self.header().into_iter().map(HyperlinkLine::from).collect();
        for member in &self.members {
            lines.extend(prefix_hyperlink_lines(
                member.nested_detail_lines(
                    width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
                ),
                crate::transcript_geometry::CHILD_PREFIX.into(),
                crate::transcript_geometry::CHILD_PREFIX.into(),
            ));
        }
        if self.status().is_some()
            && !self
                .members
                .iter()
                .any(|member| member.as_any().is::<ToolOutputCell>())
        {
            lines
                .push(Line::from("  Exact model output unavailable in this history.".dim()).into());
        }
        lines
    }

    fn activity_parts(
        &self,
        width: u16,
        _header_reserve: usize,
        expanded: &HashSet<String>,
    ) -> Option<super::activity_parts::ActivityParts> {
        if expanded.contains(&format!("code-mode:{}", self.exec_call_id)) {
            Some(self.detail_parts(width, expanded))
        } else {
            Some(super::activity_parts::ActivityParts {
                lines: self.compact_hyperlink_lines(width),
                header: self.activity_header(),
                output: self.activity_disclosure(width),
                controls: Vec::new(),
            })
        }
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn activity_disclosure(&self, width: u16) -> Option<ActivityDisclosure> {
        self.members
            .iter()
            .any(|cell| {
                cell.has_nested_details(
                    width.saturating_sub(crate::transcript_geometry::CHILD_INDENT),
                )
            })
            .then_some(ActivityDisclosure::Generic)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.members
            .iter()
            .flat_map(|cell| cell.raw_lines())
            .collect()
    }
}
