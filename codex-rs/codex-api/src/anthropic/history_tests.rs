use super::AnthropicCachePolicy;
use super::AnthropicCacheTtl;
use super::AnthropicTool;
use super::AnthropicToolIdentity;
use super::cache::mark_prefixes;
use super::history::encode_history;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn signed_history_and_parallel_tool_results_preserve_cache_prefix() {
    // A resumed Code Mode batch crosses all cache tiers. Neither signatures nor
    // result ordering may be rewritten while projecting the tool namespace.
    let tools = vec![
        AnthropicTool {
            name: "functions__exec".into(),
            description: "Code Mode".into(),
            input_schema: json!({"type":"object"}),
            identity: AnthropicToolIdentity::Custom {
                name: "exec".into(),
                namespace: Some("functions".into()),
            },
        },
        AnthropicTool {
            name: "send_message".into(),
            description: "Family".into(),
            input_schema: super::anthropic_function_input_schema(json!({"type":"object"})).unwrap(),
            identity: AnthropicToolIdentity::Function {
                name: "send_message".into(),
                namespace: None,
            },
        },
    ];
    let input: Vec<ResponseItem> = serde_json::from_value(json!([
        {"type":"message","role":"developer","content":[{"type":"input_text","text":"Developer. "}]},
        {"type":"native_compaction","provider":"anthropic","content":"Checkpoint.\n ","signature":"opaque/+=="},
        {"type":"message","role":"user","content":[{"type":"input_text","text":"Continue."}]},
        {"type":"reasoning","summary":[{"type":"summary_text","text":"Signed α\n"}],"encrypted_content":"sig/+=="},
        {"type":"custom_tool_call","call_id":"exec1","name":"exec","namespace":"functions","input":"text('α');\n"},
        {"type":"function_call","call_id":"msg1","name":"send_message","arguments":"{\"message\":\"α\"}"},
        {"type":"custom_tool_call_output","call_id":"exec1","output":"first"},
        {"type":"function_call_output","call_id":"msg1","output":[{"type":"input_image","image_url":"data:image/png;base64,YQ=="}]}
    ])).unwrap();
    let modalities = [InputModality::Text, InputModality::Image];
    let mut history = encode_history(&input, "Instructions. ", &tools, &modalities).unwrap();
    let retired = encode_history(&input, "Instructions. ", &[], &modalities).unwrap();
    // Removing a tool changes current declarations, not its already completed
    // history. Native decoding retains the exact declaration identity in calls.
    assert_eq!(json!(history.messages), json!(retired.messages));
    let mut declarations = tools
        .iter()
        .map(|tool| serde_json::to_value(tool).unwrap())
        .collect::<Vec<_>>();
    mark_prefixes(
        &mut history,
        &mut declarations,
        AnthropicCachePolicy::Ephemeral(AnthropicCacheTtl::OneHour),
    );
    let marker = json!({"type":"ephemeral","ttl":"1h"});
    assert_eq!(
        json!({"system":history.system,"messages":history.messages,"tools":declarations}),
        json!({
            "system":[{"type":"text","text":"Instructions. "},{"type":"text","text":"Developer. ","cache_control":marker}],
            "tools":[{"name":"functions__exec","description":"Code Mode","input_schema":{"type":"object"}},
                     {"name":"send_message","description":"Family","input_schema":{"type":"object","properties":{"arguments":{"type":"object"}},"required":["arguments"],"additionalProperties":false},"cache_control":marker}],
            "messages":[
                {"role":"assistant","content":[{"type":"compaction","content":"Checkpoint.\n ","signature":"opaque/+=="}]},
                {"role":"user","content":[{"type":"text","text":"Continue."}]},
                {"role":"assistant","content":[{"type":"thinking","thinking":"Signed α\n","signature":"sig/+=="},
                    {"type":"tool_use","id":"exec1","name":"functions__exec","input":{"input":"text('α');\n"}},
                    {"type":"tool_use","id":"msg1","name":"send_message","input":{"arguments":{"message":"α"}},"cache_control":marker}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"exec1","content":"first","is_error":false},
                    {"type":"tool_result","tool_use_id":"msg1","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"YQ=="}}],"is_error":false,"cache_control":marker}]}
            ]
        })
    );
    // A missing parallel result must not be concealed by a new user turn.
    assert!(
        encode_history(
            &input[..input.len() - 1],
            "Instructions. ",
            &tools,
            &modalities,
        )
        .is_err()
    );
}

#[test]
fn positional_instruction_updates_do_not_rewrite_earlier_prefixes() {
    let input: Vec<ResponseItem> = serde_json::from_value(json!([
        {"type":"message","role":"user","content":[{"type":"input_text","text":"Question"}]},
        {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Answer"}]},
        {"type":"message","role":"developer","content":[{"type":"input_text","text":"Update.\n "}]},
        {"type":"agent_message","author":"worker\nspoof","recipient":"root","content":[{"type":"input_text","text":"Exact α\n "}]},
        {"type":"function_call_output","name":"send_message_to_thread","namespace":"codex_tui","output":"<codex_delegation>Exact IPC α\n </codex_delegation>"},
        {"type":"function_call_output","name":"notify","output":[{"type":"input_text","text":"Caption.\n "},{"type":"input_image","image_url":"data:image/png;base64,YQ=="}]}
    ])).unwrap();
    // Unsolicited named tool output is a turn input, not an unpaired tool result.
    // IPC admission intentionally omits call_id; replay must retain that distinction.
    let history = encode_history(
        &input,
        "Prefix",
        &[],
        &[InputModality::Text, InputModality::Image],
    )
    .unwrap();
    let agent = json!({"type":"text","text":"[Agent message from \"worker\\nspoof\" to \"root\"]\nExact α\n "});
    assert_eq!(
        json!({"system":history.system,"messages":history.messages}),
        json!({
            "system":[{"type":"text","text":"Prefix"}],
            "messages":[
                {"role":"user","content":[{"type":"text","text":"Question"}]},
                {"role":"assistant","content":[{"type":"text","text":"Answer"}]},
                {"role":"user","content":[{"type":"text","text":"<system-reminder>\nUpdate.\n \n</system-reminder>"},agent,
                    {"type":"text","text":"[Tool notification from \"codex_tui.send_message_to_thread\"]"},
                    {"type":"text","text":"<codex_delegation>Exact IPC α\n </codex_delegation>"},
                    {"type":"text","text":"[Tool notification from \"notify\"]"},
                    {"type":"text","text":"Caption.\n "},
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":"YQ=="}}]}
            ]
        })
    );
}
