use super::*;
use crate::history_cell::activity_parts::FoldState;
use base64::Engine;
use codex_protocol::mcp::CallToolResult;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

#[test]
fn mcp_file_receipts_share_native_hunks_and_independent_disclosures() {
    let snapshot =
        |uri: &str, hash: char| json!({"uri":uri,"sha256":hash.to_string().repeat(64),"bytes":4});
    let text = json!({"kind":"text","publication":"applied","operation":"modify",
        "before":snapshot("file:///workspace/a.rs", '0'),"after":snapshot("file:///workspace/a.rs", '1'),
        "unifiedDiff":"--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n"});
    let mut rename = text.clone();
    rename["operation"] = json!("rename");
    rename["before"] = snapshot("file://remote/workspace/old%20name.rs", '0');
    rename["after"] = snapshot("file://remote/workspace/new%20name.rs", '1');
    let mut preview = text.clone();
    preview["publication"] = json!("preview");
    let summary = json!({"kind":"summary","publication":"applied","operation":"modify","before":snapshot("file:///workspace/blob.bin", '0'),"after":snapshot("file:///workspace/blob.bin", '1'),"reason":"binary"});
    let refused = json!({"kind":"pathOnly","publication":"notApplied","operation":"create","beforeUri":null,"afterUri":"file:///workspace/refused.rs"});
    let mut hostile_label = text.clone();
    for endpoint in ["before", "after"] {
        hostile_label[endpoint]["uri"] = json!("file:///workspace/a%1B%0A%E2%80%AE.rs");
    }
    let mut snapshots = Vec::new();
    for (label, outcome, files, omitted) in [
        ("text and rename", "applied", vec![text.clone(), rename], 0),
        ("partial failure", "partial", vec![text, refused], 0),
        ("metadata only", "applied", vec![hostile_label], 0),
        ("preview", "preview", vec![preview], 0),
        ("summary and omissions", "applied", vec![summary], 2),
        ("unknown", "unknown", Vec::new(), 0),
    ] {
        let result = CallToolResult {
            content: if label == "metadata only" {
                Vec::new()
            } else {
                vec![
                    json!({"type":"text","text":if label == "partial failure" { "Publication partially failed; do not retry." } else { "ordinary result remains accessible" }}),
                ]
            },
            structured_content: None,
            is_error: (label == "partial failure").then_some(true),
            meta: Some(
                json!({"codex/fileChanges":{"version":1,"outcome":outcome,"files":files,"omittedFiles":omitted}}),
            ),
        };
        // Historical replay takes exactly the same adapter path as a live completion.
        let replayed: CallToolResult =
            serde_json::from_str(&serde_json::to_string(&result).unwrap()).unwrap();
        let mut cell = McpToolCallCell::new(
            "edit".into(),
            McpInvocation {
                server: "semedit".into(),
                environment_id: None,
                tool: "edit".into(),
                arguments: None,
            },
            /*animations_enabled*/ false,
        );
        cell.complete(Duration::ZERO, Ok(replayed));
        let census = crate::history_cell::invocation::edit_parts(
            std::iter::once(&cell as &dyn HistoryCell),
            100,
            &FoldState::new(),
            false,
        );
        assert_eq!(
            Text::from(visible_lines(census.lines))
                .to_string()
                .matches("Edited")
                .count(),
            usize::from(outcome == "applied"),
            "{label}",
        );
        let closed = cell
            .activity_parts(
                /*width*/ 100,
                /*header_reserve*/ 0,
                &Default::default(),
            )
            .unwrap();
        let opened = cell
            .activity_parts(
                /*width*/ 100,
                /*header_reserve*/ 0,
                &FoldState::from(["mcp-file:edit:0".into()]),
            )
            .unwrap();
        snapshots.push(format!(
            "{label}\nCompact\n{}\nFirst file expanded\n{}\nTranscript\n{}",
            Text::from(visible_lines(closed.lines)),
            Text::from(visible_lines(opened.lines)),
            Text::from(cell.transcript_lines(/*width*/ 100))
        ));
        if label == "text and rename" {
            let input = cell.invocation_input(100);
            let edits = cell.receipt_parts(100, &FoldState::new(), false, true);
            assert_eq!(
                input.parts.lines[0].line.to_string().find("MCP"),
                edits.lines[0].line.to_string().find("Edited"),
            );
            assert_eq!(edits.controls.len(), 2);
            assert_eq!(
                opened
                    .controls
                    .iter()
                    .map(|control| (control.id.as_str(), control.expanded))
                    .collect::<Vec<_>>(),
                vec![("mcp-file:edit:0", true), ("mcp-file:edit:1", false)]
            );
            assert_eq!(closed.controls.len(), 2);
            let mut direct = cell.clone();
            direct.invocation.arguments = Some(json!({"target":"long-direct-argument".repeat(12)}));
            let output = ToolOutputCell::from_output(
                "direct-output",
                Some("edit"),
                "edit",
                Some("semedit"),
                &codex_protocol::models::FunctionCallOutputBody::Text("chosen model text".into()),
            )
            .unwrap();
            let group = ToolOutputGroup::from_cells([
                Arc::new(direct.clone()) as Arc<dyn HistoryCell>,
                Arc::new(output),
            ]);
            let render = |expanded: FoldState| {
                group
                    .activity_parts(/*width*/ 60, /*header_reserve*/ 14, &expanded)
                    .unwrap()
            };
            let collapsed = render(FoldState::new());
            let expected = direct
                .activity_parts(
                    /*width*/ 60,
                    /*header_reserve*/ 14,
                    &FoldState::new(),
                )
                .unwrap();
            assert_eq!(collapsed.lines, expected.lines);
            assert!(collapsed.lines[0].width() <= 46);
            let expanded = render(FoldState::from([
                "mcp:edit".into(),
                "output:tool-output-tokens:direct-output".into(),
                "mcp-file:edit:0".into(),
                "result:direct-output".into(),
            ]));
            let expanded_text = Text::from(visible_lines(expanded.lines));
            assert!(expanded_text.to_string().contains("chosen model text"));
            assert!(
                !expanded_text
                    .to_string()
                    .contains("ordinary result remains accessible")
            );
            assert_eq!(
                expanded
                    .controls
                    .iter()
                    .map(|control| (control.id.as_str(), control.expanded))
                    .collect::<Vec<_>>(),
                vec![
                    ("mcp-file:edit:0", true),
                    ("mcp-file:edit:1", false),
                    ("result:direct-output", true),
                ]
            );
            snapshots.push(format!(
                "Direct captured result\nCollapsed\n{}\nExpanded with first diff\n{}",
                Text::from(visible_lines(collapsed.lines)),
                expanded_text
            ));
        } else if label == "metadata only" {
            assert!(closed.output.is_none());
            assert_eq!(closed.controls.len(), 1);
            assert!(
                cell.transcript_lines(/*width*/ 100)
                    .iter()
                    .all(|line| !line.to_string().contains(['\u{1b}', '\u{202e}']))
            );
        }
    }
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn mcp_inventory_connection_states() {
    use McpServerConnectionStatus as Status;

    let mut statuses = [
        ("unknown", None),
        ("starting", Some(Status::Starting)),
        ("failed", Some(Status::Failed)),
        ("disabled", Some(Status::Disabled)),
        ("deferred", Some(Status::NotStarted)),
        ("connected-empty", Some(Status::Connected)),
        ("cancelled", Some(Status::Cancelled)),
        ("auth", Some(Status::AuthenticationRequired)),
    ]
    .into_iter()
    .map(|(name, runtime_status)| McpServerStatus {
        environment_id: None,
        server_capabilities: None,
        tool_list_meta: None,
        name: name.to_string(),
        runtime_status,
        plugin_id: None,
        http_origin: None,
        server_info: None,
        tools: HashMap::new(),
        tools_error: None,
        resources: Vec::new(),
        resource_templates: Vec::new(),
        auth_status: McpAuthStatus::Unknown,
    })
    .collect::<Vec<_>>();
    for environment in ["env-a", "env-b"] {
        let mut status = statuses[0].clone();
        status.name = "notes".to_string();
        status.environment_id = Some(environment.to_string());
        statuses.push(status);
    }
    let cell =
        new_mcp_tools_output_from_statuses(&statuses, McpServerStatusDetail::ToolsAndAuthOnly);
    let rendered = cell
        .display_lines(/*width*/ 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(rendered);
    for (name, expected) in [
        ("auth", status_style(StatusTone::Attention)),
        ("cancelled", Style::default().dim()),
        ("connected-empty", status_style(StatusTone::Success)),
        ("deferred", Style::default().dim()),
        ("disabled", Style::default().dim()),
        ("failed", status_style(StatusTone::Failure)),
        ("starting", accent_style()),
        ("unknown", Style::default().dim()),
    ] {
        let row = cell
            .lines
            .iter()
            .find(|line| line.spans.get(1).is_some_and(|span| span.content == name))
            .expect("connection-state row");
        assert_eq!(
            (row.spans[0].style, row.spans[3].style),
            (expected, expected)
        );
    }
}

#[test]
fn mcp_inventory_older_app_server_authentication() {
    let statuses = serde_json::from_value::<Vec<McpServerStatus>>(json!([
        {
            "name": "legacy-auth",
            "tools": {},
            "resources": [],
            "resourceTemplates": [],
            "authStatus": "notLoggedIn"
        },
        {
            "name": "legacy-healthy",
            "tools": {"lookup": {"name": "lookup", "inputSchema": {"type": "object"}}},
            "resources": [],
            "resourceTemplates": [],
            "authStatus": "oAuth"
        },
        {
            "name": "runtime-disabled",
            "runtimeStatus": "disabled",
            "tools": {},
            "resources": [],
            "resourceTemplates": [],
            "authStatus": "notLoggedIn"
        }
    ]))
    .expect("mixed-version MCP statuses");
    let cell =
        new_mcp_tools_output_from_statuses(&statuses, McpServerStatusDetail::ToolsAndAuthOnly);
    let rendered = cell
        .display_lines(/*width*/ 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(rendered);
    let styles = cell
        .lines
        .iter()
        .filter(|line| {
            line.spans
                .get(1)
                .is_some_and(|span| statuses.iter().any(|status| span.content == status.name))
        })
        .map(|line| (line.spans[0].style, line.spans[3].style))
        .collect::<Vec<_>>();
    let attention = status_style(StatusTone::Attention);
    let neutral = Style::default().dim();
    assert_eq!(
        styles,
        vec![
            (attention, attention),
            (neutral, neutral),
            (neutral, neutral)
        ]
    );
}

fn result(content: Vec<Value>) -> CallToolResult {
    CallToolResult {
        content,
        structured_content: None,
        is_error: None,
        meta: None,
    }
}

#[test]
fn mcp_preview_shares_one_limit_across_blocks_and_preserves_transcript() {
    let mut cell = new_active_mcp_tool_call(
        "call-preview".into(),
        McpInvocation {
            environment_id: None,
            server: "search".into(),
            tool: "lookup".into(),
            arguments: Some(json!({"title": "Search query"})),
        },
        /*animations_enabled*/ false,
    );
    cell.complete(
        Duration::ZERO,
        Ok(result(vec![
            json!({"type": "text", "text": "first\nsecond"}),
            json!({"type": "text", "text": "third\nfourth\nfifth"}),
        ])),
    );
    let display = cell.display_lines(/*width*/ 80);
    let transcript = cell.transcript_lines(/*width*/ 80);
    let argument_previews = [
        json!({}),
        json!({
            "file_path": "foo",
            "query": "hover(\"bar\"): key=value, \\path\nnext",
            "options": {"items": [1, true, null, {"key": "value"}]},
        }),
        json!({"odd key=\"name\"": "quoted"}),
        json!("{\"query\":\"not an object\"}"),
    ]
    .into_iter()
    .flat_map(|arguments| {
        let call = new_active_mcp_tool_call(
            "call-arguments".into(),
            McpInvocation {
                server: "lsp".into(),
                environment_id: None,
                tool: "hover".into(),
                arguments: Some(arguments),
            },
            /*animations_enabled*/ false,
        );
        [200, 32].map(|width| {
            visible_lines(call.compact_hyperlink_lines(width))
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })
    })
    .collect::<Vec<_>>()
    .join("\n");
    insta::assert_snapshot!(format!(
        "compact:\n{}\n\nargument previews:\n{argument_previews}\n\nhistory:\n{}\n\ntranscript:\n{}",
        visible_lines(cell.compact_hyperlink_lines(/*width*/ 80))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        display
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        transcript
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    assert_eq!(
        cell.raw_lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        [
            "Called search.lookup({\"title\":\"Search query\"})",
            "first",
            "second",
            "third",
            "fourth",
            "fifth"
        ],
    );
}

#[test]
fn code_mode_output_shares_a_row_budget_across_blocks() {
    let mut cell = new_active_mcp_tool_call(
        "browser-call".to_string(),
        McpInvocation {
            environment_id: None,
            server: "node_repl".to_string(),
            tool: "js".to_string(),
            arguments: Some(json!({"title": "Inspect page", "code": "await tab.snapshot()"})),
        },
        /*animations_enabled*/ false,
    );
    cell.complete(
        Duration::ZERO,
        Ok(result(vec![
            json!({"type": "text", "text": "Script completed\nOutput:\n"}),
            json!({"type": "text", "text": "Page title\nNavigation\nMain content"}),
            json!({"type": "text", "text": "Button\nLink\nFooter"}),
        ])),
    );

    let display = cell
        .display_lines(/*width*/ 40)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(display, @"
    • Called MCP Inspect page
      └ Page title
        Navigation
        Main content
        +3 lines (⌃t to view transcript)
    ");
    let transcript = cell
        .transcript_lines(/*width*/ 100)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(transcript.contains("await tab.snapshot()"));
    assert!(transcript.ends_with("    Button\n    Link\n    Footer"));
}

#[test]
fn code_mode_output_preserves_trailing_failure_diagnostics_in_transcript() {
    let mut cell = new_active_mcp_tool_call(
        "browser-error".to_string(),
        McpInvocation {
            environment_id: None,
            server: "node_repl".to_string(),
            tool: "js".to_string(),
            arguments: Some(json!({"title": "Inspect page"})),
        },
        /*animations_enabled*/ false,
    );
    cell.complete(
        Duration::ZERO,
        Ok(CallToolResult {
            is_error: Some(true),
            ..result(vec![
                json!({"type": "text", "text": "Script failed"}),
                json!({"type": "text", "text": "Page title\nNavigation\nMain content\nButton\nLink\nFooter"}),
                json!({"type": "text", "text": "Script error:\npermission denied"}),
            ])
        }),
    );
    let display = cell
        .display_lines(/*width*/ 40)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(display, @"
    • Failed MCP Inspect page
      └ Script failed
        Page title
        Navigation
        +6 lines (⌃t to view transcript)
    ");
    let transcript = cell
        .transcript_lines(/*width*/ 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(transcript.ends_with("    Script error:\n    permission denied"));
}

#[test]
fn code_mode_output_row_budget_applies_after_wrapping_and_to_errors() {
    let output = format!("{}\ntranscript tail", "Browser 页面 👩‍💻\n".repeat(40));
    for server in ["node_repl", "cua_repl"] {
        for completion in [
            Ok(result(vec![json!({"type": "text", "text": output})])),
            Ok(result(vec![json!({
                "type": "text",
                "text": format!("https://example.com/{}\ntranscript tail", "页面".repeat(100)),
            })])),
            Ok(CallToolResult {
                is_error: Some(true),
                ..result(vec![json!({"type": "text", "text": output})])
            }),
            Err(output.clone()),
        ] {
            let mut cell = new_active_mcp_tool_call(
                "browser-call".to_string(),
                McpInvocation {
                    environment_id: None,
                    server: server.to_string(),
                    tool: "js".to_string(),
                    arguments: Some(json!({"title": "Inspect"})),
                },
                /*animations_enabled*/ false,
            );
            cell.complete(Duration::ZERO, completion);
            for width in [20, 40, 80] {
                let compact = cell.compact_hyperlink_lines(width);
                // The live activity strip has a single invocation row.
                assert_eq!(compact.len(), 1);
                assert!(compact[0].line.width() <= usize::from(width));
                let display = cell.display_lines(width);
                assert_eq!(display.len(), 5); // Header, three output rows, and omission hint.
                assert!(
                    display
                        .iter()
                        .all(|line| line.width() <= usize::from(width))
                );
                assert!(display.last().unwrap().to_string().starts_with("    +"));
                let transcript = cell
                    .transcript_lines(width)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    transcript
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .contains("transcript tail")
                );
            }
        }
    }
}

#[test]
fn projected_content_preserves_full_rendering() {
    let text = "{\"result\": [1, 2, 3], \"text\": \"long output 🦀\"}";
    let malformed = json!({"type": "image", "data": PNG});
    let invalid_metadata =
        json!({"type": "text", "text": "not a valid block", "annotations": {"priority": "high"}});
    let unknown = json!({"type": "future_block", "text": "unknown output 🦀"});
    let projected = McpToolResult::new(
        result(vec![
            json!({"type": "text", "text": text}),
            json!({"type": "image", "mimeType": "image/png", "data": PNG}),
            json!({"type": "audio", "mimeType": "audio/wav", "data": "audio data"}),
            json!({"type": "resource", "resource": {"uri": "file:///text.txt", "text": "resource body"}}),
            json!({"type": "resource", "resource": {"uri": "file:///blob.bin", "blob": "binary data"}}),
            json!({"type": "resource_link", "uri": "file:///linked.txt", "name": "linked"}),
            malformed.clone(),
            invalid_metadata.clone(),
            unknown.clone(),
        ]),
        McpResultKind::Standard,
    );

    let format_text = |text: &str| format_json_compact(text).unwrap_or_else(|| text.to_owned());
    assert_eq!(
        projected
            .content
            .iter()
            .map(result::McpContentBlock::render)
            .collect::<Vec<_>>(),
        vec![
            format_text(text),
            "Returned image".to_string(),
            "<audio content>".to_string(),
            "embedded resource: file:///text.txt".to_string(),
            "embedded resource: file:///blob.bin".to_string(),
            "link: file:///linked.txt".to_string(),
            format_text(&malformed.to_string()),
            format_text(&invalid_metadata.to_string()),
            format_text(&unknown.to_string()),
        ],
    );
}

#[test]
fn structured_content_remains_disclosable_in_direct_and_code_mode_results() {
    // LSP servers legitimately return only structuredContent. Display projection must
    // not mistake an empty legacy content array for an empty result.
    let value = json!({"symbol": "Widget", "file": "src/widget.rs", "line": 42});
    let mut snapshots = Vec::new();
    for (label, content) in [
        ("structured only", Vec::new()),
        (
            "with summary",
            vec![json!({"type": "text", "text": "One definition"})],
        ),
        (
            "legacy duplicate",
            vec![json!({"type": "text", "text": value.to_string()})],
        ),
    ] {
        let result = CallToolResult {
            structured_content: Some(value.clone()),
            ..result(content)
        };
        let mut direct = new_active_mcp_tool_call(
            "definition".into(),
            McpInvocation {
                server: "lsp".into(),
                environment_id: None,
                tool: "definition".into(),
                arguments: Some(json!({"query": "Widget"})),
            },
            /*animations_enabled*/ false,
        );
        let body = codex_protocol::models::FunctionCallOutputBody::Text(
            serde_json::to_string(&result).unwrap(),
        );
        direct.complete(Duration::ZERO, Ok(result));
        let batch = ToolOutputCell::from_output(
            "output",
            Some("exec"),
            "exec",
            /*namespace*/ None,
            &body,
        )
        .unwrap();
        let captured = ToolOutputCell::from_output(
            "captured-definition",
            Some("definition"),
            "definition",
            Some("lsp"),
            &body,
        )
        .unwrap();
        let cells: Vec<Arc<dyn HistoryCell>> = vec![
            Arc::new(direct.clone()),
            Arc::new(PlainHistoryCell::new(vec!["Unrelated activity".into()])),
        ];
        let (range, projected) = ToolOutputGroup::attach(&cells, Arc::new(captured)).unwrap();
        assert_eq!(range, 0..2);
        assert!(Arc::ptr_eq(&projected[1], &cells[1]));
        for (path, cell) in [
            ("direct", &direct as &dyn HistoryCell),
            ("code mode", &batch as &dyn HistoryCell),
            ("captured direct", projected[0].as_ref()),
        ] {
            assert!(cell.activity_disclosure(/*width*/ 100).is_some());
            let text = visible_lines(cell.expanded_hyperlink_lines(/*width*/ 100))
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(
                text.matches("src/widget.rs").count(),
                if path != "direct" && label == "legacy duplicate" {
                    2
                } else {
                    1
                }
            );
            snapshots.push(format!("{label}, {path}\n{text}"));
        }
    }
    insta::assert_snapshot!(snapshots.join("\n\n"));
}

#[test]
fn projected_image_marker_still_requires_a_complete_image() {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(PNG)
        .expect("decode PNG fixture");
    let truncated = base64::engine::general_purpose::STANDARD.encode(&bytes[..33]);
    let invalid = json!({"type": "image", "mimeType": "image/png", "data": truncated});
    let valid = json!({"type": "image", "mimeType": "image/png", "data": format!("data:image/png;base64,{PNG}")});

    let projected = McpToolResult::new(result(vec![invalid.clone()]), McpResultKind::Standard);
    assert!(!projected.has_image);
    assert_eq!(projected.content[0].render(), "Returned image");

    let projected = McpToolResult::new(result(vec![invalid, valid]), McpResultKind::Standard);
    assert!(projected.has_image);
}

#[test]
fn code_mode_preserves_text_fields_on_nontext_and_unknown_blocks() {
    let mut cell = new_active_mcp_tool_call(
        "call-code-mode".to_string(),
        McpInvocation {
            environment_id: None,
            server: "node_repl".to_string(),
            tool: "js".to_string(),
            arguments: Some(json!({"title": "Inspect results"})),
        },
        /*animations_enabled*/ false,
    );
    let unknown =
        json!({"type": "future_block", "text": "Script completed\nOutput:\nunknown-side output"});
    let tool_result = result(vec![
        json!({"type": "image", "mimeType": "image/png", "data": PNG, "text": "Script completed\nOutput:\nimage-side output"}),
        unknown,
    ]);
    cell.complete(Duration::ZERO, Ok(tool_result.clone()));

    let narrow = cell.display_lines(/*width*/ 16);
    assert!(narrow.iter().all(|line| line.width() <= 16));
    assert_eq!(
        narrow
            .iter()
            .skip(1)
            .take(2)
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["  └ Returned", "    image"],
    );

    let compact = cell.compact_hyperlink_lines(/*width*/ 80);
    let compact = visible_lines(compact)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let narrow_compact = cell.compact_hyperlink_lines(/*width*/ 16);
    assert!(narrow_compact.iter().all(|line| line.width() <= 16));

    let display = cell
        .display_lines(/*width*/ 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let transcript = cell
        .transcript_lines(/*width*/ 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(format!("compact:\n{compact}\n\nhistory:\n{display}\n\ntranscript:\n{transcript}"), @r#"
    compact:
    • Called node_repl.js(title="Inspect results")

    history:
    • Called MCP Inspect results
      └ Returned image
        image-side output
        unknown-side output

    transcript:
    • Called MCP node_repl.js({"title":"Inspect results"})
      └ Returned image
        Script completed
        Output:
        image-side output
        Script completed
        Output:
        unknown-side output
    "#);
    assert_eq!(
        cell.raw_lines(),
        vec![
            Line::from("Called node_repl.js({\"title\":\"Inspect results\"})"),
            Line::from("Script completed"),
            Line::from("Output:"),
            Line::from("image-side output"),
            Line::from("Script completed"),
            Line::from("Output:"),
            Line::from("unknown-side output"),
        ],
    );

    let mut cua_cell = new_active_mcp_tool_call(
        "call-cua-repl".to_string(),
        McpInvocation {
            environment_id: None,
            server: "cua_repl".to_string(),
            tool: "js".to_string(),
            arguments: None,
        },
        /*animations_enabled*/ false,
    );
    cua_cell.complete(Duration::ZERO, Ok(tool_result));
    let display = cua_cell
        .display_lines(/*width*/ 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let transcript = cua_cell
        .transcript_lines(/*width*/ 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(format!("history:\n{display}\n\ntranscript:\n{transcript}"), @"
    history:
    • Called MCP cua_repl.js
      └ Returned image
        image-side output
        unknown-side output

    transcript:
    • Called MCP cua_repl.js()
      └ Returned image
        Script completed
        Output:
        image-side output
        Script completed
        Output:
        unknown-side output
    ");
}

#[test]
fn titled_image_call_keeps_error_and_full_title_when_narrow() {
    let title = "Inspect a very long screenshot title 🦀";
    let mut cell = new_active_mcp_tool_call(
        "call".into(),
        McpInvocation {
            environment_id: None,
            server: "node_repl".into(),
            tool: "js".into(),
            arguments: Some(json!({"title": title})),
        },
        /*animations_enabled*/ false,
    );
    assert_eq!(
        cell.display_lines(/*width*/ 80)[0].to_string(),
        format!("• Calling MCP {title}")
    );
    cell.complete(
        Duration::ZERO,
        Ok(CallToolResult {
            is_error: Some(true),
            ..result(vec![
                json!({"type": "text", "text": "Screenshot partially failed"}),
                json!({"type": "image", "mimeType": "image/png", "data": PNG}),
            ])
        }),
    );
    let lines = cell.display_lines(/*width*/ 32);
    assert!(lines[0].width() <= 32);
    assert_eq!(lines[0].spans[0].style, "•".red().bold().style);
    insta::assert_snapshot!(
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    // A protocol error must stay a failure in every projection, even when it returns an image.
    assert!(
        cell.compact_mcp_lines(/*width*/ 200)[0]
            .line
            .to_string()
            .starts_with("• Failed")
    );
    assert!(cell.raw_lines()[0].to_string().starts_with("Failed"));
    assert!(
        cell.transcript_lines(/*width*/ 200)[0]
            .to_string()
            .starts_with("• Failed")
    );
    assert!(cell.raw_lines()[0].to_string().contains(title));
    assert!(
        cell.transcript_lines(/*width*/ 200)[0]
            .to_string()
            .contains(title)
    );
    cell.invocation.arguments = Some(json!({"title": "   "}));
}

/// Reconstruct only from explicit provenance, checking every displayed source slice on the way.
fn source_lines(lines: &[HyperlinkLine]) -> Vec<String> {
    let mut sources: Vec<Arc<str>> = Vec::new();
    for line in lines {
        let source = line.source.as_ref().expect("MCP row source");
        let visible = line.line.to_string();
        assert_eq!(
            &visible[source.prefix_bytes..source.prefix_bytes + source.range.len()],
            &source.text[source.range.clone()],
        );
        if !sources
            .last()
            .is_some_and(|previous| Arc::ptr_eq(previous, &source.text))
        {
            sources.push(Arc::clone(&source.text));
        }
    }
    sources
        .into_iter()
        .map(|source| source.to_string())
        .collect()
}

#[test]
fn code_mode_transcript_preserves_long_code_and_indentation_across_wrapping() {
    let code = "const values = await tools.fetch_values({ project: 'selected-project', include_details: true });\n  const total = values.reduce((sum, item) => sum + item.value, 0);\n\ntext({ total, label: 'exact code output' });";
    let mut cell = new_active_mcp_tool_call(
        "call-code".to_owned(),
        McpInvocation {
            environment_id: None,
            server: "node_repl".to_owned(),
            tool: "js".to_owned(),
            arguments: None,
        },
        /*animations_enabled*/ false,
    );
    cell.complete(
        Duration::ZERO,
        Ok(result(vec![json!({"type": "text", "text": code})])),
    );
    let expected = code.split('\n').map(str::to_owned).collect::<Vec<_>>();
    for width in [16, 24, 40, 80] {
        let lines = cell.transcript_hyperlink_lines(width);
        let source = source_lines(&lines);
        assert!(source.ends_with(&expected), "{source:?}");
        assert!(
            lines.len() > expected.len(),
            "long code should wrap at width {width}"
        );
    }
}

#[test]
fn mcp_result_preview_preserves_source_text_and_excludes_tree_gutters() {
    let text = "one long result line with repeated spaces  and enough content to wrap across several narrow rows";
    let mut cell = new_active_mcp_tool_call(
        "call-mcp".to_owned(),
        McpInvocation {
            environment_id: None,
            server: "docs".to_owned(),
            tool: "read".to_owned(),
            arguments: None,
        },
        /*animations_enabled*/ false,
    );
    cell.complete(
        Duration::ZERO,
        Ok(result(vec![json!({"type": "text", "text": text})])),
    );
    for width in [16, 24, 40, 80] {
        let lines = cell.display_hyperlink_lines(width);
        let backed = lines
            .into_iter()
            .filter(|line| line.source.is_some())
            .collect::<Vec<_>>();
        let source = source_lines(&backed);
        let output = source
            .iter()
            .find(|line| line.starts_with("one long result"))
            .expect("result source");
        assert!(
            text.starts_with(output),
            "preview must retain the original whitespace"
        );
        if width >= 40 {
            assert_eq!(output, text);
        }
        assert!(
            source
                .iter()
                .all(|line| !line.starts_with('•') && !line.starts_with("  └ "))
        );
    }
}
