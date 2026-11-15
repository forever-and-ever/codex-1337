//! Empty polling results and opaque subagent events are still compact tool entries.

use super::*;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::ToolOutputCell;
use crate::history_cell::activity_parts::FoldTarget;
use crate::history_cell::invocation::Action;
use crate::history_cell::invocation::Panel;
use crate::history_cell::invocation::Port;
use crate::multi_agents::AgentMetadata;
use codex_app_server_protocol::CollabAgentTool;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::ThreadItemPayload;
use codex_protocol::agent_wait::AgentWaitCompletion;
use codex_protocol::agent_wait::AgentWaitOutcome;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::step_admission::ArrivalItem;
use codex_protocol::step_admission::ArrivalKind;
use codex_protocol::step_admission::ArrivalSource;
use pretty_assertions::assert_eq;
use std::collections::HashMap;

#[test]
fn completed_thought_discloses_only_readable_reasoning() {
    let cwd = std::path::Path::new("/tmp");
    let mut snapshots = Vec::new();
    for (label, tokens, summary, content) in [
        (
            "counted",
            Some(1300),
            vec!["Provider summary"],
            vec!["First block Ω", "Second block"],
        ),
        ("unknown", None, Vec::new(), vec!["First block Ω"]),
        (
            "summary-only",
            Some(1300),
            vec!["Summary is not raw reasoning"],
            Vec::new(),
        ),
        ("encrypted-only", Some(1300), Vec::new(), Vec::new()),
        ("opaque-unknown", None, Vec::new(), Vec::new()),
        ("blank", None, Vec::new(), vec![" \n\t"]),
    ] {
        let readable = content.iter().any(|text| !text.trim().is_empty());
        let cells: Vec<Arc<dyn HistoryCell>> =
            vec![Arc::new(crate::history_cell::ThoughtCell::from_item(
                codex_app_server_protocol::ThreadItem {
                    payload: ThreadItemPayload::Thought {
                        id: format!("thought:{label}"),
                        response_id: label.into(),
                        summary: summary.into_iter().map(str::to_owned).collect(),
                        content: content.into_iter().map(str::to_owned).collect(),
                        reported_reasoning_tokens: tokens,
                    },
                    started_at_ms: None,
                    completed_at_ms: Some(1_700_000_000_000),
                },
                cwd,
            ))];
        let mut view = TranscriptView::default();
        let clock = crate::history_cell::HistoryTimestamp::from_millis(1_700_000_000_000)
            .unwrap()
            .span()
            .content
            .trim()
            .to_owned();
        let closed = tests::text(&tests::render(&mut view, &cells, 64, 6));
        assert_eq!(closed.matches("[cot]").count(), usize::from(readable));
        assert!(!closed.contains("Thought") && !closed.contains("First block"));
        snapshots.push(format!(
            "{label} closed\n{}",
            closed.replace(&clock, "HH:MM")
        ));
        if !readable {
            continue;
        }
        let target = view
            .current_layout(&cells, 0)
            .unwrap()
            .fold_targets()
            .into_iter()
            .find(|target| matches!(target, FoldTarget::Thought(_)))
            .unwrap();
        assert!(view.focus_fold(&cells, 0, target.clone()));
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors {
                fg: (220, 220, 220),
                bg: (20, 20, 20),
            },
            || {
                for width in [64, 32, 64] {
                    let buffer = tests::render(&mut view, &cells, width, 8);
                    let rendered = tests::text(&buffer);
                    assert_eq!(rendered.matches("[cot]").count(), 1);
                    assert_eq!(rendered.matches("First block Ω").count(), 1);
                    assert!(
                        !rendered.contains("Provider summary") && !rendered.contains("Thought")
                    );
                    let (row, line) = rendered
                        .lines()
                        .enumerate()
                        .find(|(_, line)| line.contains("First block"))
                        .unwrap();
                    let column = line.find("First block").unwrap();
                    assert_eq!(
                        buffer[(
                            crate::transcript_geometry::CONTROL_COLUMN,
                            u16::try_from(row).unwrap()
                        )]
                            .fg,
                        crate::warpaint::Bracket::Reasoning.color()
                    );
                    snapshots.push(format!(
                        "{label} open width={width}\n{}",
                        rendered.replace(&clock, "HH:MM")
                    ));
                    view.begin_selection(
                        &cells,
                        u16::try_from(column).unwrap(),
                        u16::try_from(row).unwrap(),
                        1,
                    );
                    let length = view.current_layout(&cells, 0).unwrap().text().len();
                    let selection = view.selection.as_mut().unwrap();
                    selection.start.offset = 0;
                    selection.end.offset = length;
                    let copied = view.selected_text(&cells).unwrap();
                    assert!(copied.contains("First block Ω") && !copied.contains("[cot]"));
                    view.end_selection(&cells);
                }
            },
        );
        assert!(view.focus_fold(&cells, 0, target));
        view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        let closed = tests::text(&tests::render(&mut view, &cells, 64, 6));
        assert!(!closed.contains("First block"));
    }
    insta::assert_snapshot!(snapshots.join("\n"));
}

#[test]
fn captured_output_modes_preserve_blocks_lexemes_and_wrapped_copy() {
    use crate::history_cell::activity_parts::FoldState as HashSet;
    use crate::history_cell::activity_parts::FoldTarget;
    use crate::history_cell::output_provenance::OutputMode;
    use crate::history_cell::output_provenance::OutputProvenance;
    use crate::history_cell::output_provenance::client_notice;
    let json = r#"{"a":9007199254740993123456789,"a":1.2300e+04,"text":"first\nsecond\tend","literal":"\\n","key\n":"\u0061"}"#;
    let plain = "prefix {\"embedded\":true}";
    let invalid = "{\"truncated\":";
    let body = FunctionCallOutputBody::ContentItems(
        [json, plain, invalid]
            .map(|text| FunctionCallOutputContentItem::InputText { text: text.into() })
            .into(),
    );
    let output = Arc::new(
        ToolOutputCell::from_output("mode", Some("mode"), "tool", /*namespace*/ None, &body)
            .unwrap(),
    );
    let cells: Vec<Arc<dyn HistoryCell>> = vec![output.clone()];
    let mut view = TranscriptView::default();
    view.record_tool_output_tokens("tool-output-tokens:mode".into(), "mode".into(), Some(123));
    view.invocations.apply(
        "mode",
        Action::Toggle(Port::Output, Panel::Visible(OutputMode::Fmt)),
    );
    let mut painted = Vec::new();
    for mode in [OutputMode::Fmt, OutputMode::Raw, OutputMode::Fmt] {
        tests::render(&mut view, &cells, 80, 14);
        let layout = view.current_layout(&cells, 0).unwrap();
        let target = FoldTarget::Invocation {
            call_id: "mode".into(),
            action: Action::Format(Port::Output, mode),
        };
        let (row, columns, _) = layout.fold_position(&target).unwrap();
        assert_eq!(layout.fold_at(row, columns.start), Some(target));
        assert!(view.toggle_disclosure_at(&cells, columns.start, u16::try_from(row).unwrap()));
        let buffer = tests::render(&mut view, &cells, 80, 14);
        let layout = view.current_layout(&cells, 0).unwrap();
        let text = layout.text().to_owned();
        assert!(text.ends_with(&format!("\n{plain}\n{invalid}")));
        if mode == OutputMode::Raw {
            assert!(text.ends_with(&[json, plain, invalid].join("\n")));
        } else {
            assert!(text.contains("9007199254740993123456789"));
            assert!(text.contains("1.2300e+04"));
            assert!(text.contains("\"text\": \"first\nsecond\tend\""));
            assert!(text.contains(r#""literal": "\\n""#));
            assert!(text.contains(r#""key\n": "\u0061""#));
            assert_eq!(text.matches("\"a\":").count(), 2);
        }
        let row = layout.row_for_offset(0);
        view.begin_selection(
            &cells,
            crate::transcript_geometry::TITLE_COLUMN,
            u16::try_from(row).unwrap(),
            1,
        );
        let selection = view.selection.as_mut().unwrap();
        selection.start.offset = 0;
        selection.end.offset = layout.text().len();
        tests::render(&mut view, &cells, 34, 18);
        assert_eq!(view.selected_text(&cells), Some(text));
        view.end_selection(&cells);
        painted.push(format!("{mode:?}\n{buffer:?}"));
    }
    let mut surfaces = output.output_lines(/*width*/ 80, OutputMode::Fmt);
    surfaces.push(client_notice("Stored result · exact capture unavailable"));
    let layout = TextLayout::new(surfaces, /*width*/ 80);
    assert!(layout.text().contains("Stored result"));
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 14,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 0);
    painted.push(format!("provenance surfaces\n{buffer:?}"));
    let narrow = layout.rewrap(/*width*/ 24);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 24,
    );
    let mut buffer = Buffer::empty(area);
    narrow.render(area, &mut buffer, /*start_row*/ 0);
    assert!(tests::text(&buffer).contains("Stored result"));
    assert!(narrow.text().contains("Stored result"));
    assert_eq!(narrow.text(), layout.text());
    assert!(
        output
            .output_lines(/*width*/ 80, OutputMode::Fmt)
            .iter()
            .any(|line| line.source.as_ref().unwrap().output_provenance
                == OutputProvenance::Formatted)
    );
    // Safety must remain a visible, copyable projection even when JSON caps reject the capture.
    let unsafe_text = "\u{1b}[31munsafe\u{7}\rend\u{7f}\u{85}\nlast\tcolumn";
    let safe_text = r"\u{1b}[31munsafe\u{7}\rend\u{7f}\u{85}".to_owned() + "\nlast\tcolumn";
    for (source, safe) in [
        (unsafe_text.to_owned(), safe_text.clone()),
        (
            "{\r\"key\":true}".into(),
            r"{\r".to_owned() + "\"key\":true}",
        ),
        (
            format!("{unsafe_text}{}", "x".repeat(256 * 1024)),
            format!("{safe_text}{}", "x".repeat(256 * 1024)),
        ),
    ] {
        let paint = source == unsafe_text;
        let output = Arc::new(
            ToolOutputCell::from_output(
                "unsafe",
                Some("unsafe"),
                "tool",
                /*namespace*/ None,
                &FunctionCallOutputBody::Text(source),
            )
            .unwrap(),
        );
        assert!(!output.has_formatted_output());
        for mode in [OutputMode::Raw, OutputMode::Fmt] {
            let lines = output.output_lines(u16::MAX, mode);
            assert_eq!(
                lines
                    .first()
                    .unwrap()
                    .source
                    .as_ref()
                    .unwrap()
                    .output_provenance,
                OutputProvenance::Formatted
            );
            assert_eq!(
                lines
                    .last()
                    .unwrap()
                    .source
                    .as_ref()
                    .unwrap()
                    .output_provenance,
                OutputProvenance::Client
            );
            let layout = TextLayout::new(lines, u16::MAX);
            assert!(layout.text().starts_with(&safe));
        }
        if !paint {
            continue;
        }
        let cells: Vec<Arc<dyn HistoryCell>> = vec![output.clone()];
        let mut view = TranscriptView::default();
        view.invocations.apply(
            "unsafe",
            Action::Toggle(Port::Output, Panel::Visible(OutputMode::Fmt)),
        );
        tests::render(&mut view, &cells, /*width*/ 32, /*height*/ 12);
        let layout = view.current_layout(&cells, /*index*/ 0).unwrap();
        let start = layout.text().find(&safe).unwrap();
        view.begin_selection(&cells, /*column*/ 4, /*row*/ 1, /*clicks*/ 1);
        let selection = view.selection.as_mut().unwrap();
        selection.start.offset = start;
        selection.end.offset = start + safe.len();
        tests::render(&mut view, &cells, /*width*/ 24, /*height*/ 16);
        assert_eq!(view.selected_text(&cells).as_deref(), Some(safe.as_str()));
        view.end_selection(&cells);
        let mut buffer = tests::render(&mut view, &cells, /*width*/ 32, /*height*/ 12);
        // The words wrap beside the bracket's stroke; read them without it.
        for row in 0..buffer.area.height {
            buffer[(crate::transcript_geometry::CONTROL_COLUMN, row)].set_symbol(" ");
        }
        assert!(
            tests::text(&buffer)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("escaped for display")
        );
        let layout = view.current_layout(&cells, 0).unwrap();
        let target = FoldTarget::Invocation {
            call_id: "unsafe".into(),
            action: Action::Format(Port::Output, OutputMode::Raw),
        };
        for layout in [layout.as_ref(), &layout.rewrap(24)] {
            let (row, columns, _) = layout.fold_position(&target).unwrap();
            assert_eq!(layout.fold_at(row, columns.start), Some(target.clone()));
            let capture = layout.text().find(&safe).unwrap();
            assert!(
                !layout.text()[..capture].contains(['[', ']']),
                "{:?}",
                layout.text()
            );
        }
        painted.push(format!("escaped capture\n{buffer:?}"));
    }
    // Context inspectors and replay use the same evidence boundary as tool output.
    // Submitted definitions are model-bound, but their client JSON is not a provider prompt.
    // Preserve that evidence distinction through replay, formatting and selection.
    for (source, kind, expected) in [
        (
            ArrivalSource::Captured {
                blocks: vec![json.into()],
            },
            ArrivalKind::Injection,
            OutputProvenance::Captured,
        ),
        (
            ArrivalSource::Captured {
                blocks: vec![json.into()],
            },
            ArrivalKind::InterAgent,
            OutputProvenance::Captured,
        ),
        (
            ArrivalSource::ToolDeclarations { text: json.into() },
            ArrivalKind::Injection,
            OutputProvenance::Definitions(OutputMode::Raw),
        ),
        (
            ArrivalSource::Legacy,
            ArrivalKind::Injection,
            OutputProvenance::Client,
        ),
    ] {
        let mut encoded = serde_json::to_value(ArrivalItem {
            placement: None,
            id: "arrival-provenance".into(),
            response_id: "response".into(),
            announced_at_ms: 0,
            kind,
            source: source.clone(),
            label: "arbitrary label — not a discriminator".into(),
            text: (source == ArrivalSource::Legacy).then(|| json.into()),
        })
        .unwrap();
        if source == ArrivalSource::Legacy {
            encoded.as_object_mut().unwrap().remove("source");
        }
        let replay =
            crate::history_cell::ArrivalCell::new(serde_json::from_value(encoded).unwrap());
        let raw = crate::history_cell::output_provenance::raw_cell_lines(&replay, u16::MAX);
        assert!(
            TextLayout::new(raw.clone(), u16::MAX)
                .text()
                .ends_with(json)
        );
        let body = raw
            .iter()
            .find(|line| line.line.to_string().contains("9007199254740993123456789"))
            .unwrap();
        assert_eq!(body.source.as_ref().unwrap().output_provenance, expected);
        if matches!(source, ArrivalSource::Captured { .. }) {
            let bracket = if kind == ArrivalKind::InterAgent {
                crate::warpaint::Bracket::Message
            } else {
                crate::warpaint::Bracket::Harness
            };
            assert_eq!(body.source.as_ref().unwrap().bracket, Some(bracket));
        }
        for mode in [OutputMode::Raw, OutputMode::Fmt] {
            let body = FeedEntry::feed_body(
                &replay,
                u16::MAX,
                &HashSet::from(["arrival-raw:arrival-provenance".into()]),
                mode,
            );
            let layout = TextLayout::new(body.lines, u16::MAX);
            assert!(layout.text().contains("9007199254740993123456789"));
            assert!(layout.text().contains("1.2300e+04"));
            assert_eq!(layout.text().matches("\"a\":").count(), 2);
            assert_eq!(
                layout.text().contains("Legacy arrival"),
                source == ArrivalSource::Legacy
            );
            assert_eq!(
                layout.text().contains("Tool Declarations"),
                matches!(source, ArrivalSource::ToolDeclarations { .. })
            );
        }
    }
    // The compact view must project only a validated harness envelope, never rewrite
    // the capture or strip an envelope-shaped section inside the actual message.
    for message_type in ["MESSAGE", "NEW_TASK", "FINAL_ANSWER", "FINAL_MESSAGE"] {
        let payload = "  Keep this indentation.\nMessage Type: MESSAGE\nTask name: /nested\nSender: /nested\nPayload:\nKeep this too.\n";
        let capture = format!(
            "Message Type: {message_type}\nTask name: /root\nSender: /root/scout\nPayload:\n{payload}"
        );
        let arrival = crate::history_cell::ArrivalCell::new(ArrivalItem {
            placement: None,
            id: "message-envelope".into(),
            response_id: "response".into(),
            announced_at_ms: 0,
            kind: ArrivalKind::InterAgent,
            label: "/root/scout".into(),
            text: None,
            source: ArrivalSource::Captured {
                blocks: vec![capture.clone()],
            },
        });
        let title = FeedEntry::feed_title(&arrival).to_string();
        assert_eq!(
            title,
            if message_type == "MESSAGE" {
                "Message from /root/scout".into()
            } else {
                format!("Message from /root/scout ({message_type})")
            }
        );
        assert!(!arrival.has_formatted_output());
        for mode in [OutputMode::Raw, OutputMode::Fmt] {
            let body = FeedEntry::feed_body(&arrival, u16::MAX, &HashSet::new(), mode);
            assert!(body.header.is_none());
            assert!(body.controls.is_empty());
            assert_eq!(
                body.lines
                    .iter()
                    .map(|line| line.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
                payload.trim()
            );
        }
        assert_eq!(
            crate::history_cell::output_provenance::raw_cell_lines(&arrival, u16::MAX)
                .iter()
                .map(|line| line.line.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            capture
        );
        let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(arrival)];
        let mut view = TranscriptView::default();
        for width in [104, 32, 104] {
            tests::render(&mut view, &cells, width, 30);
            let layout = view.current_layout(&cells, 0).unwrap();
            assert!(
                !layout
                    .fold_targets()
                    .iter()
                    .any(|target| { matches!(target, FoldTarget::OutputMode(_)) })
            );
            assert!(!layout.text().contains("Context"));
            assert_eq!(layout.text().matches("Message Type:").count(), 1);
        }
    }
    for (kind, sender, prefix) in [
        (ArrivalKind::InterAgent, "/other", ""),
        (
            ArrivalKind::InterAgent,
            "/root/scout",
            "A quoted envelope:\n",
        ),
        (ArrivalKind::Injection, "/root/scout", ""),
    ] {
        let capture = format!(
            "{prefix}Message Type: MESSAGE\nTask name: /root\nSender: {sender}\nPayload:\nUnchanged."
        );
        let arrival = crate::history_cell::ArrivalCell::new(ArrivalItem {
            placement: None,
            id: "unrecognized-envelope".into(),
            response_id: "response".into(),
            announced_at_ms: 0,
            kind,
            label: "/root/scout".into(),
            text: None,
            source: ArrivalSource::Captured {
                blocks: vec![capture.clone()],
            },
        });
        let body = FeedEntry::feed_body(&arrival, u16::MAX, &HashSet::new(), OutputMode::Raw);
        assert!(
            body.lines
                .iter()
                .map(|line| line.line.to_string())
                .collect::<Vec<_>>()
                .join("\n")
                .ends_with(&capture)
        );
    }

    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (230, 230, 230),
            bg: (20, 20, 20),
        },
        || {
            use crate::history_cell::output_provenance::ContextAuthor;
            use crate::history_cell::output_provenance::captured_lines;
            let mut brackets = Vec::new();
            for author in [
                ContextAuthor::User,
                ContextAuthor::Assistant,
                ContextAuthor::Subagent,
                ContextAuthor::Environment,
            ] {
                let mut styles = Vec::new();
                for mode in [OutputMode::Raw, OutputMode::Fmt] {
                    let layout = TextLayout::new(captured_lines("source", author, mode), 20);
                    let area = Rect::new(0, 0, 20, 1);
                    let mut buffer = Buffer::empty(area);
                    layout.render(area, &mut buffer, 0);
                    styles.push((buffer[(0, 0)].fg, buffer[(0, 0)].bg));
                }
                assert_eq!(
                    styles[1].1,
                    ratatui::style::Color::Reset,
                    "Fmt must use the default background"
                );
                assert_ne!(styles[0].0, styles[1].0, "Fmt must announce repainting");
                let bracket = captured_lines("source", author, OutputMode::Raw)[0]
                    .source
                    .as_ref()
                    .and_then(|source| source.bracket);
                assert!(
                    !brackets.contains(&bracket),
                    "Authors hang distinct brackets"
                );
                brackets.push(bracket);
            }
            // A renderer's span-level background/reverse must not leak through Client provenance.
            let mut decorated = vec![HyperlinkLine::from(Line::from(
                ratatui::text::Span::styled(
                    "human-only annotation",
                    ratatui::style::Style::default()
                        .bg(ratatui::style::Color::Red)
                        .add_modifier(ratatui::style::Modifier::REVERSED),
                ),
            ))];
            crate::history_cell::output_provenance::mark_lines(
                &mut decorated,
                OutputProvenance::Client,
            );
            let mut promoted = decorated.clone();
            crate::history_cell::output_provenance::mark_lines(
                &mut promoted,
                OutputProvenance::Formatted,
            );
            let promoted = TextLayout::new(promoted, 30);
            let mut buffer = Buffer::empty(Rect::new(0, 0, 30, 1));
            promoted.render(buffer.area, &mut buffer, 0);
            assert_eq!(buffer[(0, 0)].bg, ratatui::style::Color::Reset);
            let layout = TextLayout::new(decorated, 8);
            let area = Rect::new(0, 0, 8, 4);
            let mut buffer = Buffer::empty(area);
            layout.render(area, &mut buffer, 0);
            for cell in &buffer.content {
                assert_eq!(cell.bg, ratatui::style::Color::Reset);
                assert!(!cell.modifier.contains(ratatui::style::Modifier::REVERSED));
            }
            // Authorship survives source-backed and streaming raw projections alike;
            // treating every non-message cell as a client annotation repaints plans/reasoning.
            let cwd = std::path::Path::new(".");
            let assistant_cells: [Box<dyn HistoryCell>; 4] = [
                Box::new(crate::history_cell::ReasoningSummaryCell::new(
                    "Reasoning".into(),
                    "Consider the constraint.".into(),
                    cwd,
                    false,
                )),
                Box::new(crate::history_cell::new_proposed_plan(
                    "Inspect the source.".into(),
                    cwd,
                )),
                Box::new(crate::history_cell::new_proposed_plan_stream(
                    vec!["Inspect the source."],
                    false,
                )),
                Box::new(crate::history_cell::StreamingPlanTailCell::new(
                    vec!["Inspect the source.".into()],
                    false,
                )),
            ];
            for cell in assistant_cells {
                let lines =
                    crate::history_cell::output_provenance::raw_cell_lines(cell.as_ref(), 80);
                assert_ne!(lines, [] as [HyperlinkLine; 0]);
                for line in lines {
                    let source = line.source.as_ref().unwrap();
                    assert_eq!(source.bracket, Some(crate::warpaint::Bracket::Response));
                    assert_eq!(source.output_provenance, OutputProvenance::Unspecified);
                }
            }
            // The same submitted definitions cross Preamble, /context and Injection surfaces.
            // Rewrapping retains provenance fills and selectable human headings.
            let sources = vec![
                codex_protocol::ContextSource::Prompt {
                    label: "Base instructions".into(),
                    text: json.into(),
                },
                codex_protocol::ContextSource::ToolDeclarations { text: json.into() },
            ];
            let parts = vec![codex_protocol::PreamblePart {
                section: codex_protocol::PreambleSection::Tools,
                group: None,
                label: "tool".into(),
                tokens: codex_protocol::CensusCount::Counted(12),
            }];
            let preamble = crate::history_cell::PreambleCell::from_item(
                codex_app_server_protocol::ThreadItem {
                    payload: ThreadItemPayload::Preamble {
                        id: "provenance-preamble".into(),
                        response_id: "response".into(),
                        parts: parts.clone(),
                        sources: sources.clone(),
                        raw: None,
                        window_id: None,
                        change: None,
                        reported_input_tokens: None,
                    },
                    started_at_ms: None,
                    completed_at_ms: Some(0),
                },
            );
            let inventory =
                crate::history_cell::PreambleCell::inventory(codex_protocol::ContextInventory {
                    parts,
                    sources,
                    raw: String::new(),
                });
            let arrival = crate::history_cell::ArrivalCell::new(ArrivalItem {
                placement: None,
                id: "provenance-definitions".into(),
                response_id: "response".into(),
                announced_at_ms: 0,
                kind: ArrivalKind::Injection,
                label: "Additional tools".into(),
                text: None,
                source: ArrivalSource::ToolDeclarations { text: json.into() },
            });
            for mode in [OutputMode::Raw, OutputMode::Fmt] {
                let expanded = HashSet::from([
                    "preamble-raw:provenance-preamble".into(),
                    "preamble-raw:context-inventory".into(),
                    "arrival-raw:provenance-definitions".into(),
                ]);
                for (entry, captures) in [
                    (&preamble as &dyn FeedEntry, 2),
                    (&inventory as &dyn FeedEntry, 2),
                    (&arrival as &dyn FeedEntry, 1),
                ] {
                    let lines = entry.feed_body(u16::MAX, &expanded, mode).lines;
                    let definitions: Vec<_> = lines
                        .iter()
                        .filter(|line| {
                            line.source.as_ref().is_some_and(|source| {
                                source.output_provenance == OutputProvenance::Definitions(mode)
                            })
                        })
                        .collect();
                    assert_ne!(definitions, [] as [&HyperlinkLine; 0]);
                    assert!(definitions.iter().all(|line| {
                        line.line.style.bg
                            == match mode {
                                OutputMode::Raw => None,
                                OutputMode::Fmt => Some(ratatui::style::Color::Reset),
                            }
                    }));
                    for line in &lines {
                        let Some(source) = line.source.as_ref() else {
                            continue;
                        };
                        if source.output_provenance != OutputProvenance::Client {
                            continue;
                        }
                        assert_eq!(source.line_style.bg, Some(ratatui::style::Color::Reset));
                        assert!(
                            !source
                                .line_style
                                .add_modifier
                                .contains(ratatui::style::Modifier::REVERSED)
                        );
                    }
                    let layout = TextLayout::new(lines, u16::MAX).rewrap(24);
                    assert!(layout.text().contains("Tool Declarations"));

                    assert_eq!(
                        layout.text().matches("9007199254740993123456789").count(),
                        captures
                    );
                    let area = Rect::new(0, 0, 24, 18);
                    let mut buffer = Buffer::empty(area);
                    layout.render(area, &mut buffer, 0);
                    if captures == 1 {
                        painted.push(format!("{mode:?} submitted definitions\n{buffer:?}"));
                    }
                }
            }
            // AdditionalTools arrivals must not dump their definitions into an otherwise open feed.
            let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(arrival)];
            let mut view = TranscriptView::default();
            let closed = tests::render(&mut view, &cells, 64, 6);
            assert!(!tests::text(&closed).contains("Raw Context"));
            let parent = view.current_layout(&cells, 0).unwrap().fold_targets()[0].clone();
            assert!(view.focus_fold(&cells, 0, parent));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
            let folded = tests::render(&mut view, &cells, 64, 6);
            assert!(tests::text(&folded).contains("Raw Context"));
            assert!(
                !view
                    .current_layout(&cells, 0)
                    .unwrap()
                    .text()
                    .contains(json)
            );
            painted.push(format!("definitions folded\n{folded:?}"));
            assert!(view.focus_fold(
                &cells,
                0,
                FoldTarget::GroupedPart {
                    id: "arrival-raw:provenance-definitions".into(),
                    group: crate::history_cell::activity_parts::FoldGroup::RawContext
                }
            ));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
            tests::render(&mut view, &cells, 64, 12);
            let layout = view.current_layout(&cells, 0).unwrap();
            let start = layout.text().find(json).unwrap();
            assert!(layout.text().contains("Tool Declarations"));
            view.begin_selection(&cells, 6, 1, 1);
            let selection = view.selection.as_mut().unwrap();
            selection.start.offset = start;
            selection.end.offset = start + json.len();
            tests::render(&mut view, &cells, 32, 16);
            assert_eq!(view.selected_text(&cells), Some(json.into()));
            view.end_selection(&cells);
            assert!(view.focus_fold(&cells, 0, FoldTarget::OutputMode(OutputMode::Fmt)));
            view.handle_key(crossterm::event::KeyCode::Right.into(), &cells);
            tests::render(&mut view, &cells, 64, 16);
            let formatted = view.current_layout(&cells, 0).unwrap();
            assert!(formatted.text().contains("9007199254740993123456789"));
            assert!(formatted.text().contains("1.2300e+04"));
            assert_eq!(formatted.text().matches("\"a\":").count(), 2);
            assert!(view.focus_fold(
                &cells,
                0,
                FoldTarget::GroupedPart {
                    id: "arrival-raw:provenance-definitions".into(),
                    group: crate::history_cell::activity_parts::FoldGroup::RawContext
                }
            ));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
            let folded = tests::render(&mut view, &cells, 32, 8);
            assert!(
                !view
                    .current_layout(&cells, 0)
                    .unwrap()
                    .text()
                    .contains(json)
            );
            assert!(tests::text(&folded).contains("Raw Context"));
            let thought = crate::history_cell::ThoughtCell::from_item(
                codex_app_server_protocol::ThreadItem {
                    payload: ThreadItemPayload::Thought {
                        id: "thought-author".into(),
                        response_id: "response".into(),
                        summary: Vec::new(),
                        content: vec!["Readable reasoning.".into()],
                        reported_reasoning_tokens: Some(12),
                    },
                    started_at_ms: None,
                    completed_at_ms: Some(0),
                },
                cwd,
            );
            let lines = crate::history_cell::output_provenance::raw_cell_lines(&thought, 80);
            assert_ne!(lines, [] as [HyperlinkLine; 0]);
            assert!(lines.iter().all(|line| {
                line.source
                    .as_ref()
                    .is_some_and(|source| source.output_provenance == OutputProvenance::Reasoning)
            }));
        },
    );
    let blocks = [json.to_owned(), "  trailing\n".to_owned()];
    let projected = crate::history_cell::output_provenance::captured_blocks(
        &blocks,
        crate::history_cell::output_provenance::ContextAuthor::Subagent,
        OutputMode::Raw,
    );
    assert_eq!(
        projected
            .iter()
            .filter(
                |line| line.source.as_ref().unwrap().output_provenance == OutputProvenance::Client
            )
            .count(),
        2
    );
    assert_eq!(
        TextLayout::new(projected, u16::MAX).text(),
        format!("Text block 1\n{}\nText block 2\n{}", blocks[0], blocks[1])
    );
    let sources = [
        codex_protocol::ContextSource::Prompt {
            label: "instructions".into(),
            text: json.into(),
        },
        codex_protocol::ContextSource::ToolDeclarations { text: json.into() },
    ];
    let lines = crate::history_cell::output_provenance::context_sources(&sources, OutputMode::Raw);
    let evidence: Vec<_> = lines
        .iter()
        .filter(|line| line.line.to_string() == json)
        .collect();
    assert_eq!(evidence.len(), 2);
    assert_eq!(
        evidence[0].source.as_ref().unwrap().output_provenance,
        OutputProvenance::Captured
    );
    assert_eq!(
        evidence[1].source.as_ref().unwrap().output_provenance,
        OutputProvenance::Definitions(OutputMode::Raw)
    );
    insta::assert_snapshot!(painted.join("\n"));
}

#[test]
fn mixed_feed_keeps_its_grid_through_resize_and_details() {
    use crate::diff_model::FileChange;
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::new_active_exec_command;
    use crate::history_cell::AgentMarkdownCell;
    use crate::history_cell::HistoryTimestamp;
    use crate::history_cell::McpInvocation;
    use crate::history_cell::PrefixedWrappedHistoryCell;
    use crate::history_cell::ToolOutputGroup;
    use crate::history_cell::new_active_mcp_tool_call;
    use crate::history_cell::new_patch_events;
    use crate::history_cell::new_user_prompt;
    use codex_app_server_protocol::CommandExecutionSource;
    use codex_shell_command::parse_command::parse_command;
    use std::collections::HashMap;
    use std::path::Path;
    use std::time::Duration;

    let cwd = Path::new(".");
    let timestamp = HistoryTimestamp::from_millis(/*timestamp*/ 1_700_000_000_000);
    let prose = |source: &str| {
        let mut cell = AgentMarkdownCell::new(source.into(), cwd);
        cell.timestamp = timestamp;
        Arc::new(cell) as Arc<dyn HistoryCell>
    };
    let command = vec!["bash".into(), "-lc".into(), "cat foo.rs".into()];
    let mut shell = new_active_exec_command(
        "shell".into(),
        command.clone(),
        parse_command(&command),
        CommandExecutionSource::UnifiedExecStartup,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    shell.complete_call(
        "shell",
        CommandOutput::new(/*exit_code*/ 0, "source".into()),
        Duration::ZERO,
    );
    shell.completed_at = timestamp;
    let mut poll = new_active_exec_command(
        "poll".into(),
        vec!["bash".into(), "-lc".into(), "poll foo".into()],
        Vec::new(),
        CommandExecutionSource::UnifiedExecInteraction,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    poll.complete_call(
        "poll",
        CommandOutput::new(/*exit_code*/ 0, String::new()),
        Duration::ZERO,
    );
    let mut mcp = new_active_mcp_tool_call(
        "mcp".into(),
        McpInvocation {
            server: "tools".into(),
            tool: "inspect".into(),
            arguments: None,
        },
        /*animations_enabled*/ false,
    );
    mcp.complete(
        Duration::ZERO,
        Ok(codex_protocol::mcp::CallToolResult {
            content: Vec::new(),
            structured_content: None,
            is_error: None,
            meta: None,
        }),
    );
    mcp.completed_at = timestamp;
    let batch = ToolOutputCell::from_output(
        "batch",
        Some("outer"),
        "exec",
        /*namespace*/ None,
        &FunctionCallOutputBody::Text("combined result".into()),
    )
    .unwrap();
    let group = ToolOutputGroup::from_cells([
        Arc::new(poll) as Arc<dyn HistoryCell>,
        Arc::new(shell) as Arc<dyn HistoryCell>,
        Arc::new(mcp),
        Arc::new(batch),
    ]);
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(new_user_prompt(
            "Review the layout".into(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )),
        Arc::new({
            let mut cell = PrefixedWrappedHistoryCell::new(
                "Activity Reviewing layout\n\nMore text",
                crate::history_cell::agent_prefix(/*first*/ true),
                crate::history_cell::agent_prefix(/*first*/ false),
            );
            cell.timestamp = timestamp;
            cell
        }),
        group,
    ];
    // Distinct calls coalesce only their Edited heading, never their file controls or bills.
    for file in ["foo.rs", "bar.rs"] {
        cells.extend(
            new_patch_events(
                HashMap::from([(
                    file.into(),
                    FileChange::Add {
                        content: "new line\n".into(),
                    },
                )]),
                cwd,
            )
            .into_iter()
            .map(|cell| Arc::new(cell) as Arc<dyn HistoryCell>),
        );
    }
    let tail = prose(
        "The layout keeps prose, code, and tools aligned.\n\n```rust\n  source_indent();\n``` ",
    );
    let mut view = TranscriptView::default();
    view.record_tool_output_tokens("tool-output-tokens:batch".into(), "outer".into(), Some(128));
    let mut snapshots = Vec::new();
    for width in [64, 32] {
        view.set_presentation(/*detailed*/ false, HistoryRenderMode::Rich);
        view.jump_to_latest();
        cells.push(Arc::clone(&tail));
        let committed = tests::render(&mut view, &cells, width, /*height*/ 50);
        let text = tests::text(&committed);
        assert_eq!(text.matches("  128").count(), 1);
        let layout = view.current_layout(&cells, cells.len() - 1).unwrap();
        assert!(layout.text().contains("  source_indent();"));
        assert!(!layout.text().starts_with(' '));
        // Clocks and disclosure hit targets occupy distinct synthetic columns, while source
        // offsets remain unchanged through live finalization, wrapping, and expansion.
        let clock = timestamp.unwrap().span().content.trim().to_owned();
        assert!(!layout.text().contains(&clock));
        let tool_layout = view.current_layout(&cells, /*index*/ 2).unwrap();
        let target = FoldTarget::Invocation {
            call_id: "poll".into(),
            action: Action::Toggle(
                Port::Input,
                Panel::Visible(crate::history_cell::output_provenance::OutputMode::Fmt),
            ),
        };
        let (row, columns, _) = tool_layout.fold_position(&target).unwrap();
        assert!(tool_layout.fold_at(row, 0).is_none());
        assert_eq!(
            tool_layout.fold_at(row, columns.start),
            Some(target.clone())
        );
        snapshots.push(format!("width={width}\n{}", text.trim_end()));
        if width == 64 {
            assert!(view.focus_fold(&cells, 2, target));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
            let expanded = tests::render(&mut view, &cells, width, /*height*/ 50);
            let text = tests::text(&expanded);
            assert!(text.contains("poll foo"));
            assert!(!text.contains("tokens"));
            snapshots.push(format!("expanded width={width}\n{}", text.trim_end()));
            view.handle_key(crossterm::event::KeyCode::Enter.into(), &cells);
        }
        view.set_presentation(/*detailed*/ true, HistoryRenderMode::Rich);
        view.jump_to_latest();
        let detailed = tests::render(&mut view, &cells, width, /*height*/ 50);
        snapshots.push(format!(
            "detailed width={width}\n{}",
            tests::text(&detailed).trim_end()
        ));
        cells.pop();
    }
    let clock = timestamp.unwrap().span().content.trim().to_owned();
    insta::assert_snapshot!(snapshots.join("\n\n").replace(&clock, "14:17"));
    view.set_presentation(/*detailed*/ false, HistoryRenderMode::Raw);
    view.jump_to_latest();
    let raw = tests::render(&mut view, &cells, /*width*/ 64, /*height*/ 50);
    assert!(tests::text(&raw).starts_with("Review the layout\n"));
}

#[test]
fn waits_and_agent_events_preserve_captured_text_without_extra_gaps() {
    let process = |state: Option<i64>| {
        serde_json::json!({
            "chunk_id": "sample", "wall_time_seconds": 5.0, "output": "",
            "exit_code": state, "session_id": if state.is_none() { Some(42) } else { None },
        })
        .to_string()
    };
    let running = "Script running with cell ID 7\nWall time 1.0 seconds\nOutput:\n";
    let completed = "Script completed\nWall time 5.0 seconds\nOutput:\n";
    let cases = [
        (
            "exec",
            FunctionCallOutputBody::Text(format!("{completed}{}", process(None))),
            47,
        ),
        ("exec", FunctionCallOutputBody::Text(running.into()), 18),
        ("wait", FunctionCallOutputBody::Text(running.into()), 18),
        // Code Mode can split the envelope and process result into separate text blocks.
        (
            "wait",
            FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: completed.into(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: process(Some(0)),
                },
            ]),
            47,
        ),
        (
            "exec",
            FunctionCallOutputBody::Text(format!("{completed}{}", process(Some(2)))),
            47,
        ),
        ("wait", FunctionCallOutputBody::Text(completed.into()), 13),
        (
            "wait",
            FunctionCallOutputBody::Text(
                "Script terminated\nWall time 1.0 seconds\nOutput:\n".into(),
            ),
            13,
        ),
        (
            "exec",
            FunctionCallOutputBody::Text("a computed value".into()),
            4,
        ),
        (
            "wait",
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
                text: "Script completed\nOutput:\n".into(),
            }]),
            7,
        ),
        (
            "exec",
            FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "Script completed\nOutput:\n".into(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: "Script failed\nOutput:\n".into(),
                },
            ]),
            14,
        ),
        (
            "exec",
            FunctionCallOutputBody::Text("Script completed\nOutput:\n".into()),
            7,
        ),
    ];
    let agent_item = |kind, id: &str| ThreadItemPayload::SubAgentActivity {
        id: id.into(),
        kind,
        agent_thread_id: "00000000-0000-0000-0000-000000000001".into(),
        agent_path: "/root/worker".into(),
        message_preview: None,
        model: None,
        reasoning_effort: None,
    };
    let agent = |kind| -> Arc<dyn HistoryCell> {
        Arc::from(
            crate::multi_agents::sub_agent_activity_history_cell(&agent_item(
                kind,
                &format!("{kind:?}"),
            ))
            .unwrap(),
        )
    };
    let mut cells: Vec<Arc<dyn HistoryCell>> = vec![
        Arc::new(PlainHistoryCell::new(vec!["Before tools".into()])),
        agent(SubAgentActivityKind::Started),
    ];
    let mut view = TranscriptView::default();
    for (index, (name, output, tokens)) in cases.iter().enumerate() {
        let id = format!("poll-{index}");
        let cell =
            ToolOutputCell::from_output(&id, Some(&id), name, /*namespace*/ None, output).unwrap();
        let header_only =
            matches!(output, FunctionCallOutputBody::ContentItems(items) if items.len() == 1);
        assert_eq!(
            cell.activity_disclosure(/*width*/ 100).is_some(),
            !header_only
        );
        view.record_tool_output_tokens(format!("tool-output-tokens:{id}"), id, Some(*tokens));
        cells.push(Arc::new(cell));
        if index == 1 {
            cells.push(agent(SubAgentActivityKind::Interacted));
        }
        if index == 4 {
            cells.push(agent(SubAgentActivityKind::Interrupted));
        }
    }
    cells.push(agent(SubAgentActivityKind::Completed));
    let polling = crate::history_cell::ToolOutputGroup::from_cells([1, 3].map(|index| {
        Arc::new(
            ToolOutputCell::from_output(
                &format!("notification-{index}"),
                Some("one-call"),
                cases[index].0,
                /*namespace*/ None,
                &cases[index].1,
            )
            .unwrap(),
        ) as Arc<dyn HistoryCell>
    }));
    assert!(
        polling.display_lines(/*width*/ 100)[0]
            .to_string()
            .ends_with("Waited")
    );
    assert!(polling.activity_disclosure(/*width*/ 100).is_some());
    cells.push(polling);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 100, /*height*/ 40,
    );
    let mut buffer = Buffer::empty(area);
    cells.push(Arc::new(PlainHistoryCell::new(vec!["After tools".into()])));
    view.render(area, &mut buffer, &cells);
    let rendered = tests::text(&buffer).trim_end().to_owned();
    assert_eq!(rendered.matches("\n\n").count(), 2);
    insta::assert_snapshot!(rendered);

    // A following live tool must be as tightly spaced as its committed counterpart.
    cells.pop();
    let tail = ToolOutputCell::from_output(
        "tail",
        Some("tail"),
        "wait",
        /*namespace*/ None,
        &FunctionCallOutputBody::Text(running.into()),
    )
    .unwrap();
    let ids = tail.activity_ids();
    view.sync_live_activity(&cells, ids);
    view.sync_live_invocation_tail(100, None, &tail, Vec::new());
    let mut live = Buffer::empty(area);
    view.render(area, &mut live, &cells);
    view.sync_live_tail(/*width*/ 100, /*key*/ None, |_| None);
    cells.push(Arc::new(tail));
    let mut committed = Buffer::empty(area);
    view.render(area, &mut committed, &cells);
    assert_eq!(live, committed);

    // Raw output retains both harness execution headers and literal script/notify text.
    let isolated = [3, 8, 9, 10].map(|index| {
        let id = format!("header-{index}");
        Arc::new(
            ToolOutputCell::from_output(
                &id,
                Some(&id),
                cases[index].0,
                /*namespace*/ None,
                &cases[index].1,
            )
            .unwrap(),
        ) as Arc<dyn HistoryCell>
    });
    let mut expanded_view = TranscriptView::default();
    for cell in &isolated {
        let output = cell.as_any().downcast_ref::<ToolOutputCell>().unwrap();
        expanded_view.invocations.apply(
            output.outer_call_id().unwrap(),
            Action::Toggle(
                Port::Output,
                Panel::Visible(crate::history_cell::output_provenance::OutputMode::Raw),
            ),
        );
    }
    let expanded = tests::render(
        &mut expanded_view,
        &isolated,
        /*width*/ 100,
        /*height*/ 24,
    );
    let text = tests::text(&expanded).trim_end().to_string();
    assert_eq!(text.matches("Script completed").count(), 4);
    assert_eq!(text.matches("Script failed").count(), 1);
    insta::assert_snapshot!("execution_headers_and_literal_text", text);

    // Empty returns may precede their measurement; only a known zero hides them. The
    // status slot and media must not be mistaken for an empty successful return.
    let mut examples = Vec::new();
    for (label, name, body, tokens) in [
        (
            "unknown",
            "tool",
            FunctionCallOutputBody::Text(String::new()),
            None,
        ),
        (
            "positive",
            "tool",
            FunctionCallOutputBody::Text(String::new()),
            Some(3),
        ),
        (
            "zero",
            "tool",
            FunctionCallOutputBody::Text(String::new()),
            Some(0),
        ),
        (
            "completed",
            "exec",
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
                text: completed.into(),
            }]),
            Some(0),
        ),
        (
            "failed",
            "exec",
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputText {
                text: "Script failed\nOutput:\n".into(),
            }]),
            Some(0),
        ),
        (
            "media",
            "exec",
            FunctionCallOutputBody::ContentItems(vec![FunctionCallOutputContentItem::InputImage {
                image: codex_protocol::models::ImageReference::Inline {
                    image_url: "data:image/png;base64,AA==".into(),
                },
                detail: None,
            }]),
            Some(0),
        ),
    ] {
        let cell = Arc::new(
            ToolOutputCell::from_output(label, Some(label), name, /*namespace*/ None, &body)
                .unwrap(),
        ) as Arc<dyn HistoryCell>;
        let cells = vec![
            agent(SubAgentActivityKind::Started),
            cell.clone(),
            agent(SubAgentActivityKind::Completed),
        ];
        let mut view = TranscriptView::default();
        view.record_tool_output_tokens(format!("tool-output-tokens:{label}"), label.into(), tokens);
        let buffer = tests::render(&mut view, &cells, /*width*/ 64, /*height*/ 6);
        examples.push(format!("{label}\n{}", tests::text(&buffer).trim_end()));
        if matches!(label, "zero" | "completed") {
            let layout = view.current_layout(&cells, /*index*/ 1).unwrap();
            assert!(layout.text().is_empty() && layout.fold_targets().is_empty());
        }
        assert_eq!(view.activity_output_tokens(&cell.activity_ids()), tokens);
    }
    insta::assert_snapshot!("empty_return_measurements", examples.join("\n\n"));

    // Wait wake evidence survives lifecycle revision without becoming output authority.
    let wait_item = |status, wait_completion| ThreadItemPayload::CollabAgentToolCall {
        id: "agent-wait".into(),
        tool: CollabAgentTool::Wait,
        status,
        sender_thread_id: "00000000-0000-0000-0000-000000000001".into(),
        receiver_thread_ids: Vec::new(),
        prompt: None,
        model: None,
        reasoning_effort: None,
        wait_completion,
        agents_states: HashMap::default(),
    };
    let cwd = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        std::env::current_dir().unwrap(),
    )
    .unwrap();
    let mut retained = vec![agent(SubAgentActivityKind::Started)];
    let mut stages = Vec::new();
    {
        let status = CollabAgentToolCallStatus::Completed;
        let cell = Arc::new(crate::history_cell::ToolItemCell::new(
            "agent-wait".into(),
            crate::multi_agents::tool_call_history_cell(
                &wait_item(status.clone(), None),
                /*cached_spawn_request*/ None,
                |_| AgentMetadata::default(),
            )
            .unwrap(),
        )) as Arc<dyn HistoryCell>;
        let revision = crate::history_cell::reconcile_tool_calls(&retained, &cell);
        for (index, cell) in revision.replacements {
            retained[index] = cell;
        }
        retained.extend(revision.remainder);
        let mut view = TranscriptView::default();
        let buffer = tests::render(&mut view, &retained, /*width*/ 64, /*height*/ 6);
        stages.push(format!("{status:?}\n{}", tests::text(&buffer).trim_end()));
        let replay = crate::thread_transcript::thread_items_to_transcript_cells(
            /*thread_id*/ None,
            &cwd,
            [
                wait_item(CollabAgentToolCallStatus::InProgress, None),
                wait_item(status, None),
            ],
            crate::thread_transcript::RawReasoningVisibility::Hidden,
            /*config*/ None,
            &|_| AgentMetadata::default(),
            crate::history_cell::ImagePathOrigin::Local,
        );
        assert_eq!(replay.len(), 1);
        assert_eq!(
            retained[1].display_lines(/*width*/ 64),
            replay[0].display_lines(/*width*/ 64)
        );
        {
            assert!(!tests::text(&buffer).contains("waiting"));
            assert_eq!(retained[1].raw_lines(), [] as [Line<'_>; 0]);
            assert!(tests::text(&buffer).contains("outcome unavailable"));
        }
    }
    insta::assert_snapshot!("agent_wait_lifecycle", stages.join("\n\n"));
    // A tool activity owns its exact capture across interleaving; a later autonomous
    // completion of that same agent is not the tool's completion or result owner.
    let capture = r#"{"previous_status":"running"}"#;
    let interrupted = agent_item(SubAgentActivityKind::Interrupted, "interrupt-p");
    let replay = crate::thread_transcript::thread_items_to_transcript_cells(
        None,
        &cwd,
        [
            interrupted.clone(),
            agent_item(SubAgentActivityKind::Interacted, "message-q"),
            ThreadItemPayload::FunctionCallOutput {
                id: "interrupt-result".into(),
                call_id: Some("interrupt-p".into()),
                name: "interrupt_agent".into(),
                namespace: Some("wrangler_agents".into()),
                output: FunctionCallOutputBody::Text(capture.into()),
            },
            agent_item(SubAgentActivityKind::Completed, "autonomous-completion"),
        ],
        crate::thread_transcript::RawReasoningVisibility::Hidden,
        None,
        &|_| AgentMetadata::default(),
        crate::history_cell::ImagePathOrigin::Local,
    );
    assert_eq!(replay.len(), 3);
    assert_eq!(replay[0].outer_call_id(), Some("interrupt-p"));
    assert!(
        replay[0]
            .activity_ids()
            .contains(&"agent-activity:interrupt-p".into())
    );
    assert!(
        replay[0]
            .raw_lines()
            .iter()
            .any(|line| line.to_string() == capture)
    );
    assert_eq!(replay[1].outer_call_id(), Some("message-q"));
    assert_eq!(replay[2].tool_call_ids(), [] as [&str; 0]);
    assert_eq!(replay[2].outer_call_id(), None);
    let live = crate::multi_agents::sub_agent_activity_history_cell(&interrupted).unwrap();
    let scoped = crate::thread_transcript::scoped_item_cells(
        interrupted,
        &cwd,
        crate::code_mode_transcript::Source::Live,
        &|_| AgentMetadata::default(),
        crate::history_cell::ImagePathOrigin::Local,
    );
    assert_eq!(live.tool_call_ids(), scoped[0].tool_call_ids());
    assert_eq!(live.activity_ids(), scoped[0].activity_ids());
    assert_eq!(live.outer_call_id(), scoped[0].outer_call_id());

    let mut view = TranscriptView::default();
    let closed = tests::render(&mut view, &replay, 100, 12);
    assert!(!tests::text(&closed).contains(capture));
    assert!(view.focus_fold(
        &replay,
        0,
        FoldTarget::Invocation {
            call_id: "interrupt-p".into(),
            action: Action::Toggle(
                Port::Output,
                Panel::Visible(crate::history_cell::output_provenance::OutputMode::Raw),
            ),
        }
    ));
    view.handle_key(crossterm::event::KeyCode::Enter.into(), &replay);
    let result = tests::render(&mut view, &replay, 100, 12);
    assert!(tests::text(&result).contains(capture));
    // Missing legacy ownership is not proximity: its output item still has a named fold.
    let orphan = Arc::new(
        ToolOutputCell::from_output(
            "orphan-result",
            None,
            "inspect",
            Some("tools"),
            &FunctionCallOutputBody::Text("legacy exact result".into()),
        )
        .unwrap(),
    ) as Arc<dyn HistoryCell>;
    assert_eq!(orphan.outer_call_id(), None);
    assert!(
        orphan.display_lines(100)[0]
            .to_string()
            .contains("tools.inspect")
    );
    assert!(crate::history_cell::ToolOutputGroup::attach(&replay, orphan.clone()).is_none());
    let expanded = FoldState::from_iter(orphan.activity_ids());
    let folded = orphan
        .output_parts(
            100,
            0,
            &expanded,
            crate::history_cell::output_provenance::OutputMode::Raw,
        )
        .unwrap();
    assert_eq!(folded.controls[0].id, "result:orphan-result");
    assert!(!folded.controls[0].expanded);
    assert!(
        !TextLayout::new(folded.lines, 100)
            .text()
            .contains("legacy exact result")
    );
    for (status, outcome, label) in [
        (
            CollabAgentToolCallStatus::Completed,
            AgentWaitOutcome::TimedOut,
            "no updates before timeout",
        ),
        (
            CollabAgentToolCallStatus::Completed,
            AgentWaitOutcome::MailboxActivity,
            "updates available",
        ),
        (
            CollabAgentToolCallStatus::Completed,
            AgentWaitOutcome::Steered,
            "new input arrived",
        ),
        (
            CollabAgentToolCallStatus::Failed,
            AgentWaitOutcome::TimedOut,
            "Agent wait failed",
        ),
        (
            CollabAgentToolCallStatus::Interrupted,
            AgentWaitOutcome::TimedOut,
            "Agent wait interrupted",
        ),
    ] {
        let item = wait_item(
            status,
            Some(AgentWaitCompletion {
                outcome,
                elapsed_ms: 1234,
            }),
        );
        let cell =
            crate::multi_agents::tool_call_history_cell(&item, None, |_| AgentMetadata::default())
                .unwrap();
        let heading = cell.display_lines(120)[0].to_string();
        assert!(heading.contains(label), "{heading}");
        assert!(!heading.contains("task completed"));
        assert_eq!(cell.raw_lines(), [] as [Line<'_>; 0]);
    }
}
