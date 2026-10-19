//! Initial context, with a token summary and a separately disclosed retained raw value.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use super::activity_parts::ActivityHeader;
use super::activity_parts::ActivityParts;
use super::activity_parts::PartControl;
use crate::history_cell::activity_parts::FoldState;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use crate::transcript_view::FeedEntry;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::CensusCount;
use codex_protocol::PreambleChange;
use codex_protocol::PreamblePart;
use codex_protocol::PreambleSection;
use codex_protocol::num_format::format_with_separators;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::collections::BTreeMap;

/// Changed parts named in full before the rest are counted.
const CHANGE_NAMES: usize = 8;

#[derive(Debug)]
pub(crate) struct PreambleCell {
    id: String,
    timestamp: Option<HistoryTimestamp>,
    parts: Vec<PreamblePart>,
    raw: Option<String>,
    sources: Vec<codex_protocol::ContextSource>,
    title: &'static str,
    change: Option<PreambleChange>,
    reported_input_tokens: Option<u64>,
}

impl PreambleCell {
    pub(crate) fn from_item(item: ThreadItem) -> Self {
        let ThreadItem::Preamble {
            id,
            completed_at_ms,
            parts,
            raw,
            sources,
            change,
            reported_input_tokens,
            ..
        } = item
        else {
            unreachable!("PreambleCell requires a Preamble item")
        };
        Self {
            title: "Preamble",
            id,
            timestamp: HistoryTimestamp::from_millis(completed_at_ms),
            parts,
            raw,
            sources,
            change,
            reported_input_tokens,
        }
    }

    pub(crate) fn inventory(inventory: codex_protocol::ContextInventory) -> Self {
        Self {
            id: "context-inventory".into(),
            timestamp: None,
            title: "Context snapshot",
            parts: inventory.parts,
            sources: inventory.sources,
            raw: (!inventory.raw.is_empty()).then_some(inventory.raw),
            change: None,
            reported_input_tokens: None,
        }
    }

    pub(crate) fn source_lines(
        &self,
        mode: super::output_provenance::OutputMode,
    ) -> Vec<HyperlinkLine> {
        if self.sources.is_empty() {
            let mut lines = vec![super::output_provenance::client_notice(
                "Legacy mixed capture — prompt and declaration boundaries were not retained",
            )];
            if let Some(raw) = &self.raw {
                lines.extend(super::output_provenance::client_lines(raw, mode));
            }
            lines
        } else {
            super::output_provenance::context_sources(&self.sources, mode)
        }
    }

    fn tokens(&self) -> CensusCount {
        self.parts.iter().map(|part| part.tokens).sum()
    }

    fn heading(&self) -> Line<'static> {
        let mut heading = Line::default();
        heading.push_span(
            if self.raw.is_some() || !self.sources.is_empty() {
                self.title
            } else {
                "Context census (legacy)"
            }
            .bold(),
        );
        match (self.tokens(), &self.change) {
            (CensusCount::Omitted(_), _) => {
                heading.push_span(" · count incomplete".dim());
            }
            (CensusCount::Counted(after), Some(change)) => {
                let before = change.previous_tokens;
                let delta = if after >= before {
                    format!("+{}", count(after - before))
                } else {
                    format!("-{}", count(before - after))
                };
                heading.push_span(format!(" · changed {delta}").dim());
            }
            (CensusCount::Counted(_), None) => {}
        }
        heading
    }

    fn summary_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for section in [
            PreambleSection::Instructions,
            PreambleSection::Tools,
            PreambleSection::Context,
        ] {
            let parts: Vec<&PreamblePart> = self
                .parts
                .iter()
                .filter(|part| part.section == section)
                .collect();
            if parts.is_empty() {
                continue;
            }
            let title = match section {
                PreambleSection::Instructions => "Instructions",
                PreambleSection::Tools => "Tools",
                PreambleSection::Context => "Context",
            };
            let total = parts.iter().map(|part| part.tokens).sum();
            lines.push(row(total, /*depth*/ 0, title.bold()));
            for entry in entries(&parts) {
                match entry {
                    Entry::Part(part) => lines.push(row(
                        part.tokens,
                        /*depth*/ 1,
                        part.label.clone().into(),
                    )),
                    Entry::Group {
                        name,
                        total,
                        members,
                    } => {
                        lines.push(row(
                            total,
                            /*depth*/ 1,
                            format!("{name} ({})", members.len()).into(),
                        ));
                        for member in members {
                            lines.push(row(
                                member.tokens,
                                /*depth*/ 2,
                                member.label.clone().dim(),
                            ));
                        }
                    }
                }
            }
        }
        if let Some(reported) = self.reported_input_tokens {
            lines.push(row(
                CensusCount::Counted(reported),
                /*depth*/ 0,
                "reported input of the whole request, conversation included".dim(),
            ));
        }
        if let Some(change) = &self.change {
            for (sign, names) in [
                ("+", &change.added),
                ("-", &change.removed),
                ("~", &change.resized),
            ] {
                if names.is_empty() {
                    continue;
                }
                let mut listed = names
                    .iter()
                    .take(CHANGE_NAMES)
                    .map(|name| format!("{sign}{name}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                if names.len() > CHANGE_NAMES {
                    listed.push_str(" and ");
                    listed.push_str(&(names.len() - CHANGE_NAMES).to_string());
                    listed.push_str(" more");
                }
                lines.push(Line::from(listed.dim()));
            }
        }
        lines
    }

    fn raw_fold(&self) -> String {
        format!("preamble-raw:{}", self.id)
    }
}

enum Entry<'a> {
    Part(&'a PreamblePart),
    Group {
        name: &'a str,
        total: CensusCount,
        members: Vec<&'a PreamblePart>,
    },
}

/// Grouped tools collapse into their group; everything sorts by size, largest first.
fn entries<'a>(parts: &[&'a PreamblePart]) -> Vec<Entry<'a>> {
    let mut groups = BTreeMap::<&str, Vec<&PreamblePart>>::new();
    let mut entries = Vec::new();
    for part in parts {
        match &part.group {
            Some(group) => groups.entry(group.as_str()).or_default().push(part),
            None => entries.push(Entry::Part(part)),
        }
    }
    for (name, mut members) in groups {
        members.sort_by_key(|member| std::cmp::Reverse(count_order(member.tokens)));
        let total = members.iter().map(|member| member.tokens).sum();
        entries.push(Entry::Group {
            name,
            total,
            members,
        });
    }
    entries.sort_by_key(|entry| {
        std::cmp::Reverse(count_order(match entry {
            Entry::Part(part) => part.tokens,
            Entry::Group { total, .. } => *total,
        }))
    });
    entries
}

// Uncounted rows sort separately; they are not zero-sized sources.
fn count_order(tokens: CensusCount) -> (bool, u64) {
    match tokens {
        CensusCount::Counted(tokens) => (false, tokens),
        CensusCount::Omitted(_) => (true, 0),
    }
}

fn row(tokens: CensusCount, depth: usize, label: ratatui::text::Span<'static>) -> Line<'static> {
    let (count, omission) = match tokens {
        CensusCount::Counted(tokens) => (count(tokens), None),
        CensusCount::Omitted(_) => ("uncounted".into(), Some(" (census work limit)".dim())),
    };
    let mut line = Line::from(vec![
        format!("{count:>9}  {}", "  ".repeat(depth)).dim(),
        label,
    ]);
    if let Some(omission) = omission {
        line.push_span(omission);
    }
    line
}

fn count(tokens: u64) -> String {
    format_with_separators(i64::try_from(tokens).unwrap_or(i64::MAX))
}

impl FeedEntry for PreambleCell {
    fn feed_kind(&self) -> crate::transcript_view::EntryKind {
        use crate::transcript_view::EntryKind;
        EntryKind::Preamble
    }
    fn feed_id(&self) -> String {
        self.id.clone()
    }
    fn feed_disclosure(&self) -> crate::transcript_view::HeaderDisclosure {
        crate::transcript_view::HeaderDisclosure::Collapsed
    }

    fn feed_title(&self) -> Line<'static> {
        self.heading()
    }

    fn feed_body(
        &self,
        _width: u16,
        expanded: &FoldState,
        mode: super::output_provenance::OutputMode,
    ) -> ActivityParts {
        use super::output_provenance::OutputProvenance;
        use super::output_provenance::mark_lines;
        let mut lines: Vec<HyperlinkLine> =
            self.summary_lines().into_iter().map(Into::into).collect();
        mark_lines(&mut lines, OutputProvenance::Client);
        let mut controls = Vec::new();
        if self.raw.is_some() || !self.sources.is_empty() {
            let id = self.raw_fold();
            let expanded =
                expanded.is_open(super::activity_parts::FoldGroup::RawContext, &id, false);
            lines.push("".into());
            controls.push(PartControl {
                group: super::activity_parts::FoldGroup::RawContext,
                id,
                line: lines.len(),
                expanded,
            });
            let mut heading = vec![HyperlinkLine::from(Line::from("Raw Context".bold()))];
            mark_lines(&mut heading, OutputProvenance::Client);
            lines.extend(heading);
            if expanded {
                lines.extend(self.source_lines(mode));
            }
        }
        ActivityParts {
            lines,
            header: Some(ActivityHeader {
                source_offset: "Raw Context".len(),
                expand_on_truncation: false,
            }),
            output: None,
            controls,
        }
    }
}

impl HistoryCell for PreambleCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }

    fn has_formatted_output(&self) -> bool {
        self.raw.is_some() || !self.sources.is_empty()
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![self.id.clone()]
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        Some(ActivityDisclosure::Generic)
    }

    fn activity_header_tokens(&self) -> Option<u64> {
        self.tokens().counted()
    }

    fn activity_header(&self) -> Option<ActivityHeader> {
        let mut header = ActivityHeader::at_end(&self.heading());
        header.expand_on_truncation = false;
        Some(header)
    }

    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.display_hyperlink_lines(width))
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::prefix_hyperlink_lines(
            vec![self.heading().into()],
            super::agent_prefix(/*first*/ true),
            "  ".into(),
        )
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        if self.sources.is_empty() {
            return self.raw.as_deref().map_or_else(Vec::new, |raw| {
                super::sanitize_user_text(raw.into())
                    .split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect()
            });
        }
        self.sources
            .iter()
            .flat_map(|source| {
                let text = match source {
                    codex_protocol::ContextSource::Prompt { text, .. }
                    | codex_protocol::ContextSource::ToolDeclarations { text } => text,
                };
                text.split('\n').map(|line| Line::from(line.to_owned()))
            })
            .collect()
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut expanded = FoldState::new();
        expanded.insert(self.raw_fold());
        let mut lines = self.display_hyperlink_lines(width);
        lines.extend(
            self.feed_body(width, &expanded, super::output_provenance::OutputMode::Raw)
                .lines,
        );
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }
}

#[cfg(test)]
#[path = "preamble_tests.rs"]
mod tests;
