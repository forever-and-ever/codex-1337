//! Sticky context follows the visible turn while leaving source text and hit rows intact.

use super::*;
use crate::history_cell::new_spoken_user_prompt;
use crate::history_cell::new_user_prompt;
use crate::transcript_geometry::GUTTER_COLUMNS;
use crate::transcript_view::tests::cell;
use crate::transcript_view::tests::render;
use crate::transcript_view::tests::text;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;
use ratatui::style::Modifier;

fn user(message: &str) -> Arc<dyn HistoryCell> {
    Arc::new(new_user_prompt(
        message.into(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

#[test]
fn prompt_header_tracks_reading_and_yields_to_visible_prompts_and_short_screens() {
    let mut cells = vec![
        user("First question"),
        cell("one\ntwo\nthree\nfour\nfive"),
        user("Second café\nquestion"),
        user("\u{0007}"),
        cell("six\nseven\neight\nnine\nten"),
    ];
    let mut view = TranscriptView::default();
    let mut snapshots = Vec::new();
    snapshots.push(format!(
        "Latest\n{}",
        text(&render(
            &mut view, &cells, /*width*/ 30, /*height*/ 5
        ))
    ));
    assert_eq!(view.area.y, 1);
    view.jump_to_entry(&cells, /*index*/ 1);
    snapshots.push(format!(
        "Earlier answer\n{}",
        text(&render(
            &mut view, &cells, /*width*/ 30, /*height*/ 5
        ))
    ));
    assert_eq!(view.visible[0].index, 1);
    cells.insert(/*index*/ 0, user("Prepend history"));
    view.history_loaded(&cells, 0..1);
    snapshots.push(format!(
        "After prepend\n{}",
        text(&render(
            &mut view, &cells, /*width*/ 30, /*height*/ 5
        ))
    ));
    assert!(matches!(
        view.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            &cells,
        ),
        Some(ViewAction::Changed)
    ));
    snapshots.push(format!(
        "Prompt visible\n{}",
        text(&render(
            &mut view, &cells, /*width*/ 30, /*height*/ 5
        ))
    ));
    assert_eq!(view.area.y, 0);
    view.jump_to_latest();
    snapshots.push(format!(
        "Short screen\n{}",
        text(&render(
            &mut view, &cells, /*width*/ 30, /*height*/ 3
        ))
    ));
    assert_eq!(view.area.y, 0);
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn header_sanitizes_controls_and_truncates_by_display_width() {
    let cells = vec![user("\x1b[31m界 café\r\nnext question"), cell("answer")];
    let (line, _) = line(
        &cells, /*first*/ 1, /*width*/ 18, /*detailed*/ false,
    )
    .unwrap();
    assert!(line.width() <= 18);
    insta::assert_snapshot!(line.to_string());
}

#[test]
fn pinned_prompt_matches_the_original_prompt_style() {
    for prompt in [
        new_user_prompt("keep going".into(), Vec::new(), Vec::new(), Vec::new()),
        new_spoken_user_prompt("keep going".into()),
    ] {
        let width = 30;
        let mut expected = Buffer::empty(Rect::new(
            /*x*/ 0, /*y*/ 0, width, /*height*/ 3,
        ));
        TextLayout::new(
            crate::transcript_geometry::gutter_lines(
                prompt.retained_hyperlink_lines(
                    width - crate::transcript_geometry::TITLE_COLUMN,
                    false,
                ),
                crate::transcript_geometry::TITLE_COLUMN,
            ),
            width,
        )
        .render(expected.area, &mut expected, /*start_row*/ 0);
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(prompt),
            cell("one\ntwo\nthree\nfour\nfive\nsix\nseven\neight"),
        ];
        let mut view = TranscriptView::default();
        let actual = render(&mut view, &cells, width, /*height*/ 7);

        // The pinned prompt wears the prompt's paint; its bracket opens, the rest folded above.
        let glyph = usize::from(crate::transcript_geometry::CONTROL_COLUMN);
        let row = |buffer: &Buffer| {
            let mut cells = buffer.content[..usize::from(width)].to_vec();
            cells.remove(glyph);
            cells
        };
        assert_eq!(row(&actual), row(&expected));
        assert_eq!(
            (actual.content[glyph].symbol(), actual.content[glyph].fg),
            (
                crate::render::bracket::Strokes::of(crate::warpaint::Bracket::Prompt).top,
                crate::warpaint::Bracket::Prompt.color()
            )
        );
        assert_eq!(view.area.y, 1);
    }
}

#[test]
fn mouse_selection_preserves_header_reservation_at_turn_boundaries() {
    let cells = vec![
        user("First question"),
        cell("first answer"),
        user("Second question"),
        cell("done 10:16 AM"),
    ];
    for width in [GUTTER_COLUMNS + 40, GUTTER_COLUMNS + 11] {
        for height in [6, 7, 8] {
            for drag in [false, true] {
                let mut view = TranscriptView::default();
                let before = render(&mut view, &cells, width, height);
                let down = MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: GUTTER_COLUMNS + 5,
                    row: height - 1 - u16::from(width < GUTTER_COLUMNS + 12),
                    modifiers: KeyModifiers::NONE,
                };
                view.handle_mouse(down, &cells);
                assert_eq!(
                    render(&mut view, &cells, width, height),
                    before,
                    "mouse down at {width}x{height}"
                );
                if !drag {
                    view.handle_mouse(
                        MouseEvent {
                            kind: MouseEventKind::Up(MouseButton::Left),
                            ..down
                        },
                        &cells,
                    );
                    assert_eq!(
                        (
                            render(&mut view, &cells, width, height),
                            view.selected_text(&cells),
                            view.is_following(),
                        ),
                        (before, None, true),
                        "empty release at {width}x{height}"
                    );
                    continue;
                }
                let end = MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    column: if width < GUTTER_COLUMNS + 12 {
                        GUTTER_COLUMNS + 2
                    } else {
                        GUTTER_COLUMNS + 13
                    },
                    row: height - 1,
                    ..down
                };
                view.handle_mouse(end, &cells);
                let selected = render(&mut view, &cells, width, height);
                assert_eq!(text(&selected), text(&before));
                assert_eq!(view.selected_text(&cells).as_deref(), Some("10:16 AM"));
                view.handle_mouse(
                    MouseEvent {
                        kind: MouseEventKind::Up(MouseButton::Left),
                        ..end
                    },
                    &cells,
                );
                assert_eq!(render(&mut view, &cells, width, height), selected);
                let Some(ViewAction::Copy(copied)) = view.handle_key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    &cells,
                ) else {
                    panic!("copy selection at {width}x{height}");
                };
                assert_eq!(copied, "10:16 AM");
                assert_eq!(
                    selected
                        .content
                        .iter()
                        .filter(|cell| cell.modifier.contains(Modifier::REVERSED))
                        .map(ratatui::buffer::Cell::symbol)
                        .collect::<String>(),
                    if width < GUTTER_COLUMNS + 12 {
                        "10:16AM"
                    } else {
                        &copied
                    }
                );
            }
        }
    }
}

#[test]
fn paused_header_reservation_survives_copy_until_scroll_or_resize() {
    let cells = vec![
        user("First question"),
        cell("first answer"),
        user("Second question"),
        cell("done 10:16 AM"),
    ];
    for scroll in [false, true] {
        let mut view = TranscriptView::default();
        let before = render(&mut view, &cells, /*width*/ 40, /*height*/ 8);
        view.begin_selection(&cells, /*column*/ 0, /*row*/ 7, /*clicks*/ 2);
        assert_eq!(
            text(&render(
                &mut view, &cells, /*width*/ 40, /*height*/ 8
            )),
            text(&before)
        );
        let selected = view.selected_text(&cells).expect("selected word");
        assert_eq!(selected, "done");
        view.copy_selected_text_with(
            &cells,
            &selected,
            /*clear_selection*/ true,
            |_, _format| Ok(crate::clipboard_copy::CopyStatus::Confirmed),
        )
        .expect("confirmed copy");
        let bookmark = view.bookmark(&cells);
        view.set_presentation(/*detailed*/ true, HistoryRenderMode::Rich);
        view.jump_to_entry(&cells, /*index*/ 0);
        render(&mut view, &cells, /*width*/ 40, /*height*/ 8);
        view.restore_bookmark(bookmark);
        assert_eq!(
            (
                render(&mut view, &cells, /*width*/ 40, /*height*/ 8),
                view.is_following(),
            ),
            (before, false)
        );
        let height = if scroll {
            view.scroll(&cells, /*rows*/ -1);
            8
        } else {
            9
        };
        render(&mut view, &cells, /*width*/ 40, height);
        // Compact message bands leave the prompt itself visible; do not pin a duplicate.
        assert_eq!(view.area.y, 0);
    }
}
