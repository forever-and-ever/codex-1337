use crate::ApiError;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use serde_json::value::RawValue;

#[derive(Deserialize)]
struct FunctionInput {
    arguments: Box<RawValue>,
}

/// Decode the schema envelope at the same boundary that defines it. Empty start
/// blocks are allowed, but the stream must reject an empty completed argument.
pub(super) fn function_arguments(input: &Value, raw: &str) -> Result<String, ApiError> {
    if input.as_object().is_some_and(Map::is_empty) && raw.is_empty() {
        return Ok(String::new());
    }
    let invalid = || ApiError::NativeStream("invalid Anthropic function arguments envelope".into());
    if input.as_object().is_none_or(|object| object.len() != 1) {
        return Err(invalid());
    }
    let arguments = input
        .get("arguments")
        .filter(|value| value.is_object())
        .ok_or_else(invalid)?;
    if raw.is_empty() {
        Ok(arguments.to_string())
    } else {
        // Preserve inner JSON bytes, not the provider envelope or reserialization.
        Ok(serde_json::from_str::<FunctionInput>(raw)
            .map_err(|_| invalid())?
            .arguments
            .get()
            .to_owned())
    }
}

/// Preserve a function's complete schema beneath the native object envelope.
/// Anthropic rejects root composition keywords; enveloping every function also
/// makes completed-call replay independent of its current declaration.
pub fn anthropic_function_input_schema(mut schema: Value) -> Result<Value, ApiError> {
    relocate_references(&mut schema)?;
    Ok(json!({
        "type": "object",
        "properties": {"arguments": schema},
        "required": ["arguments"],
        "additionalProperties": false,
    }))
}

fn relocate_references(schema: &mut Value) -> Result<(), ApiError> {
    let Some(object) = schema.as_object_mut() else {
        return Ok(());
    };
    if [
        "$id",
        "id",
        "$anchor",
        "$dynamicAnchor",
        "$dynamicRef",
        "$recursiveAnchor",
        "$recursiveRef",
    ]
    .iter()
    .any(|keyword| object.contains_key(*keyword))
    {
        return Err(ApiError::InvalidRequest {
            message: "Anthropic function schemas cannot relocate scoped IDs or anchors".into(),
        });
    }
    if let Some(reference) = object.get_mut("$ref") {
        let pointer = reference
            .as_str()
            .filter(|value| *value == "#" || value.starts_with("#/"))
            .ok_or_else(|| ApiError::InvalidRequest {
                message: "Anthropic function schemas require local JSON-pointer references".into(),
            })?;
        *reference = Value::String(format!("#/properties/arguments{}", &pointer[1..]));
    }
    // Visit schema-bearing keywords only: enum/const/default values are data,
    // and a property named `$id` must not be mistaken for a schema resource ID.
    for (keyword, child) in object {
        match keyword.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
            | "dependencies" => {
                if let Some(children) = child.as_object_mut() {
                    for child in children.values_mut() {
                        relocate_references(child)?;
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                if let Some(children) = child.as_array_mut() {
                    for child in children {
                        relocate_references(child)?;
                    }
                }
            }
            "items"
            | "additionalItems"
            | "additionalProperties"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "contains"
            | "propertyNames"
            | "not"
            | "if"
            | "then"
            | "else"
            | "contentSchema" => {
                if let Some(children) = child.as_array_mut() {
                    for child in children {
                        relocate_references(child)?;
                    }
                } else {
                    relocate_references(child)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
