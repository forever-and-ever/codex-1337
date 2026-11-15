use super::*;
use codex_tools::FreeformToolFormat;
use codex_tools::ResponsesApiNamespace;

#[test]
fn wire_projection_hides_custom_tools_and_tool_images() {
    // Responses hosts may implement functions but not custom tools. Conversion
    // must be wire-only: dispatch and replay retain the original input kind.
    // Chat-completions hosts lose images in tool results, so images ride in a user
    // message, which must follow the whole run of outputs: inside the run it would
    // part the later outputs from their calls.
    let freeform = FreeformTool {
        name: "python".into(),
        description: "Execute Python".into(),
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "grammar".into(),
            syntax: "lark".into(),
            definition: "start: /[\\s\\S]+/".into(),
        },
    };
    let specs = [
        ToolSpec::Freeform(freeform.clone()),
        ToolSpec::Namespace(ResponsesApiNamespace {
            name: "scripts".into(),
            description: "Script tools".into(),
            tools: vec![ResponsesApiNamespaceTool::Custom(freeform)],
        }),
    ];
    let input = "print(\"6*7\\n\")";
    let original: ResponseItem = serde_json::from_value(json!({
        "type": "custom_tool_call", "id": "call-item", "call_id": "call-1",
        "name": "python", "namespace": "scripts", "input": input,
    }))
    .unwrap();
    let image = "data:image/png;base64,AA==";
    let rest: Vec<ResponseItem> = serde_json::from_value(json!([
        {"type": "function_call", "call_id": "call-2", "name": "shell", "arguments": "{}"},
        {"type": "custom_tool_call_output", "call_id": "call-1", "name": "python", "output": [
            {"type": "input_text", "text": "42"}, {"type": "input_image", "image_url": image},
        ]},
        {"type": "function_call_output", "call_id": "call-2", "output": "ok"},
    ]))
    .unwrap();
    let mut request = ResponsesApiRequest {
        model: "openai/test".into(),
        stream: true,
        service_tier: None,
        openrouter: None,
        input: std::iter::once(original.clone()).chain(rest).collect(),
        tools: None,
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream_options: None,
        include: Vec::new(),
        prompt_cache_key: None,
        prompt_cache_options: None,
        text: None,
        client_metadata: None,
        access_programs: None,
    };
    let mut projection = Projection::request(&mut request, &specs).unwrap();
    let wire = serde_json::to_value(&request).unwrap();
    assert!(
        wire["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["type"] == "function")
    );
    assert_eq!(
        wire["tools"][0]["parameters"],
        json!({
            "type": "object", "properties": {"input": {"type": "string"}},
            "required": ["input"], "additionalProperties": false,
        })
    );
    assert!(
        wire["tools"][0]["description"]
            .as_str()
            .unwrap()
            .contains("start:")
    );
    assert_eq!(wire["input"][0]["type"], "function_call");
    assert_eq!(wire["input"][0]["name"], wire["tools"][1]["name"]);
    assert_eq!(wire["input"][2]["type"], "function_call_output");
    assert_eq!(wire["input"][2]["name"], wire["tools"][1]["name"]);
    assert_eq!(
        wire["input"][2]["output"],
        json!([
            {"type": "input_text", "text": "42"},
            {"type": "input_text", "text": "[image 1: attached after the tool results]"},
        ])
    );
    assert_eq!(wire["input"][3]["call_id"], "call-2");
    assert_eq!(wire["input"].as_array().unwrap().len(), 5);
    assert_eq!(wire["input"][4]["role"], "user");
    assert_eq!(
        wire["input"][4]["content"][2],
        json!({"type": "input_image", "image_url": image})
    );
    let arguments: Value =
        serde_json::from_str(wire["input"][0]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(arguments, json!({"input": input}));
    let response = request.input[0].clone();
    let events = projection.event(ResponseEvent::OutputItemAdded(response.clone()));
    assert!(
        matches!(&events[0], ResponseEvent::OutputItemAdded(ResponseItem::CustomToolCall { input, .. }) if input.is_empty())
    );
    assert!(
        projection
            .event(ResponseEvent::ToolCallInputDelta {
                item_id: ResponseItemId::from_server("call-item".into()),
                call_id: Some("call-1".into()),
                delta: "{\"input\":".into(),
            })
            .is_empty()
    );
    let events = projection.event(ResponseEvent::OutputItemDone(response.clone()));
    assert!(
        matches!(&events[0], ResponseEvent::ToolCallInputDelta { delta, .. } if delta == input)
    );
    assert!(matches!(&events[1], ResponseEvent::OutputItemDone(item) if item == &original));
    let mut malformed = response;
    if let ResponseItem::FunctionCall { arguments, .. } = &mut malformed {
        *arguments = "{\"input\":42}".into();
    }
    let events = projection.event(ResponseEvent::OutputItemDone(malformed));
    assert!(
        matches!(&events[0], ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { name, namespace, .. }) if name == "python" && namespace.as_deref() == Some("scripts"))
    );
}
