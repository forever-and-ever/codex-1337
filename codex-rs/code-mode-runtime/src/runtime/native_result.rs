use codex_protocol::ExecutionOutputMetadata;
use codex_protocol::NativeExecutionResult;
use serde_json::Map;
use serde_json::Value;

use super::value::serialize_output_text;
use super::value::v8_value_to_json;

fn identity<'s>(scope: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "codex.nativeExecutionResult");
    v8::Private::for_api(scope, name)
}

pub(super) fn brand(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
) -> Result<(), String> {
    if let Ok(object) = v8::Local::<v8::Object>::try_from(value) {
        let key = identity(scope);
        let marker = v8::Boolean::new(scope, /*value*/ true);
        if object.set_private(scope, key, marker.into()) != Some(true) {
            return Err("failed to mark native execution result".to_string());
        }
    }
    Ok(())
}

pub(super) fn serialize_text(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
    options: v8::Local<'_, v8::Value>,
) -> Result<(String, bool), String> {
    let metadata = output_metadata(scope, options)?;
    if let Some(result) = native_result(scope, value)
        && let Some(text) = result.render(metadata)
    {
        return Ok((text, true));
    }
    if let Some(text) = native_batch(scope, value, metadata) {
        return Ok((text, true));
    }
    serialize_output_text(scope, value).map(|text| (text, false))
}

fn native_batch(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
    metadata: ExecutionOutputMetadata,
) -> Option<String> {
    let array = v8::Local::<v8::Array>::try_from(value).ok()?;
    let length = array.length();
    if length == 0 {
        return None;
    }
    let to_json = v8::String::new(scope, "toJSON")?;
    if array.has(scope, to_json.into())? {
        return None;
    }
    let keys = array.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
            property_filter: v8::PropertyFilter::ALL_PROPERTIES,
            ..Default::default()
        },
    )?;
    // `length` and every dense index are the only admitted array properties.
    if u64::from(keys.length()) != u64::from(length) + 1 {
        return None;
    }
    let value_key = v8::String::new(scope, "value")?;
    NativeExecutionResult::render_batch(
        (0..length).map(|index| {
            v8::scope!(let scope, scope);
            let key = v8::String::new(scope, &index.to_string())?;
            let descriptor = array.get_own_property_descriptor(scope, key.into())?;
            let descriptor = v8::Local::<v8::Object>::try_from(descriptor).ok()?;
            if descriptor.has_own_property(scope, value_key.into()) != Some(true) {
                return None;
            }
            let value = descriptor.get(scope, value_key.into())?;
            native_result(scope, value)
        }),
        metadata,
    )
}

fn output_metadata(
    scope: &mut v8::PinScope<'_, '_>,
    options: v8::Local<'_, v8::Value>,
) -> Result<ExecutionOutputMetadata, String> {
    if options.is_undefined() {
        return Ok(ExecutionOutputMetadata::Hidden);
    }
    let invalid =
        || "text/notify options must be an object with optional boolean metadata".to_string();
    if options.is_array() || options.is_null() || options.is_function() {
        return Err(invalid());
    }
    let object = v8::Local::<v8::Object>::try_from(options).map_err(|_| invalid())?;
    let keys = object
        .get_own_property_names(
            scope,
            v8::GetPropertyNamesArgs {
                property_filter: v8::PropertyFilter::ALL_PROPERTIES,
                ..Default::default()
            },
        )
        .ok_or_else(invalid)?;
    for index in 0..keys.length() {
        let key = keys.get_index(scope, index).ok_or_else(invalid)?;
        if !key.is_string() || key.to_rust_string_lossy(scope) != "metadata" {
            return Err(invalid());
        }
    }
    let key = v8::String::new(scope, "metadata").ok_or_else(invalid)?;
    let metadata = object.get(scope, key.into()).ok_or_else(invalid)?;
    if metadata.is_undefined() {
        Ok(ExecutionOutputMetadata::Hidden)
    } else if metadata.is_boolean() {
        Ok(if metadata.boolean_value(scope) {
            ExecutionOutputMetadata::Included
        } else {
            ExecutionOutputMetadata::Hidden
        })
    } else {
        Err(invalid())
    }
}

fn native_result(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
) -> Option<NativeExecutionResult> {
    let object = v8::Local::<v8::Object>::try_from(value).ok()?;
    let identity = identity(scope);
    if object.has_private(scope, identity) != Some(true) {
        return None;
    }
    let to_json = v8::String::new(scope, "toJSON")?;
    if object.has(scope, to_json.into())? {
        return None;
    }
    // Accessors and custom serialization opt out. Inspect data properties once,
    // without retaining another body or invoking getters during presentation.
    let keys = object.get_own_property_names(
        scope,
        v8::GetPropertyNamesArgs {
            property_filter: v8::PropertyFilter::ALL_PROPERTIES,
            ..Default::default()
        },
    )?;
    let value_key = v8::String::new(scope, "value")?;
    let mut fields = Map::new();
    for index in 0..keys.length() {
        let key = keys.get_index(scope, index)?;
        if !key.is_string() {
            return None;
        }
        let name = key.to_rust_string_lossy(scope);
        if !matches!(
            name.as_str(),
            "output"
                | "exit_code"
                | "session_id"
                | "chunk_id"
                | "wall_time_seconds"
                | "original_token_count"
        ) {
            return None;
        }
        let key = v8::Local::<v8::Name>::try_from(key).ok()?;
        let descriptor = object.get_own_property_descriptor(scope, key)?;
        let descriptor = v8::Local::<v8::Object>::try_from(descriptor).ok()?;
        if descriptor.has_own_property(scope, value_key.into()) != Some(true) {
            return None;
        }
        let field = descriptor.get(scope, value_key.into())?;
        if !field.is_string() && !field.is_number() && !field.is_null() {
            return None;
        }
        let decoded = v8_value_to_json(scope, field).ok()??;
        if field.is_number() && decoded.is_null() {
            return None;
        }
        let field = decoded;
        if name == "original_token_count"
            && field
                .as_u64()
                .is_some_and(|count| count > 9_007_199_254_740_991)
        {
            return None;
        }
        fields.insert(name, field);
    }
    serde_json::from_value(Value::Object(fields)).ok()
}
