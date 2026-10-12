use codex_code_mode_protocol::normalize_code_mode_identifier;
use serde::Deserialize;
use serde_json::json;

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

pub(super) fn describe_tool_callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue<v8::Value>,
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
    let description = scope.get_slot::<RuntimeState>().and_then(|state| {
        state
            .enabled_tools
            .iter()
            .find(|tool| tool.global_name == name)
            .map(|tool| match options.detail {
                DescriptionDetail::Contract => tool.description.clone(),
                DescriptionDetail::Output => tool.describe_output(),
                DescriptionDetail::Schema => tool.describe_schema(),
            })
    });
    let Some(description) = description else {
        throw_type_error(scope, &format!("unknown tool `{name}`"));
        return;
    };
    let Some(description) = v8::String::new(scope, &description) else {
        throw_type_error(scope, "failed to allocate tool description");
        return;
    };
    retval.set(description.into());
}

pub(super) fn search_tools_callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut retval: v8::ReturnValue<v8::Value>,
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
