use super::*;
use codex_tools::FreeformToolFormat;
use codex_tools::ResponsesApiNamespace;

#[test]
fn freeform_projection_round_trip() {
    // Responses hosts may implement functions but not custom tools. Conversion
    // must be wire-only: dispatch and replay retain the original input kind.
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
    let output: ResponseItem = serde_json::from_value(json!({
        "type": "custom_tool_call_output", "call_id": "call-1", "name": "python", "output": "42",
    }))
    .unwrap();
    let mut request = ResponsesApiRequest {
        model: "openai/test".into(),
        stream: true,
        service_tier: None,
        openrouter: None,
        input: vec![original.clone(), output],
        tools: None,
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream_options: None,
        include: Vec::new(),
        prompt_cache_key: None,
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
    assert_eq!(wire["input"][1]["type"], "function_call_output");
    assert_eq!(wire["input"][1]["name"], wire["tools"][1]["name"]);
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
