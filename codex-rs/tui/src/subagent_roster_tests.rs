use super::*;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;

const AREA: Rect = Rect::new(4, 7, 48, 4);

fn paint(roster: &SharedRoster) -> String {
    let mut buffer = Buffer::empty(AREA);
    RosterView(roster.clone()).render(AREA, &mut buffer);
    buffer
        .content
        .chunks(usize::from(AREA.width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mouse(roster: &SharedRoster, kind: MouseEventKind, column: u16, row: u16) -> MouseAction {
    roster.lock().unwrap().mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn roster_pointer_targets_and_scroll_survive_background_updates() {
    let roster = SharedRoster::default();
    let agents: Vec<_> = (0..10)
        .map(|n| (ThreadId::new(), format!("worker-{n}")))
        .collect();
    roster
        .lock()
        .unwrap()
        .reconcile(agents.clone(), Some(agents[0].0));
    let initial = paint(&roster);
    mouse(&roster, MouseEventKind::Moved, 8, 7);
    mouse(&roster, MouseEventKind::Down(MouseButton::Left), 8, 7);
    // A newly active worker must not replace the pressed target before release.
    {
        let mut state = roster.lock().unwrap();
        state.rows[9].status = Some(ThreadStatus::Active {
            active_flags: Vec::new(),
        });
        state.sort_by_activity();
        state.reconcile(agents.clone(), Some(agents[0].0));
    }
    paint(&roster);
    assert!(
        matches!(mouse(&roster, MouseEventKind::Up(MouseButton::Left), 8, 7), MouseAction::Open(id) if id == agents[0].0)
    );
    mouse(&roster, MouseEventKind::Moved, 0, 0);
    paint(&roster);
    assert_eq!(roster.lock().unwrap().rows[0].id, agents[9].0);

    for _ in 0..4 {
        mouse(&roster, MouseEventKind::ScrollDown, 8, 8);
        paint(&roster);
    }
    let scrolled = paint(&roster);
    roster
        .lock()
        .unwrap()
        .reconcile(agents.clone(), Some(agents[0].0));
    assert_eq!(paint(&roster), scrolled); // Watched row does not snap the viewport back.
    insta::assert_snapshot!(
        "roster_scrollbar",
        format!("Initial:\n{initial}\nScrolled:\n{scrolled}")
    );

    // Grab the actual thumb and drag to the bottom; release is never a row click.
    let (top, _) = roster.lock().unwrap().scrollbar().unwrap();
    mouse(
        &roster,
        MouseEventKind::Down(MouseButton::Left),
        AREA.right() - 1,
        AREA.y + top as u16,
    );
    mouse(
        &roster,
        MouseEventKind::Drag(MouseButton::Left),
        AREA.right() - 1,
        AREA.bottom() + 5,
    );
    paint(&roster);
    assert!(!matches!(
        mouse(
            &roster,
            MouseEventKind::Up(MouseButton::Left),
            AREA.right() - 1,
            AREA.bottom() + 5
        ),
        MouseAction::Open(_)
    ));
    assert_eq!(roster.lock().unwrap().first, 6);

    paint(&roster);
    mouse(&roster, MouseEventKind::Down(MouseButton::Left), 8, 7);
    let removed = {
        let mut state = roster.lock().unwrap();
        let first = state.first;
        let removed = state.rows[first].id;
        state.rows[first].status = Some(ThreadStatus::NotLoaded);
        state.reconcile(
            agents
                .iter()
                .filter(|(id, _)| *id != removed)
                .cloned()
                .collect(),
            Some(agents[0].0),
        );
        removed
    };
    paint(&roster);
    assert!(!matches!(
        mouse(&roster, MouseEventKind::Up(MouseButton::Left), 8, 7),
        MouseAction::Open(_)
    ));
    mouse(&roster, MouseEventKind::Moved, 0, 0);
    assert!(
        !roster
            .lock()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.id == removed)
    );

    // A hidden or resized roster cannot inherit a press on its former geometry.
    paint(&roster);
    mouse(&roster, MouseEventKind::Down(MouseButton::Left), 8, 7);
    let mut state = roster.lock().unwrap();
    state.begin_frame();
    assert!(!state.finish_frame());
    drop(state);
    assert!(!matches!(
        mouse(&roster, MouseEventKind::Up(MouseButton::Left), 8, 7),
        MouseAction::Open(_)
    ));
}
