use super::exec_sections;
use crate::context::ContextualUserFragment;
use codex_code_mode::CodeModeToolKind;
use codex_code_mode::ImageDetailVisibility;
use codex_code_mode::MediaHelpers;
use codex_code_mode::ToolDefinition;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

/// The census parses headings that `build_exec_tool_description` renders in another crate.
#[tokio::test]
async fn exec_sections_split_the_rendered_nested_tools_losslessly() {
    let tool = |name: &str| ToolDefinition {
        summary: String::new(),
        name: name.to_string(),
        tool_name: ToolName::plain(name),
        description: format!("{name} does work.\n\n### Not a heading"),
        kind: CodeModeToolKind::Function,
        input_schema: None,
        output_schema: None,
    };
    let description = codex_code_mode::build_exec_tool_description(
        &[tool("update_plan"), tool("mcp__depot__consult")],
        &[],
        &BTreeMap::new(),
        /*max_yield_time_ms*/ 600_000,
        /*code_mode_only*/ true,
        MediaHelpers {
            image_detail: ImageDetailVisibility::Visible,
            audio: true,
            generated_image: true,
        },
        codex_code_mode::DeferredToolDiscovery::Catalog,
        /*messages*/ None,
    );
    let (guidance, sections) = exec_sections(&description);
    assert_eq!(
        sections.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        vec!["update_plan", "mcp__depot__consult"]
    );
    let rejoined: String = std::iter::once(guidance)
        .chain(sections.iter().map(|(_, text)| *text))
        .collect();
    assert_eq!(rejoined, description);
    // Delimiter-looking user text must never acquire declaration provenance.
    let text = "α\n--- Tool declarations ---\n{\"not\":\"a schema\"}\n";
    let sources = super::context_sources(
        text,
        &super::PreambleTools::History(std::borrow::Cow::Borrowed(&[])),
        &[super::InjectedText {
            role: "developer".into(),
            text: text.into(),
        }],
    );
    assert_eq!(
        sources,
        vec![
            codex_protocol::ContextSource::Prompt {
                label: "Base instructions".into(),
                text: text.into()
            },
            codex_protocol::ContextSource::ToolDeclarations { text: "[]".into() },
            codex_protocol::ContextSource::Prompt {
                label: "developer".into(),
                text: text.into()
            },
        ]
    );

    // Base ownership is positional: a merged sibling and later amendment still count.
    let mut merged = ContextualUserFragment::into(crate::context::BaseInstructionsFragment(
        "canonical base".into(),
    ));
    let mut content = codex_context_fragments::to_annotated_content(&mut merged).unwrap();
    content.push(codex_context_fragments::AnnotatedContent::text(
        "merged sibling",
        ContentItemKind("developer_instructions".into()),
        ContentItemMetadata::skills(),
    ));
    codex_context_fragments::set_annotated_content(&mut merged, content).unwrap();
    let mut amendment = merged.clone();
    codex_context_fragments::set_annotated_content(
        &mut amendment,
        vec![codex_context_fragments::AnnotatedContent::text(
            "<instruction_diff>updated base</instruction_diff>",
            ContentItemKind("context.instruction_diff/base_instructions".into()),
            ContentItemMetadata::default(),
        )],
    )
    .unwrap();
    let prompt = crate::client_common::Prompt {
        input: vec![merged, amendment],
        tools: std::sync::Arc::from([]),
        parallel_tool_calls: false,
        base_instructions: codex_protocol::models::BaseInstructions {
            text: String::new(),
            provenance: None,
        },
        prompt_cache_ttl: None,
        output_schema: None,
        output_schema_strict: false,
        cyber_access_program: None,
    };
    let (session, _) = Box::pin(crate::session::tests::make_session_and_context()).await;
    let mark = session
        .prepare_preamble(&prompt)
        .await
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        mark.sources,
        vec![
            codex_protocol::ContextSource::Prompt {
                label: "Base instructions".into(),
                text: "canonical base".into(),
            },
            codex_protocol::ContextSource::ToolDeclarations { text: "[]".into() },
            codex_protocol::ContextSource::Prompt {
                label: "developer".into(),
                text: "merged sibling".into(),
            },
            codex_protocol::ContextSource::Prompt {
                label: "developer".into(),
                text: "<instruction_diff>updated base</instruction_diff>".into(),
            },
        ],
    );
}
