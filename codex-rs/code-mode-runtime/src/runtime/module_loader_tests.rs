//! Checks that tool response delivery preserves live results without rooting discarded values.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;

use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc;

use super::super::PendingToolCall;
use super::super::RuntimeState;
use super::super::value::v8_value_to_json;
use super::resolve_tool_response;
use crate::v8_init::ensure_v8_initialized;

#[test]
fn discarded_tool_response_is_collectible_before_cell_ends() {
    ensure_v8_initialized().expect("initialize V8");
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let (runtime_command_tx, _runtime_command_rx) = std_mpsc::channel();
    scope.set_slot(RuntimeState {
        event_tx: event_tx.into(),
        pending_tool_calls: HashMap::new(),
        pending_timeouts: HashMap::new(),
        stored_values: HashMap::new(),
        stored_value_writes: HashMap::new(),
        enabled_tools: Default::default(),
        next_tool_call_id: 1,
        next_timeout_id: 1,
        tool_call_id: "cell".to_string(),
        runtime_command_tx,
        exit_requested: None,
        sh_calls: Vec::new(),
    });

    let promise = {
        v8::scope!(let scope, scope);
        let resolver = v8::PromiseResolver::new(scope).expect("create tool promise");
        let promise = resolver.get_promise(scope);
        let resolver = v8::Global::new(scope, resolver);
        scope
            .get_slot_mut::<RuntimeState>()
            .expect("runtime state")
            .pending_tool_calls
            .insert(
                "tool".to_string(),
                PendingToolCall {
                    resolver,
                    native_execution: true,
                },
            );
        v8::Global::new(scope, promise)
    };
    let response = json!({"wall_time_seconds": 0.001, "exit_code": 0, "output": "tool result"});
    resolve_tool_response(scope, "tool", Ok(response.clone())).expect("resolve tool promise");
    scope.perform_microtask_checkpoint();
    scope.low_memory_notification();

    let weak_result = {
        v8::scope!(let scope, scope);
        let promise = v8::Local::new(scope, &promise);
        assert_eq!(promise.state(), v8::PromiseState::Fulfilled);
        let result = promise.result(scope);
        assert_eq!(v8_value_to_json(scope, result), Ok(Some(response)));
        v8::Weak::new(scope, result)
    };
    drop(promise);
    scope.low_memory_notification();

    assert!(weak_result.is_empty(), "discarded response is still rooted");
}

#[test]
fn native_result_presentation_follows_each_promise_and_current_values() {
    use super::super::RuntimeEvent;
    use codex_code_mode_protocol::CodeModeToolKind;
    use codex_code_mode_protocol::ToolDefinition;
    use codex_code_mode_protocol::enabled_tool_metadata;
    use codex_protocol::ToolName;

    ensure_v8_initialized().expect("initialize V8");
    let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (runtime_command_tx, _runtime_command_rx) = std_mpsc::channel();
    let tools = [
        (
            "exec_command",
            ToolName::namespaced("functions", "exec_command"),
        ),
        (
            "mcp__fake__exec_command",
            ToolName::namespaced("mcp__fake__", "exec_command"),
        ),
        ("write_stdin", ToolName::plain("write_stdin")),
    ];
    scope.set_slot(RuntimeState {
        event_tx: event_tx.into(),
        pending_tool_calls: HashMap::new(),
        pending_timeouts: HashMap::new(),
        stored_values: HashMap::new(),
        stored_value_writes: HashMap::new(),
        enabled_tools: tools
            .into_iter()
            .map(|(name, tool_name)| {
                enabled_tool_metadata(ToolDefinition {
                    name: name.to_string(),
                    tool_name,
                    summary: String::new(),
                    description: String::new(),
                    kind: CodeModeToolKind::Function,
                    input_schema: None,
                    output_schema: None,
                })
            })
            .map(Arc::new)
            .collect(),
        next_tool_call_id: 1,
        next_timeout_id: 1,
        tool_call_id: "cell".to_string(),
        runtime_command_tx,
        exit_requested: None,
        sh_calls: Vec::new(),
    });
    super::super::globals::install_globals(scope, &[]).expect("install globals");
    let promise = super::evaluate_main_module(scope, r#"
const first = tools.exec_command({});
const other = tools.mcp__fake__exec_command({});
const running = tools.write_stdin({});
const emptyOne = tools.exec_command({});
const emptyTwo = tools.exec_command({});
const shell = sh('first', 'middle', 'last');
const result = await first;
text(result); notify(result);
text(result, {metadata:true});
text(JSON.stringify(result));
text({...result}); text(JSON.parse(JSON.stringify(result)));
store("saved", result); text(load("saved")); text({result}); text([result,"mixed"]);
text(await other);
const active = await running; text(active);
result.output = "changed"; text(result);
result.output = " \n"; notify(result);
result.extra = "kept"; text(result); delete result.extra;
result.toJSON = () => ({custom:"own"}); text(result); delete result.toJSON;
Object.setPrototypeOf(result, {toJSON(){ return {custom:"inherited"}; }}); text(result);
Object.setPrototypeOf(result, Object.prototype);
result.exit_code = 1.5; text(result);
result.exit_code = 1; text(result);
result.exit_code = null; result.session_id = 23; text(result);
result.exit_code = 0; text(result);
for (const options of [null, [], {metadata:1}, {extra:true}]) {
  try { text(result, options); } catch (error) { text(String(error).includes("options must be an object")); }
}
delete result.session_id; result.output = ""; text(result);
delete result.exit_code; text(result);
result.output = '{"body":true}\n'; text(result);
const a = await emptyOne, b = await emptyTwo;
result.exit_code = 0; result.output = "same";
text([result,a,b,active]); notify([result,a,b,active]);
result.output = "same \n\n"; text([result,a,b,active]); result.output = "same";
text([a,b], {metadata:true}); notify([a,b], {metadata:true});
text([result,result], {max_tokens: 8});
a.output = " "; text([a,b,b]);
result.exit_code = 1; text([a,result]);
result.session_id = 23; text([a,result]);
text([a,{...b}]); text([]);
const sparse = [a,b]; delete sparse[0]; text(sparse);
const custom = [a,b]; custom.toJSON = () => ({batch:"custom"}); text(custom);
delete custom.toJSON; Object.setPrototypeOf(custom,{toJSON(){return {batch:"inherited"};}}); text(custom);
const accessor = [a,b]; Object.defineProperty(accessor,0,{get(){return a;}}); text(accessor);
await shell;
"#).expect("evaluate script");
    // Responses arrive in reverse order. Origin belongs to the promise, not delivery order.
    let completed = json!({"chunk_id":"abcd12", "wall_time_seconds":0.003421, "exit_code":0, "original_token_count":214, "output":"literal\n"});
    let empty_one = json!({"wall_time_seconds":0.5,"exit_code":0,"output":""});
    let empty_two = json!({"wall_time_seconds":0.75,"exit_code":0,"output":""});
    resolve_tool_response(scope, "tool-5", Ok(empty_two.clone())).unwrap();
    resolve_tool_response(scope, "tool-4", Ok(empty_one.clone())).unwrap();
    resolve_tool_response(
        scope,
        "tool-3",
        Ok(json!({"wall_time_seconds":0.2, "session_id":42, "output":"partial"})),
    )
    .unwrap();
    resolve_tool_response(scope, "tool-2", Ok(completed.clone())).unwrap();
    resolve_tool_response(scope, "tool-1", Ok(completed.clone())).unwrap();
    scope.perform_microtask_checkpoint();
    let shell_sections = ["first", "middle", "last"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            format!(
                "[{}]\n{name}-head{}{name}-tail",
                index + 1,
                "x".repeat(1000)
            )
        })
        .collect::<Vec<_>>();
    for (index, section) in shell_sections.iter().enumerate() {
        let output = section.split_once('\n').unwrap().1;
        resolve_tool_response(
            scope,
            &format!("tool-{}", index + 6),
            Ok(json!({"wall_time_seconds":0.0,"exit_code":0,"output":output})),
        )
        .unwrap();
    }
    scope.perform_microtask_checkpoint();
    assert!(matches!(
        super::completion_state(scope, promise.as_ref()),
        super::CompletionState::Completed {
            error_text: None,
            ..
        }
    ));
    let mut text = Vec::new();
    let mut notifications = Vec::new();
    let mut batches = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        match event {
            RuntimeEvent::ContentItem(
                codex_code_mode_protocol::FunctionCallOutputContentItem::InputText {
                    text: value,
                    max_tokens: None,
                },
            ) => text.push(value),
            RuntimeEvent::ContentItem(
                codex_code_mode_protocol::FunctionCallOutputContentItem::InputTextBatch {
                    sections,
                    max_tokens,
                },
            ) => {
                batches.push((sections.clone(), max_tokens));
                text.push(codex_protocol::join_execution_sections(sections));
            }
            RuntimeEvent::Notify { text, .. } => notifications.push(text),
            _ => {}
        }
    }
    assert!(batches.contains(&(
        vec![
            "[1]\nsame".to_string(),
            "[2–3] Completed".to_string(),
            "[4] Running (session 42)\npartial".to_string()
        ],
        None
    )));
    assert!(batches.contains(&(
        vec!["[1]\nsame".to_string(), "[2]\nsame".to_string()],
        Some(8)
    )));
    assert!(
        batches.contains(&(shell_sections.clone(), None)),
        "sh must preserve the first, middle, and last command as separate native sections"
    );
    let full = completed.to_string();
    let mut modified = completed.clone();
    modified["output"] = json!(" \n");
    modified["extra"] = json!("kept");
    let mut contradictory = completed.clone();
    contradictory["output"] = json!(" \n");
    contradictory["session_id"] = json!(23);
    let mut fractional = completed.clone();
    fractional["output"] = json!(" \n");
    fractional["exit_code"] = json!(1.5);
    let mut whitespace = empty_one;
    whitespace["output"] = json!(" ");
    let mut failed = completed.clone();
    failed.as_object_mut().unwrap().shift_remove("exit_code");
    failed["output"] = json!("same");
    failed["exit_code"] = json!(1);
    failed["session_id"] = json!(23);
    let text = text
        .into_iter()
        .map(|text| {
            if text.trim() == text {
                serde_json::from_str::<serde_json::Value>(&text)
                    .map(|value| value.to_string())
                    .unwrap_or(text)
            } else {
                text
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        text,
        vec![
            "literal\n".to_string(),
            "exit=0 wall=0.003421s chunk=abcd12 tokens≈214".to_string() + "\nliteral\n",
            full.clone(),
            full.clone(),
            full.clone(),
            full.clone(),
            json!({"result":completed}).to_string(),
            json!([completed, "mixed"]).to_string(),
            full,
            "Running (session 42)\npartial".to_string(),
            "changed".to_string(),
            modified.to_string(),
            json!({"custom":"own"}).to_string(),
            json!({"custom":"inherited"}).to_string(),
            fractional.to_string(),
            "Exit 1\n \n".to_string(),
            "Running (session 23)\n \n".to_string(),
            contradictory.to_string(),
            "true".to_string(),
            "true".to_string(),
            "true".to_string(),
            "true".to_string(),
            "Completed".to_string(),
            "Execution status unavailable".to_string(),
            "{\"body\":true}\n".to_string(),
            "[1]\nsame\n[2–3] Completed\n[4] Running (session 42)\npartial".to_string(),
            "[1]\nsame \n\n[2–3] Completed\n[4] Running (session 42)\npartial".to_string(),
            "[1] exit=0 wall=0.5s\n[2] exit=0 wall=0.75s".to_string(),
            "[1]\nsame\n[2]\nsame".to_string(),
            "[1]\n \n[2–3] Completed".to_string(),
            "[1]\n \n[2] Exit 1\nsame".to_string(),
            json!([whitespace, failed]).to_string(),
            json!([whitespace, empty_two]).to_string(),
            "[]".to_string(),
            json!([null, empty_two]).to_string(),
            json!({"batch":"custom"}).to_string(),
            json!({"batch":"inherited"}).to_string(),
            json!([whitespace, empty_two]).to_string(),
            codex_protocol::join_execution_sections(shell_sections),
        ]
    );
    assert_eq!(
        notifications,
        vec![
            "literal\n",
            " \n",
            "[1]\nsame\n[2–3] Completed\n[4] Running (session 42)\npartial",
            "[1] exit=0 wall=0.5s\n[2] exit=0 wall=0.75s"
        ]
    );
}
