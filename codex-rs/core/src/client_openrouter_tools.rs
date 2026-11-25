use codex_api::ResponseEvent;
use codex_api::ResponsesApiRequest;
use codex_api::ResponsesApiTools;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;
use codex_tools::FreeformTool;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

#[derive(serde::Serialize, Deserialize)]
enum Input {
    Function,
    Freeform,
}

#[derive(serde::Serialize, Deserialize)]
struct Tool {
    identity: ToolName,
    alias: String,
    input: Input,
}

#[derive(serde::Serialize, Deserialize)]
pub(in crate::client) struct Projection {
    tools: Vec<Tool>,
    freeform_items: HashSet<ResponseItemId>,
}

// Aliases depend on identity, never the current inventory: old calls must replay
// after a tool has been removed. Reserve the generated prefix from plain names.
fn alias(identity: &ToolName) -> String {
    let identity = identity.clone().with_default_namespace();
    let readable = if identity.is_default_namespace() {
        (!identity.name.contains("__")).then(|| identity.name.clone())
    } else {
        identity
            .namespace
            .as_deref()
            .filter(|namespace| {
                !namespace.contains("__")
                    && !namespace.ends_with('_')
                    && !identity.name.contains("__")
                    && !identity.name.starts_with('_')
            })
            .map(|namespace| format!("{namespace}__{}", identity.name))
    };
    if let Some(readable) = readable
        && !readable.starts_with("codex_or_")
        && readable.len() <= 64
        && readable
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return readable;
    }
    let digest = Uuid::new_v5(
        &Uuid::NAMESPACE_OID,
        json!([
            "codex-openrouter-tool-v1",
            identity.namespace,
            identity.name
        ])
        .to_string()
        .as_bytes(),
    );
    format!("codex_or_{}", digest.simple())
}

fn freeform(tool: &FreeformTool) -> Value {
    let mut description = format!(
        "{}\n\nThe required string parameter `input` carries the exact raw text described by the tool's {} grammar ({}), not a JSON-encoded script.",
        tool.description, tool.format.syntax, tool.format.r#type,
    );
    if tool.format.definition.len() <= 4096 {
        description.push_str("\n\n");
        description.push_str(&tool.format.definition);
    }
    json!({
        "type": "function", "name": tool.name, "description": description,
        "strict": true,
        "parameters": {
            "type": "object", "properties": {"input": {"type": "string"}},
            "required": ["input"], "additionalProperties": false
        }
    })
}

impl Projection {
    pub(super) fn request(request: &mut ResponsesApiRequest, specs: &[ToolSpec]) -> Result<Self> {
        let mut projection = Self {
            tools: Vec::new(),
            freeform_items: HashSet::new(),
        };
        let mut wire = Vec::new();
        for spec in specs {
            match spec {
                ToolSpec::Function(tool) => projection.push(
                    &mut wire,
                    serde_json::to_value(spec)?,
                    ToolName::new(None, tool.name.clone()),
                    Input::Function,
                )?,
                ToolSpec::Freeform(tool) => projection.push(
                    &mut wire,
                    freeform(tool),
                    ToolName::new(None, tool.name.clone()),
                    Input::Freeform,
                )?,
                ToolSpec::Namespace(namespace) => {
                    for (index, tool) in namespace.tools.iter().enumerate() {
                        let (mut value, name, input) = match tool {
                            ResponsesApiNamespaceTool::Function(tool) => (
                                serde_json::to_value(ToolSpec::Function(tool.clone()))?,
                                &tool.name,
                                Input::Function,
                            ),
                            ResponsesApiNamespaceTool::Custom(tool) => {
                                (freeform(tool), &tool.name, Input::Freeform)
                            }
                        };
                        if index == 0 && !namespace.description.is_empty() {
                            value["description"] = json!(format!(
                                "{}\n\n{}",
                                namespace.description,
                                value["description"].as_str().ok_or_else(|| {
                                    CodexErr::Fatal(
                                        "OpenRouter function tool has no description".into(),
                                    )
                                })?
                            ));
                        }
                        projection.push(
                            &mut wire,
                            value,
                            ToolName::new(Some(namespace.name.clone()), name.clone()),
                            input,
                        )?;
                    }
                }
                ToolSpec::ToolSearch { .. } | ToolSpec::WebSearch { .. } => {
                    return Err(CodexErr::InvalidRequest(
                        "OpenAI hosted tools cannot be sent to OpenRouter".into(),
                    ));
                }
            }
        }
        request.tools = Some(ResponsesApiTools::from(Arc::from(
            serde_json::value::to_raw_value(&wire)?,
        )));
        // The function inventory replaces Responses Lite's developer tool bundle.
        request
            .input
            .retain(|item| !matches!(item, ResponseItem::AdditionalTools { .. }));
        let calls = request
            .input
            .iter()
            .filter_map(|item| match item {
                ResponseItem::CustomToolCall {
                    call_id,
                    name,
                    namespace,
                    ..
                }
                | ResponseItem::FunctionCall {
                    call_id,
                    name,
                    namespace,
                    ..
                } => Some((
                    call_id.clone(),
                    alias(&ToolName::new(namespace.clone(), name.clone())),
                )),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let input = std::mem::take(&mut request.input);
        request.input = hoist_images(input.into_iter().map(|item| project_input(item, &calls)));
        Ok(projection)
    }

    fn push(
        &mut self,
        wire: &mut Vec<Value>,
        mut value: Value,
        identity: ToolName,
        input: Input,
    ) -> Result<()> {
        let alias = alias(&identity);
        if self.tools.iter().any(|tool| tool.alias == alias) {
            return Err(CodexErr::InvalidRequest(format!(
                "Duplicate OpenRouter tool identity: {identity}"
            )));
        }
        value["name"] = json!(alias);
        wire.push(value);
        self.tools.push(Tool {
            identity,
            alias,
            input,
        });
        Ok(())
    }

    fn item(&self, item: ResponseItem, added: bool) -> ResponseItem {
        let ResponseItem::FunctionCall {
            encrypted_content,
            status,
            id,
            mut name,
            mut namespace,
            arguments,
            encrypted_function_args,
            call_id,
            internal_chat_message_metadata_passthrough,
        } = item
        else {
            return item;
        };
        let Some(tool) = self.tools.iter().find(|tool| tool.alias == name) else {
            return ResponseItem::FunctionCall {
                encrypted_content,
                status,
                id,
                name,
                namespace,
                arguments,
                encrypted_function_args,
                call_id,
                internal_chat_message_metadata_passthrough,
            };
        };
        name.clone_from(&tool.identity.name);
        namespace.clone_from(&tool.identity.namespace);
        if matches!(tool.input, Input::Freeform) {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                input: String,
            }
            let input = if added {
                Some(String::new())
            } else {
                serde_json::from_str::<Arguments>(&arguments)
                    .ok()
                    .map(|arguments| arguments.input)
            };
            if let Some(input) = input {
                return ResponseItem::CustomToolCall {
                    id,
                    status,
                    encrypted_content,
                    call_id,
                    name,
                    namespace,
                    input,
                    internal_chat_message_metadata_passthrough,
                };
            }
            // A malformed wrapper has no canonical freeform input. Retain the
            // function call so the registry rejects its kind with a paired error;
            // never turn a damaged JSON envelope into executable script text.
        }
        ResponseItem::FunctionCall {
            encrypted_content,
            status,
            id,
            name,
            namespace,
            arguments,
            encrypted_function_args,
            call_id,
            internal_chat_message_metadata_passthrough,
        }
    }

    pub(super) fn event(&mut self, event: ResponseEvent) -> Vec<ResponseEvent> {
        match event {
            ResponseEvent::OutputItemAdded(item) => {
                let item = self.item(item, true);
                if let ResponseItem::CustomToolCall { id: Some(id), .. } = &item {
                    self.freeform_items.insert(id.clone());
                }
                vec![ResponseEvent::OutputItemAdded(item)]
            }
            ResponseEvent::ToolCallInputDelta { ref item_id, .. }
                if self.freeform_items.contains(item_id) =>
            {
                Vec::new()
            }
            ResponseEvent::OutputItemDone(item) => {
                let item = self.item(item, false);
                let mut events = Vec::new();
                if let ResponseItem::CustomToolCall {
                    id: Some(id),
                    call_id,
                    input,
                    ..
                } = &item
                {
                    events.push(ResponseEvent::ToolCallInputDelta {
                        item_id: id.clone(),
                        call_id: Some(call_id.clone()),
                        delta: input.clone(),
                    });
                }
                if let Some(id) = item.id() {
                    self.freeform_items.remove(id);
                }
                events.push(ResponseEvent::OutputItemDone(item));
                events
            }
            event => vec![event],
        }
    }

    pub(super) fn settled(&self, event: ResponseEvent) -> ResponseEvent {
        match event {
            ResponseEvent::OutputItemAdded(item) => {
                ResponseEvent::OutputItemAdded(self.item(item, true))
            }
            ResponseEvent::OutputItemDone(item) => {
                ResponseEvent::OutputItemDone(self.item(item, false))
            }
            event => event,
        }
    }
}

fn project_input(item: ResponseItem, calls: &HashMap<String, String>) -> ResponseItem {
    match item {
        ResponseItem::CustomToolCall {
            encrypted_content,
            status,
            id,
            name,
            namespace,
            input,
            call_id,
            internal_chat_message_metadata_passthrough,
            ..
        } => ResponseItem::FunctionCall {
            encrypted_content,
            status,
            id,
            name: alias(&ToolName::new(namespace, name)),
            namespace: None,
            arguments: json!({"input": input}).to_string(),
            encrypted_function_args: None,
            call_id,
            internal_chat_message_metadata_passthrough,
        },
        ResponseItem::CustomToolCallOutput {
            id,
            call_id,
            name,
            output,
            internal_chat_message_metadata_passthrough,
        } => ResponseItem::FunctionCallOutput {
            id,
            name: name.map(|name| {
                calls
                    .get(&call_id)
                    .cloned()
                    .unwrap_or_else(|| alias(&ToolName::new(None, name)))
            }),
            call_id: Some(call_id),
            namespace: None,
            output,
            internal_chat_message_metadata_passthrough,
        },
        ResponseItem::FunctionCall {
            encrypted_content,
            status,
            id,
            name,
            namespace,
            arguments,
            encrypted_function_args,
            call_id,
            internal_chat_message_metadata_passthrough,
        } => ResponseItem::FunctionCall {
            encrypted_content,
            status,
            id,
            name: alias(&ToolName::new(namespace, name)),
            namespace: None,
            arguments,
            encrypted_function_args,
            call_id,
            internal_chat_message_metadata_passthrough,
        },
        ResponseItem::FunctionCallOutput {
            id,
            call_id,
            name,
            namespace,
            output,
            internal_chat_message_metadata_passthrough,
        } => ResponseItem::FunctionCallOutput {
            id,
            name: name.map(|name| {
                call_id
                    .as_ref()
                    .and_then(|id| calls.get(id))
                    .cloned()
                    .unwrap_or_else(|| alias(&ToolName::new(namespace, name)))
            }),
            call_id,
            namespace: None,
            output,
            internal_chat_message_metadata_passthrough,
        },
        item => item,
    }
}

/// Chat-completions hosts drop, misread or reject images in tool results, yet read
/// them in user messages. Each run of tool outputs therefore cedes its images to one
/// user message after the run, leaving numbered stand-ins: a user message inside the
/// run would part the later outputs from their calls.
fn hoist_images(items: impl Iterator<Item = ResponseItem>) -> Vec<ResponseItem> {
    let mut wire = Vec::new();
    let mut images = Vec::new();
    for mut item in items {
        match &mut item {
            ResponseItem::FunctionCallOutput { output, .. } => {
                if let FunctionCallOutputBody::ContentItems(parts) = &mut output.body {
                    *parts = std::mem::take(parts)
                        .into_iter()
                        .map(|part| match part {
                            FunctionCallOutputContentItem::InputImage { image, detail } => {
                                images.push(ContentItem::InputImage { image, detail });
                                FunctionCallOutputContentItem::InputText {
                                    text: format!(
                                        "[image {}: attached after the tool results]",
                                        images.len()
                                    ),
                                }
                            }
                            part => part,
                        })
                        .collect();
                }
            }
            _ => wire.extend(attachment(std::mem::take(&mut images))),
        }
        wire.push(item);
    }
    wire.extend(attachment(images));
    wire
}

/// The user message carrying a run's hoisted `images`, if it has any.
fn attachment(images: Vec<ContentItem>) -> Option<ResponseItem> {
    if images.is_empty() {
        return None;
    }
    let preamble = ContentItem::InputText {
        text: concat!(
            "<harness_attachment>\n",
            "Images from the tool results above, by number. ",
            "The harness moved them here; this is not user input.\n",
            "</harness_attachment>",
        )
        .into(),
    };
    let content = std::iter::once(preamble)
        .chain((1..).zip(images).flat_map(|(number, image)| {
            [
                ContentItem::InputText {
                    text: format!("[image {number}]"),
                },
                image,
            ]
        }))
        .collect();
    Some(ResponseItem::Message {
        id: None,
        role: "user".into(),
        content,
        encrypted_content: None,
        status: None,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

#[cfg(test)]
#[path = "client_openrouter_tools_tests.rs"]
mod tests;
