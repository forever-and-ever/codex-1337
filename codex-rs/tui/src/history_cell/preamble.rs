//! The census of the standing model context one request carried, with its parts disclosed.

use super::ActivityDisclosure;
use super::HistoryCell;
use super::HistoryTimestamp;
use super::activity_parts::ActivityHeader;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::visible_lines;
use codex_app_server_protocol::ThreadItem;
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
    change: Option<PreambleChange>,
    reported_input_tokens: Option<u64>,
}

impl PreambleCell {
    pub(crate) fn from_item(item: ThreadItem) -> Self {
        let ThreadItem::Preamble {
            id,
            completed_at_ms,
            parts,
            change,
            reported_input_tokens,
            ..
        } = item
        else {
            unreachable!("PreambleCell requires a Preamble item")
        };
        Self {
            id,
            timestamp: HistoryTimestamp::from_millis(completed_at_ms),
            parts,
            change,
            reported_input_tokens,
        }
    }

    fn tokens(&self) -> u64 {
        self.parts.iter().map(|part| part.tokens).sum()
    }

    fn heading(&self) -> Line<'static> {
        let mut heading = Line::default();
        heading.push_span("Preamble".bold());
        if let Some(change) = &self.change {
            let (after, before) = (self.tokens(), change.previous_tokens);
            let delta = if after >= before {
                format!("+{}", count(after - before))
            } else {
                format!("-{}", count(before - after))
            };
            heading.push_span(format!(" · changed {delta}").dim());
        }
        heading
    }

    fn detail_lines(&self) -> Vec<Line<'static>> {
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
                reported,
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
                    listed.push_str(&format!(" and {} more", names.len() - CHANGE_NAMES));
                }
                lines.push(Line::from(listed.dim()));
            }
        }
        lines
    }
}

enum Entry<'a> {
    Part(&'a PreamblePart),
    Group {
        name: &'a str,
        total: u64,
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
        members.sort_by_key(|member| std::cmp::Reverse(member.tokens));
        let total = members.iter().map(|member| member.tokens).sum();
        entries.push(Entry::Group {
            name,
            total,
            members,
        });
    }
    entries.sort_by_key(|entry| {
        std::cmp::Reverse(match entry {
            Entry::Part(part) => part.tokens,
            Entry::Group { total, .. } => *total,
        })
    });
    entries
}

fn row(tokens: u64, depth: usize, label: ratatui::text::Span<'static>) -> Line<'static> {
    Line::from(vec![
        format!("{:>9}  {}", count(tokens), "  ".repeat(depth)).dim(),
        label,
    ])
}

fn count(tokens: u64) -> String {
    format_with_separators(i64::try_from(tokens).unwrap_or(i64::MAX))
}

impl HistoryCell for PreambleCell {
    fn transcript_timestamp(&self) -> Option<HistoryTimestamp> {
        self.timestamp
    }

    fn activity_ids(&self) -> Vec<String> {
        vec![self.id.clone()]
    }

    fn activity_disclosure(&self, _width: u16) -> Option<ActivityDisclosure> {
        Some(ActivityDisclosure::Generic)
    }

    fn activity_header_tokens(&self) -> Option<u64> {
        Some(self.tokens())
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
        self.display_lines(u16::MAX)
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.expanded_hyperlink_lines(width)
    }

    fn expanded_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = self.display_hyperlink_lines(width);
        lines.extend(crate::terminal_hyperlinks::prefix_hyperlink_lines(
            self.detail_lines().into_iter().map(Into::into).collect(),
            "  ".into(),
            "  ".into(),
        ));
        lines
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        visible_lines(self.transcript_hyperlink_lines(width))
    }
}

#[cfg(test)]
#[path = "preamble_tests.rs"]
mod tests;
