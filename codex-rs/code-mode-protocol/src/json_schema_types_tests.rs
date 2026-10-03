use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn renders_recursive_local_refs_with_escaped_pointer_segments() {
    // A shared cycle must preserve both edges without recursively copying either node.
    let schema = json!({
        "type": "object",
        "properties": { "clauses": { "type": "array", "items": { "$ref": "#/$defs/Boolean~1Clause~0v1" } } },
        "$defs": {
            "Boolean/Clause~v1": {
                "type": "object",
                "properties": { "query": { "$ref": "#/$defs/Query" } }
            },
            "Query": { "oneOf": [
                { "type": "string" },
                { "type": "object", "properties": { "clauses": { "type": "array", "items": { "$ref": "#/$defs/Boolean~1Clause~0v1" } } } }
            ] }
        }
    });
    assert_eq!(render_json_schema_to_typescript(&schema), RenderedJsonSchema {
        declarations: "type T_Boolean_Clause_v1 = { query?: T_Query; };\ntype T_Query = string | { clauses?: Array<T_Boolean_Clause_v1>; };\n".to_string(),
        root_type: "{ clauses?: Array<T_Boolean_Clause_v1>; }".to_string(),
    });
}

#[test]
fn renders_ref_siblings_uri_fragments_and_all_of_precedence() {
    assert_eq!(
        render_json_schema_to_typescript(&json!({
            "$ref": "#/$defs/Foo%20Bar", "enum": ["A"],
            "$defs": { "Foo Bar": { "type": "string" } }
        })),
        RenderedJsonSchema {
            declarations: "type T_Foo_Bar = string;\n".to_string(),
            root_type: r#"(T_Foo_Bar) & ("A")"#.to_string(),
        }
    );
    assert_eq!(
        render_json_schema_to_typescript(&json!({
            "allOf": [ { "$ref": "#/$defs/Choice" }, { "type": "object", "properties": { "value": { "type": "string" } } } ],
            "$defs": { "Choice": { "oneOf": [{ "type": "string" }, { "type": "number" }] } }
        })),
        RenderedJsonSchema {
            declarations: "type T_Choice = string | number;\n".to_string(),
            root_type: "T_Choice & { value?: string; }".to_string(),
        }
    );
}

#[test]
fn leaves_local_refs_under_nested_schema_resources_unresolved() {
    let schema = json!({
        "$defs": { "Choice": { "type": "string" } },
        "type": "object",
        "properties": { "nested": {
            "$id": "urn:nested", "$defs": { "Choice": { "type": "number" } },
            "type": "object", "properties": { "value": { "$ref": "#/$defs/Choice" } }
        } }
    });
    assert_eq!(
        render_json_schema_to_typescript(&schema),
        RenderedJsonSchema::inline("{ nested?: { value?: unknown; }; }")
    );
}

#[test]
fn renders_shared_operation_targets_once_without_losing_last_variant() {
    // A SEMEDIT-like operation union repeatedly shares the same target graph.
    // Inline expansion previously spent its ref budget before the final operation.
    let schema = json!({
        "oneOf": (0..40).map(|index| json!({
            "type": "object", "properties": {
                "operation": { "const": format!("edit_{index:02}") },
                "target": { "$ref": "#/$defs/Target" }
            }, "required": ["operation", "target"], "additionalProperties": false
        })).collect::<Vec<_>>(),
        "$defs": {
            "Target": { "type": "object", "properties": {
                "path": { "type": "string", "description": "Repository-relative Rust source path." },
                "symbol": { "$ref": "#/$defs/Symbol" },
                "parent": { "anyOf": [{ "$ref": "#/$defs/Target" }, { "type": "null" }] }
            }, "required": ["path", "symbol"], "additionalProperties": false },
            "Symbol": { "type": "object", "properties": {
                "name": { "type": "string" }, "kind": { "enum": ["function", "impl", "trait"] }
            }, "required": ["name", "kind"], "additionalProperties": false }
        }
    });
    let rendered = render_json_schema_to_typescript(&schema);
    assert_eq!(
        rendered.declarations,
        "type T_Target = {\n  parent?: T_Target | null;\n  // Repository-relative Rust source path.\n  path: string;\n  symbol: T_Symbol;\n};\ntype T_Symbol = { kind: \"function\" | \"impl\" | \"trait\"; name: string; };\n"
    );
    assert!(
        rendered
            .root_type
            .ends_with("{ operation: \"edit_39\"; target: T_Target; }")
    );
    assert!(!rendered.root_type.contains("unknown"));
    let inline_target = "{\n  parent?: unknown | null;\n  // Repository-relative Rust source path.\n  path: string;\n  symbol: { kind: \"function\" | \"impl\" | \"trait\"; name: string; };\n}";
    let inline_bytes = rendered.root_type.replace("T_Target", inline_target).len();
    let alias_bytes = rendered.declarations.len() + rendered.root_type.len();
    assert!(alias_bytes * 3 < inline_bytes);
}

#[test]
fn rendered_schema_has_a_hard_size_cap() {
    for schema in [
        json!({ "type": "object", "properties": { "value": { "type": "string", "description": "x".repeat(DEFAULT_INPUT_SCHEMA_MAX_BYTES) } } }),
        json!({ "$ref": "#/$defs/Value", "$defs": { "Value": { "const": "x".repeat(DEFAULT_INPUT_SCHEMA_MAX_BYTES * RENDER_WORK_MULTIPLIER) } } }),
    ] {
        assert_eq!(
            render_json_schema_to_typescript(&schema),
            RenderedJsonSchema::inline("unknown")
        );
    }
    let mut names = BTreeSet::new();
    let schema = json!({ "$ref": "#/$defs/Value", "$defs": { "Value": { "type": "string" } } });
    assert_eq!(
        render_json_schema_to_typescript_with_budget(&schema, 1, &mut names),
        RenderedJsonSchema::inline("unknown")
    );
    assert!(names.is_empty());
}
