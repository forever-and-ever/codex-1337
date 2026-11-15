use super::DEFAULT_OUTPUT_CHARS;
use codex_app_server_protocol::delegation::DelegatedMessage;

use codex_app_server_protocol::ImageReference;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::UserInput;
use serde_json::Value;
use serde_json::json;

pub(super) fn thread_summary(thread: &Thread) -> Value {
    let mut summary = json!({
        "id": thread.id,
        "kind": "codex",
        "projectId": thread.project_id,
        "title": thread.name,
        "summary": truncate(&thread.preview, /*limit*/ 300),
        "status": match &thread.status {
            ThreadStatus::Idle => "idle",
            ThreadStatus::NotLoaded => "notLoaded",
            ThreadStatus::SystemError => "systemError",
            ThreadStatus::Active { .. } => "active",
        },
        "cwd": thread.cwd,
        "updatedAt": thread.updated_at
    });
    if let Some(role) = &thread.context_role {
        summary["contextRole"] = json!(role);
    }
    summary
}

pub(super) fn turn_summary(turn: &Turn, include_outputs: bool, output_chars: usize) -> Value {
    let mut items: Vec<Value> = turn
        .items
        .iter()
        .rev()
        .filter_map(|item| Some(match item.payload.native_item() {
            ThreadItemPayload::CodeModeOwned(_) => unreachable!("ownership ingress rejects nesting"),
            ThreadItemPayload::ToolOutputTokens { .. } | ThreadItemPayload::CodeMode(_) | ThreadItemPayload::Thought { .. } | ThreadItemPayload::Preamble { .. } | ThreadItemPayload::StepAdmission(_)
        | ThreadItemPayload::ToolInput(_) | ThreadItemPayload::Arrival(_) => return None,
            ThreadItemPayload::UserMessage { id, content, .. } => json!({
                "type": "userMessage",
                "id": id,
                "content": content.iter().map(|input| match input {
                    UserInput::Text { text, .. } => {
                        let mut input = json!({"type": "text", "text": truncate(text, DEFAULT_OUTPUT_CHARS)});
                        if let Some(delegated) = DelegatedMessage::parse(text)
                        {
                            input["codexDelegation"] = json!({
                                "sourceThreadId": delegated.source_thread_id,
                                "sourceThreadName": delegated.source_thread_name,
                                "sentAt": delegated.sent_at,
                                "input": truncate(&delegated.input, DEFAULT_OUTPUT_CHARS)
                            });
                        }
                        input
                    }
                    UserInput::Image {
                        image: ImageReference::Inline { url },
                        ..
                    } => json!({"type": "image", "url": url}),
                    UserInput::Image {
                        image: ImageReference::File { file_id },
                        ..
                    } => json!({"type": "image", "fileId": file_id}),
                    UserInput::LocalImage { path, .. } => json!({"type": "localImage", "path": path}),
                    UserInput::Audio { audio } => match audio {
                        codex_app_server_protocol::AudioReference::Inline { url } => json!({"type": "audio", "url": url}),
                        codex_app_server_protocol::AudioReference::File { file_id } => json!({"type": "audio", "fileId": file_id}),
                    },
                    UserInput::LocalAudio { path } => json!({"type": "localAudio", "path": path}),
                    UserInput::Skill { name, path } => json!({"type": "skill", "name": name, "path": path}),
                    UserInput::Mention { name, path } => json!({"type": "mention", "name": name, "path": path}),
                }).collect::<Vec<_>>()
            }),
            ThreadItemPayload::HookPrompt { id, fragments } => json!({
                "type": "hookPrompt", "id": id, "fragmentCount": fragments.len()
            }),
            ThreadItemPayload::FunctionCallOutput {
                id,
                call_id: _,
                name,
                namespace,
                output,
            } => {
                let mut item = json!({
                    "type": "functionCallOutput", "id": id, "name": name, "namespace": namespace
                });
                if let Some(delegated) =
                    DelegatedMessage::from_tool_output(name, namespace.as_deref(), output)
                {
                    item["codexDelegation"] = json!({
                        "sourceThreadId": delegated.source_thread_id,
                        "sourceThreadName": delegated.source_thread_name,
                        "sentAt": delegated.sent_at,
                        "input": truncate(&delegated.input, DEFAULT_OUTPUT_CHARS)
                    });
                }
                if include_outputs {
                    item["output"] = output_summary(
                        output.to_text().as_deref().unwrap_or("[non-text output]"),
                        output_chars,
                    );
                }
                item
            }
            ThreadItemPayload::AgentMessage { id, text, phase, .. } => json!({
                "type": "agentMessage", "id": id, "text": text, "phase": phase
            }),
            ThreadItemPayload::Plan { id, text } => json!({
                "type": "plan", "id": id, "text": truncate(text, DEFAULT_OUTPUT_CHARS)
            }),
            ThreadItemPayload::Reasoning {
                id,
                summary,
                content,
            } => {
                let mut item = json!({
                    "type": "reasoning",
                    "id": id,
                    "summary": summary.iter().map(|text| truncate(text, DEFAULT_OUTPUT_CHARS)).collect::<Vec<_>>()
                });
                if include_outputs {
                    item["content"] = json!(
                        content.iter().map(|text| output_summary(text, output_chars)).collect::<Vec<_>>()
                    );
                }
                item
            }
            ThreadItemPayload::CommandExecution {
                id,
                command,
                cwd,
                aggregated_output,
                exit_code,
                status,
                duration_ms,
                ..
            } => {
                let mut item = json!({
                    "type": "commandExecution",
                    "id": id,
                    "command": truncate(command, DEFAULT_OUTPUT_CHARS),
                    "cwd": cwd,
                    "exitCode": exit_code,
                    "status": status,
                    "durationMs": duration_ms
                });
                if include_outputs && let Some(output) = aggregated_output {
                    item["output"] = output_summary(output, output_chars);
                }
                item
            }
            ThreadItemPayload::FileChange {
                id,
                changes,
                status,
            } => json!({
                "type": "fileChange",
                "id": id,
                "status": status,
                "changes": changes.iter().map(|change| {
                    let mut item = json!({"path": change.path, "kind": change.kind});
                    if include_outputs {
                        item["diff"] = output_summary(&change.diff, output_chars);
                    }
                    item
                }).collect::<Vec<_>>()
            }),
            ThreadItemPayload::McpToolCall {
                id,
                server,
                tool,
                status,
                arguments,
                duration_ms,
                ..
            } => json!({
                "type": "mcpToolCall", "id": id, "server": server, "tool": tool,
                "arguments": arguments, "status": status, "durationMs": duration_ms
            }),
            ThreadItemPayload::DynamicToolCall {
                id,
                namespace,
                tool,
                arguments,
                status,
                success,
                duration_ms,
                ..
            } => json!({
                "type": "dynamicToolCall", "id": id, "namespace": namespace,
                "tool": tool, "arguments": arguments, "status": status,
                "success": success, "durationMs": duration_ms
            }),
            ThreadItemPayload::CollabAgentToolCall {
                id,
                tool,
                status,
                sender_thread_id,
                receiver_thread_ids,
                prompt,
                model,
                reasoning_effort,
                ..
            } => json!({
                "type": "collabAgentToolCall", "id": id, "tool": tool,
                "status": status, "senderThreadId": sender_thread_id,
                "receiverThreadIds": receiver_thread_ids, "prompt": prompt,
                "model": model, "reasoningEffort": reasoning_effort
            }),
            ThreadItemPayload::SubAgentActivity {
                id,
                kind,
                agent_thread_id,
                agent_path,
                ..
            } => json!({
                "type": "subAgentActivity", "id": id, "kind": kind,
                "agentThreadId": agent_thread_id, "agentPath": agent_path
            }),
            ThreadItemPayload::WebSearch(item) => json!({
                "type": "webSearch", "id": item.id,
                "query": truncate(&item.query, DEFAULT_OUTPUT_CHARS), "action": item.action
            }),
            ThreadItemPayload::ImageView { id, path } => json!({
                "type": "imageView", "id": id, "path": path
            }),
            ThreadItemPayload::Sleep(item) => json!({
                "type": "sleep", "id": item.id, "durationMs": item.duration_ms
            }),
            ThreadItemPayload::ImageGeneration(item) => {
                let mut image = json!({
                    "type": "imageGeneration", "id": item.id, "status": item.status,
                    "revisedPrompt": item.revised_prompt.as_deref().map(|prompt| truncate(prompt, DEFAULT_OUTPUT_CHARS)),
                    "savedPath": item.saved_path
                });
                match &item.image {
                    codex_app_server_protocol::GeneratedImageRef::Inline { result } => {
                        if include_outputs {
                            image["result"] = output_summary(result, output_chars);
                        }
                    }
                    codex_app_server_protocol::GeneratedImageRef::File { file_id } => {
                        image["fileId"] = json!(file_id);
                    }
                }
                image
            }
            ThreadItemPayload::EnteredReviewMode { id, review } => json!({
                "type": "enteredReviewMode", "id": id, "review": truncate(review, DEFAULT_OUTPUT_CHARS)
            }),
            ThreadItemPayload::ExitedReviewMode { id, review } => json!({
                "type": "exitedReviewMode", "id": id, "review": truncate(review, DEFAULT_OUTPUT_CHARS)
            }),
            ThreadItemPayload::ContextCompaction { id } => json!({
                "type": "contextCompaction", "id": id
            }),
        }))
        .take(20)
        .collect();
    items.reverse();
    json!({
        "id": turn.id,
        "status": turn.status,
        "error": turn.error.as_ref().map(|error| json!({
            "message": error.message,
            "additionalDetails": error.additional_details
        })),
        "startedAt": turn.started_at,
        "completedAt": turn.completed_at,
        "durationMs": turn.duration_ms,
        "items": items
    })
}

pub(super) fn output_summary(text: &str, limit: usize) -> Value {
    let original_chars = text.chars().count();
    if original_chars <= limit {
        json!({"text": text, "truncated": false})
    } else {
        json!({
            "text": text.chars().take(limit).collect::<String>(),
            "truncated": true,
            "originalChars": original_chars
        })
    }
}

pub(super) fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    if limit == 0 {
        return String::new();
    }
    format!("{}…", text.chars().take(limit - 1).collect::<String>())
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
