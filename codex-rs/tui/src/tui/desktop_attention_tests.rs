use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn i3_attention_distinguishes_visible_unfocused_tabs_and_floating_focus() {
    let tree = json!({
        "type": "root",
        "nodes": [
            {"type": "workspace", "id": 10, "nodes": [
                {"window": 101, "focused": false, "nodes": [], "floating_nodes": []}
            ], "floating_nodes": []},
            {"type": "workspace", "id": 20, "layout": "tabbed", "focus": [4, 2, 3],
                "nodes": [
                    {"id": 2, "window": 102, "focused": false, "nodes": [], "floating_nodes": []},
                    {"id": 3, "window": 103, "focused": false, "nodes": [], "floating_nodes": []}
                ], "floating_nodes": [{"id": 4, "window": 104, "focused": true, "nodes": [], "floating_nodes": []}]},
            {"type": "workspace", "id": 30, "name": "__i3_scratch", "nodes": [
                {"window": 105, "focused": false, "nodes": [], "floating_nodes": []}
            ], "floating_nodes": []}
        ], "floating_nodes": []
    });
    // get_tree workspace nodes have no `visible` field; only get_workspaces does.
    let workspaces = HashMap::from([(10, false), (20, true)]);
    let observed = [101, 102, 103, 104, 105, 106]
        .map(|window_id| window_attention(&tree, window_id, Some(true), &workspaces));
    assert_eq!(
        observed,
        [
            Some(WindowAttention {
                visible: Some(false),
                focused: Some(false)
            }),
            Some(WindowAttention {
                visible: Some(true),
                focused: Some(false)
            }),
            Some(WindowAttention {
                visible: Some(false),
                focused: Some(false)
            }),
            Some(WindowAttention {
                visible: Some(true),
                focused: Some(true)
            }),
            Some(WindowAttention {
                visible: Some(false),
                focused: Some(false)
            }),
            None,
        ]
    );
    assert_eq!(
        window_attention(&tree, /*window_id*/ 102, Some(true), &HashMap::new()),
        Some(WindowAttention {
            visible: None,
            focused: Some(false)
        })
    );
}

#[tokio::test]
async fn desktop_attention_recovers_on_input_and_fails_open() {
    let mut tui = crate::tui::test_support::make_test_tui().expect("test tui");
    tui.presentation_over_ssh = false;
    tui.set_desktop_attention(DesktopAttention {
        idle: true,
        ..Default::default()
    });
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Frozen
    );
    tui.note_terminal_input();
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Normal
    );

    tui.set_desktop_attention(DesktopAttention {
        visible: Some(false),
        ..Default::default()
    });
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Frozen
    );
    tui.note_terminal_input();
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Normal
    );

    // Alacritty need not send an initial focus event: the first i3 snapshot is enough.
    tui.set_desktop_attention(DesktopAttention {
        visible: Some(true),
        focused: Some(false),
        ..Default::default()
    });
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Reduced
    );
    tui.note_terminal_input();
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Normal
    );

    tui.set_desktop_attention(DesktopAttention {
        visible: Some(true),
        focused: Some(false),
        ..Default::default()
    });
    tui.set_desktop_attention(DesktopAttention {
        visible: Some(true),
        ..Default::default()
    });
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Normal
    );
    tui.presentation_over_ssh = true;
    tui.reconcile_presentation();
    assert_eq!(
        tui.presentation_mode(),
        super::super::PresentationMode::Reduced
    );
}
