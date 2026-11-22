//! Cache reuse, bounded lifetime, and invalidation across frames.

use super::*;
use pretty_assertions::assert_eq;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[derive(Debug, Default)]
struct Cell {
    renders: AtomicUsize,
    tick: AtomicUsize,
    mutable: bool,
}

impl HistoryCell for Cell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.renders.fetch_add(/*val*/ 1, Ordering::Relaxed);
        vec![format!("tick {}", self.tick.load(Ordering::Relaxed)).into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        vec![]
    }

    fn has_stable_transcript_height(&self) -> bool {
        !self.mutable
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        Some(self.tick.load(Ordering::Relaxed) as u64)
    }
}

#[test]
fn tool_output_disclosure_shows_fixed_token_column() {
    let layout = activity_layout(
        ActivityTranscriptLines {
            output_mode: None,
            is_tool_entry: true,
            header: Some(crate::history_cell::activity_parts::ActivityHeader::at_end(
                &Line::from("Called"),
            )),
            controls: Vec::new(),

            activity: vec![HyperlinkLine::from("Called tool")],
            auxiliary: Vec::new(),
            disclosure: Some(ActivityDisclosure::Generic),
            header_tokens: Some(1_280),
        },
        crate::transcript_geometry::GUTTER_COLUMNS,
        /*width*/ 80,
        /*expanded*/ false,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 2,
    );
    let mut buffer = Buffer::empty(area);
    layout.render(area, &mut buffer, /*start_row*/ 0);
    insta::assert_snapshot!(format!("{buffer:?}"));
}

#[test]
fn tool_heading_decorations_preserve_source_and_hit_targets() {
    use crate::history_cell::McpInvocation;
    use crate::history_cell::new_active_mcp_tool_call;
    use codex_protocol::mcp::CallToolResult;
    use serde_json::json;
    use std::time::Duration;
    use unicode_segmentation::UnicodeSegmentation;

    let mut mcp = new_active_mcp_tool_call(
        "unicode-call".into(),
        McpInvocation {
            server: "server".into(),
            environment_id: None,
            tool: "search".into(),
            arguments: Some(
                json!({"query": "页面 👩‍💻 e\u{301} a long query", "path": "页面 👩‍💻/lib.rs"}),
            ),
        },
        /*animations_enabled*/ false,
    );
    mcp.complete(
        Duration::ZERO,
        Ok(CallToolResult {
            content: vec![json!({"type": "text", "text": "retained result\nsecond result line"})],
            is_error: None,
            structured_content: None,
            meta: None,
        }),
    );
    let mut snapshots = Vec::new();
    for (width, expanded, tokens) in [
        (90, false, Some(82)),
        (58, false, Some(82)),
        (90, true, Some(82)),
        (58, false, None),
    ] {
        let layout = activity_layout(
            ActivityTranscriptLines {
                output_mode: None,
                is_tool_entry: true,
                header: mcp.activity_header(),
                controls: Vec::new(),

                activity: if expanded {
                    mcp.expanded_hyperlink_lines(crate::transcript_geometry::content_width(
                        width,
                        HistoryRenderMode::Rich,
                    ))
                } else {
                    mcp.compact_hyperlink_lines(crate::transcript_geometry::content_width(
                        width,
                        HistoryRenderMode::Rich,
                    ))
                },
                auxiliary: Vec::new(),
                header_tokens: tokens,
                disclosure: mcp.activity_disclosure(width),
            },
            crate::transcript_geometry::GUTTER_COLUMNS,
            width,
            expanded,
        );
        let area = Rect::new(/*x*/ 0, /*y*/ 0, width, /*height*/ 8);
        let mut buffer = Buffer::empty(area);
        layout.render(area, &mut buffer, /*start_row*/ 0);
        assert_eq!(
            layout.disclosure_columns(),
            crate::transcript_geometry::CONTROL_COLUMN
                ..crate::transcript_geometry::CONTROL_COLUMN
                    + crate::transcript_geometry::CONTROL_COLUMNS
        );
        assert!(!layout.text().contains("tokens)"));
        assert!(layout.text().starts_with("Called"));
        if tokens.is_some() {
            let text = crate::transcript_view::tests::text(&buffer);
            assert!([false, true].into_iter().any(|open| {
                text.contains(&format!(
                    "{}   • Called",
                    crate::render::disclosure::glyph(open)
                ))
            }));
            assert!(text.lines().next().is_some_and(|row| row.ends_with("  82")));
        }
        assert_eq!(layout.position_at(/*row*/ 0, /*column*/ 0), 0);
        if !expanded {
            assert_eq!(layout.row_count(), 1);
        }
        snapshots.push(format!(
            "width={width}, expanded={expanded}, tokens={tokens:?}\n{buffer:?}"
        ));
    }
    // Every selectable character lands back on the same source offset after the fixed gutter.
    let layout = activity_layout(
        ActivityTranscriptLines {
            output_mode: None,
            is_tool_entry: true,
            header: Some(crate::history_cell::activity_parts::ActivityHeader::at_end(
                &Line::from(vec!["• ".into(), "Ran".into()]),
            )),
            controls: Vec::new(),

            activity: vec![
                Line::from(vec!["• ".into(), "Ran".bold(), " echo 界 👩‍💻".into()]).into(),
            ],
            auxiliary: Vec::new(),
            header_tokens: Some(1_337),
            disclosure: None,
        },
        crate::transcript_geometry::GUTTER_COLUMNS,
        /*width*/ 80,
        /*expanded*/ false,
    );
    for (offset, _) in layout.text().grapheme_indices(/*is_extended*/ true) {
        let column = layout.column_for_offset(offset);
        assert_eq!(layout.position_at(/*row*/ 0, column), offset);
    }
    assert!(!layout.disclosure);
    assert_eq!(layout.rewrap(/*width*/ 60).text(), layout.text());
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn layouts_refresh_for_width_animation_and_mutable_frames() {
    for mutable in [false, true] {
        let cell = Arc::new(Cell {
            mutable,
            ..Cell::default()
        });
        let history = cell.clone() as Arc<dyn HistoryCell>;
        let mut cache = LayoutCache::default();
        let get = |cache: &mut LayoutCache, width| {
            cache.get(
                &history,
                width,
                CellPresentation {
                    separated: false,
                    turn_tip_space: false,
                    expanded: false,
                    disclosure: false,
                    continued_edit: false,
                    feed: 0,
                    detailed: false,
                },
                || {
                    let history = Arc::clone(&history);
                    Box::new(move || {
                        TextLayout::new(history.transcript_hyperlink_lines(width), width)
                    })
                },
            )
        };
        cache.begin_frame();
        get(&mut cache, /*width*/ 20);
        get(&mut cache, /*width*/ 20);
        assert_eq!(cell.renders.load(Ordering::Relaxed), 1);
        cache.begin_frame();
        get(&mut cache, /*width*/ 20);
        assert_eq!(
            cell.renders.load(Ordering::Relaxed),
            1 + usize::from(mutable)
        );
        cell.tick.store(/*val*/ 2, Ordering::Relaxed);
        cache.begin_frame();
        let next = get(&mut cache, /*width*/ 20);
        assert_eq!(next.unwrap().text(), "tick 2");
        get(&mut cache, /*width*/ 10);
        assert_eq!(
            cell.renders.load(Ordering::Relaxed),
            3 + usize::from(mutable)
        );
        crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors {
                fg: (12, 34, 56),
                bg: (65, 43, 21),
            },
            || {
                cache.begin_frame();
                get(&mut cache, /*width*/ 10);
                assert_eq!(
                    cell.renders.load(Ordering::Relaxed),
                    4 + usize::from(mutable)
                );
            },
        );
    }
    let mut history: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(Cell::default())];
    let mut view = TranscriptView::default();
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 20, /*height*/ 2,
    );
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer, &history);
    assert!(view.has_visible_animation(&history));
    view.begin_selection(
        &history, /*column*/ 1, /*row*/ 0, /*clicks*/ 3,
    );
    assert!(!view.has_visible_animation(&history));
    view.end_selection(&history);
    view.jump_to_latest();
    history.push(Arc::new(crate::history_cell::PlainHistoryCell::new(
        vec!["plain".into(); 4],
    )));
    view.render(area, &mut buffer, &history);
    assert!(!view.has_visible_animation(&history));
}

#[path = "layout_preparation_tests.rs"]
mod preparation;
