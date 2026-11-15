//! Copy and painting regressions over the shared source-backed text layout.

use super::TextLayout;
use super::gutter;
use crate::history_cell::activity_parts::FoldState;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::adaptive_wrap_hyperlink_lines;
use crate::terminal_hyperlinks::prefix_hyperlink_lines;
use crate::text_selection::SelectionUnit;
use crate::wrapping::RtOptions;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;

#[test]
fn token_gutter_rounding_color_and_reflow_preserve_source() {
    use crate::transcript_geometry::GUTTER_COLUMNS;
    use crate::transcript_geometry::TOKEN_COLUMN;
    use crate::transcript_geometry::TOKEN_COLUMNS;

    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (0, 0, 0),
        },
        || {
            let colors =
                [0, 301, 1_001, 3_001, 10_001].map(|tokens| gutter::token_span(tokens).style.fg);
            for (tokens, expected, tier) in [
                (0, "    0", 0),
                (300, "  300", 0),
                (301, "  301", 1),
                (1_000, " 1000", 1),
                (1_001, " 1001", 2),
                (3_000, " 3000", 2),
                (3_001, " 3001", 3),
                (10_000, "10000", 3),
                (10_001, "10001", 4),
                (99_999, "99999", 4),
                (100_000, " 100K", 4),
                (123_456, " 123K", 4),
                (999_499, " 999K", 4),
                (999_500, "1.00M", 4),
                (1_234_567, "1.23M", 4),
                (12_345_678, "12.3M", 4),
                (123_456_789, " 123M", 4),
                (1_000_000_000, "1.00G", 4),
                (1_000_000_000_000, "1.00T", 4),
                (1_000_000_000_000_000, "1.00P", 4),
                (u64::MAX, "18.4E", 4),
            ] {
                let span = gutter::token_span(tokens);
                assert_eq!(
                    (span.content.as_ref(), span.style.fg),
                    (expected, colors[tier])
                );
                assert_eq!(span.width(), usize::from(TOKEN_COLUMNS));
            }
            assert_eq!(
                colors
                    .iter()
                    .copied()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                5
            );
            let source = "• Ran a command whose body wraps over several continuation rows";
            for tokens in [None, Some(0), Some(999_500)] {
                let layout = TextLayout::new(
                    crate::transcript_geometry::rich_lines(vec![
                        Line::from(source).bold().dim().into(),
                    ]),
                    /*width*/ 80,
                )
                .with_header_tokens(tokens, crate::transcript_geometry::TokenSeat::Gutter)
                .with_leading_separator();
                for width in [80, 40, 10] {
                    let layout = layout.rewrap(width).with_leading_spacer();
                    assert_eq!(layout.text(), source);
                    let area = Rect::new(/*x*/ 0, /*y*/ 0, width, /*height*/ 12);
                    let mut buffer = Buffer::empty(area);
                    layout.render(area, &mut buffer, /*start_row*/ 0);
                    let rendered = crate::transcript_view::tests::text(&buffer);
                    if width >= TOKEN_COLUMN + TOKEN_COLUMNS {
                        let expected = tokens.map_or("     ".to_owned(), |count| {
                            gutter::token_span(count).content.into_owned()
                        });
                        assert_eq!(
                            (TOKEN_COLUMN..TOKEN_COLUMN + TOKEN_COLUMNS)
                                .map(|x| buffer[(x, 2)].symbol())
                                .collect::<String>(),
                            expected
                        );
                        assert_eq!(buffer[(GUTTER_COLUMNS, 2)].fg, Color::Reset);
                        if tokens.is_some() {
                            assert_eq!(buffer[(TOKEN_COLUMN, 2)].modifier, Modifier::empty());
                        }
                        assert_eq!(
                            rendered.matches("1.00M").count(),
                            usize::from(tokens == Some(999_500))
                        );
                        for row in 3..area.height {
                            assert_eq!(
                                (TOKEN_COLUMN..TOKEN_COLUMN + TOKEN_COLUMNS)
                                    .map(|x| buffer[(x, row)].symbol())
                                    .collect::<String>(),
                                "     "
                            );
                        }
                        for (row_index, row) in layout.rows.iter().enumerate() {
                            for (offset, _) in source[row.source.clone()]
                                .grapheme_indices(/*is_extended*/ true)
                            {
                                let offset = offset + row.source.start;
                                assert_eq!(
                                    layout.position_at(row_index, layout.column_for_offset(offset)),
                                    offset
                                );
                            }
                        }
                    } else {
                        assert!(!rendered.contains("1.00M") && !rendered.contains("1.00"));
                    }
                }
            }
        },
    );
}

#[test]
fn copying_wrapped_text_preserves_only_hard_newlines() {
    let layout = TextLayout::new(
        vec!["alpha beta gamma".into(), "  next line".into()],
        /*width*/ 10,
    );
    assert_eq!(layout.text(), "alpha beta gamma\n  next line");
    assert_eq!(
        (
            layout.position_at(/*row*/ 1, /*column*/ 0),
            layout.row_for_offset(/*offset*/ 11)
        ),
        (11, 1),
    );
    assert_eq!(
        &layout.text()[SelectionUnit::Line.range(layout.text(), /*offset*/ 12)],
        "alpha beta gamma\n"
    );

    // A commit tick can divide one logical code line across several history cells.
    let mut stream = crate::streaming::controller::StreamController::new(
        Some(7),
        std::path::Path::new("/"),
        crate::history_cell::HistoryRenderMode::Rich,
    );
    stream.push("```text\nalpha   beta\n\n界界界界界\nnext\n```\n");
    let mut cells: Vec<std::sync::Arc<dyn crate::history_cell::HistoryCell>> = Vec::new();
    while let Some(cell) = stream.on_commit_tick_batch(/*max_lines*/ 1).0 {
        cells.push(cell.into());
    }
    let expected = TextLayout::new(
        cells
            .iter()
            .flat_map(|cell| cell.transcript_hyperlink_lines(/*width*/ 9))
            .collect(),
        /*width*/ 9,
    );
    let area = Rect::new(
        /*x*/ 0,
        /*y*/ 0,
        /*width*/ crate::transcript_geometry::GUTTER_COLUMNS + 5,
        /*height*/ 64,
    );
    let mut view = crate::transcript_view::TranscriptView::default();
    view.render(area, &mut Buffer::empty(area), &cells);
    view.begin_selection(
        &cells,
        /*column*/ crate::transcript_geometry::GUTTER_COLUMNS - 2,
        /*row*/ 0,
        /*clicks*/ 1,
    );
    view.extend_selection(
        /*column*/ crate::transcript_geometry::GUTTER_COLUMNS + 4,
        /*row*/ 63,
    );
    assert_eq!(view.selected_text(&cells).as_deref(), Some(expected.text()));
}

#[test]
fn scrolled_plaintext_selection_matches_painted_tab_and_word_boundaries() {
    let layout = TextLayout::new(
        vec![
            "hidden".into(),
            "  A\tBCD".into(),
            "These few words will unfortunately not wrap nicely.".into(),
        ],
        /*width*/ 15,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 15, /*height*/ 3,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 1);
    assert_eq!(buffer[(8, 0)].symbol(), "B");
    assert_eq!(buffer[(0, 2)].symbol(), "w");

    let visible = layout.visible_plaintext(/*start*/ 1, area.height);
    let start = visible.position_at(/*row*/ 0, /*column*/ 2);
    let end = visible.position_at(/*row*/ 2, /*column*/ 4);
    assert_eq!(&visible.text()[start..end], "A\tBCD\nThese few words will");
    assert_eq!(visible.text(), "  A\tBCD\nThese few words will");
}

#[test]
fn hit_testing_uses_graphemes_and_does_not_select_padding() {
    let layout = TextLayout::new(vec!["a界e\u{301}👩‍💻".into()], /*width*/ 12);
    assert_eq!(
        (0..12)
            .map(|column| layout.position_at(/*row*/ 0, column))
            .collect::<Vec<_>>(),
        vec![0, 1, 1, 4, 7, 7, 18, 18, 18, 18, 18, 18],
    );
    assert_eq!(
        &layout.text()[SelectionUnit::Word.range(layout.text(), /*offset*/ 4)],
        "e\u{301}"
    );
}

#[test]
fn visible_rows_preserve_styles_and_wrapped_hyperlinks() {
    let mut line = HyperlinkLine::new(Line::from("read ".green()));
    line.push_span("documentation".cyan(), Some("https://example.com/docs"));
    let layout = TextLayout::new(vec![line], /*width*/ 8);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 8, /*height*/ 2,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 1);
    assert_eq!(buffer[(0, 0)].fg, Color::Cyan);
    assert_eq!(
        buffer[(0, 0)].symbol(),
        "\x1b]8;;https://example.com/docs\x07d\x1b]8;;\x07",
    );
    assert_eq!(layout.position_at(/*row*/ 1, /*column*/ 0), 5);
}

#[test]
fn selection_highlights_wide_graphemes_without_trailing_padding() {
    let layout = TextLayout::new(vec!["a界b".into()], /*width*/ 8);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 8, /*height*/ 1,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 0);
    layout.highlight(1..4, area, &mut buffer, /*start_row*/ 0);
    assert_eq!(
        (0..8)
            .map(|column| buffer[(column, 0)].modifier.contains(Modifier::REVERSED))
            .collect::<Vec<_>>(),
        vec![false, true, true, false, false, false, false, false],
    );
}

#[test]
fn selected_newlines_respect_wrapping_and_synthetic_rows() {
    for (width, expected) in [(8, vec![(6, 5)]), (4, vec![])] {
        let layout = TextLayout::new(
            vec!["".into(), "alpha beta  ".into(), "".into(), "界".into()],
            width,
        )
        .with_disclosure_control_at("details".into(), /*source_offset*/ 0)
        .with_leading_separator();
        let area = Rect::new(/*x*/ 2, /*y*/ 1, width, /*height*/ 9);
        let mut buffer = Buffer::empty(area);
        layout.render(area, &mut buffer, /*start_row*/ 0);
        for range in [0..1, 13..15] {
            layout.highlight_selection(
                range,
                /*next*/ None,
                area,
                &mut buffer,
                /*start_row*/ 0,
            );
        }
        assert_eq!(
            (area.top()..area.bottom())
                .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
                .collect::<Vec<_>>(),
            expected,
        );
    }
}

#[test]
fn offsets_can_reach_rows_after_u16_limit() {
    let layout = TextLayout::new(vec!["x".repeat(/*n*/ 65_538).into()], /*width*/ 1);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 1, /*height*/ 1,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 65_537);
    assert_eq!(
        (
            layout.row_count(),
            layout.row_for_offset(/*offset*/ 65_537),
            layout.position_at(/*row*/ 65_537, /*column*/ 0),
            buffer[(0, 0)].symbol(),
        ),
        (65_538, 65_537, 65_537, "x"),
    );
}

#[test]
fn aligned_text_excludes_synthetic_leading_columns() {
    let mut line = HyperlinkLine::new(Line::default().right_aligned());
    line.push_span("wide".into(), Some("https://example.com"));
    let layout = TextLayout::new(vec![line], /*width*/ 8);
    assert_eq!(layout.link_at(/*row*/ 0, /*column*/ 3), None);
    assert_eq!(
        layout.link_at(/*row*/ 0, /*column*/ 4).as_deref(),
        Some("https://example.com")
    );
    assert_eq!(
        (0..8)
            .map(|column| layout.position_at(/*row*/ 0, column))
            .collect::<Vec<_>>(),
        vec![0, 0, 0, 0, 0, 1, 2, 3],
    );
}

#[test]
fn empty_logical_lines_keep_their_hard_breaks() {
    let layout = TextLayout::new(vec!["".into(), "text".into(), "".into()], /*width*/ 10);
    assert_eq!(
        (
            layout.text(),
            layout.row_count(),
            SelectionUnit::Line.range(layout.text(), /*offset*/ 0),
            SelectionUnit::Line.range(layout.text(), /*offset*/ 5)
        ),
        ("\ntext\n", 3, 0..1, 1..6),
    );
}

#[test]
fn source_provenance_preserves_spaces_and_excludes_gutters() {
    let source = "alpha   beta gamma";
    let lines = adaptive_wrap_hyperlink_lines(&[source.into()], RtOptions::new(/*width*/ 10));
    let lines = prefix_hyperlink_lines(lines, "› ".bold(), "  ".into());
    let layout = TextLayout::new(lines, /*width*/ 12);
    assert_eq!(layout.text(), source);
    assert_eq!(layout.position_at(/*row*/ 0, /*column*/ 0), 0);
    assert_eq!(layout.position_at(/*row*/ 0, /*column*/ 2), 0);
    assert_eq!(layout.column_for_offset(/*offset*/ 0), 2);
    let layout = layout.with_fold_header(super::HeaderRows {
        lines: vec![
            super::HeaderRows::label(Line::from("12:34 界 Command [Raw] [Fmt]  123"), 6..16),
            super::HeaderRows::label(Line::from("[in] [out]"), 0..0),
        ],
        fold: None,
        tool_mark: None,
        controls: vec![
            super::HeaderControl {
                row: 0,
                columns: 17..22,
                target: crate::history_cell::activity_parts::FoldTarget::OutputMode(
                    crate::history_cell::output_provenance::OutputMode::Raw,
                ),
            },
            super::HeaderControl {
                row: 0,
                columns: 23..28,
                target: crate::history_cell::activity_parts::FoldTarget::OutputMode(
                    crate::history_cell::output_provenance::OutputMode::Fmt,
                ),
            },
        ],
    });
    let expected = format!("界 Command\n{source}");
    // Paint-only liveness survives header insertion, spacers and reflow without becoming source.
    let label = "Code Mode (α)";
    let label_width = unicode_width::UnicodeWidthStr::width(label);
    let header = super::HeaderRows {
        lines: vec![super::HeaderRows::label(Line::from(label), 0..label_width)],
        fold: None,
        controls: Vec::new(),
        tool_mark: None,
    }
    .with_tool_mark(Some(crate::motion::MotionMode::Animated), 40);
    let mark_column = u16::try_from(label_width + 1).unwrap();
    let nested = TextLayout::with_invocation_rows(
        vec![super::HeaderRows::label(Line::from(label), 0..label_width)],
        40,
        Vec::new(),
    )
    .with_tool_mark(super::ToolMark {
        row: 0,
        column: mark_column,
        motion: crate::motion::MotionMode::Animated,
    })
    .with_fold_header(header)
    .with_leading_separator();
    for width in [40, 20, 40] {
        let layout = nested.rewrap(width);
        let area = ratatui::layout::Rect::new(0, 0, width, 6);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        layout.render(area, &mut buffer, 0);
        for row in [1, 2] {
            assert_eq!(buffer[(mark_column, row)].symbol(), "•");
            assert!(layout.has_animated_tool_mark(usize::from(row)));
        }
        assert_eq!(layout.text(), format!("{label}\n{label}"));
        let mut lines = Vec::new();
        layout.copy_lines(0..layout.text().len(), "", &mut lines);
        assert_eq!(
            crate::markdown_copy::literal_selection(&lines, layout.text()),
            layout.text()
        );
    }
    for width in [12, 48, 12] {
        let resized = layout.rewrap(width);
        assert_eq!(resized.text(), expected);
        let mut lines = Vec::new();
        resized.copy_lines(0..resized.text().len(), "", &mut lines);
        assert_eq!(
            crate::markdown_copy::literal_selection(&lines, resized.text()),
            expected
        );
        assert_eq!(resized.position_at(0, 9), 4);
    }
}

#[test]
fn selected_revision_rewraps_both_directions_with_styles_and_indents() {
    let mut source = HyperlinkLine::new(Line::from("alpha ".green()));
    source.push_span("beta gamma".cyan(), Some("https://example.com"));
    let lines = adaptive_wrap_hyperlink_lines(
        &[source],
        RtOptions::new(/*width*/ 10)
            .initial_indent("› ".into())
            .subsequent_indent("  ".into()),
    );
    let narrow = TextLayout::new(lines, /*width*/ 10);
    let wide = narrow.rewrap(/*width*/ 24);
    let restored = wide.rewrap(/*width*/ 10);
    assert_eq!(
        (
            narrow.text(),
            wide.text(),
            restored.text(),
            wide.row_count()
        ),
        (
            "alpha beta gamma",
            "alpha beta gamma",
            "alpha beta gamma",
            1
        ),
    );
    let narrow_area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 10, /*height*/ 3,
    );
    let mut original_buffer = Buffer::empty(narrow_area);
    let mut restored_buffer = Buffer::empty(narrow_area);
    narrow.render(narrow_area, &mut original_buffer, /*start_row*/ 0);
    restored.render(narrow_area, &mut restored_buffer, /*start_row*/ 0);
    assert_eq!(restored_buffer, original_buffer);
    assert_eq!(
        wide.link_at(/*row*/ 0, /*column*/ 8).as_deref(),
        Some("https://example.com")
    );
}

#[test]
fn partial_stream_fragment_does_not_copy_unseen_source_text() {
    let lines =
        adaptive_wrap_hyperlink_lines(&["alpha beta gamma".into()], RtOptions::new(/*width*/ 7));
    let first = TextLayout::new(vec![lines[0].clone()], /*width*/ 7);
    let last = TextLayout::new(vec![lines[2].clone()], /*width*/ 7);
    assert_eq!((first.text(), last.text()), ("alpha", "gamma"));
}

#[test]
fn narrow_wrapping_keeps_joined_emoji_whole() {
    let layout = TextLayout::new(vec!["👩‍💻x".into()], /*width*/ 2);
    assert_eq!(
        (
            layout.row_count(),
            layout.position_at(/*row*/ 1, /*column*/ 0)
        ),
        (2, 11),
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 2, /*height*/ 2,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 0);
    assert_eq!(buffer[(0, 0)].symbol(), "👩‍💻");
}

#[test]
fn fullscreen_markdown_keeps_a_fitting_url_on_one_row() {
    use crate::history_cell::AgentMarkdownCell;
    use crate::history_cell::HistoryCell;
    let cell = AgentMarkdownCell::new(
        "See the documentation at https://example.com/docs/reference.".into(),
        std::path::Path::new("."),
    );
    let layout = TextLayout::new(
        cell.retained_hyperlink_lines(/*width*/ 44, /*detailed*/ false),
        /*width*/ 44,
    );
    let rows = layout
        .rows
        .iter()
        .map(|row| row.line.line.to_string())
        .collect::<Vec<_>>();
    insta::assert_snapshot!(rows.join("\n"), @"
    See the documentation at
    https://example.com/docs/reference.
    ");
}

#[test]
fn source_backed_markdown_copy_preserves_code_indentation() {
    use crate::history_cell::AgentMarkdownCell;
    use crate::history_cell::HistoryCell;
    let cell = AgentMarkdownCell::new(
        "first **paragraph** with words\n\n```rust\n    let value = 12;\n```".into(),
        std::path::Path::new("."),
    );
    let layout = TextLayout::new(
        cell.display_hyperlink_lines(/*width*/ 14),
        /*width*/ 14,
    );
    assert!(layout.text().contains("first paragraph with words"));
    assert!(layout.text().contains("    let value = 12;"));
    assert_eq!(layout.rewrap(/*width*/ 40).text(), layout.text());
}

#[test]
fn repeated_source_prefix_never_becomes_a_synthetic_indent() {
    let source = "x x x x x";
    let wrapped = adaptive_wrap_hyperlink_lines(
        &[HyperlinkLine::from(source)],
        RtOptions::new(/*width*/ 4)
            .initial_indent("x ".into())
            .subsequent_indent("x ".into()),
    );
    let layout = TextLayout::new(wrapped, /*width*/ 4);
    assert_eq!(layout.text(), source);
    assert_eq!(layout.rewrap(/*width*/ 12).text(), source);
}

#[test]
fn command_and_output_copy_preserve_hard_lines_across_resize() {
    use crate::exec_cell::CommandOutput;
    use crate::exec_cell::ExecCall;
    use crate::exec_cell::ExecCell;
    use crate::history_cell::HistoryCell;
    let command = "printf 'alpha beta gamma delta'";
    let output = "  output alpha beta gamma delta\nnext line";
    let cell = ExecCell::new(
        ExecCall {
            rows: Default::default(),
            call_id: "copy-test".into(),
            command: vec!["bash".into(), "-lc".into(), command.into()],
            parsed: Vec::new(),
            output: Some(CommandOutput::new(/*exit_code*/ 0, output.into())),
            source: codex_app_server_protocol::CommandExecutionSource::Agent,
            start_time: None,
            duration: None,
            interaction_input: None,
        },
        /*animations_enabled*/ false,
    );
    let rendered = crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            let parts = cell
                .activity_parts(
                    /*width*/ 32,
                    /*header_reserve*/ 0,
                    &FoldState::new(),
                )
                .unwrap();
            let expanded = cell
                .activity_ids()
                .into_iter()
                .chain(parts.controls.into_iter().map(|control| control.id))
                .collect();
            [
                (
                    "expanded_command_copy",
                    cell.activity_parts(/*width*/ 32, /*header_reserve*/ 0, &expanded)
                        .unwrap()
                        .lines,
                    format!(
                        "• Command\n{command}\n\nStored result · exact capture unavailable\n{output}"
                    ),
                ),
                (
                    "detailed_command_copy",
                    cell.transcript_hyperlink_lines(/*width*/ 32),
                    format!("{command}\n{output}"),
                ),
            ]
        },
    );
    for (name, lines, expected) in rendered {
        let layout = TextLayout::new(lines, /*width*/ 32);
        assert_eq!(layout.text(), expected);
        let narrow = layout.rewrap(/*width*/ 24);
        let wide = narrow.rewrap(/*width*/ 64);
        assert_eq!((narrow.text(), wide.text()), (layout.text(), layout.text()));
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 24, /*height*/ 8,
        );
        let mut buffer = Buffer::empty(area);
        narrow.render(area, &mut buffer, /*start_row*/ 0);
        insta::assert_snapshot!(name, format!("{buffer:?}"));
    }
}

#[test]
fn background_stdin_copy_preserves_indentation_and_newlines() {
    use crate::history_cell::HistoryCell;
    use crate::history_cell::UnifiedExecInteractionCell;
    let stdin = "    first long line of input\nnext";
    let cell = UnifiedExecInteractionCell::new(/*command_display*/ None, stdin.into());
    let layout = TextLayout::new(
        cell.transcript_hyperlink_lines(/*width*/ 24),
        /*width*/ 24,
    );
    assert!(layout.text().ends_with(stdin));
    assert_eq!(layout.rewrap(/*width*/ 48).text(), layout.text());
}
