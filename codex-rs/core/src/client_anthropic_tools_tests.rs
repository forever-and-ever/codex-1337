use super::project_tools;
use crate::tools::handlers::multi_agents_spec::create_send_message_tool;
use codex_api::AnthropicToolIdentity;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ResponsesApiNamespace;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn aliases_preserve_dispatch_identity_across_collisions_and_catalogue_order() {
    // Flattening a namespace can collide with a direct name; choosing the first
    // declaration would make replay aliases depend on discovery order.
    let handbook = codex_code_mode::build_exec_tool_description_for(
        codex_code_mode::presentation::ToolDialect::Anthropic,
        &[],
        &[],
        &BTreeMap::default(),
        600_000,
        true,
        codex_code_mode::MediaHelpers {
            image_detail: codex_code_mode::ImageDetailVisibility::Visible,
            audio: false,
            generated_image: false,
        },
        codex_code_mode::DeferredToolDiscovery::Catalog,
        None,
    );
    assert!(!handbook.contains("declare const tools"));
    let exec = FreeformTool {
        name: "exec".to_string(),
        description: handbook,
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: "start: /[\\s\\S]+/".to_string(),
        },
    };
    let mut specs = vec![
        ToolSpec::Function(function("functions__exec")),
        ToolSpec::Namespace(ResponsesApiNamespace {
            name: "functions".to_string(),
            description: "Code Mode controls".to_string(),
            tools: vec![ResponsesApiNamespaceTool::Custom(exec)],
        }),
        create_send_message_tool(),
        ToolSpec::Namespace(ResponsesApiNamespace {
            name: "mcp__長い".repeat(20),
            description: String::new(),
            tools: vec![ResponsesApiNamespaceTool::Function(function("lookup"))],
        }),
    ];
    let tools = project_tools(&specs).unwrap();
    specs.reverse();
    let reversed = project_tools(&specs).unwrap();
    assert_eq!(
        tools.as_ref(),
        reversed.iter().rev().cloned().collect::<Vec<_>>()
    );
    for spec in &specs {
        for tool in project_tools(std::slice::from_ref(spec)).unwrap().iter() {
            assert_eq!(
                tools
                    .iter()
                    .find(|original| original.identity == tool.identity),
                Some(tool)
            );
        }
    }

    let identities = vec![
        AnthropicToolIdentity::Function {
            name: "functions__exec".to_string(),
            namespace: None,
        },
        AnthropicToolIdentity::Custom {
            name: "exec".to_string(),
            namespace: Some("functions".to_string()),
        },
        AnthropicToolIdentity::Function {
            name: "send_message".to_string(),
            namespace: None,
        },
        AnthropicToolIdentity::Function {
            name: "lookup".to_string(),
            namespace: Some("mcp__長い".repeat(20)),
        },
    ];
    // The captured definitions, rather than splitting alias text, restore the
    // original function/custom distinction and namespace after a response.
    let restored = tools
        .iter()
        .map(|tool| {
            assert!(tool.name.len() <= 64);
            assert!(
                tool.name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            );
            tools
                .iter()
                .find(|definition| definition.name == tool.name)
                .unwrap()
                .identity
                .clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(restored, identities);
    assert_ne!(tools[0].name, tools[1].name);
    assert_eq!(
        tools[1].input_schema,
        json!({
            "type": "object",
            "properties": {"input": {"type": "string"}},
            "required": ["input"],
            "additionalProperties": false,
        })
    );
    assert!(tools[1].description.contains("ALL_TOOLS"));
    assert_eq!(tools[2].name, "send_message");
    // Native rejects root oneOf, but dropping it would permit both encrypted
    // and plaintext messages. The envelope retains the actual family contract.
    let ToolSpec::Function(family) = create_send_message_tool() else {
        panic!("family tool must be a function");
    };
    assert_eq!(
        tools[2].input_schema,
        json!({
            "type": "object",
            "properties": {"arguments": family.parameters},
            "required": ["arguments"],
            "additionalProperties": false,
        })
    );
    assert_eq!(
        tools[0].input_schema,
        json!({
            "type": "object",
            "properties": {"arguments": {
                "type": "object",
                "properties": {"payload": {"$ref": "#/properties/arguments/$defs/payload"}},
                "$defs": {"payload": {"type": "object", "properties": {
                    "literal": {"enum": [{"$ref": "instance data"}]},
                    "recursive": {"$ref": "#/properties/arguments"}
                }}}
            }},
            "required": ["arguments"],
            "additionalProperties": false,
        })
    );
    for reference in ["#anchor", "other.json#/$defs/payload"] {
        let mut unsupported = function("unsupported");
        unsupported.parameters.schema_ref = Some(reference.into());
        assert!(project_tools(&[ToolSpec::Function(unsupported)]).is_err());
    }

    // Default-namespace aliases remain one dispatch address in Codex.
    specs.push(ToolSpec::Namespace(ResponsesApiNamespace {
        name: "functions".to_string(),
        description: String::new(),
        tools: vec![ResponsesApiNamespaceTool::Function(function(
            "send_message",
        ))],
    }));
    assert!(project_tools(&specs).is_err());
}

fn function(name: &str) -> ResponsesApiTool {
    ResponsesApiTool {
        name: name.to_string(),
        description: "Run a tool".to_string(),
        strict: false,
        defer_loading: None,
        parameters: serde_json::from_value(json!({
            "type": "object",
            "properties": {"payload": {"$ref": "#/$defs/payload"}},
            "$defs": {"payload": {"type": "object", "properties": {
                "literal": {"enum": [{"$ref": "instance data"}]},
                "recursive": {"$ref": "#"}
            }}}
        }))
        .unwrap(),
        output_schema: None,
    }
}
