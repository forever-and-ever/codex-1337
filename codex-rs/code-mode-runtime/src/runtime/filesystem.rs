//! Familiar JS helpers over the executor-bound filesystem tool, not the V8 host.

use super::RuntimeState;

const FACTORY: &str = r"(file => Object.freeze({
    async readFile(path, options = {}) {
        if (typeof options === 'string') options = {encoding: options};
        const result = await file({...options, operation: 'read', path});
        return typeof result === 'string' ? result : new Uint8Array(result);
    },
    async writeFile(path, data, options = {}) {
        if (data instanceof Uint8Array) data = Array.from(data);
        else if (typeof data !== 'string') throw new TypeError('fs.writeFile data must be a string or Uint8Array');
        await file({...options, operation: 'write', path, data});
    }
}))";

pub(super) fn helper<'s>(
    scope: &mut v8::PinScope<'s, '_>,
) -> Result<Option<v8::Local<'s, v8::Value>>, String> {
    let Some(index) = scope.get_slot::<RuntimeState>().and_then(|state| {
        state
            .enabled_tools
            .iter()
            .position(|tool| tool.tool_name.is_default_namespace() && tool.tool_name.name == "fs")
    }) else {
        return Ok(None);
    };
    let fail = || "failed to install fs helpers".to_string();
    let file = super::globals::tool_function(scope, index)?.into();
    let source = v8::String::new(scope, FACTORY).ok_or_else(fail)?;
    let factory = v8::Script::compile(scope, source, /*origin*/ None)
        .and_then(|script| script.run(scope))
        .and_then(|value| v8::Local::<v8::Function>::try_from(value).ok())
        .ok_or_else(fail)?;
    let receiver = v8::undefined(scope).into();
    factory
        .call(scope, receiver, &[file])
        .map(Some)
        .ok_or_else(fail)
}
