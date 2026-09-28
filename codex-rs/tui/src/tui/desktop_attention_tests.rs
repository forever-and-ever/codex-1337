use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn i3_visibility_distinguishes_hidden_workspaces_and_tabs_from_unknown_windows() {
    let tree = json!({
        "type": "root",
        "nodes": [
            {"type": "workspace", "id": 10, "nodes": [
                {"window": 101, "nodes": [], "floating_nodes": []}
            ], "floating_nodes": []},
            {"type": "workspace", "id": 20, "layout": "tabbed", "focus": [4, 2, 3],
                "nodes": [
                    {"id": 2, "window": 102, "nodes": [], "floating_nodes": []},
                    {"id": 3, "window": 103, "nodes": [], "floating_nodes": []}
                ], "floating_nodes": [{"id": 4, "window": 104, "nodes": [], "floating_nodes": []}]},
            {"type": "workspace", "id": 30, "name": "__i3_scratch", "nodes": [
                {"window": 105, "nodes": [], "floating_nodes": []}
            ], "floating_nodes": []}
        ], "floating_nodes": []
    });
    // get_tree workspace nodes have no `visible` field; only get_workspaces does.
    let workspaces = HashMap::from([(10, false), (20, true)]);
    assert_eq!(
        window_visibility(&tree, 101, Some(true), &workspaces),
        Some(false)
    );
    assert_eq!(
        window_visibility(&tree, 102, Some(true), &workspaces),
        Some(true)
    );
    assert_eq!(
        window_visibility(&tree, 103, Some(true), &workspaces),
        Some(false)
    );
    assert_eq!(
        window_visibility(&tree, 104, Some(true), &workspaces),
        Some(true)
    );
    assert_eq!(
        window_visibility(&tree, 105, Some(true), &workspaces),
        Some(false)
    );
    assert_eq!(window_visibility(&tree, 106, Some(true), &workspaces), None);
    assert_eq!(
        window_visibility(&tree, 102, Some(true), &HashMap::new()),
        None
    );
}

#[tokio::test]
async fn idle_and_hidden_visibility_admit_paint_only_after_human_return() {
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.presentation_over_ssh = false;
    tui.set_desktop_attention(/*idle*/ true, /*visible*/ None);
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Frozen
    );
    tui.note_terminal_input();
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Normal
    );

    tui.set_desktop_attention(/*idle*/ false, /*visible*/ Some(false));
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Frozen
    );
    tui.note_terminal_input();
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Normal
    );
}
