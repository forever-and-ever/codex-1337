//! `sh`: run commands through `tools.exec_command` and print what they settle to.
//!
//! The helper is JavaScript over two natives no script can reach: one prints settled calls the
//! way `text` prints native results, the other keeps the cell open until each `sh` call has
//! printed, so an unawaited call still reports.

use codex_code_mode_protocol::FunctionCallOutputContentItem;
use codex_code_mode_protocol::is_exec_command;
use codex_protocol::ExecutionOutputMetadata;
use codex_protocol::SettledExecution;

use super::RuntimeEvent;
use super::RuntimeState;
use super::native_result::native_result;
use super::value::serialize_output_text;
use super::value::throw_type_error;
use super::value::value_to_error_text;

const FACTORY: &str = r#"(exec, print, hold) => (...commands) => hold((async () => {
  const settled = await Promise.allSettled(
    commands.map(async (command) => exec(typeof command === "string" ? { cmd: command } : command)),
  );
  print(settled);
  return settled.map((call) => (call.status === "fulfilled" ? call.value : call.reason));
})())"#;

/// `sh` over the cell's `exec_command`, or nothing when the cell has none.
pub(super) fn helper<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    tools: v8::Local<'s, v8::Object>,
) -> Result<Option<v8::Local<'s, v8::Value>>, String> {
    let Some(global_name) = scope.get_slot::<RuntimeState>().and_then(|state| {
        state
            .enabled_tools
            .iter()
            .find(|tool| is_exec_command(&tool.tool_name))
            .map(|tool| tool.global_name.clone())
    }) else {
        return Ok(None);
    };
    let fail = || "failed to install sh".to_string();
    let key = v8::String::new(scope, &global_name).ok_or_else(fail)?;
    let exec = tools.get(scope, key.into()).ok_or_else(fail)?;
    let print = native(scope, print_callback).ok_or_else(fail)?;
    let hold = native(scope, hold_callback).ok_or_else(fail)?;
    let source = v8::String::new(scope, FACTORY).ok_or_else(fail)?;
    let factory = v8::Script::compile(scope, source, /*origin*/ None)
        .and_then(|script| script.run(scope))
        .and_then(|factory| v8::Local::<v8::Function>::try_from(factory).ok())
        .ok_or_else(fail)?;
    let receiver = v8::undefined(scope).into();
    factory
        .call(scope, receiver, &[exec, print.into(), hold.into()])
        .map(Some)
        .ok_or_else(fail)
}

/// Whether an `sh` call is still running; the cell outlives it unless the script exited.
pub(super) fn running(scope: &mut v8::PinScope<'_, '_>) -> bool {
    let Some(state) = scope.get_slot_mut::<RuntimeState>() else {
        return false;
    };
    if state.exit_requested.is_some() {
        return false;
    }
    let calls = std::mem::take(&mut state.sh_calls);
    let calls = calls
        .into_iter()
        .filter(|call| v8::Local::new(scope, call).state() == v8::PromiseState::Pending)
        .collect::<Vec<_>>();
    let running = !calls.is_empty();
    if let Some(state) = scope.get_slot_mut::<RuntimeState>() {
        state.sh_calls = calls;
    }
    running
}

fn native<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) -> Option<v8::Local<'s, v8::Function>> {
    v8::FunctionTemplate::new(scope, callback).get_function(scope)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 requires this callback ABI, whose argument handle only exposes borrowed access"
)]
fn print_callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _retval: v8::ReturnValue<'_, v8::Value>,
) {
    let settled = args.get(0);
    let Ok(calls) = v8::Local::<v8::Array>::try_from(settled) else {
        throw_type_error(scope, "sh settled no calls");
        return;
    };
    if calls.length() == 0 {
        return;
    }
    let calls = (0..calls.length())
        .map(|index| {
            let call = calls.get_index(scope, index)?;
            settle(scope, call)
        })
        .collect::<Vec<_>>();
    let rendered = match calls.as_slice() {
        [call] => call
            .as_ref()
            .and_then(|call| call.render(ExecutionOutputMetadata::Hidden))
            .map(|text| vec![text]),
        _ => SettledExecution::render_batch_sections(calls, ExecutionOutputMetadata::Hidden),
    };
    let mut sections = match rendered.map_or_else(
        || serialize_output_text(scope, settled).map(|text| vec![text]),
        Ok,
    ) {
        Ok(sections) => sections,
        Err(error_text) => {
            throw_type_error(scope, &error_text);
            return;
        }
    };
    if let Some(state) = scope.get_slot::<RuntimeState>()
        && state.exit_requested.is_none()
    {
        let item = if sections.len() == 1 {
            FunctionCallOutputContentItem::InputText {
                text: sections.remove(0),
                max_tokens: None,
            }
        } else {
            FunctionCallOutputContentItem::InputTextBatch {
                sections,
                max_tokens: None,
            }
        };
        let _ = state
            .event_tx
            .blocking_send(RuntimeEvent::ContentItem(item));
    }
}

/// One `Promise.allSettled` entry; `None` when its value is no native result.
fn settle(
    scope: &mut v8::PinScope<'_, '_>,
    call: v8::Local<'_, v8::Value>,
) -> Option<SettledExecution> {
    let call = v8::Local::<v8::Object>::try_from(call).ok()?;
    let status = v8::String::new(scope, "status")?;
    let fulfilled = call.get(scope, status.into())?.to_rust_string_lossy(scope) == "fulfilled";
    let field = v8::String::new(scope, if fulfilled { "value" } else { "reason" })?;
    let field = call.get(scope, field.into())?;
    if fulfilled {
        native_result(scope, field).map(SettledExecution::Fulfilled)
    } else {
        Some(SettledExecution::Rejected(value_to_error_text(
            scope, field,
        )))
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "V8 requires this callback ABI, whose argument handle only exposes borrowed access"
)]
fn hold_callback(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    mut retval: v8::ReturnValue<'_, v8::Value>,
) {
    let call = args.get(0);
    let Ok(promise) = v8::Local::<v8::Promise>::try_from(call) else {
        throw_type_error(scope, "sh holds only promises");
        return;
    };
    let promise = v8::Global::new(scope, promise);
    if let Some(state) = scope.get_slot_mut::<RuntimeState>()
        && state.exit_requested.is_none()
    {
        state.sh_calls.push(promise);
    }
    retval.set(call);
}
