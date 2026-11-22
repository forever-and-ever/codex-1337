use codex_code_mode_protocol::EnabledToolMetadata;
use codex_code_mode_protocol::normalize_code_mode_identifier;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use super::RuntimeState;
use super::value::json_to_v8;
use super::value::throw_type_error;
use super::value::v8_value_to_json;

const DEFAULT_SEARCH_LIMIT: usize = 8;
const MAX_SEARCH_LIMIT: usize = 20;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchOptions {
    namespace: Option<String>,
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescribeOptions {
    #[serde(default)]
    detail: DescriptionDetail,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DescriptionDetail {
    #[default]
    Contract,
    Output,
    Schema,
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 requires this callback ABI, whose argument handle only exposes borrowed access"
)]
pub(super) fn describe_tool_callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    mut retval: v8::ReturnValue<'_, v8::Value>,
) {
    if !args.get(0).is_string() {
        throw_type_error(scope, "describe_tool name must be a string");
        return;
    }
    let name = args.get(0).to_rust_string_lossy(scope);
    let options = if args.get(1).is_undefined() {
        DescribeOptions::default()
    } else {
        match v8_value_to_json(scope, args.get(1)).and_then(|value| {
            serde_json::from_value::<DescribeOptions>(value.unwrap_or_default())
                .map_err(|error| error.to_string())
        }) {
            Ok(options) => options,
            Err(error) => {
                throw_type_error(scope, &format!("invalid describe_tool options: {error}"));
                return;
            }
        }
    };
    let Some(state) = scope.get_slot::<RuntimeState>() else {
        throw_type_error(scope, "runtime state unavailable");
        return;
    };
    let description = match resolve_tool(&name, &state.enabled_tools) {
        Ok(tool) => match options.detail {
            DescriptionDetail::Contract => tool.description.clone(),
            DescriptionDetail::Output => tool.describe_output(),
            DescriptionDetail::Schema => tool.describe_schema(),
        },
        Err(error) => {
            throw_type_error(scope, &error);
            return;
        }
    };
    let Some(description) = v8::String::new(scope, &description) else {
        throw_type_error(scope, "failed to allocate tool description");
        return;
    };
    retval.set(description.into());
}

fn resolve_tool<'a>(
    name: &str,
    tools: &'a [Arc<EnabledToolMetadata>],
) -> Result<&'a EnabledToolMetadata, String> {
    if let Some(tool) = tools.iter().find(|tool| tool.global_name == name) {
        return Ok(tool);
    }
    let alias = name.strip_prefix("tools.").unwrap_or(name);
    let rewritten = alias.replace('.', "__");
    let matches = tools
        .iter()
        .filter(|tool| {
            tool.global_name == alias
                || tool.global_name == rewritten
                || tool.tool_name.to_string() == alias
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [tool] => Ok(tool),
        [] => {
            if tools.is_empty() {
                return Err(format!("unknown tool `{name}`: no tools are enabled"));
            }
            let mut suggestions = tools.iter().collect::<Vec<_>>();
            suggestions.sort_by_key(|tool| {
                let common = tool
                    .global_name
                    .chars()
                    .zip(rewritten.chars())
                    .take_while(|(left, right)| left == right)
                    .count()
                    + tool
                        .global_name
                        .chars()
                        .rev()
                        .zip(rewritten.chars().rev())
                        .take_while(|(left, right)| left == right)
                        .count();
                (std::cmp::Reverse(common), &tool.global_name)
            });
            Err(format!(
                "unknown tool `{name}`. Did you mean: {}? Use search_tools to find an enabled canonical name.",
                suggestions
                    .iter()
                    .take(3)
                    .map(|tool| tool.global_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        _ => Err(format!(
            "ambiguous tool alias `{name}`. Did you mean: {}? Use an exact canonical name.",
            matches
                .iter()
                .take(3)
                .map(|tool| tool.global_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 requires this callback ABI, whose argument handle only exposes borrowed access"
)]
pub(super) fn search_tools_callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    mut retval: v8::ReturnValue<'_, v8::Value>,
) {
    if !args.get(0).is_string() {
        throw_type_error(scope, "search_tools query must be a string");
        return;
    }
    let query = args
        .get(0)
        .to_rust_string_lossy(scope)
        .trim()
        .to_lowercase();
    let options = if args.get(1).is_undefined() {
        SearchOptions::default()
    } else {
        match v8_value_to_json(scope, args.get(1)).and_then(|value| {
            serde_json::from_value(value.unwrap_or_default()).map_err(|error| error.to_string())
        }) {
            Ok(options) => options,
            Err(error) => {
                throw_type_error(scope, &format!("invalid search_tools options: {error}"));
                return;
            }
        }
    };
    let limit = options
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .min(MAX_SEARCH_LIMIT);
    if limit == 0 {
        throw_type_error(scope, "search_tools limit must be a positive integer");
        return;
    }
    let Some(state) = scope.get_slot::<RuntimeState>() else {
        throw_type_error(scope, "runtime state unavailable");
        return;
    };
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    let mut ranked = state
        .enabled_tools
        .iter()
        .filter_map(|tool| {
            if options.namespace.as_ref().is_some_and(|namespace| {
                !normalize_code_mode_identifier(
                    tool.tool_name.namespace.as_deref().unwrap_or("functions"),
                )
                .starts_with(namespace)
                    && !tool.global_name.starts_with(namespace)
            }) {
                return None;
            }
            let name = tool.global_name.to_lowercase();
            let summary = tool.summary.to_lowercase();
            if !tokens
                .iter()
                .all(|token| name.contains(token) || summary.contains(token))
            {
                return None;
            }
            let rank = if name == query {
                0
            } else if name.contains(&query) {
                1
            } else if tokens.iter().all(|token| name.contains(token)) {
                2
            } else {
                3
            };
            Some((rank, tool))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_rank, left), (right_rank, right)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| left.global_name.cmp(&right.global_name))
    });
    let matches = ranked
        .iter()
        .skip(options.offset)
        .take(limit)
        .map(|(_, tool)| json!({"name": tool.global_name, "description": tool.summary}))
        .collect::<Vec<_>>();
    let next_offset = options.offset.saturating_add(matches.len());
    let truncated = next_offset < ranked.len();
    let result = json!({
        "matches": matches,
        "truncated": truncated,
        "next_offset": truncated.then_some(next_offset),
    });
    let Some(result) = json_to_v8(scope, &result) else {
        throw_type_error(scope, "failed to allocate tool search result");
        return;
    };
    retval.set(result);
}
